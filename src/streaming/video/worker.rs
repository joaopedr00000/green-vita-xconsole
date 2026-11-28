use super::decoder::{HwVideoDecoder,FrameTiming};
use super::metrics;
use super::{DecodedFrame, DecoderConfig, DirectVideoOutput};
use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select_biased, unbounded};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

// Faster streams arrive in larger bursts. Keep the normal 30 fps limit small, but
// allow enough room for roughly 100 ms of compressed video near the observed 55 fps.
const MIN_PENDING_ACCESS_UNITS: usize = 2;
const MAX_PENDING_ACCESS_UNITS: usize = 6;

struct QueuedAccessUnit {
    data: Vec<u8>,
    queued_at: Instant,
    source_duration_us:Option<u64>,
    clean_idr:bool,
    generation: u64,
}

enum DecoderCommand {
    Reset,
    Stop,
}

pub(crate) type DecodeResult = Result<DecodedFrame, String>;

pub struct VideoDecodeWorker {
    access_units: Sender<QueuedAccessUnit>,
    commands: Sender<DecoderCommand>,
    generation: Arc<AtomicU64>,
    pub(crate) latest_result: Arc<Mutex<Option<DecodeResult>>>,
    pub(crate) result_ready: Arc<tokio::sync::Notify>,
}

impl VideoDecodeWorker {
    pub fn spawn(config: DecoderConfig, direct_output: Arc<DirectVideoOutput>) -> Result<Self> {
        let decoder =
            HwVideoDecoder::new(config).context("failed to create hardware H264 decoder")?;
        metrics::clear_stages();
        direct_output.decoder_ready.store(true, Ordering::Release);
        let (access_units, worker_access_units) = bounded(MAX_PENDING_ACCESS_UNITS);
        let (commands, worker_commands) = unbounded();
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&generation);
        let latest_result = Arc::new(Mutex::new(None));
        let worker_latest_result = Arc::clone(&latest_result);
        let result_ready = Arc::new(tokio::sync::Notify::new());
        let worker_result_ready = Arc::clone(&result_ready);
        let worker_direct_output = Arc::clone(&direct_output);

        std::thread::Builder::new()
            .name("green-vita-video-decode".to_owned())
            .spawn(move || {
                #[cfg(target_os = "vita")]
                pin_decoder_thread();
                run_decode_loop(
                    worker_access_units,
                    worker_commands,
                    worker_generation,
                    worker_latest_result,
                    worker_result_ready,
                    decoder,
                    config,
                    worker_direct_output,
                )
            })
            .context("failed to spawn video decode worker")?;

        Ok(Self {
            access_units,
            commands,
            generation,
            latest_result,
            result_ready,
        })
    }

    pub fn submit_access_unit(&self, data: Vec<u8>, source_frame_duration_us: Option<u64>,clean_idr:bool) -> bool {
        let source_fps = source_frame_duration_us
            .filter(|duration| *duration > 0)
            .map(|duration| 1_000_000 / duration)
            .unwrap_or(30);
        let extra_capacity = source_fps
            .saturating_sub(30)
            .min(25)
            .saturating_mul((MAX_PENDING_ACCESS_UNITS - MIN_PENDING_ACCESS_UNITS) as u64)
            .saturating_add(12)
            / 25;
        let pending_limit = MIN_PENDING_ACCESS_UNITS + extra_capacity as usize;
        if self.access_units.len() >= pending_limit {
            metrics::METRICS.queue_full.fetch_add(1, Ordering::Relaxed);
            return false;
        }

        let access_unit = QueuedAccessUnit {
            data,
            queued_at: Instant::now(),
            source_duration_us:source_frame_duration_us,
            clean_idr,
            generation: self.generation.load(Ordering::Acquire),
        };
        match self.access_units.try_send(access_unit) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                metrics::METRICS.queue_full.fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    pub fn reset_decoder(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        metrics::METRICS.resets.fetch_add(1, Ordering::Relaxed);
        let _ = self.commands.send(DecoderCommand::Reset);
    }

    pub fn begin_resync(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        metrics::METRICS.resyncs.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for VideoDecodeWorker {
    fn drop(&mut self) {
        let _ = self.commands.send(DecoderCommand::Stop);
    }
}

#[cfg(target_os = "vita")]
fn pin_decoder_thread() {
    let thread_id = unsafe { vitasdk_sys::sceKernelGetThreadId() };
    let result = unsafe {
        vitasdk_sys::sceKernelChangeThreadCpuAffinityMask(
            thread_id,
            vitasdk_sys::SCE_KERNEL_CPU_MASK_USER_2 as i32,
        )
    };
    if result < 0 {
        eprintln!("Failed to pin video decoder thread to user CPU 2: {result:#x}");
    }
    // RTC is already time-critical on USER_1. Do not raise it further.
    // Extreme optionally elevates the decoder on its existing USER_2 core,
    // not the renderer or audio workers; failure leaves OS defaults intact.
    if crate::settings::latency_mode()==2 {
        let priority=unsafe{sdl2::sys::SDL_SetThreadPriority(sdl2::sys::SDL_ThreadPriority::SDL_THREAD_PRIORITY_HIGH)};
        crate::diagnostics::step("decoder_priority",if priority==0{"HIGH_OK"}else{"HIGH_UNAVAILABLE"});
    }
}

fn run_decode_loop(
    access_units: Receiver<QueuedAccessUnit>,
    commands: Receiver<DecoderCommand>,
    generation: Arc<AtomicU64>,
    latest_result: Arc<Mutex<Option<DecodeResult>>>,
    result_ready: Arc<tokio::sync::Notify>,
    initial_decoder: HwVideoDecoder,
    config: DecoderConfig,
    direct_output: Arc<DirectVideoOutput>,
) {
    let mut decoder = Some(initial_decoder);
    let mut restart_trial_used=false;

    loop {
        select_biased! {
            recv(commands) -> command => match command {
                Ok(DecoderCommand::Reset) => {
                    decoder = None;
                    continue;
                }
                Ok(DecoderCommand::Stop) | Err(_) => break,
            },
            recv(access_units) -> access_unit => {
                let Ok(access_unit) = access_unit else { break };
                metrics::QUEUE_DEPTH.store(access_units.len() as u64,Ordering::Relaxed);
                decode_queued_access_unit(
                    &mut decoder,
                    config,
                    &generation,
                    &latest_result,
                    &result_ready,
                    access_unit,
                    &direct_output,
                    access_units.len(),
                    &mut restart_trial_used,
                );
            }
        }
    }
}

fn decode_queued_access_unit(
    decoder: &mut Option<HwVideoDecoder>,
    config: DecoderConfig,
    generation: &AtomicU64,
    latest_result: &Mutex<Option<DecodeResult>>,
    result_ready: &tokio::sync::Notify,
    mut access_unit: QueuedAccessUnit,
    direct_output: &DirectVideoOutput,
    remaining:usize,
    restart_trial_used:&mut bool,
) {
    if access_unit.generation != generation.load(Ordering::Acquire) {
        return;
    }

    let dequeued_at=Instant::now();
    let texture_started_at=Instant::now();
    let wait_ms=crate::streaming::policy::experimental_texture_wait_ms(crate::settings::latency_mode(),dequeued_at.saturating_duration_since(access_unit.queued_at).as_micros() as u64,remaining,access_unit.source_duration_us);
    let Some(direct_target) = direct_output.lock_decode_target(wait_ms) else {
        // Do not decode until the renderer has registered its three GXM textures. There is no
        // legacy output buffer to copy from anymore.
        metrics::METRICS.skipped.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let texture_acquired_at=Instant::now();
    if access_unit.generation != generation.load(Ordering::Acquire) {return;}
    // A single controlled restart at a naturally arriving self-contained IDR.
    // Never drain reference pictures or discard arbitrary compressed P frames.
    let trial=access_unit.clean_idr && !*restart_trial_used &&
        decoder.as_ref().is_some_and(HwVideoDecoder::trial_ready);
    let resync=access_unit.clean_idr &&
        decoder.as_ref().is_some_and(|d|d.generation_changed(access_unit.generation));
    let trial_lag=if trial && !resync {decoder.as_ref().and_then(HwVideoDecoder::lag)}else{None};
    if trial_lag.is_some() || resync{
        if let Some(before)=trial_lag{
            *restart_trial_used=true;
            crate::diagnostics::decoder_restart_begin(before);
        }else{crate::diagnostics::step("decoder_restart","CLEAN_IDR_RESYNC");}
        *decoder=None;
    }
    if decoder.is_none() {
        match HwVideoDecoder::new(config) {
            Ok(mut new_decoder) => {if let Some(before)=trial_lag{new_decoder.mark_trial(before);} *decoder = Some(new_decoder);},
            Err(error) => {
                publish_result(
                    latest_result,
                    result_ready,
                    Err(format!("failed to recreate H264 decoder: {error:#}")),
                );
                return;
            }
        }
    }

    // Measure the hardware call and contain an unexpected decoder panic inside its worker.
    let decode_started_at = Instant::now();
    let stages=crate::streaming::hud::DecodeStages {queue_us:dequeued_at.saturating_duration_since(access_unit.queued_at).as_micros() as u64,texture_us:texture_acquired_at.saturating_duration_since(texture_started_at).as_micros() as u64,decode_us:0,..Default::default()};
    let timing=FrameTiming{enqueued:access_unit.queued_at,stages,generation:access_unit.generation};
    let decode_result=catch_unwind(AssertUnwindSafe(||decoder.as_mut().expect("decoder recreated above").decode(&mut access_unit.data,direct_target.target(),timing)));
    let decode_finished_at=Instant::now();
    if access_unit.generation != generation.load(Ordering::Acquire) {
        return;
    }

    match decode_result {
        Ok(Ok(Some(timing))) if timing.generation==generation.load(Ordering::Acquire) => {
            metrics::METRICS.decode_us.store(timing.stages.decode_us,Ordering::Relaxed);
            metrics::METRICS.decoded.fetch_add(1, Ordering::Relaxed);
            let frame = direct_target.publish(decode_finished_at,timing.enqueued,timing.stages);
            metrics::METRICS.pipeline_age_us.store(
                if timing.stages.verified{timing.enqueued.elapsed().as_micros() as u64}else{0},
                Ordering::Relaxed,
            );
            publish_result(
                latest_result,
                result_ready,
                Ok(frame),
            );
        }
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            *decoder = None;
            publish_result(latest_result, result_ready, Err(error.to_string()));
        }
        Err(_) => {
            eprintln!("H264 decoder panicked; recreating decoder on next frame");
            *decoder = None;
            publish_result(
                latest_result,
                result_ready,
                Err("H264 decoder panicked and was restarted".to_owned()),
            );
        }
    }
}

fn publish_result(
    slot: &Mutex<Option<DecodeResult>>,
    result_ready: &tokio::sync::Notify,
    result: DecodeResult,
) {
    if let Ok(mut latest) = slot.lock()
        && latest.replace(result).is_some()
    {
        metrics::METRICS.replaced.fetch_add(1, Ordering::Relaxed);
    }
    result_ready.notify_one();
}
