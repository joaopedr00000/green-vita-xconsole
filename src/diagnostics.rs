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
pub(crate) fn metrics(rx_kbps: u64, jitter_us: u64, gaps: u64, late: u64, drops: u64, errors: u64) {
    let m = &crate::streaming::video::metrics::METRICS;
    write_record("stream_metrics", &format!(
        "rx_kbps={rx_kbps} jitter_us={jitter_us} gaps={gaps} late={late} drops={drops} errors={errors} input_us={} display_us={} pipe_us={} audio_ms={} audio_drops={}",
        m.input_dispatch_us.load(Ordering::Relaxed), m.display_queue_us.load(Ordering::Relaxed),
        m.total_video_pipeline_us.load(Ordering::Relaxed), m.audio_target_ms.load(Ordering::Relaxed),
        m.audio_drops.load(Ordering::Relaxed)));
}

pub(crate) fn initialize() {
    let _ = SENDER.get_or_init(|| {
        let (tx, rx) = sync_channel::<String>(QUEUE_CAPACITY);
        std::thread::Builder::new().name("green-vita-diagnostics".into()).spawn(move || {
            let _ = std::fs::create_dir_all(DIR);
            let started = Instant::now();
            let epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            let mut records = std::collections::VecDeque::with_capacity(MAX_EVENTS);
            let mut sequence = 0u64;
            while let Ok(first) = rx.recv() {
                for record in std::iter::once(first).chain(rx.try_iter().take(QUEUE_CAPACITY)) {
                    sequence = sequence.saturating_add(1);
                    if records.len() == MAX_EVENTS { records.pop_front(); }
                    records.push_back(format!("  {} +{}ms {}", sequence, started.elapsed().as_millis(), record));
                }
                let mut output = format!("GreenVita {} | UTC epoch {} | redacted local diagnostics\\nDropped queue records: {}\\n", env!("CARGO_PKG_VERSION"), epoch, DROPPED.load(Ordering::Relaxed));
                for line in &records { output.push_str(line); output.push('\\n'); }
                let _ = crate::fs_utils::write_file_truncating(FILE, output.as_bytes());
                std::thread::sleep(Duration::from_millis(100));
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
