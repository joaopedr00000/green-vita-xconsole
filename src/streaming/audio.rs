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
    AUDIO_BYTES_PER_SECOND * super::policy::pcm_cap_ms(target_ms) / 1_000
}
fn baseline_ms() -> u32 {
    match active_stream_profile() { StreamProfile::LowLatency => 50, StreamProfile::Balanced => 60, StreamProfile::Quality => 80 }
}
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
    retry_worker_after: Instant,
    worker_ready: bool,
    low_water_ms:u32,
    last_arrival:Option<Instant>,
    arrival_gap_us:u64,
    refill_period_bytes:u32,
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
        // Keep the negotiated device period unchanged. A refill of at least
        // one period can continue playback without an avoidable pause.
        let refill_period_bytes=u32::from(spec.samples.max(1))*AUDIO_CHANNELS as u32*2;
        crate::streaming::video::metrics::AUDIO_DEVICE_PERIOD_US.store(u64::from(spec.samples)*1_000_000/(spec.freq.max(1) as u64),Ordering::Relaxed);
        let (packets_tx, samples_rx) = spawn_decode_worker()?;

        Ok(Self {
            queue,
            packets_tx,
            samples_rx,
            started: false,
            prebuffer_ms:super::policy::audio_start_ms(crate::settings::latency_mode(),baseline_ms()),
            last_underrun: Instant::now(),
            last_adjustment: Instant::now(),
            fade_next_packet: true,
            retry_worker_after: Instant::now(),
            worker_ready: true,
            low_water_ms:u32::MAX,last_arrival:None,arrival_gap_us:0,refill_period_bytes,
        })
    }

    pub fn submit_packets(&mut self, packets: Vec<Bytes>) {
        if !self.worker_ready {
            if Instant::now() >= self.retry_worker_after { self.restart_decode_worker(); }
            if !self.worker_ready { return; }
        }
        let now=Instant::now();
        let queued_bytes=self.queue.size();
        let queued_ms=queued_bytes.saturating_mul(1000)/AUDIO_BYTES_PER_SECOND;
        let was_started=self.started;
        let empty_at_entry=was_started && queued_bytes==0;
        let mut refilling=empty_at_entry;
        if empty_at_entry{self.fade_next_packet=true;}
        if self.started{self.low_water_ms=self.low_water_ms.min(queued_ms);}
        if !packets.is_empty(){
            if let Some(at)=self.last_arrival{let us=now.saturating_duration_since(at).as_micros().min(1_000_000) as u64;self.arrival_gap_us=(self.arrival_gap_us.saturating_mul(7)+us)/8;}
            self.last_arrival=Some(now);
        }

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
                    if self.worker_ready { let _ = self.packets_tx.try_send(packet); }
                    else { break; }
                }
            }
        }

        loop {
            match self.samples_rx.try_recv() {
                Ok(mut samples) => {
                    let queued=self.queue.size().saturating_mul(1000)/AUDIO_BYTES_PER_SECOND;
                    // Never speed up audio during a refill or just after startup.
                    if self.started && queued==0{refilling=true;self.fade_next_packet=true;}
                    let trim=super::policy::audio_rate_trim_frames(crate::settings::latency_mode(),self.started && !refilling,queued,self.prebuffer_ms,samples.len()/AUDIO_CHANNELS);
                    let removed=if trim>0{super::policy::shorten_stereo_pcm(&mut samples,trim)}else{0};
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
                            // The two stereo channels receive the same gain for each frame.
                            let gain = (index / AUDIO_CHANNELS) as f32 / (fade_samples / AUDIO_CHANNELS) as f32;
                            *sample = (*sample as f32 * gain) as i16;
                        }
                        self.fade_next_packet = false;
                    }
                    if let Err(error) = self.queue.queue_audio(&samples) {
                        let _ = error;
                        METRICS.audio_drops.fetch_add(1,Ordering::Relaxed);
                        crate::diagnostics::step("audio", "OUTPUT_QUEUE_FAILED");
                        self.fade_next_packet = true;
                        continue;
                    }
                    crate::streaming::video::metrics::AUDIO_TRIM_FRAMES.fetch_add(removed as u64,Ordering::Relaxed);
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
        // An empty SDL software queue is not proof of a hardware underrun.
        // Drain decoded PCM first. If one device period is available again,
        // continue playback; only pause/rebuffer when the refill is insufficient.
        let queued_after=self.queue.size();
        if self.started && (refilling || queued_after==0){
            self.low_water_ms=0;
            if super::policy::audio_refill_sufficient(queued_after,self.refill_period_bytes){
                crate::streaming::video::metrics::AUDIO_REFILLS.fetch_add(1,Ordering::Relaxed);
            }else{
                self.queue.pause();
                self.started=false;
                self.prebuffer_ms=(self.prebuffer_ms+10).min(110);
                self.last_underrun=Instant::now();
                self.last_adjustment=Instant::now();
                self.low_water_ms=u32::MAX;
                METRICS.audio_underruns.fetch_add(1,Ordering::Relaxed);
                self.fade_next_packet=true;
            }
        }
        if self.last_underrun.elapsed()>=Duration::from_secs(30) && self.last_adjustment.elapsed()>=Duration::from_secs(30){
            let mode=crate::settings::latency_mode();
            let floor=super::policy::audio_floor_ms(mode,baseline_ms());
            self.prebuffer_ms=super::policy::audio_next_target_ms(self.prebuffer_ms,floor,self.started,self.low_water_ms);
            self.low_water_ms=u32::MAX;
            self.last_adjustment=Instant::now();
        }
        METRICS.audio_target_ms.store(self.prebuffer_ms as u64,Ordering::Relaxed);
        crate::streaming::video::metrics::AUDIO_QUEUED_MS.store((self.queue.size() as u64)*1000/(AUDIO_BYTES_PER_SECOND as u64),Ordering::Relaxed);
        crate::streaming::video::metrics::AUDIO_ARRIVAL_US.store(self.arrival_gap_us,Ordering::Relaxed);
    }

    pub fn clear_session(&mut self) {
        self.prebuffer_ms=super::policy::audio_start_ms(crate::settings::latency_mode(),baseline_ms());
        self.low_water_ms=u32::MAX;self.last_arrival=None;self.arrival_gap_us=0;
        crate::streaming::video::metrics::AUDIO_QUEUED_MS.store(0,Ordering::Relaxed);
        crate::streaming::video::metrics::AUDIO_ARRIVAL_US.store(0,Ordering::Relaxed);
        crate::streaming::video::metrics::AUDIO_TRIM_FRAMES.store(0,Ordering::Relaxed);
        crate::streaming::video::metrics::AUDIO_REFILLS.store(0,Ordering::Relaxed);
        METRICS.audio_target_ms.store(self.prebuffer_ms as u64,Ordering::Relaxed);
        self.last_underrun = Instant::now();
        self.last_adjustment = Instant::now();
        self.retire_decode_worker();
        self.retry_worker_after = Instant::now();
    }

    fn retire_decode_worker(&mut self) {
        // Disconnect both channels before creating another decoder: no previous
        // session samples can enter the new SDL queue, even on spawn failure.
        let (empty_tx, empty_rx) = sync_channel::<Bytes>(1);
        let (pcm_tx, pcm_rx) = sync_channel::<Vec<i16>>(1);
        drop(empty_rx); drop(pcm_tx);
        self.packets_tx = empty_tx;
        self.samples_rx = pcm_rx;
        self.queue.pause(); self.queue.clear();
        self.started = false; self.fade_next_packet = true;
        self.worker_ready = false;
    }

    fn restart_decode_worker(&mut self) {
        self.retire_decode_worker();
        self.retry_worker_after = Instant::now() + Duration::from_secs(2);
        match spawn_decode_worker() {
            Ok((packets_tx, samples_rx)) => {
                self.packets_tx = packets_tx;
                self.samples_rx = samples_rx;
                self.worker_ready = true;
            }
            Err(_) => crate::diagnostics::step("audio", "DECODER_RESTART_FAILED"),
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
                        METRICS.audio_drops.fetch_add(1,Ordering::Relaxed);
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
