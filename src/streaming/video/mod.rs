mod decoder;
mod decoder_internal;
pub const AVCDEC_NUM_REF_FRAMES:u32=1;
pub const AU_PTS_STEP:u64=1500;
#[derive(Clone,Copy,PartialEq,Eq,Debug)]
pub enum VideoPixelFormat{Bgr565,Iyuv,Rgba8888,Nv12}
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
    pub(crate) format:VideoPixelFormat,
}

struct DirectVideoOutputState {
    targets: Option<Vec<VideoTextureTarget>>,
    displayed: Option<usize>,
    pending: Option<DecodedFrame>,
    next_generation: u64,
}

/// Synchronizes the decoder thread with the SDL/GXM textures owned by the render thread.
/// Pointers are stored as integers so the platform-specific unsafe boundary stays in the code
/// that registers and consumes the textures.
pub(crate) struct DirectVideoOutput {
    state: Mutex<DirectVideoOutputState>,
    frame_displayed: Condvar,
    frame_ready:tokio::sync::Notify,
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
            frame_ready:tokio::sync::Notify::new(),
            decoder_ready: AtomicBool::new(false),
            width,
            height,
        }
    }

    pub(crate) fn set_targets(&self, targets: Vec<VideoTextureTarget>) {
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
        self.frame_ready.notify_one();
    }

    pub(crate) fn has_pending(&self)->bool{
        self.state.try_lock().ok().is_some_and(|s|s.targets.is_some() && s.pending.is_some())
    }
    /// Single render consumer. Stored Notify permit prevents a missed publish
    /// between checking the state and awaiting the signal. A stale permit is
    /// rechecked against pending state; never treat a notification as a frame.
    pub(crate) async fn wait_ready(&self,timeout:Duration)->bool{
        let end=Instant::now()+timeout;
        loop {
            if self.has_pending(){return true;}
            let left=end.saturating_duration_since(Instant::now());
            if left.is_zero(){return false;}
            if tokio::time::timeout(left,self.frame_ready.notified()).await.is_err(){return self.has_pending();}
        }
    }

    /// Nonblocking claim for the frame-ready render shortcut. Ownership is
    /// identical to the normal path; an in-flight decode is never interrupted.
    pub(crate) fn try_claim_latest(&self)->Option<DecodedFrame>{
        let mut state=self.state.try_lock().ok()?;
        state.targets.as_ref()?;
        let frame=state.pending.take()?;
        state.displayed=Some(frame.texture_index);
        drop(state);
        self.frame_displayed.notify_one();
        Some(frame)
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
        if state.pending.is_some() && state.targets.as_ref().is_some_and(|t|t.len()==2) {
            let (waited_state, _) = self
                .frame_displayed
                .wait_timeout_while(state, Duration::from_millis(wait_ms).min(MAX_PENDING_TEXTURE_WAIT), |state| {
                    state.targets.is_some() && state.pending.is_some()
                })
                .ok()?;
            state = waited_state;
        }
        let count=state.targets.as_ref()?.len();
        let pending=state.pending.map(|f|f.texture_index);
        // Keep the pending picture available until a newer picture actually exists.
        let index=crate::streaming::policy::free_texture(count,state.displayed,pending).or(pending)?;
        let target=*state.targets.as_ref()?.get(index)?;
        // Reuse only the unpublished-to-screen target. Invalidate the older
        // generation before writing so a no-output/error cannot expose stale pixels.
        // The pending texture is never the displayed one. Clear its generation
        // even on the ordinary timeout before writing into it. This protects
        // against a no-output/error return on both recovery and normal paths.
        if pending==Some(index) && state.pending.take().is_some() {
            metrics::OUTPUT_REPLACEMENTS.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
            if wait_ms<67 {metrics::CATCH_UP_REPLACEMENTS.fetch_add(1,std::sync::atomic::Ordering::Relaxed);}
        }
        Some(DirectVideoTargetGuard {
            state,
            ready:&self.frame_ready,
            target,
            index,
        })
    }
}

pub(super) struct DirectVideoTargetGuard<'a> {
    state: MutexGuard<'a, DirectVideoOutputState>,
    ready:&'a tokio::sync::Notify,
    target: VideoTextureTarget,
    index: usize,
}

impl DirectVideoTargetGuard<'_> {
    pub(super) fn publish(mut self,decoded_at:Instant,enqueued_at:Instant,stages:crate::streaming::hud::DecodeStages) -> DecodedFrame {
        self.state.next_generation = self.state.next_generation.wrapping_add(1);
        let frame=DecodedFrame{texture_index:self.index,generation:self.state.next_generation,decoded_at,enqueued_at,stages};
        if self.state.pending.replace(frame).is_some(){metrics::OUTPUT_REPLACEMENTS.fetch_add(1,std::sync::atomic::Ordering::Relaxed);}
        drop(self.state);
        self.ready.notify_one();
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
