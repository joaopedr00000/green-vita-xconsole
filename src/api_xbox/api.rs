//! HTTP client for the xCloud/xHome streaming APIs: sessions, titles, consoles, wait times.

use crate::api_xbox::auth::EndpointCredentials;
use crate::api_xbox::stream::{StartStreamResponse, Stream};
use anyhow::{Context, Result};
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Cosmetic Xbox One Edge client fingerprint. Cannot substitute real Xbox authorization.
const XBOX_ONE_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; Xbox; Xbox One) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/114.0.0.0 Safari/537.36 Edge/44.18363.8131";

const DEVICE_INFO_JSON: &str = r#"{"appInfo":{"env":{"clientAppId":"www.xbox.com","clientAppType":"browser","clientAppVersion":"26.1.97","clientSdkVersion":"10.3.7","httpEnvironment":"prod","sdkInstallId":""}},"dev":{"hw":{"make":"Microsoft","model":"unknown","sdktype":"web"},"os":{"name":"android","ver":"22631.2715","platform":"desktop"},"displayInfo":{"dimensions":{"widthInPixels":1280,"heightInPixels":720},"pixelDensity":{"dpiX":1,"dpiY":1}},"browser":{"browserName":"chrome","browserVersion":"140.0.3485.54"}}}"#;

/// Experimental console client metadata; server-side hardware checks may reject it.
const XBOX_ONE_DEVICE_INFO_JSON: &str = r#"{"appInfo":{"env":{"clientAppId":"XboxOne","clientAppType":"console","clientAppVersion":"10.0.22621","clientSdkVersion":"10.3.7","httpEnvironment":"prod","sdkInstallId":""}},"dev":{"hw":{"make":"Microsoft","model":"Xbox One","sdktype":"console"},"os":{"name":"XboxOne","ver":"10.0.22621","platform":"console"},"displayInfo":{"dimensions":{"widthInPixels":1280,"heightInPixels":720},"pixelDensity":{"dpiX":1,"dpiY":1}},"browser":{"browserName":"edge","browserVersion":"44.18363.8131"}}}"#;

#[derive(Debug, Clone)]
pub struct ApiClientConfig {
    pub locale: String,
    pub home: EndpointCredentials,
    pub cloud: EndpointCredentials,
    pub cloud_f2p: Option<EndpointCredentials>,
    /// Optional xgpuconsole token. Normal catalog and Remote Play use original tokens.
    pub console_cloud: Option<EndpointCredentials>,
}

impl Default for ApiClientConfig {
    fn default() -> Self {
        Self {
            locale: "en-US".to_owned(),
            home: EndpointCredentials {
                host: String::new(),
                token: String::new(),
                default_host: String::new(),
                regions: Vec::new(),
            },
            cloud: EndpointCredentials {
                host: String::new(),
                token: String::new(),
                default_host: String::new(),
                regions: Vec::new(),
            },
            cloud_f2p: None,
            console_cloud: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamKind {
    Home,
    Cloud,
}

impl StreamKind {
    pub fn as_path(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Cloud => "cloud",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsolesResponse {
    pub results: Vec<Console>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitTimeResponse {
    pub estimated_total_wait_time_in_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Console {
    pub device_name: String,
    pub server_id: String,
    pub power_state: String,
    pub console_type: String,
}

#[derive(Debug, Clone)]
pub struct ApiClient {
    client: Client,
    pub config: ApiClientConfig,
}

impl ApiClient {
    pub fn new(config: ApiClientConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_default(),
            config,
        }
    }

    fn credentials_for(&self, kind: StreamKind) -> &EndpointCredentials {
        match kind {
            StreamKind::Home => &self.config.home,
            StreamKind::Cloud => &self.config.cloud,
        }
    }

    pub async fn get_consoles(&self) -> Result<ConsolesResponse> {
        self.get_json(StreamKind::Home, "/v6/servers/home").await
    }

    pub async fn get_titles(&self) -> Result<Value> {
        self.get_json(StreamKind::Cloud, "/v2/titles").await
    }

    pub async fn get_wait_time(
        &self,
        kind: StreamKind,
        target_id: &str,
    ) -> Result<WaitTimeResponse> {
        self.get_json(kind, &format!("/v1/waittime/{target_id}"))
            .await
    }

    pub async fn get_active_session_paths(&self, kind: StreamKind) -> Result<Vec<String>> {
        let response: Value = self
            .get_json(kind, &format!("/v5/sessions/{}/active", kind.as_path()))
            .await?;
        let mut paths = Vec::new();
        collect_session_paths(&response, &mut paths);
        Ok(paths)
    }

    pub async fn stop_session(&self, kind: StreamKind, session_path: &str) -> Result<Value> {
        let path = format!("/{}", session_path.trim_start_matches('/'));
        self.delete_json(kind, &path).await
    }

    /// Original entry point, now with a silent xgpuconsole-first attempt for cloud play.
    /// The game catalog and all other requests remain on the original web offering.
    pub async fn start_stream(&self, kind: StreamKind, title_or_server_id: &str) -> Result<Stream> {
        self.start_stream_internal(kind, title_or_server_id, true).await
    }

    /// Original xgpuweb flow without a console retry; called after a console session fails.
    pub(crate) async fn start_web_stream(&self, title_id: &str) -> Result<Stream> {
        self.start_stream_internal(StreamKind::Cloud, title_id, false).await
    }

    async fn start_stream_internal(
        &self,
        kind: StreamKind,
        title_or_server_id: &str,
        allow_console: bool,
    ) -> Result<Stream> {
        let body = json!({
            "clientSessionId": "",
            "titleId": if kind == StreamKind::Cloud { title_or_server_id } else { "" },
            "systemUpdateGroup": "",
            "settings": {
                "nanoVersion": "V3;WebrtcTransport.dll",
                "enableOptionalDataCollection": false,
                "enableTextToSpeech": false,
                "highContrast": 0,
                "locale": self.config.locale,
                "useIceConnection": false,
                "timezoneOffsetMinutes": 120,
                "sdkType": "web",
                "osName": "android",
            },
            "serverId": if kind == StreamKind::Home { title_or_server_id } else { "" },
            "fallbackRegionNames": [],
        });
        let path = format!("/v5/sessions/{}/play", kind.as_path());

        if kind == StreamKind::Home {
            return self
                .start_stream_with_credentials(self.config.home.clone(), &path, &body, false)
                .await;
        }

        if allow_console {
            if let Some(credentials) = self.config.console_cloud.clone() {
                let mut console_body = body.clone();
                console_body["settings"]["sdkType"] = json!("console");
                console_body["settings"]["osName"] = json!("XboxOne");
                crate::diagnostics::step("xgpuconsole_session", "ATTEMPTING");
                match self.start_stream_with_credentials(credentials, &path, &console_body, true).await {
                    Ok(stream) => {
                        crate::diagnostics::step("xgpuconsole_session", "CREATED_NOT_VIDEO_PROOF");
                        return Ok(stream);
                    },
                    Err(error) => {
                        crate::diagnostics::failure("xgpuconsole_session", &format!("{error:#}"));
                        eprintln!("xgpuconsole session refused; retrying original xgpuweb: {error:#}");
                    }
                }
            }
        }

        match self
            .start_stream_with_credentials(self.config.cloud.clone(), &path, &body, false)
            .await
        {
            Ok(stream) => {
                crate::diagnostics::step("xgpuweb_session", "CREATED");
                Ok(stream)
            },
            Err(error) if error.to_string().contains("OfferingDoesNotContainTitle") => {
                let Some(fallback) = self.config.cloud_f2p.clone() else {
                    return Err(error);
                };
                crate::diagnostics::step("xgpuwebf2p", "FALLBACK_REQUESTED");
                self.start_stream_with_credentials(fallback, &path, &body, false)
                    .await
            }
            Err(error) => Err(error),
        }
    }

    async fn start_stream_with_credentials(
        &self,
        credentials: EndpointCredentials,
        path: &str,
        body: &Value,
        is_console: bool,
    ) -> Result<Stream> {
        let response: StartStreamResponse = self
            .request_json(&credentials, Method::POST, path, Some(body))
            .await?;
        Ok(Stream::new(self.clone(), credentials, response, is_console))
    }

    pub async fn get_json<T>(&self, kind: StreamKind, path: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.request_json(self.credentials_for(kind), Method::GET, path, None)
            .await
    }

    pub async fn delete_json<T>(&self, kind: StreamKind, path: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.request_json(self.credentials_for(kind), Method::DELETE, path, None)
            .await
    }

    /// Shared HTTP+JSON path for both `kind`-routed requests above and `Stream`'s per-session
    /// credential requests in `api_xbox::stream`.
    pub(super) async fn request_json<T>(
        &self,
        credentials: &EndpointCredentials,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let url = format!("{}{}", credentials.host, path);
        let is_console = self.config.console_cloud.as_ref()
            .is_some_and(|console| console.host == credentials.host
                && console.token == credentials.token);
        let mut request = self
            .client
            .request(method, url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header("X-Gssv-Client", if is_console { "XboxOne" } else { "XboxComBrowser" })
            .header("X-MS-Device-Info", if is_console { XBOX_ONE_DEVICE_INFO_JSON } else { DEVICE_INFO_JSON });
        if is_console {
            request = request.header("User-Agent", XBOX_ONE_USER_AGENT);
        }

        if !credentials.token.is_empty() {
            request = request.bearer_auth(&credentials.token);
        }
        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await.context("xCloud HTTP request failed")?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("failed to read xCloud response body")?;
        if !status.is_success() {
            anyhow::bail!("xCloud request failed with {status}: {text}");
        }
        if text.trim().is_empty() {
            let retry = json!({ "status": status.as_u16() });
            return serde_json::from_value(retry)
                .context("failed to decode empty-body status marker");
        }
        serde_json::from_str::<T>(&text)
            .with_context(|| format!("failed to decode xCloud JSON response: {text}"))
    }
}

fn collect_session_paths(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_session_paths(item, output);
            }
        }
        Value::Object(map) => {
            for (key, val) in map {
                if (key == "sessionPath" || key == "path")
                    && let Some(path) = val.as_str()
                {
                    output.push(path.to_owned());
                }
                collect_session_paths(val, output);
            }
        }
        _ => {}
    }
}
