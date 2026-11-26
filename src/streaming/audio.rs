use anyhow::{Context, Result, bail};
use bytes::Bytes;
use sdl2::audio::{AudioQueue, AudioSpecDesired};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use crate::settings::{StreamProfile, active_stream_profile};
use crate::streaming::video::metrics::METRICS;
use std::ptr::NonNull;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};

pub const AUDIO_SAMPLE_RATE: i32 = 48_000;
const AUDIO_CHANNELS: usize = 2;

const AUDIO_BYTES_PER_SECOND: u32 = AUDIO_SAMPLE_RATE as u32 * AUDIO_CHANNELS as u32 * 2;
// Keep audio close to live video; the previous 240ms cap made audible events
// noticeably late after jitter. Retain conservative startup headroom for 2.4GHz.
fn pcm_queue_cap_bytes(target_ms: u32) -> u32 {
    AUDIO_BYTES_PER_SECOND * (target_ms.clamp(50, 110) + 40) / 1_000
}
const AUDIO_START_BUFFER_BYTES: u32 = AUDIO_BYTES_PER_SECOND * 60 / 1_000;
const MAX_OPUS_FRAME_SAMPLES_PER_CHANNEL: usize = 5_760;
const MAX_PENDING_OPUS_PACKETS: usize = 32;
const MAX_PENDING_PCM_BUFFERS: usize = 8;

const OPUS_OK: i32 = 0;

#[repr(C)]
struct OpusDecoderState {
    _private: [u8; 0],
}

#[link(name = "opus", kind = "static")]
unsafe extern "C" {
    fn opus_decoder_create(
        sample_rate: i32,
        channels: i32,
        error: *mut i32,
    ) -> *mut OpusDecoderState;
    fn opus_decode(
        decoder: *mut OpusDecoderState,
        data: *const u8,
        length: i32,
        pcm: *mut i16,
        frame_size: i32,
        decode_fec: i32,
    ) -> i32;
    fn opus_decoder_destroy(decoder: *mut OpusDecoderState);
}

struct NativeOpusDecoder {
    state: NonNull<OpusDecoderState>,
}

unsafe impl Send for NativeOpusDecoder {}

impl NativeOpusDecoder {
    fn new() -> Result<Self> {
        let mut error = OPUS_OK;
        // SAFETY: libopus initializes and exclusively owns the returned opaque decoder state.
        let state =
            unsafe { opus_decoder_create(AUDIO_SAMPLE_RATE, AUDIO_CHANNELS as i32, &mut error) };
        if error != OPUS_OK {
            if !state.is_null() {
                // SAFETY: a non-null state returned by libopus must be released with this function.
                unsafe { opus_decoder_destroy(state) };
            }
            bail!("libopus failed to create a decoder: error {error}");
        }
        let state = NonNull::new(state).context("libopus returned a null decoder")?;
        Ok(Self { state })
    }

    fn decode(&mut self, packet: &[u8], pcm: &mut [i16]) -> Result<usize> {
        let packet_len = i32::try_from(packet.len()).context("Opus packet is too large")?;
        // SAFETY: `state` is a live decoder, and `pcm` has room for the maximum Opus frame.
        let decoded = unsafe {
            opus_decode(
                self.state.as_ptr(),
                packet.as_ptr(),
                packet_len,
                pcm.as_mut_ptr(),
                MAX_OPUS_FRAME_SAMPLES_PER_CHANNEL as i32,
                0,
            )
        };
        if decoded < OPUS_OK {
            bail!("libopus decode error {decoded}");
        }
        Ok(decoded as usize)
    }
}

impl Drop for NativeOpusDecoder {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner and the state has not previously been destroyed.
        unsafe { opus_decoder_destroy(self.state.as_ptr()) };
    }
}

pub struct AudioRenderer {
    queue: AudioQueue<i16>,
    packets_tx: SyncSender<Bytes>,
    samples_rx: Receiver<Vec<i16>>,
    started: bool,
    prebuffer_ms: u32,
    last_underrun: Instant,
    last_adjustment: Instant,
    fade_next_packet: bool,
}

impl AudioRenderer {
    pub fn new(audio: &sdl2::AudioSubsystem) -> Result<Self> {
        let desired = AudioSpecDesired {
            freq: Some(AUDIO_SAMPLE_RATE),
            channels: Some(AUDIO_CHANNELS as u8),
            samples: Some(1024),
        };
        let queue = audio
            .open_queue(None, &desired)
            .map_err(anyhow::Error::msg)
            .context("failed to open SDL audio queue")?;
        let spec = queue.spec();
        if spec.freq != AUDIO_SAMPLE_RATE || spec.channels != AUDIO_CHANNELS as u8 {
            eprintln!(
                "SDL audio opened as {} Hz / {} channel(s), requested {} Hz / {} channel(s)",
                spec.freq, spec.channels, AUDIO_SAMPLE_RATE, AUDIO_CHANNELS
            );
        }
        let (packets_tx, samples_rx) = spawn_decode_worker()?;

        Ok(Self {
            queue,
            packets_tx,
            samples_rx,
            started: false,
            prebuffer_ms: match active_stream_profile() {
                StreamProfile::LowLatency => 50,
                StreamProfile::Balanced => 60,
                StreamProfile::Quality => 80,
            },
            last_underrun: Instant::now(),
            last_adjustment: Instant::now(),
            fade_next_packet: true,
        })
    }

    pub fn submit_packets(&mut self, packets: Vec<Bytes>) {
        if self.started && self.queue.size() == 0 {
            self.queue.pause();
            self.started = false;
            // Interruption: increase headroom temporarily rather than repeatedly clearing PCM.
            self.prebuffer_ms = (self.prebuffer_ms + 10).min(110);
            self.last_underrun = Instant::now();
            self.last_adjustment = Instant::now();
            METRICS.audio_underruns.fetch_add(1, Ordering::Relaxed);
            self.fade_next_packet = true;
        }
        if self.last_underrun.elapsed() >= Duration::from_secs(30)
            && self.last_adjustment.elapsed() >= Duration::from_secs(30) {
            let baseline = match active_stream_profile() {
                StreamProfile::LowLatency => 50,
                StreamProfile::Balanced => 60,
                StreamProfile::Quality => 80,
            };
            self.prebuffer_ms = self.prebuffer_ms.saturating_sub(5).max(baseline);
            self.last_adjustment = Instant::now();
        }
        METRICS.audio_target_ms.store(self.prebuffer_ms as u64, Ordering::Relaxed);

        for packet in packets {
            match self.packets_tx.try_send(packet) {
                Ok(()) => {},
                Err(TrySendError::Full(_)) => {
                    METRICS.audio_drops.fetch_add(1, Ordering::Relaxed);
                    self.fade_next_packet = true;
                }
                Err(TrySendError::Disconnected(packet)) => {
                    eprintln!("Audio decode worker stopped; restarting");
                    self.restart_decode_worker();
                    let _ = self.packets_tx.try_send(packet);
                }
            }
        }

        loop {
            match self.samples_rx.try_recv() {
                Ok(mut samples) => {
                    let sample_bytes = (samples.len() * size_of::<i16>()) as u32;
                    if self.queue.size().saturating_add(sample_bytes) > pcm_queue_cap_bytes(self.prebuffer_ms) {
                        // Discard this *new* PCM packet, never abruptly clear already queued
                        // samples. The buffer drains naturally toward the live stream.
                        METRICS.audio_drops.fetch_add(1, Ordering::Relaxed);
                        self.fade_next_packet = true;
                        continue;
                    }
                    if self.fade_next_packet {
                        // Fade in the next playable packet to hide the edge of a dropped one.
                        let fade_samples = (AUDIO_SAMPLE_RATE as usize / 200) * AUDIO_CHANNELS;
                        for (index, sample) in samples.iter_mut().take(fade_samples).enumerate() {
                            let gain = index as f32 / fade_samples as f32;
                            *sample = (*sample as f32 * gain) as i16;
                        }
                        self.fade_next_packet = false;
                    }
                    if let Err(error) = self.queue.queue_audio(&samples) {
                        eprintln!("Failed to queue SDL audio: {error}");
                    }
                    if !self.started && self.queue.size() >= AUDIO_BYTES_PER_SECOND * self.prebuffer_ms / 1_000 {
                        self.queue.resume();
                        self.started = true;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    eprintln!("Audio decode worker output disconnected; restarting");
                    self.restart_decode_worker();
                    break;
                }
            }
        }
    }

    pub fn clear_session(&mut self) {
        self.restart_decode_worker();
    }

    fn restart_decode_worker(&mut self) {
        self.queue.pause();
        self.queue.clear();
        self.started = false;
        self.fade_next_packet = true;
        match spawn_decode_worker() {
            Ok((packets_tx, samples_rx)) => {
                self.packets_tx = packets_tx;
                self.samples_rx = samples_rx;
            }
            Err(error) => eprintln!("Failed to restart audio decode worker: {error:#}"),
        }
    }
}

fn spawn_decode_worker() -> Result<(SyncSender<Bytes>, Receiver<Vec<i16>>)> {
    let (packets_tx, packets_rx) = sync_channel::<Bytes>(MAX_PENDING_OPUS_PACKETS);
    let (samples_tx, samples_rx) = sync_channel::<Vec<i16>>(MAX_PENDING_PCM_BUFFERS);

    let mut decoder = NativeOpusDecoder::new().context("failed to create Opus decoder")?;

    std::thread::Builder::new()
        .name("green-vita-audio-decode".to_owned())
        .spawn(move || {
            let mut decode_buf = vec![0i16; MAX_OPUS_FRAME_SAMPLES_PER_CHANNEL * AUDIO_CHANNELS];
            while let Ok(packet) = packets_rx.recv() {
                let samples_per_channel = match decoder.decode(&packet, &mut decode_buf) {
                    Ok(samples_per_channel) => samples_per_channel,
                    Err(error) => {
                        eprintln!("Failed to decode Opus audio packet: {error}");
                        continue;
                    }
                };

                let sample_count = samples_per_channel * AUDIO_CHANNELS;
                if samples_tx
                    .send(decode_buf[..sample_count].to_vec())
                    .is_err()
                {
                    break;
                }
            }
        })
        .context("failed to spawn audio decode worker")?;

    Ok((packets_tx, samples_rx))
}
