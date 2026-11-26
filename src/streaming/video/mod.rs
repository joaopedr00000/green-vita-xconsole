mod decoder;
mod memory;
pub(crate) mod metrics;
mod worker;

pub const STREAM_WIDTH: u32 = 1280;
pub const STREAM_HEIGHT: u32 = 720;
pub const HW_OUTPUT_WIDTH: u32 = 960;
pub const HW_OUTPUT_HEIGHT: u32 = 544;

pub use memory::reserve_decoder_cdram;
pub use metrics::video_performance_summary;
pub use worker::VideoDecodeWorker;

use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration,Instant};

// Let a short render hitch absorb at most two 30 fps intervals. Together with the
// frame already pending for presentation, this caps the microbuffer at three frames.
const MAX_PENDING_TEXTURE_WAIT: Duration = Duration::from_millis(67);

#[derive(Clone, Copy)]
pub(crate) struct VideoTextureTarget {
    pub(crate) ptr: usize,
    pub(crate) pitch: u32,
    pub(crate) capacity: u32,
}

struct DirectVideoOutputState {
    targets: Option<[VideoTextureTarget; 2]>,
    displayed: Option<usize>,
    pending: Option<DecodedFrame>,
    next_generation: u64,
}

/// Synchronizes the decoder thread with the two SDL/GXM textures owned by the render thread.
/// Pointers are stored as integers so the platform-specific unsafe boundary stays in the code
/// that registers and consumes the textures.
pub(crate) struct DirectVideoOutput {
    state: Mutex<DirectVideoOutputState>,
    frame_displayed: Condvar,
    pub(crate) decoder_ready: AtomicBool,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl DirectVideoOutput {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        Self {
            state: Mutex::new(DirectVideoOutputState {
                targets: None,
                displayed: None,
                pending: None,
                next_generation: 0,
            }),
            frame_displayed: Condvar::new(),
            decoder_ready: AtomicBool::new(false),
            width,
            height,
        }
    }

    pub(crate) fn set_targets(&self, targets: [VideoTextureTarget; 2]) {
        if let Ok(mut state) = self.state.lock() {
            state.targets = Some(targets);
            state.displayed = None;
            state.pending = None;
        }
    }

    pub(crate) fn clear_targets(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.targets = None;
            state.displayed = None;
            state.pending = None;
        }
        self.frame_displayed.notify_all();
    }

    /// Renderer claims the newest complete picture and reserves that texture
    /// under the same lock used by AVCDEC. No RTC or application mailbox hop.
    pub(crate) fn claim_latest(&self) -> Option<DecodedFrame> {
        let mut state=self.state.lock().ok()?;
        state.targets.as_ref()?;
        let frame=state.pending.take()?;
        state.displayed=Some(frame.texture_index);
        drop(state);
        self.frame_displayed.notify_one();
        Some(frame)
    }

    pub(super) fn lock_decode_target(&self,wait_ms:u64) -> Option<DirectVideoTargetGuard<'_>> {
        let mut state = self.state.lock().ok()?;
        if state.pending.is_some() {
            let (waited_state, _) = self
                .frame_displayed
                .wait_timeout_while(state, Duration::from_millis(wait_ms).min(MAX_PENDING_TEXTURE_WAIT), |state| {
                    state.targets.is_some() && state.pending.is_some()
                })
                .ok()?;
            state = waited_state;
        }
        let targets = state.targets?;
        let index = state
            .pending
            .map(|frame| frame.texture_index)
            .unwrap_or_else(|| state.displayed.map_or(0, |displayed| 1 - displayed));
        // Reuse only the unpublished-to-screen target. Invalidate the older
        // generation before writing so a no-output/error cannot expose stale pixels.
        // The pending texture is never the displayed one. Clear its generation
        // even on the ordinary timeout before writing into it. This protects
        // against a no-output/error return on both recovery and normal paths.
        if state.pending.take().is_some() {
            metrics::OUTPUT_REPLACEMENTS.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
            if wait_ms<67 {metrics::CATCH_UP_REPLACEMENTS.fetch_add(1,std::sync::atomic::Ordering::Relaxed);}
        }
        Some(DirectVideoTargetGuard {
            state,
            target: targets[index],
            index,
        })
    }
}

pub(super) struct DirectVideoTargetGuard<'a> {
    state: MutexGuard<'a, DirectVideoOutputState>,
    target: VideoTextureTarget,
    index: usize,
}

impl DirectVideoTargetGuard<'_> {
    pub(super) fn publish(mut self,decoded_at:Instant,enqueued_at:Instant,stages:crate::streaming::hud::DecodeStages) -> DecodedFrame {
        self.state.next_generation = self.state.next_generation.wrapping_add(1);
        let frame=DecodedFrame{texture_index:self.index,generation:self.state.next_generation,decoded_at,enqueued_at,stages};
        self.state.pending=Some(frame);
        frame
    }
}

#[derive(Clone,Copy)]
pub struct DecodedFrame {
    pub texture_index: usize,
    pub generation: u64,
    /// GPU frame age is measured on the Vita at presentation, not guessed from RTT.
    pub decoded_at: std::time::Instant,
    pub enqueued_at: std::time::Instant,
    pub(crate) stages:crate::streaming::hud::DecodeStages,
}

#[derive(Clone, Copy)]
pub struct DecoderConfig {
    pub decode_width: u32,
    pub decode_height: u32,
    pub output_width: u32,
    pub output_height: u32,
}
