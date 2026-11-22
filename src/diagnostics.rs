//! Privacy-conscious diagnostics for experimental xgpuconsole and 720p playback.
//! Logs only fixed stage names and allowlisted server error codes. Never log tokens,
//! session IDs, authorization headers, user names, raw server bodies, or identifiers.
use std::sync::Mutex;

const DIR: &str = "ux0:data/xcloud-rust";
const FILE: &str = "ux0:data/xcloud-rust/connection-status.txt";
const MAX_EVENTS: usize = 32;
static WRITE_LOCK: Mutex<()> = Mutex::new(());

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

fn write_record(stage: &str, result: &str) {
    let Ok(_guard) = WRITE_LOCK.lock() else { return };
    if std::fs::create_dir_all(DIR).is_err() {
        return;
    }

    let old = std::fs::read_to_string(FILE).unwrap_or_default();
    let mut lines: Vec<&str> = old.lines()
        .filter(|line| line.starts_with("  ") && line.len() <= 180)
        .collect();
    if lines.len() >= MAX_EVENTS {
        lines.drain(..=lines.len() - MAX_EVENTS);
    }
    let mut output = String::from(
        "GreenVita PS Vita | Connection diagnostics (720p max)\n\
         Status entries are observations, NOT proof of authentic Xbox hardware.\n\
         File contains no tokens or account identifiers.\n",
    );
    for line in lines { output.push_str(line); output.push('\n'); }
    // All calls supply internal constant stages and known device classes only.
    output.push_str("  ");
    output.push_str(stage);
    output.push_str(": ");
    output.push_str(result);
    output.push('\n');
    let _ = crate::fs_utils::write_file_truncating(FILE, output.as_bytes());
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
