use crate::api::streaming::rtc::rtp;
use crate::api::streaming::rtc::bitrate::{AdaptiveBitrate, remember_healthy_rx};
use crate::settings::{StreamProfile, active_stream_profile};
use crate::streaming::video::{DecodedFrame, DecoderConfig, DirectVideoOutput, VideoDecodeWorker};
use anyhow::Result;
use bytes::Bytes;
use rtc::media_stream::MediaStreamTrackId;
use rtc::peer_connection::RTCPeerConnection;
use rtc::rtp::Packet;
use rtc::rtp_transceiver::RTCRtpReceiverId;
use std::sync::Arc;
use std::time::{Duration, Instant};

const STREAM_STATS_INTERVAL: Duration = Duration::from_secs(1);
const NACK_MAX_GAP: u16 = 48;
const NACK_MAX_PENDING: usize = 64;
const NACK_REPORT_LIMIT: usize = 24;
const NACK_GRACE: Duration = Duration::from_millis(12);
const NACK_INTERVAL: Duration = Duration::from_millis(35);
const NACK_EXPIRY: Duration = Duration::from_millis(180);

#[derive(Default)]
struct VideoStats {
    dropped: u64,
    decode_errors: u64,
    rx_payload_bytes: u64,
    nack_requests: u64,
    nack_packets: u64,
    detected_sequence_gaps: u64,
    recovered_or_late_packets: u64,
    last_sample_duration_us: Option<u64>,
    encoded_resolution: Option<(u32, u32)>,
}

pub(crate) struct VideoReceiver {
    track_id: Option<MediaStreamTrackId>,
    receiver_id: Option<RTCRtpReceiverId>,
    ssrc: Option<u32>,
    rtp: rtp::VideoRtp,
    pub(crate) decoder: VideoDecodeWorker,
    pub(crate) latest_frame: Option<(u64, DecodedFrame)>,
    next_frame_id: u64,
    pub(crate) received_packet: bool,
    last_stats_report: Instant,
    stats: VideoStats,
    bitrate: AdaptiveBitrate,
    last_decoder_error_logged: Option<Instant>,
    pending_recovery_log: bool,
    expected_video_sequence: Option<u16>,
    missing_video_packets: Vec<(u16, Instant)>,
    last_nack_report: Option<Instant>,
    last_rtp_arrival: Option<(Instant, u32)>,
    jitter_us: u64,
    profile: StreamProfile,
    last_status_damage: (u64, u64),
    clean_status_seconds: u32,
}

impl VideoReceiver {
    pub(crate) fn new(
        config: DecoderConfig,
        direct_output: Arc<DirectVideoOutput>,
    ) -> Result<Self> {
        Ok(Self {
            track_id: None,
            receiver_id: None,
            ssrc: None,
            rtp: rtp::VideoRtp::new(),
            decoder: VideoDecodeWorker::spawn(config, direct_output)?,
            latest_frame: None,
            next_frame_id: 0,
            received_packet: false,
            last_stats_report: Instant::now(),
            stats: VideoStats::default(),
            bitrate: AdaptiveBitrate::new(),
            last_decoder_error_logged: None,
            pending_recovery_log: false,
            expected_video_sequence: None,
            missing_video_packets: Vec::new(),
            last_nack_report: None,
            last_rtp_arrival: None,
            jitter_us: 0,
            profile: active_stream_profile(),
            last_status_damage: (0, 0),
            clean_status_seconds: 0,
        })
    }

    pub(crate) fn open(
        &mut self,
        track_id: MediaStreamTrackId,
        receiver_id: RTCRtpReceiverId,
        ssrc: u32,
    ) {
        self.track_id = Some(track_id);
        self.receiver_id = Some(receiver_id);
        self.ssrc = Some(ssrc);
        self.expected_video_sequence = None;
        self.missing_video_packets.clear();
        self.last_nack_report = None;
        self.last_rtp_arrival = None;
        self.jitter_us = 0;
    }

    pub(crate) fn handles(&self, track_id: &MediaStreamTrackId) -> bool {
        self.track_id.as_ref() == Some(track_id)
    }

    pub(crate) fn receive(&mut self, packet: Packet, keyframe_requested: &mut bool) {
        self.received_packet = true;
        // Measure actual received payload, not requested encoder bitrate.
        self.stats.rx_payload_bytes = self.stats.rx_payload_bytes
            .saturating_add(packet.payload.len() as u64);
        let now = Instant::now();
        self.observe_rtp_jitter(now, packet.header.timestamp);
        self.record_video_sequence(packet.header.sequence_number, now);
        let jitter_ms = self.jitter_us / 1_000;
        // Keep RTP packets in decoding order. Quality favors resilience;
        // low-latency narrows but never disables the H264 recovery window.
        let grace_ms = match self.profile {
            StreamProfile::LowLatency => (16 + jitter_ms / 2).clamp(16, 24),
            StreamProfile::Balanced => (24 + jitter_ms).clamp(24, 40),
            StreamProfile::Quality => (32 + jitter_ms).clamp(32, 48),
        };
        self.rtp.set_reorder_grace_ms(grace_ms);
        self.bitrate.packet_received();
        let sample_stats = self.rtp.receive(&self.decoder, packet, keyframe_requested);
        self.stats.dropped = self
            .stats
            .dropped
            .saturating_add(sample_stats.dropped as u64);
        self.bitrate.frame_damaged(sample_stats.dropped as u64);
        if sample_stats.source_frame_duration_us.is_some() {
            self.stats.last_sample_duration_us = sample_stats.source_frame_duration_us;
        }
        if sample_stats.encoded_resolution.is_some() {
            self.stats.encoded_resolution = sample_stats.encoded_resolution;
        }
    }

    pub(crate) fn drain_decoder(&mut self, keyframe_requested: &mut bool) {
        let mut decode_errors = 0u64;
        while let Some(result) = self
            .decoder
            .latest_result
            .lock()
            .ok()
            .and_then(|mut latest| latest.take())
        {
            match result {
                Ok(frame) => {
                    if self.pending_recovery_log {
                        crate::diagnostics::step("h264_decoder", "RECOVERED_AFTER_ERROR");
                        self.pending_recovery_log = false;
                    }
                    self.next_frame_id = self.next_frame_id.wrapping_add(1);
                    self.latest_frame = Some((self.next_frame_id, frame));
                }
                Err(error) => {
                    eprintln!("Failed to decode H264 video frame: {error}");
                    decode_errors = decode_errors.saturating_add(1);
                    *keyframe_requested = true;
                }
            }
        }

        if decode_errors > 0 {
            let now = Instant::now();
            if self.last_decoder_error_logged.is_none_or(|old| {
                now.duration_since(old) >= Duration::from_secs(3)
            }) {
                crate::diagnostics::step("h264_decoder", "RESET_AND_KEYFRAME_REQUESTED");
                self.last_decoder_error_logged = Some(now);
            }
            self.pending_recovery_log = true;
            self.bitrate.frame_damaged(decode_errors.saturating_mul(2));
            self.stats.decode_errors = self.stats.decode_errors.saturating_add(decode_errors);
            self.decoder.reset_decoder();
            self.rtp.wait_for_keyframe();
        }
    }

    /// RFC3550-style interarrival jitter approximation in microseconds.
    /// RTP video uses a 90 kHz clock; no server timestamps or identities are stored.
    fn observe_rtp_jitter(&mut self, now: Instant, timestamp: u32) {
        if let Some((last_at, last_timestamp)) = self.last_rtp_arrival {
            let elapsed_us = now.saturating_duration_since(last_at).as_micros() as i64;
            let media_us = (timestamp.wrapping_sub(last_timestamp) as i32 as i64)
                .saturating_mul(1_000_000) / 90_000;
            let delta = elapsed_us.saturating_sub(media_us).unsigned_abs().min(250_000);
            self.jitter_us = (self.jitter_us.saturating_mul(15) + delta) / 16;
        }
        self.last_rtp_arrival = Some((now, timestamp));
    }

    /// Observe sequence gaps without assuming late packets are irretrievably lost.
    /// A missing packet is discarded from this list on arrival or after 180ms.
    fn record_video_sequence(&mut self, sequence: u16, now: Instant) {
        if self.missing_video_packets.iter().any(|(seq, _)| *seq == sequence) {
            self.stats.recovered_or_late_packets = self.stats.recovered_or_late_packets.saturating_add(1);
        }
        self.missing_video_packets.retain(|(seq, at)| {
            *seq != sequence && now.duration_since(*at) <= NACK_EXPIRY
        });
        if let Some(expected) = self.expected_video_sequence {
            let distance = sequence.wrapping_sub(expected);
            if distance == 0 {
                self.expected_video_sequence = Some(sequence.wrapping_add(1));
            } else if distance < 32768 {
                if distance <= NACK_MAX_GAP {
                    self.stats.detected_sequence_gaps = self.stats.detected_sequence_gaps.saturating_add(u64::from(distance));
                    for offset in 0..distance {
                        if self.missing_video_packets.len() >= NACK_MAX_PENDING {
                            break;
                        }
                        let lost = expected.wrapping_add(offset);
                        if !self.missing_video_packets.iter().any(|(seq, _)| *seq == lost) {
                            self.missing_video_packets.push((lost, now));
                        }
                    }
                }
                self.expected_video_sequence = Some(sequence.wrapping_add(1));
            }
            // Very old/out-of-order packet: do not advance the sequence cursor.
        } else {
            self.expected_video_sequence = Some(sequence.wrapping_add(1));
        }
    }

    /// Real, bounded receiver-side RTCP NACK. The remote sender may ignore it,
    /// but this at least asks for missing packets rather than merely advertising
    /// NACK in the SDP. The peer type remains unchanged (Vita RTC compatibility).
    pub(crate) fn request_nack(&mut self, peer: &mut RTCPeerConnection, now: Instant) {
        self.missing_video_packets.retain(|(_, at)| {
            now.duration_since(*at) <= NACK_EXPIRY
        });
        if self.last_nack_report.is_some_and(|last| {
            now.duration_since(last) < NACK_INTERVAL
        }) {
            return;
        }
        let nacks: Vec<_> = self.missing_video_packets.iter()
            .filter(|(_, at)| now.duration_since(*at) >= NACK_GRACE)
            .take(NACK_REPORT_LIMIT)
            .map(|(seq, _)| {
                rtcp::transport_feedbacks::transport_layer_nack::NackPair::new(*seq)
            })
            .collect();
        if nacks.is_empty() {
            return;
        }
        if let (Some(receiver_id), Some(ssrc)) = (self.receiver_id, self.ssrc)
            && let Some(mut receiver) = peer.rtp_receiver(receiver_id)
        {
            let report_count = nacks.len() as u64;
            let nack = rtcp::transport_feedbacks::transport_layer_nack::TransportLayerNack {
                sender_ssrc: 0,
                media_ssrc: ssrc,
                nacks,
            };
            if receiver.write_rtcp(vec![Box::new(nack)]).is_ok() {
                self.last_nack_report = Some(now);
                self.stats.nack_requests = self.stats.nack_requests.saturating_add(1);
                self.stats.nack_packets = self.stats.nack_packets.saturating_add(report_count);
            }
        }
    }

    /// RTCP REMB is a bounded receiver request; the service may ignore it.
    /// Never send 1080p/1440p metadata or renegotiate a larger decoder surface.
    pub(crate) fn adapt_bitrate(&mut self, peer: &mut RTCPeerConnection, now: Instant) {
        let Some(bitrate) = self.bitrate.sample(now) else { return; };
        if let (Some(receiver_id), Some(ssrc)) = (self.receiver_id, self.ssrc)
            && let Some(mut receiver) = peer.rtp_receiver(receiver_id) {
            let feedback = rtcp::payload_feedbacks::receiver_estimated_maximum_bitrate::
                ReceiverEstimatedMaximumBitrate {
                    sender_ssrc: 0,
                    bitrate: bitrate as f32,
                    ssrcs: vec![ssrc],
                };
            if let Err(error) = receiver.write_rtcp(vec![Box::new(feedback)]) {
                eprintln!("720p bitrate feedback could not be sent: {error}");
            }
        }
    }

    pub(crate) fn request_keyframe(&self, peer: &mut RTCPeerConnection) {
        // A PLI needs both identifiers recorded when the remote video track was opened.
        if let (Some(receiver_id), Some(ssrc)) = (self.receiver_id, self.ssrc)
            && let Some(mut receiver) = peer.rtp_receiver(receiver_id)
        {
            let pli = rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication {
                sender_ssrc: 0,
                media_ssrc: ssrc,
            };
            let _ = receiver.write_rtcp(vec![Box::new(pli)]);
        }
    }

    pub(crate) fn status(&mut self, now: Instant) -> Option<String> {
        if now.duration_since(self.last_stats_report) < STREAM_STATS_INTERVAL {
            return None;
        }
        let elapsed = now.duration_since(self.last_stats_report).as_secs_f64();
        self.last_stats_report = now;
        let rx_mbps = (self.stats.rx_payload_bytes as f64) * 8.0
            / elapsed / 1_000_000.0;
        self.stats.rx_payload_bytes = 0;
        if self.stats.dropped == self.last_status_damage.0
            && self.stats.decode_errors == self.last_status_damage.1
            && self.missing_video_packets.is_empty()
            && rx_mbps >= 2.5
        {
            self.clean_status_seconds = self.clean_status_seconds.saturating_add(1);
            if self.clean_status_seconds >= 8 {
                remember_healthy_rx((rx_mbps * 1_000_000.0) as u32);
            }
        } else {
            self.clean_status_seconds = 0;
        }
        self.last_status_damage = (self.stats.dropped, self.stats.decode_errors);

        let performance = crate::streaming::video::video_performance_summary();
        let source_fps = self
            .stats
            .last_sample_duration_us
            .filter(|duration| *duration > 0)
            .map(|duration| 1_000_000 / duration)
            .unwrap_or(0);
        let encoded_resolution = self
            .stats
            .encoded_resolution
            .map(|(width, height)| format!("{width}x{height}"))
            .unwrap_or_else(|| "?".to_owned());
        Some(format!(
            "enc:{encoded_resolution} srcfps:{source_fps} {performance} rx:{rx_mbps:.1}Mbps remb:{:.1}Mbps jitter:{}ms gaps:{} late:{} nack:{}/{} wait:{} drop:{} err:{}",
            self.bitrate.current_mbps(),
            self.jitter_us / 1_000,
            self.stats.detected_sequence_gaps,
            self.stats.recovered_or_late_packets,
            self.stats.nack_requests,
            self.stats.nack_packets,
            u8::from(self.rtp.waiting_for_keyframe()),
            self.stats.dropped,
            self.stats.decode_errors,
        ))
    }
}

pub(crate) struct AudioReceiver {
    track_id: Option<MediaStreamTrackId>,
    rtp: rtp::AudioRtp,
    pub(crate) packets: Vec<Bytes>,
}

impl AudioReceiver {
    pub(crate) fn new(sample_rate: u32, payload_type: u8) -> Self {
        Self {
            track_id: None,
            rtp: rtp::AudioRtp::new(sample_rate, payload_type),
            packets: Vec::new(),
        }
    }

    pub(crate) fn open(&mut self, track_id: MediaStreamTrackId) {
        self.track_id = Some(track_id);
    }

    pub(crate) fn handles(&self, track_id: &MediaStreamTrackId) -> bool {
        self.track_id.as_ref() == Some(track_id)
    }

    pub(crate) fn receive(&mut self, packet: Packet) {
        self.rtp.receive(packet, &mut self.packets);
    }
}
