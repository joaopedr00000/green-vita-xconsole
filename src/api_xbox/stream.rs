use crate::api_xbox::api::ApiClient;
use crate::api_xbox::auth::{EndpointCredentials, MsalAuth};
use anyhow::{Context, Result};
use reqwest::Method;
use rtc::peer_connection::transport::RTCIceCandidateInit;
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::{Ipv4Addr, Ipv6Addr};

use tokio::time::{Duration, sleep};

const XBOX_HOME_STREAM_PORT: u16 = 9002;


#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartStreamResponse {
    pub session_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamState {
    New,
    Provisioning,
    WaitingForResources,
    ReadyToConnect,
    Provisioned,
    Error,
}

impl StreamState {
    fn from_api(value: &str) -> Self {
        match value {
            "Provisioning" => Self::Provisioning,
            "WaitingForResources" => Self::WaitingForResources,
            "ReadyToConnect" => Self::ReadyToConnect,
            "Provisioned" => Self::Provisioned,
            "Failed" | "Error" => Self::Error,
            _ => Self::New,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Stream {
    api_client: ApiClient,
    credentials: EndpointCredentials,
    pub session_id: String,
    session_path: String,
    pub state: StreamState,
    /// True only for the experimental xgpuconsole play attempt.
    pub(crate) is_console: bool,
    /// Xbox Remote Play (xHome), distinct from experimental xgpuconsole cloud offering.
    is_home: bool,
    /// Optional RFC1918 LAN address entered by the owner; never used for xCloud.
    console_lan_ip: Option<Ipv4Addr>,
}

#[derive(Deserialize)]
struct StateResponse {
    state: String,
    #[serde(rename = "errorDetails", default)]
    error_details: Option<Value>,
}

impl Stream {
    pub fn new(
        api_client: ApiClient,
        credentials: EndpointCredentials,
        response: StartStreamResponse,
        is_console: bool,
        is_home: bool,
    ) -> Self {
        let session_id = response
            .session_path
            .rsplit('/')
            .next()
            .unwrap_or(&response.session_path)
            .to_owned();

        Self {
            api_client,
            credentials,
            session_id,
            session_path: response.session_path,
            state: StreamState::New,
            is_console,
            is_home,
            console_lan_ip: None,
        }
    }

    /// Only local/private addresses are permitted as manual LAN optimizations.
    pub fn set_console_lan_ip(&mut self, ip: Ipv4Addr) {
        if self.is_home && ip.is_private() {
            self.console_lan_ip = Some(ip);
            crate::diagnostics::step("xhome_route", "LAN_IPV4_OVERRIDE_ENABLED");
        }
    }

    pub fn session_path(&self) -> String {
        format!("/{}", self.session_path.trim_start_matches('/'))
    }

    fn session_endpoint(&self, suffix: &str) -> String {
        format!("{}/{}", self.session_path(), suffix)
    }

    pub async fn refresh_state(&mut self) -> Result<StreamState> {
        let response: StateResponse = self
            .api_client
            .request_json(
                &self.credentials,
                Method::GET,
                &self.session_endpoint("state"),
                None,
            )
            .await?;
        let new_state = StreamState::from_api(&response.state);
        if new_state != self.state {
            if self.is_console {
                match new_state {
                    StreamState::Provisioning => crate::diagnostics::step("console_state", "PROVISIONING"),
                    StreamState::WaitingForResources => crate::diagnostics::step("console_state", "WAITING_FOR_RESOURCES"),
                    StreamState::ReadyToConnect => crate::diagnostics::step("console_state", "READY_TO_CONNECT"),
                    StreamState::Provisioned => crate::diagnostics::step("console_state", "PROVISIONED_NOT_VIDEO_PROOF"),
                    StreamState::New | StreamState::Error => {},
                }
            }
        }
        self.state = new_state;
        if new_state == StreamState::Error {
            // The service may include errorDetails.code; never expose raw JSON or secrets.
            let code = response.error_details.as_ref()
                .and_then(|value| value.get("code").or_else(|| value.get("errorCode")))
                .and_then(Value::as_str)
                .unwrap_or("UNCLASSIFIED_FAILURE");
            crate::diagnostics::failure("session_state", code);
            anyhow::bail!("stream session rejected: {}", crate::diagnostics::error_code(code));
        }
        Ok(self.state)
    }

    pub async fn poll_provisioning(&mut self, auth: &mut MsalAuth) -> Result<StreamState> {
        let state = self.refresh_state().await?;
        match state {
            StreamState::ReadyToConnect => {
                let passport_token = auth.get_passport_token().await?;
                self.send_msal_auth(&passport_token).await?;
            }
            StreamState::Error => anyhow::bail!("stream session entered an error state"),
            StreamState::New
            | StreamState::Provisioning
            | StreamState::WaitingForResources
            | StreamState::Provisioned => {}
        }
        Ok(state)
    }

    pub async fn send_sdp_offer(&self, sdp: &str) -> Result<String> {
        let body = json!({
            "messageType": "offer",
            "sdp": sdp,
            "requestId": "1",
            "configuration": {
                "chatConfiguration": {
                    "bytesPerSample": 2,
                    "expectedClipDurationMs": 20,
                    "format": {
                        "codec": "opus",
                        "container": "webm",
                    },
                    "numChannels": 1,
                    "sampleFrequencyHz": 24000,
                },
                "chat": { "minVersion": 1, "maxVersion": 1 },
                "control": { "minVersion": 1, "maxVersion": 3 },
                "input": { "minVersion": 1, "maxVersion": 9 },
                "message": { "minVersion": 1, "maxVersion": 1 },
                "reliableinput": { "minVersion": 9, "maxVersion": 9 },
                "unreliableinput": { "minVersion": 9, "maxVersion": 9 },
            },
        });
        let _: Value = self
            .api_client
            .request_json(
                &self.credentials,
                Method::POST,
                &self.session_endpoint("sdp"),
                Some(&body),
            )
            .await?;
        let response = self.wait_for_sdp_response().await?;
        check_exchange_error(&response)?;
        extract_answer_sdp(&response)
    }

    pub async fn wait_for_sdp_response(&self) -> Result<Value> {
        loop {
            let value: Value = self
                .api_client
                .request_json(
                    &self.credentials,
                    Method::GET,
                    &self.session_endpoint("sdp"),
                    None,
                )
                .await?;
            if value.get("status").and_then(Value::as_u64) != Some(204) {
                return Ok(value);
            }
            sleep(Duration::from_millis(500)).await;
        }
    }

    pub async fn post_ice_candidates(&self, candidates: Vec<RTCIceCandidateInit>) -> Result<()> {
        let body = json!({
            "candidates": candidates
                .iter()
                .map(serialize_local_candidate)
                .collect::<Vec<_>>()
        });
        let response: Value = self
            .api_client
            .request_json(
                &self.credentials,
                Method::POST,
                &self.session_endpoint("ice"),
                Some(&body),
            )
            .await?;
        check_exchange_error(&response)?;
        Ok(())
    }

    pub async fn poll_ice_candidates(&self) -> Result<Option<Vec<RTCIceCandidateInit>>> {
        let value: Value = self
            .api_client
            .request_json(
                &self.credentials,
                Method::GET,
                &self.session_endpoint("ice"),
                None,
            )
            .await?;
        if value.get("status").and_then(Value::as_u64) == Some(204) {
            return Ok(None);
        }
        check_exchange_error(&value)?;
        Ok(Some(extract_remote_candidates(
            &value,
            self.is_home,
            self.console_lan_ip,
        )))
    }

    pub async fn send_keepalive(&self) -> Result<Value> {
        self.api_client
            .request_json(
                &self.credentials,
                Method::POST,
                &self.session_endpoint("keepalive"),
                None,
            )
            .await
    }

    pub async fn send_msal_auth(&self, user_token: &str) -> Result<Value> {
        self.api_client
            .request_json(
                &self.credentials,
                Method::POST,
                &self.session_endpoint("connect"),
                Some(&json!({ "userToken": user_token })),
            )
            .await
    }

    pub async fn stop(&self) -> Result<Value> {
        self.api_client
            .request_json(
                &self.credentials,
                Method::DELETE,
                &self.session_path(),
                None,
            )
            .await
    }
}

fn check_exchange_error(value: &Value) -> Result<()> {
    if let Some(error_details) = value.get("errorDetails").filter(|v| !v.is_null()) {
        anyhow::bail!("xCloud exchange failed: {error_details}");
    }
    Ok(())
}

fn extract_answer_sdp(response: &Value) -> Result<String> {
    response
        .get("exchangeResponse")
        .and_then(Value::as_str)
        .and_then(|exchange| serde_json::from_str::<Value>(exchange).ok())
        .and_then(|parsed| parsed.get("sdp").and_then(Value::as_str).map(str::to_owned))
        .with_context(|| {
            format!("xCloud SDP answer response was missing an SDP payload: {response}")
        })
}

fn serialize_local_candidate(candidate: &RTCIceCandidateInit) -> String {
    let mut value = json!({
        "candidate": candidate.candidate,
        "sdpMid": candidate
            .sdp_mid
            .as_deref()
            .filter(|mid| !mid.is_empty())
            .unwrap_or("0"),
        "sdpMLineIndex": candidate.sdp_mline_index.unwrap_or(0),
    });

    if let Some(username_fragment) = &candidate.username_fragment {
        value["usernameFragment"] = Value::String(username_fragment.clone());
    }

    serde_json::to_string(&value).expect("ICE candidate JSON serialization should not fail")
}

fn extract_remote_candidates(
    response: &Value,
    is_home: bool,
    lan_override: Option<Ipv4Addr>,
) -> Vec<RTCIceCandidateInit> {
    let Some(exchange) = response.get("exchangeResponse").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(exchange) else {
        return Vec::new();
    };
    let candidates = parsed
        .as_array()
        .or_else(|| parsed.get("candidates").and_then(Value::as_array));
    let Some(candidates) = candidates else {
        return Vec::new();
    };

    let mut decoded = Vec::new();
    for item in candidates {
        let parsed: Option<RTCIceCandidateInit> = match item {
            Value::String(value) => serde_json::from_str(value).ok(),
            value => serde_json::from_value(value.clone()).ok(),
        };
        let Some(parsed) = parsed else { continue };
        if is_home {
            for derived in teredo_host_candidates(&parsed, lan_override) {
                if !decoded.iter().any(|known: &RTCIceCandidateInit|
                    known.candidate == derived.candidate) {
                    decoded.push(derived);
                }
            }
        }
        if let Some(normalized) = normalize_decoded_candidate(parsed) {
            if !decoded.iter().any(|known: &RTCIceCandidateInit|
                known.candidate == normalized.candidate) {
                decoded.push(normalized);
            }
        }
    }
    decoded
}

fn normalize_decoded_candidate(mut candidate: RTCIceCandidateInit) -> Option<RTCIceCandidateInit> {
    candidate.candidate = normalize_remote_candidate(&candidate.candidate)?;
    if candidate.sdp_mid.as_deref() == Some("") {
        candidate.sdp_mid = Some("0".to_owned());
    }
    if candidate.sdp_mline_index.is_none() {
        candidate.sdp_mline_index = Some(0);
    }
    Some(candidate)
}

/// Xbox Remote Play can advertise Teredo IPv6 candidates that encode its public
/// IPv4 and UDP port. rtc's Vita IPv4 socket cannot connect to those IPv6
/// candidates directly. Create IPv4 ICE candidates for xHome ONLY; preserve
/// the original IPv4 candidates so LAN hosts keep their normal behavior.
///
/// RFC 4380: 2001:0000::/32, obfuscated UDP port and IPv4 in trailing segments.
fn teredo_host_candidates(
    candidate: &RTCIceCandidateInit,
    lan_override: Option<Ipv4Addr>,
) -> Vec<RTCIceCandidateInit> {
    let Some((wan, encoded_port)) = candidate_address(&candidate.candidate)
        .and_then(teredo_endpoint) else { return Vec::new() };
    let ip = lan_override.filter(Ipv4Addr::is_private).unwrap_or(wan);
    let candidate_for_port = |foundation: u8, port: u16| RTCIceCandidateInit {
        candidate: format!("candidate:{foundation} 1 UDP 1 {ip} {port} typ host"),
        sdp_mid: candidate.sdp_mid.clone().filter(|s| !s.is_empty())
            .or_else(|| Some("0".to_owned())),
        sdp_mline_index: Some(candidate.sdp_mline_index.unwrap_or(0)),
        username_fragment: candidate.username_fragment.clone(),
        url: None,
    };
    let mut out = vec![candidate_for_port(10, XBOX_HOME_STREAM_PORT)];
    if encoded_port != XBOX_HOME_STREAM_PORT && encoded_port != 0 {
        out.push(candidate_for_port(11, encoded_port));
    }
    out
}
fn candidate_address(candidate: &str) -> Option<&str> {
    let trimmed = candidate.trim();
    let trimmed = trimmed.strip_prefix("a=").unwrap_or(trimmed);
    trimmed.starts_with("candidate:").then(|| trimmed.split_whitespace().nth(4)).flatten()
}
fn teredo_endpoint(address: &str) -> Option<(Ipv4Addr, u16)> {
    let segments = address.parse::<Ipv6Addr>().ok()?.segments();
    if segments[0] != 0x2001 || segments[1] != 0 { return None; }
    let port = segments[5] ^ u16::MAX;
    let ipv4_obfuscated = (u32::from(segments[6]) << 16) | u32::from(segments[7]);
    Some((Ipv4Addr::from(ipv4_obfuscated ^ u32::MAX), port))
}

fn normalize_remote_candidate(candidate: &str) -> Option<String> {
    let candidate = candidate.trim();
    if candidate.is_empty()
        || candidate.eq_ignore_ascii_case("a=end-of-candidates")
        || candidate.eq_ignore_ascii_case("end-of-candidates")
    {
        return None;
    }

    let candidate = candidate.strip_prefix("a=").unwrap_or(candidate);
    if !candidate.starts_with("candidate:") {
        return None;
    }
    if candidate
        .split_whitespace()
        .nth(4)
        .is_some_and(|address| address.contains(':'))
    {
        return None;
    }

    Some(candidate.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_local_ice_as_api_xbox_candidate_string() {
        let candidate = RTCIceCandidateInit {
            candidate: "candidate:1 1 udp 1 192.0.2.10 5000 typ host".to_owned(),
            sdp_mid: Some(String::new()),
            sdp_mline_index: Some(0),
            username_fragment: None,
            url: None,
        };

        let serialized = serialize_local_candidate(&candidate);
        let payload: Value =
            serde_json::from_str(&serialized).expect("serialized ICE candidate is valid JSON");

        assert_eq!(
            payload,
            json!({
                "candidate": "candidate:1 1 udp 1 192.0.2.10 5000 typ host",
                "sdpMid": "0",
                "sdpMLineIndex": 0,
            })
        );
    }

    #[test]
    fn extracts_remote_ice_from_array_or_wrapper_payload() {
        let remote = json!([
            {
                "candidate": "a=candidate:2 1 UDP 1 203.0.113.10 9002 typ host ",
                "sdpMid": "0",
                "sdpMLineIndex": 0
            },
            {
                "candidate": "a=end-of-candidates",
                "sdpMid": "0",
                "sdpMLineIndex": 0
            },
            {
                "candidate": "a=candidate:3 1 UDP 1 2603:1050:4:D3::ADB:C44F 9002 typ host ",
                "sdpMid": "0",
                "sdpMLineIndex": 0
            }
        ]);
        let array_response = json!({ "exchangeResponse": remote.to_string() });
        let wrapper_response =
            json!({ "exchangeResponse": json!({ "candidates": remote }).to_string() });

        let candidates = extract_remote_candidates(&array_response, false, None);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].candidate,
            "candidate:2 1 UDP 1 203.0.113.10 9002 typ host"
        );
        assert_eq!(extract_remote_candidates(&wrapper_response, false, None).len(), 1);
    }
    #[test]
    fn recovers_xhome_ipv4_endpoint_from_teredo_without_altering_cloud() {
        let teredo = json!([{"candidate":
            "a=candidate:4 1 UDP 1 2001:0:4136:e378:8000:63bf:3fff:fdd2 9002 typ host",
            "sdpMid":"0", "sdpMLineIndex":0}]);
        let response = json!({"exchangeResponse":teredo.to_string()});
        assert!(extract_remote_candidates(&response, false, None).is_empty());
        let home = extract_remote_candidates(&response, true, None);
        assert_eq!(home.len(), 2);
        assert!(home.iter().any(|c| c.candidate.contains("192.0.2.45 9002")));
        assert!(home.iter().any(|c| c.candidate.contains("192.0.2.45 40000")));
        let lan = extract_remote_candidates(
            &response, true, Some(Ipv4Addr::new(192,168,1,10)));
        assert!(lan.iter().all(|c| c.candidate.contains("192.168.1.10")));
    }
    #[test]
    fn invalid_or_non_teredo_ipv6_does_not_generate_ipv4_candidates() {
        assert_eq!(teredo_endpoint("2603:1050:4:D3::ADB:C44F"), None);
        assert_eq!(teredo_endpoint("bad"), None);
    }

}
