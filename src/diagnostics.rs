//! Privacy-conscious diagnostics for experimental xgpuconsole and 720p playback.
//! Logs only fixed stage names and allowlisted server error codes. Never log tokens,
//! session IDs, authorization headers, user names, raw server bodies, or identifiers.
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DIR: &str = "ux0:data/xcloud-rust";
const FILE: &str = "ux0:data/xcloud-rust/connection-status.txt";
const MAX_EVENTS: usize = 512;
const QUEUE_CAPACITY: usize = 128;
static SENDER: OnceLock<Option<SyncSender<String>>> = OnceLock::new();
static DROPPED: AtomicU64 = AtomicU64::new(0);
static LAST_METRIC: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

/// A short status line; no dynamic user-provided data is accepted.
pub(crate) fn step(stage: &'static str, result: &'static str) {
    write_record(stage, result);
}

/// Collapse a server error down to an allowlisted diagnostic reason.
pub(crate) fn error_code(message: &str) -> &'static str {
    if message.contains("DeviceTypeNotAllowed") {
        "DeviceTypeNotAllowed"
    } else if message.contains("OfferingDoesNotContainTitle") {
        "OfferingDoesNotContainTitle"
    } else if message.contains("OfferingAccessDenied") {
        "OfferingAccessDenied"
    } else if message.contains("SessionNotActive") {
        "SessionNotActive"
    } else if message.contains("SessionNotFound") {
        "SessionNotFound"
    } else if message.contains("403") {
        "HTTP_403"
    } else if message.contains("401") {
        "HTTP_401"
    } else if message.contains("400") {
        "HTTP_400"
    } else if message.contains("404") {
        "HTTP_404"
    } else if message.contains("429") {
        "HTTP_429"
    } else if message.contains("500") || message.contains("502")
        || message.contains("503") || message.contains("504") {
        "HTTP_5XX"
    } else if message.contains("timed out") || message.contains("timeout") {
        "TIMEOUT"
    } else {
        "UNCLASSIFIED_FAILURE"
    }
}

pub(crate) fn failure(stage: &'static str, error: &str) {
    write_record(stage, error_code(error));
}

/// The catalog can return a device allowlist, but this isn't an entitlement.
/// Never assume a title is playable just because it is present in the catalog.
pub(crate) fn allowed_devices(devices: &[String]) {
    if devices.is_empty() {
        step("catalog_device_restriction", "NOT_LISTED_OR_UNRESTRICTED");
        return;
    }
    // Only known public device class labels, never opaque strings from the server.
    let known = ["XBOXONE", "SCARLETT", "SAMSUNGTV", "WINDOWS", "ANDROID", "IOS", "WEB"];
    let allowed: Vec<&str> = known.into_iter()
        .filter(|k| devices.iter().any(|d| d.eq_ignore_ascii_case(k)))
        .collect();
    let description = if allowed.is_empty() {
        "OTHER_DEVICE_CLASSES".to_owned()
    } else {
        allowed.join(",")
    };
    write_record("catalog_allowed_device_types", &description);
}

pub(crate) fn video_resolution(width: u32, height: u32) {
    if width > 1280 || height > 720 {
        step("server_video_resolution", "ABOVE_720P_REJECTED");
    } else {
        step("server_video_resolution", "AT_OR_BELOW_720P");
    }
}

/// Only aggregate numeric measurements produced locally, never remote text.
pub(crate) fn metrics(rx_kbps: u64, jitter_us: u64, gaps: u64, late: u64, drops: u64, errors: u64, requested_kbps: u64, waiting: bool) {
    // HUD remains live; disk telemetry is sampled only once per five seconds.
    let Ok(mut last) = LAST_METRIC.try_lock() else { return; };
    if last.is_some_and(|at| at.elapsed() < Duration::from_secs(5)) { return; }
    *last = Some(Instant::now());
    drop(last);
    let m = &crate::streaming::video::metrics::METRICS;
    write_record("stream_metrics", &format!(
        "rx_kbps={rx_kbps} requested_kbps={requested_kbps} jitter_us={jitter_us} gaps={gaps} late={late} drops={drops} errors={errors} waiting={waiting} input_us={} display_us={} pipe_us={} audio_ms={} audio_drops={}",
        m.input_dispatch_us.load(Ordering::Relaxed), m.display_queue_us.load(Ordering::Relaxed),
        m.total_video_pipeline_us.load(Ordering::Relaxed), m.audio_target_ms.load(Ordering::Relaxed),
        m.audio_drops.load(Ordering::Relaxed)));
    write_record("input_metrics", &crate::native_input::telemetry());
}

pub(crate) fn initialize() {
    let _ = SENDER.get_or_init(|| {
        let (tx, rx) = sync_channel::<String>(QUEUE_CAPACITY);
        std::thread::Builder::new().name("green-vita-diagnostics".into()).spawn(move || {
            let _ = std::fs::create_dir_all(DIR);
            let started = Instant::now();
            let epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            use std::io::Write;
            let mut sequence = 0u64;
            let mut segment_count = 0usize;
            let mut file: Option<std::fs::File> = None;
            while let Ok(first) = rx.recv() {
                for record in std::iter::once(first).chain(rx.try_iter().take(QUEUE_CAPACITY)) {
                    if file.is_none() || segment_count >= MAX_EVENTS {
                        // Two bounded segments; keep the previous segment through rotation.
                        file.take();
                        let previous = format!("{FILE}.previous");
                        if segment_count >= MAX_EVENTS {
                            let _ = std::fs::remove_file(&previous);
                            let _ = std::fs::rename(FILE, &previous);
                        }
                        let _ = std::fs::remove_file(FILE);
                        file = std::fs::File::create(FILE).ok();
                        segment_count = 0;
                        if let Some(out) = file.as_mut() {
                            if writeln!(out,"GreenVita {} | UTC epoch {} | redacted local diagnostics",env!("CARGO_PKG_VERSION"),epoch).is_err() { file = None; }
                        }
                    }
                    sequence = sequence.saturating_add(1);
                    if let Some(out) = file.as_mut() {
                        if writeln!(out,"  {} +{}ms dropped={} {}",sequence,started.elapsed().as_millis(),DROPPED.load(Ordering::Relaxed),record).is_ok() {
                            segment_count += 1;
                        } else { file = None; }
                    }
                }
                if let Some(out) = file.as_mut() { let _ = out.flush(); }
                // Back off on unavailable storage, not the stream producer.
                if file.is_none() { std::thread::sleep(Duration::from_secs(2)); }
            }
        }).ok().map(|_| tx)
    });
}

fn write_record(stage: &str, result: &str) {
    initialize();
    if let Some(Some(tx)) = SENDER.get() {
        // All callers pass fixed internal labels or locally computed numeric aggregates.
        if tx.try_send(format!("{stage}: {result}")).is_err() {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::error_code;

    #[test]
    fn server_errors_are_redacted() {
        assert_eq!(error_code("DeviceTypeNotAllowed, secret=abc"), "DeviceTypeNotAllowed");
        assert_eq!(error_code("unknown token abc"), "UNCLASSIFIED_FAILURE");
        assert_eq!(error_code("HTTP 403 forbidden"), "HTTP_403");
    }
}

static LAST_HUD:std::sync::Mutex<Option<Instant>>=std::sync::Mutex::new(None);
pub(crate) fn hud_metrics(h:&crate::streaming::hud::StreamHud){
    let Ok(mut last)=LAST_HUD.try_lock() else{return;};if last.is_some_and(|at|at.elapsed()<Duration::from_secs(5)){return;}*last=Some(Instant::now());drop(last);
    let m=&h.playback;
    if let Some(s)=m.stages{write_record("video_stages",&format!("queue_us={} texture_us={} decode_us={} display_us={} pre_present_us={} claim_us={} draw_us={} present_call_us={} total_us={} mean_us={} max_us={} samples={} depth={} stale_handoffs={} output_replacements={} catch_up_replacements={} p95_us={} gap_p95_us={} wakes={} swaps_saved={} mode={}",s.decode.queue_us,s.decode.texture_us,s.decode.decode_us,s.display_us,s.pre_present_us,s.claim_us,s.draw_us,s.present_call_us,s.total_us,m.pipeline_avg_us,m.pipeline_max_us,m.stage_samples,m.queue_depth,m.stale_handoffs,m.output_replacements,m.catch_up_replacements,m.pipeline_p95_us,m.gap_p95_us,m.frame_wakes,m.idle_swaps_saved,crate::settings::latency_mode()));}
    let (n,last,avg,max)=crate::native_input::change_metrics();
    write_record("latency_lab",&format!("session_secs={} audio_target_ms={} audio_sdl_ms={} audio_batch_gap_ema_us={} audio_trim_frames={} telemetry_missed={} raw_busy_reads={} raw_changes={} raw_change_last_us={} raw_change_avg_us={} raw_change_max_us={} nack_grace_ms={} late_unrequested={} missing_expired={} missing_untracked={}",h.session_secs,m.audio_ms,m.audio_queued_ms,m.audio_arrival_us,m.audio_trim_frames,m.telemetry_missed,crate::native_input::busy_reads(),n,last,avg,max,h.nack_grace_ms,h.unrequested_late,h.expired_missing,h.untracked_missing));
    use crate::streaming::video::metrics::{AUDIO_REFILLS,AUDIO_DEVICE_PERIOD_US,AUDIO_PLC_FRAMES,AUDIO_HANDOFF_DROPS,AUDIO_HANDOFF_MAX_US};
    write_record("opennow_lab",&format!("decoder=public textures={} format={} plc_frames={} audio_handoff_drops={} audio_handoff_max_us={}",crate::experimental::texture_count(),crate::experimental::texture_format(),AUDIO_PLC_FRAMES.load(Ordering::Relaxed),AUDIO_HANDOFF_DROPS.load(Ordering::Relaxed),AUDIO_HANDOFF_MAX_US.load(Ordering::Relaxed)));
    write_record("audio_refill",&format!("refills_without_rebuffer={} device_period_us={}",AUDIO_REFILLS.load(Ordering::Relaxed),AUDIO_DEVICE_PERIOD_US.load(Ordering::Relaxed)));
    if let Some(r)=h.rtt{write_record("ice_rtt",&format!("current_us={} average_us={} responses={} observed_age_ms={}",r.current_us,r.average_us,r.responses,r.age().as_millis()));}
}

pub(crate) fn benchmark_result(r:&crate::streaming::benchmark::Report){
    write_record("benchmark60_lab",&format!("decoder=public textures={} format={}",crate::experimental::texture_count(),crate::experimental::texture_format()));
    for (stage,d) in [("benchmark60_pipeline",r.pipeline),("benchmark60_interval",r.gap),("benchmark60_claim",r.claim)]{
        write_record(stage,&format!("mode={} profile={} window_s={} samples={} omitted={} min_us={} avg_us={} median_us={} p95_us={} p99_us={} max_us={} direct_wakes={}",r.mode,r.profile,r.seconds,d.count,d.omitted,d.min,d.avg,d.median,d.p95,d.p99,d.max,r.direct_wakes));
    }
}
