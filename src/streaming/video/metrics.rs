use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use crate::streaming::hud::PresentedStages;
#[derive(Default)]
struct StageWindow {last:Option<PresentedStages>,sum:u64,max:u64,count:u64,totals:Vec<u64>,gaps:Vec<u64>}
static STAGES:Mutex<StageWindow>=Mutex::new(StageWindow{last:None,sum:0,max:0,count:0,totals:Vec::new(),gaps:Vec::new()});
pub(crate) static QUEUE_DEPTH:AtomicU64=AtomicU64::new(0);
pub(crate) static STALE_HANDOFFS:AtomicU64=AtomicU64::new(0);
pub(crate) static OUTPUT_REPLACEMENTS:AtomicU64=AtomicU64::new(0);
pub(crate) static CATCH_UP_REPLACEMENTS:AtomicU64=AtomicU64::new(0);
pub(crate) static FRAME_WAKES:AtomicU64=AtomicU64::new(0);
pub(crate) static IDLE_SWAPS_SAVED:AtomicU64=AtomicU64::new(0);
pub(crate) static AUDIO_QUEUED_MS:AtomicU64=AtomicU64::new(0);
pub(crate) static AUDIO_ARRIVAL_US:AtomicU64=AtomicU64::new(0);
pub(crate) static AUDIO_TRIM_FRAMES:AtomicU64=AtomicU64::new(0);
pub(crate) static AUDIO_REFILLS:AtomicU64=AtomicU64::new(0);
pub(crate) static AUDIO_PLC_FRAMES:AtomicU64=AtomicU64::new(0);
pub(crate) static AUDIO_HANDOFF_DROPS:AtomicU64=AtomicU64::new(0);
pub(crate) static AUDIO_HANDOFF_MAX_US:AtomicU64=AtomicU64::new(0);
pub(crate) static AUDIO_DEVICE_PERIOD_US:AtomicU64=AtomicU64::new(0);
pub(crate) static TELEMETRY_MISSED:AtomicU64=AtomicU64::new(0);
pub(crate) fn presented(stages:PresentedStages){if !stages.decode.verified{return;}if let Ok(mut s)=STAGES.try_lock(){s.last=Some(stages);s.sum=s.sum.saturating_add(stages.total_us);s.max=s.max.max(stages.total_us);s.count+=1;if s.totals.len()<256{s.totals.push(stages.total_us);}if stages.present_gap_us>0 && s.gaps.len()<256{s.gaps.push(stages.present_gap_us);}}else{TELEMETRY_MISSED.fetch_add(1,Ordering::Relaxed);}}
pub(crate) fn clear_stages(){if let Ok(mut w)=DECODER_WINDOW.lock(){*w=DecoderWindow::default();}if let Ok(mut s)=STAGES.lock(){*s=StageWindow::default();}QUEUE_DEPTH.store(0,Ordering::Relaxed);STALE_HANDOFFS.store(0,Ordering::Relaxed);OUTPUT_REPLACEMENTS.store(0,Ordering::Relaxed);CATCH_UP_REPLACEMENTS.store(0,Ordering::Relaxed);FRAME_WAKES.store(0,Ordering::Relaxed);IDLE_SWAPS_SAVED.store(0,Ordering::Relaxed);TELEMETRY_MISSED.store(0,Ordering::Relaxed);}

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

pub(crate) fn snapshot() -> crate::streaming::hud::PlaybackMetrics {
    use crate::streaming::hud::PlaybackMetrics;
    let sum=METRICS.rtp_assembly_sum_us.swap(0,Ordering::Relaxed);
    let count=METRICS.rtp_assembly_count.swap(0,Ordering::Relaxed);
    let (stages,pipeline_avg_us,pipeline_max_us,stage_samples,pipeline_p95_us,gap_p95_us)=STAGES.lock().map(|mut s|{let p95=crate::streaming::hud::percentile95(&mut s.totals);let gap=crate::streaming::hud::percentile95(&mut s.gaps);let r=(s.last,s.sum.checked_div(s.count).unwrap_or(0),s.max,s.count,p95,gap);s.sum=0;s.max=0;s.count=0;s.totals.clear();s.gaps.clear();r}).unwrap_or_default();
    PlaybackMetrics {
        stages,pipeline_avg_us,pipeline_max_us,stage_samples,pipeline_p95_us,gap_p95_us,frame_wakes:FRAME_WAKES.load(Ordering::Relaxed),idle_swaps_saved:IDLE_SWAPS_SAVED.load(Ordering::Relaxed),queue_depth:QUEUE_DEPTH.load(Ordering::Relaxed),stale_handoffs:STALE_HANDOFFS.load(Ordering::Relaxed),output_replacements:OUTPUT_REPLACEMENTS.load(Ordering::Relaxed),catch_up_replacements:CATCH_UP_REPLACEMENTS.load(Ordering::Relaxed),
        rtp_avg_us:sum.checked_div(count).unwrap_or(0),rtp_max_us:METRICS.rtp_assembly_max_us.swap(0,Ordering::Relaxed),
        decoded:METRICS.decoded.swap(0,Ordering::Relaxed),presented:METRICS.presented.swap(0,Ordering::Relaxed),
        decode_us:METRICS.decode_us.load(Ordering::Relaxed),queue_age_us:METRICS.pipeline_age_us.load(Ordering::Relaxed),
        input_us:METRICS.input_dispatch_us.load(Ordering::Relaxed),display_us:METRICS.display_queue_us.load(Ordering::Relaxed),
        pipeline_us:METRICS.total_video_pipeline_us.load(Ordering::Relaxed),audio_ms:METRICS.audio_target_ms.load(Ordering::Relaxed),audio_queued_ms:AUDIO_QUEUED_MS.load(Ordering::Relaxed),audio_arrival_us:AUDIO_ARRIVAL_US.load(Ordering::Relaxed),audio_trim_frames:AUDIO_TRIM_FRAMES.load(Ordering::Relaxed),telemetry_missed:TELEMETRY_MISSED.load(Ordering::Relaxed),
        underruns:METRICS.audio_underruns.load(Ordering::Relaxed),audio_drops:METRICS.audio_drops.load(Ordering::Relaxed),
        skipped:METRICS.skipped.swap(0,Ordering::Relaxed),replaced:METRICS.replaced.swap(0,Ordering::Relaxed),
        queue_full:METRICS.queue_full.swap(0,Ordering::Relaxed),resyncs:METRICS.resyncs.load(Ordering::Relaxed),resets:METRICS.resets.load(Ordering::Relaxed),
    }
}
pub fn video_performance_summary() -> String { summary(&snapshot()) }
pub(crate) fn summary(m:&crate::streaming::hud::PlaybackMetrics)->String {
    format!("fps d/p:{}/{} us r:{}/{} d/a:{}/{} inputq:{}us displayq:{}us pipe:{}us audio:{}ms au:{}/{} skip:{} repl:{} q:{} rs:{} rst:{}",m.decoded,m.presented,m.rtp_avg_us,m.rtp_max_us,m.decode_us,m.queue_age_us,m.input_us,m.display_us,m.pipeline_us,m.audio_ms,m.underruns,m.audio_drops,m.skipped,m.replaced,m.queue_full,m.resyncs,m.resets)
}

#[derive(Clone,Copy,Debug,Default)]
pub(crate) struct DecoderWindow{
    pub pending:u64,pub lag_x100:u64,pub call_us:u64,pub wait_us:u64,pub matched:u64,
    pub calls:u64,pub period_us:u64,pub over_budget:u64,
    pub at:Option<std::time::Instant>,
}
static DECODER_WINDOW:Mutex<DecoderWindow>=Mutex::new(DecoderWindow{pending:0,lag_x100:0,call_us:0,wait_us:0,matched:0,calls:0,period_us:0,over_budget:0,at:None});
pub(crate) fn record_decoder_window(w:DecoderWindow){if let Ok(mut slot)=DECODER_WINDOW.try_lock(){*slot=w;}}
pub(crate) fn decoder_window()->Option<DecoderWindow>{
    let w=*DECODER_WINDOW.try_lock().ok()?;
    w.at.filter(|at|at.elapsed()<std::time::Duration::from_secs(12))?;
    Some(w)
}
