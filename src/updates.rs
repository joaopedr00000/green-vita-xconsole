//! Read-only update notification. Never installs VPKs or follows untrusted download URLs.
use std::time::Duration;
const CURRENT_VERSION: &str = "v0.3.0-beta.1";
const RELEASES_URL: &str = "https://api.github.com/repos/joaopedr00000/green-vita-xconsole/releases?per_page=5";

fn version_rank(tag: &str) -> Option<(u32, u32, u32, u32, u32)> {
    let normalized = tag.strip_prefix('v').unwrap_or(tag);
    let (core, suffix) = normalized.split_once('-').unwrap_or((normalized, ""));
    let mut n = core.split('.');
    let (major, minor, patch) = (
        n.next()?.parse::<u32>().ok()?,
        n.next()?.parse::<u32>().ok()?,
        n.next()?.parse::<u32>().ok()?,
    );
    if n.next().is_some() { return None; }
    let (stable, beta) = if suffix.is_empty() {
        (1, 0)
    } else if let Some(beta) = suffix.strip_prefix("beta.") {
        (0, beta.parse::<u32>().ok()?)
    } else {
        return None;
    };
    Some((major, minor, patch, stable, beta))
}

pub(crate) async fn check_for_update() -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .build().ok()?;
    let releases: serde_json::Value = client.get(RELEASES_URL)
        .header(reqwest::header::USER_AGENT, "GreenVita-Vita/0.3 (update-check)")
        .send().await.ok()?
        .error_for_status().ok()?
        .json().await.ok()?;
    let current = version_rank(CURRENT_VERSION)?;
    releases.as_array()?.iter()
        .filter_map(|release| release.get("tag_name").and_then(|t| t.as_str()))
        .filter_map(|tag| version_rank(tag).map(|rank| (tag, rank)))
        .filter(|(_, rank)| *rank > current)
        .max_by_key(|(_, rank)| *rank)
        .map(|(tag, _)| tag.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compares_beta_versions_without_lexicographic_errors() {
        assert!(version_rank("v0.3.0-beta.12") > version_rank("v0.3.0-beta.2"));
        assert!(version_rank("v0.3.0") > version_rank("v0.3.0-beta.99"));
        assert!(version_rank("v0.3.1-beta.1") > version_rank("v0.3.0"));
    }
}
