use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) struct VideoMetrics {
    pub(crate) rtp_assembly_sum_us: AtomicU64,
    pub(crate) rtp_assembly_count: AtomicU64,
    pub(crate) rtp_assembly_max_us: AtomicU64,
    pub(crate) decode_us: AtomicU64,
    /// Time from latest Vita gamepad sample to dispatch into RTC data channel.
    pub(crate) input_dispatch_us: AtomicU64,
    pub(crate) display_queue_us: AtomicU64,
    pub(crate) total_video_pipeline_us: AtomicU64,
    pub(crate) audio_underruns: AtomicU64,
    pub(crate) audio_drops: AtomicU64,
    pub(crate) audio_target_ms: AtomicU64,
    pub(crate) pipeline_age_us: AtomicU64,
    pub(crate) decoded: AtomicU64,
    pub(crate) skipped: AtomicU64,
    pub(crate) presented: AtomicU64,
    pub(crate) replaced: AtomicU64,
    pub(crate) queue_full: AtomicU64,
    pub(crate) resyncs: AtomicU64,
    pub(crate) resets: AtomicU64,
}

pub(crate) static METRICS: VideoMetrics = VideoMetrics {
    rtp_assembly_sum_us: AtomicU64::new(0),
    rtp_assembly_count: AtomicU64::new(0),
    rtp_assembly_max_us: AtomicU64::new(0),
    decode_us: AtomicU64::new(0),
    input_dispatch_us: AtomicU64::new(0),
    display_queue_us: AtomicU64::new(0),
    total_video_pipeline_us: AtomicU64::new(0),
    audio_underruns: AtomicU64::new(0),
    audio_drops: AtomicU64::new(0),
    audio_target_ms: AtomicU64::new(0),
    pipeline_age_us: AtomicU64::new(0),
    decoded: AtomicU64::new(0),
    skipped: AtomicU64::new(0),
    presented: AtomicU64::new(0),
    replaced: AtomicU64::new(0),
    queue_full: AtomicU64::new(0),
    resyncs: AtomicU64::new(0),
    resets: AtomicU64::new(0),
};

pub fn video_performance_summary() -> String {
    let rtp_sum = METRICS.rtp_assembly_sum_us.swap(0, Ordering::Relaxed);
    let rtp_count = METRICS.rtp_assembly_count.swap(0, Ordering::Relaxed);
    let rtp_average = rtp_sum.checked_div(rtp_count).unwrap_or(0);
    let rtp_max = METRICS.rtp_assembly_max_us.swap(0, Ordering::Relaxed);
    format!(
        "fps d/p:{}/{} us r:{rtp_average}/{rtp_max} d/a:{}/{} inputq:{}us displayq:{}us pipe:{}us audio:{}ms au:{}/{} skip:{} repl:{} q:{} rs:{} rst:{}",
        METRICS.decoded.swap(0, Ordering::Relaxed),
        METRICS.presented.swap(0, Ordering::Relaxed),
        METRICS.decode_us.load(Ordering::Relaxed),
        METRICS.pipeline_age_us.load(Ordering::Relaxed),
        METRICS.input_dispatch_us.load(Ordering::Relaxed),
        METRICS.display_queue_us.load(Ordering::Relaxed),
        METRICS.total_video_pipeline_us.load(Ordering::Relaxed),
        METRICS.audio_target_ms.load(Ordering::Relaxed),
        METRICS.audio_underruns.load(Ordering::Relaxed),
        METRICS.audio_drops.load(Ordering::Relaxed),
        METRICS.skipped.swap(0, Ordering::Relaxed),
        METRICS.replaced.swap(0, Ordering::Relaxed),
        METRICS.queue_full.swap(0, Ordering::Relaxed),
        METRICS.resyncs.load(Ordering::Relaxed),
        METRICS.resets.load(Ordering::Relaxed),
    )
}
