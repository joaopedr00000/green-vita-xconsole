use anyhow::{Context, Result, bail};
use reqwest::Client;
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

const CLIENT_ID: &str = "1f907974-e22b-4810-a9de-d9647380c97e";
const OAUTH_SCOPE: &str = "xboxlive.signin openid profile offline_access";
const TOKEN_STORE_DIR: &str = "ux0:data/xcloud-rust";
const TOKEN_STORE_PATH: &str = "ux0:data/xcloud-rust/xcloud-tokens.json";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// A public example Xbox One Edge UA, not hardware authentication.
const XBOX_ONE_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; Xbox; Xbox One) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/114.0.0.0 Safari/537.36 Edge/44.18363.8131";
const TOKEN_STORE_VERSION: u8 = 2;
const TOKEN_STORE_BACKUP: &str = "ux0:data/xcloud-rust/xcloud-tokens.backup";
const TOKEN_STORE_STAGE: &str = "ux0:data/xcloud-rust/xcloud-tokens.stage";
const TOKEN_KEY_MAGIC: &[u8; 8] = b"GVTKEY01";
const TOKEN_KEY_SIZE: usize = 32;
const TOKEN_KEY_RECORD_SIZE: usize = TOKEN_KEY_MAGIC.len() + TOKEN_KEY_SIZE;
const TOKEN_KEY_OFFSET: i64 = 0;
const TOKEN_NONCE_SIZE: usize = 12;
const TOKEN_AAD: &[u8] = b"green-vita/xcloud-refresh-token/v1";
const TOKEN_AAD_V2: &[u8] = b"green-vita/xcloud-refresh-token/v2/aes-256-gcm";

#[derive(Debug, Clone)]
pub struct EndpointCredentials {
    pub host: String,
    pub token: String,
    pub default_host: String,
    pub regions: Vec<RegionEndpoint>,
}
/// Only server-advertised HTTPS region hosts are selectable. Never accept
/// arbitrary user-supplied endpoints for requests carrying cloud tokens.
#[derive(Debug, Clone)]
pub struct RegionEndpoint {
    pub name: String,
    pub host: String,
}

#[derive(Debug, Clone)]
pub struct StreamingCredentials {
    pub home: EndpointCredentials,
    pub cloud: EndpointCredentials,
    pub cloud_f2p: Option<EndpointCredentials>,
    /// Optional experimental console offering, never required for normal xCloud login.
    pub console_cloud: Option<EndpointCredentials>,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceCodeResponse {
    user_code: String,
    device_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
    message: String,
}

#[derive(Debug, Clone)]
pub struct DeviceCodeAuth {
    pub user_code: String,
    pub verification_uri: String,
    pub message: String,
    device_code: String,
    pub poll_interval: Duration,
    deadline: Instant,
}

impl DeviceCodeAuth {
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

#[derive(Debug, Clone, Deserialize)]
struct UserTokenResponse {
    access_token: String,
    refresh_token: String,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeErrorResponse {
    error: String,
    error_description: Option<String>,
}

impl DeviceCodeErrorResponse {
    fn description(self) -> String {
        self.error_description.unwrap_or(self.error)
    }
}

pub enum DeviceCodePoll {
    Pending(Duration),
    Authorized,
    Restart,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct TokenStoreData {
    version: u8,
    nonce: String,
    ciphertext: String,
}

#[derive(Debug, Clone, Deserialize)]
struct LegacyTokenStoreData {
    refresh_token: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum StoredTokenData {
    Encrypted(TokenStoreData),
    Legacy(LegacyTokenStoreData),
}

#[derive(Debug, Clone, Deserialize)]
struct XstsTokenResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    display_claims: XstsDisplayClaims,
}

#[derive(Debug, Clone, Deserialize)]
struct XstsDisplayClaims {
    xui: Vec<XstsUserClaim>,
}

#[derive(Debug, Clone, Deserialize)]
struct XstsUserClaim {
    uhs: String,
}

pub struct XboxProfile {
    pub gamertag: Option<String>,
    pub gamerscore: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProfileResponse {
    #[serde(rename = "profileUsers")]
    profile_users: Vec<ProfileUser>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProfileUser {
    settings: Vec<ProfileSetting>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProfileSetting {
    id: String,
    value: String,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamingTokenResponse {
    #[serde(rename = "gsToken")]
    gs_token: String,
    #[serde(rename = "offeringSettings")]
    offering_settings: OfferingSettings,
}

#[derive(Debug, Clone, Deserialize)]
struct OfferingSettings {
    regions: Vec<StreamingRegion>,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamingRegion {
    #[serde(rename = "baseUri")]
    base_uri: String,
    #[serde(rename = "isDefault")]
    is_default: bool,
    #[serde(default)]
    name: Option<String>,
}

impl StreamingTokenResponse {
    fn into_credentials(self) -> Result<EndpointCredentials> {
        let all = self.offering_settings.regions;
        let region = all.iter()
            .find(|region| region.is_default && region.base_uri.starts_with("https://"))
            .context("streaming token response had no valid HTTPS default region")?;
        let default_host = region.base_uri.clone();
        let regions = all.into_iter()
            .filter(|region| region.base_uri.starts_with("https://"))
            .map(|region| RegionEndpoint {
                name: region.name.unwrap_or_else(|| region.base_uri.clone()),
                host: region.base_uri,
            })
            .collect();
        Ok(EndpointCredentials {
            host: default_host.clone(),
            token: self.gs_token,
            default_host,
            regions,
        })
    }
}

#[derive(Clone)]
pub struct MsalAuth {
    client: Client,
    refresh_token: Option<String>,
}

impl MsalAuth {
    pub fn new() -> Self {
        let _ = ensure_token_store_dir();
        let refresh_token = match load_saved_refresh_token() {
            Ok(Some(SavedRefreshToken::Encrypted(token, version))) => {
                if version != TOKEN_STORE_VERSION && persist_refresh_token(&token).is_err() {
                    crate::diagnostics::step("token_vault", "MIGRATION_RETAINED");
                }
                Some(token)
            },
            Ok(Some(SavedRefreshToken::Legacy(token))) => {
                // Keep the existing login until the encrypted replacement is authenticated.
                if persist_refresh_token(&token).is_err() {
                    crate::diagnostics::step("token_vault", "MIGRATION_RETAINED");
                }
                Some(token)
            }
            Ok(None) => None,
            Err(_) => {
                crate::diagnostics::step("token_vault", "READ_FAILED_PRESERVED");
                None
            }
        };

        Self {
            client: Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_default(),
            refresh_token,
        }
    }

    pub fn has_saved_login(&self) -> bool {
        self.refresh_token.is_some()
    }

    pub fn logout(&mut self) {
        self.clear_saved_login();
    }

    fn save_refresh_token(&mut self, refresh_token: String) {
        self.refresh_token = Some(refresh_token.clone());

        if persist_refresh_token(&refresh_token).is_err() {
            crate::diagnostics::step("token_vault", "SAVE_FAILED_RETAINED");
        }
    }

    fn clear_saved_login(&mut self) {
        self.refresh_token = None;
        for path in [TOKEN_STORE_PATH, TOKEN_STORE_BACKUP, TOKEN_STORE_STAGE] {
            let _ = std::fs::remove_file(path);
        }
        if let Err(error) = clear_token_key() {
            eprintln!("Could not clear xCloud login key from Safe Memory: {error:#}");
        }
    }

    async fn post_form(
        &self,
        url: &str,
        body: String,
        context_label: &str,
    ) -> Result<reqwest::Response> {
        self.client
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .with_context(|| format!("{context_label} failed"))
    }

    pub async fn request_device_code(&self) -> Result<DeviceCodeAuth> {
        let response: DeviceCodeResponse = self
            .post_form(
                "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode",
                format!("client_id={CLIENT_ID}&scope={}", urlencode(OAUTH_SCOPE)),
                "device code request",
            )
            .await?
            .error_for_status()
            .context("device code request rejected")?
            .json()
            .await
            .context("failed to decode device code response")?;

        Ok(DeviceCodeAuth {
            user_code: response.user_code,
            verification_uri: response.verification_uri,
            message: response.message,
            device_code: response.device_code,
            poll_interval: Duration::from_secs(response.interval.max(1)),
            deadline: Instant::now() + Duration::from_secs(response.expires_in),
        })
    }

    pub async fn poll_device_code(&mut self, auth: &DeviceCodeAuth) -> Result<DeviceCodePoll> {
        let response = self
            .post_form(
                "https://login.microsoftonline.com/consumers/oauth2/v2.0/token",
                format!(
                    "grant_type=urn:ietf:params:oauth:grant-type:device_code&client_id={CLIENT_ID}&device_code={}",
                    auth.device_code
                ),
                "device code poll request",
            )
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .context("failed to read device code poll response")?;
            let error: DeviceCodeErrorResponse = serde_json::from_str(&body)
                .with_context(|| format!("device code poll rejected with {status}: {body}"))?;

            return match error.error.as_str() {
                "authorization_pending" => Ok(DeviceCodePoll::Pending(auth.poll_interval)),
                "slow_down" => Ok(DeviceCodePoll::Pending(
                    auth.poll_interval + Duration::from_secs(5),
                )),
                "expired_token" | "bad_verification_code" => Ok(DeviceCodePoll::Restart),
                _ => bail!(
                    "device code poll rejected: {}",
                    error.error_description.unwrap_or(error.error)
                ),
            };
        }

        let token: UserTokenResponse = response
            .json()
            .await
            .context("failed to decode device code token response")?;
        self.save_refresh_token(token.refresh_token);
        Ok(DeviceCodePoll::Authorized)
    }

    async fn refresh_user_token(&mut self) -> Result<String> {
        let Some(refresh_token) = self.refresh_token.clone() else {
            bail!("no saved xCloud login to refresh");
        };

        let response = self
            .post_form(
                "https://login.microsoftonline.com/consumers/oauth2/v2.0/token",
                format!(
                "client_id={CLIENT_ID}&grant_type=refresh_token&refresh_token={refresh_token}&scope={}",
                urlencode(OAUTH_SCOPE)
            ),
                "token refresh request",
            )
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .context("failed to read token refresh error response")?;
            let oauth_error = serde_json::from_str::<DeviceCodeErrorResponse>(&body).ok();

            if oauth_error
                .as_ref()
                .is_some_and(|error| error.error == "invalid_grant")
            {
                self.clear_saved_login();
                let description = oauth_error
                    .map(DeviceCodeErrorResponse::description)
                    .unwrap_or(body);
                bail!("saved xCloud login expired; please sign in again: {description}");
            }

            let details = oauth_error
                .map(DeviceCodeErrorResponse::description)
                .unwrap_or(body);
            bail!("token refresh rejected with {status}: {details}");
        }

        let token: UserTokenResponse = response
            .json()
            .await
            .context("failed to decode token refresh response")?;

        self.save_refresh_token(token.refresh_token.clone());
        Ok(token.access_token)
    }

    pub async fn get_passport_token(&mut self) -> Result<String> {
        self.refresh_user_token().await?;
        let Some(refresh_token) = self.refresh_token.clone() else {
            bail!("no saved xCloud login to derive a passport token from");
        };

        let response = self
            .post_form(
                "https://login.live.com/oauth20_token.srf",
                format!(
                    "client_id={CLIENT_ID}&scope=service::http://Passport.NET/purpose::PURPOSE_XBOX_CLOUD_CONSOLE_TRANSFER_TOKEN&grant_type=refresh_token&refresh_token={refresh_token}"
                ),
                "passport token request",
            )
            .await?
            .error_for_status()
            .context("passport token request rejected")?;

        let token: UserTokenResponse = response
            .json()
            .await
            .context("failed to decode passport token response")?;

        Ok(token.access_token)
    }

    /// Shared shape of every xCloud/Xbox Live token exchange below.
    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: impl reqwest::IntoUrl,
        headers: &[(&str, &str)],
        body: &serde_json::Value,
        context_label: &str,
    ) -> Result<T> {
        let mut request = self.client.post(url).json(body);
        for (key, value) in headers {
            request = request.header(*key, *value);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("{context_label} request failed"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .with_context(|| format!("failed to read {context_label} error response"))?;
            bail!("{context_label} rejected with {status}: {body}");
        }

        response
            .json()
            .await
            .with_context(|| format!("failed to decode {context_label} response"))
    }

    async fn xsts_user_authenticate(&self, access_token: &str) -> Result<String> {
        let body = serde_json::json!({
            "Properties": {
                "AuthMethod": "RPS",
                "RpsTicket": format!("d={access_token}"),
                "SiteName": "user.auth.xboxlive.com",
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT",
        });

        let response: XstsTokenResponse = self
            .post_json(
                "https://user.auth.xboxlive.com/user/authenticate",
                &[("x-xbl-contract-version", "1")],
                &body,
                "XSTS user authentication",
            )
            .await?;

        Ok(response.token)
    }

    async fn xsts_authorize(
        &self,
        user_token: &str,
        relying_party: &str,
    ) -> Result<XstsTokenResponse> {
        let body = serde_json::json!({
            "Properties": {
                "SandboxId": "RETAIL",
                "UserTokens": [user_token],
            },
            "RelyingParty": relying_party,
            "TokenType": "JWT",
        });

        self.post_json(
            "https://xsts.auth.xboxlive.com/xsts/authorize",
            &[("x-xbl-contract-version", "1")],
            &body,
            "XSTS authorize",
        )
        .await
    }

    async fn streaming_token(
        &self,
        gssv_token: &str,
        offering: &str,
    ) -> Result<EndpointCredentials> {
        let body = serde_json::json!({
            "token": gssv_token,
            "offeringId": offering,
        });

        let headers: &[(&str, &str)] = if offering == "xgpuconsole" {
            &[("x-gssv-client", "XboxOne"), ("User-Agent", XBOX_ONE_USER_AGENT)]
        } else {
            &[("x-gssv-client", "XboxComBrowser")]
        };
        let response: StreamingTokenResponse = self
            .post_json(
                format!("https://{offering}.gssv-play-prod.xboxlive.com/v2/login/user"),
                headers,
                &body,
                &format!("streaming token (offering {offering})"),
            )
            .await?;

        response.into_credentials()
    }

    pub async fn fetch_streaming_credentials(&mut self) -> Result<StreamingCredentials> {
        let access_token = self.refresh_user_token().await?;
        let web_token = self.xsts_user_authenticate(&access_token).await?;
        let gssv_token = self
            .xsts_authorize(&web_token, "http://gssv.xboxlive.com/")
            .await?
            .token;

        let home = self.streaming_token(&gssv_token, "xhome").await?;

        crate::diagnostics::step("authentication", "XSTS_OK");
        crate::diagnostics::step("xgpuconsole_token", "ATTEMPTING");
        // Silently probe the console offering. A refusal must never prevent the
        // unmodified xgpuweb / xgpuwebf2p flow from authenticating.
        let console_cloud = match self.streaming_token(&gssv_token, "xgpuconsole").await {
            Ok(credentials) => {
                crate::diagnostics::step("xgpuconsole_token", "ACCEPTED_NOT_HARDWARE_PROOF");
                eprintln!("xgpuconsole token accepted; console session is not yet validated");
                Some(credentials)
            }
            Err(error) => {
                crate::diagnostics::failure("xgpuconsole_token", &format!("{error:#}"));
                eprintln!("xgpuconsole token unavailable; falling back to xgpuweb: {error:#}");
                None
            }
        };

        let primary = self.streaming_token(&gssv_token, "xgpuweb").await;
        let f2p = self.streaming_token(&gssv_token, "xgpuwebf2p").await;

        let (cloud, cloud_f2p) = match (primary, f2p) {
            (Ok(cloud), f2p) => (cloud, f2p.ok()),
            (Err(_), Ok(f2p)) => (f2p, None),
            (Err(error), Err(_)) => return Err(error),
        };

        crate::diagnostics::step("xgpuweb_token", "AVAILABLE");
        Ok(StreamingCredentials {
            home,
            cloud,
            cloud_f2p,
            console_cloud,
        })
    }

    /// Gamertag + avatar URL, via a separate XSTS authorization for Xbox Live's own profile API.
    pub async fn fetch_xbox_profile(&mut self) -> Result<XboxProfile> {
        let access_token = self.refresh_user_token().await?;
        let web_token = self.xsts_user_authenticate(&access_token).await?;
        let xbl = self
            .xsts_authorize(&web_token, "http://xboxlive.com")
            .await?;
        // The "user hash" half of an `XBL3.0 x=<uhs>;<token>` Authorization header.
        let uhs = xbl
            .display_claims
            .xui
            .first()
            .map(|xui| xui.uhs.as_str())
            .context("XSTS response had no user hash (uhs)")?;

        let response: ProfileResponse = self
            .client
            .get(
                "https://profile.xboxlive.com/users/me/profile/settings?settings=GameDisplayPicRaw,Gamertag,Gamerscore",
            )
            .header("x-xbl-contract-version", "3")
            .header("Authorization", format!("XBL3.0 x={uhs};{}", xbl.token))
            .send()
            .await
            .context("Xbox profile request failed")?
            .error_for_status()
            .context("Xbox profile request rejected")?
            .json()
            .await
            .context("failed to decode Xbox profile response")?;

        let settings = response
            .profile_users
            .into_iter()
            .next()
            .map(|user| user.settings)
            .unwrap_or_default();
        let setting = |id: &str| {
            settings
                .iter()
                .find(|setting| setting.id == id)
                .map(|setting| setting.value.clone())
        };

        Ok(XboxProfile {
            gamertag: setting("Gamertag"),
            gamerscore: setting("Gamerscore"),
            avatar_url: setting("GameDisplayPicRaw"),
        })
    }
}

fn ensure_token_store_dir() -> Result<()> {
    std::fs::create_dir_all(TOKEN_STORE_DIR).context("failed to create xCloud token directory")
}

enum SavedRefreshToken {
    Encrypted(String, u8),
    Legacy(String),
}

fn read_token_at(path: &str) -> Result<Option<SavedRefreshToken>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to open token vault"),
    };
    match serde_json::from_reader::<_, StoredTokenData>(file).context("invalid token vault")? {
        StoredTokenData::Encrypted(data) => {
            let version = data.version;
            Ok(Some(SavedRefreshToken::Encrypted(decrypt_refresh_token(&data)?, version)))
        }
        StoredTokenData::Legacy(data) => Ok(Some(SavedRefreshToken::Legacy(data.refresh_token))),
    }
}

fn load_saved_refresh_token() -> Result<Option<SavedRefreshToken>> {
    match read_token_at(TOKEN_STORE_PATH) {
        Ok(Some(token)) => {
            // The primary was authenticated (or is an intact legacy record).
            // Retain backups until an encrypted primary has been verified.
            if matches!(&token, SavedRefreshToken::Encrypted(_, 2)) {
                let _ = std::fs::remove_file(TOKEN_STORE_BACKUP);
                let _ = std::fs::remove_file(TOKEN_STORE_STAGE);
            }
            Ok(Some(token))
        }
        primary => match read_token_at(TOKEN_STORE_BACKUP) {
            Ok(Some(token)) => {
                crate::diagnostics::step("token_vault", "BACKUP_RECOVERED");
                // Restore a recovered credential using the verified current format.
                let value = match &token { SavedRefreshToken::Encrypted(value, _) | SavedRefreshToken::Legacy(value) => value };
                let _ = persist_refresh_token(value);
                Ok(Some(token))
            }
            _ => primary,
        },
    }
}

fn persist_refresh_token(refresh_token: &str) -> Result<()> {
    use std::io::Write;
    let data = encrypt_refresh_token(refresh_token)?;
    ensure_token_store_dir()?;
    let _ = std::fs::remove_file(TOKEN_STORE_STAGE);
    {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true)
            .open(TOKEN_STORE_STAGE).context("open staged vault")?;
        file.write_all(&serde_json::to_vec(&data)?)?;
        file.sync_all()?;
    }
    verify_token_at(TOKEN_STORE_STAGE, refresh_token)?;
    // Retain a recoverable old file until the installed replacement is authenticated.
    if std::path::Path::new(TOKEN_STORE_PATH).exists() {
        if std::path::Path::new(TOKEN_STORE_BACKUP).exists() {
            if read_token_at(TOKEN_STORE_PATH).ok().flatten().is_some() {
                std::fs::remove_file(TOKEN_STORE_BACKUP)?;
                std::fs::rename(TOKEN_STORE_PATH, TOKEN_STORE_BACKUP)?;
            } else {
                // The backup is the known recovery copy; remove only the unusable primary.
                if read_token_at(TOKEN_STORE_BACKUP).ok().flatten().is_none() { bail!("no verified recovery copy"); }
                std::fs::remove_file(TOKEN_STORE_PATH)?;
            }
        } else {
            std::fs::rename(TOKEN_STORE_PATH, TOKEN_STORE_BACKUP)?;
        }
    }
    if let Err(error) = std::fs::rename(TOKEN_STORE_STAGE, TOKEN_STORE_PATH) {
        if !std::path::Path::new(TOKEN_STORE_PATH).exists() { let _ = std::fs::rename(TOKEN_STORE_BACKUP, TOKEN_STORE_PATH); }
        return Err(error).context("install staged vault");
    }
    if let Err(error) = verify_token_at(TOKEN_STORE_PATH, refresh_token) {
        if std::path::Path::new(TOKEN_STORE_BACKUP).exists() {
            let _ = std::fs::remove_file(TOKEN_STORE_PATH);
            let _ = std::fs::rename(TOKEN_STORE_BACKUP, TOKEN_STORE_PATH);
        }
        return Err(error);
    }
    let _ = std::fs::remove_file(TOKEN_STORE_BACKUP);
    crate::diagnostics::step("token_vault", "VERIFIED_AES256_GCM");
    Ok(())
}

fn verify_token_at(path: &str, expected: &str) -> Result<()> {
    match read_token_at(path)? {
        Some(SavedRefreshToken::Encrypted(value, TOKEN_STORE_VERSION)) if value == expected => Ok(()),
        _ => bail!("vault readback verification failed"),
    }
}

fn encrypt_refresh_token(refresh_token: &str) -> Result<TokenStoreData> {
    let key = load_or_create_token_key()?;
    let mut nonce = [0u8; TOKEN_NONCE_SIZE];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| anyhow::anyhow!("failed to generate xCloud token nonce"))?;
    let cipher = token_cipher(&key, TOKEN_STORE_VERSION)?;
    let mut ciphertext = refresh_token.as_bytes().to_vec();
    cipher
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(TOKEN_AAD_V2),
            &mut ciphertext,
        )
        .map_err(|_| anyhow::anyhow!("failed to encrypt xCloud login token"))?;
    Ok(TokenStoreData {
        version: TOKEN_STORE_VERSION,
        nonce: encode_hex(&nonce),
        ciphertext: encode_hex(&ciphertext),
    })
}

fn decrypt_refresh_token(data: &TokenStoreData) -> Result<String> {
    if !matches!(data.version, 1 | TOKEN_STORE_VERSION) {
        bail!("unsupported xCloud token store version {}", data.version);
    }
    let nonce = decode_hex(&data.nonce).context("invalid xCloud token nonce")?;
    let nonce: [u8; TOKEN_NONCE_SIZE] = nonce
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid xCloud token nonce length"))?;
    let mut ciphertext = decode_hex(&data.ciphertext).context("invalid xCloud ciphertext")?;
    let key = load_token_key()?;
    let cipher = token_cipher(&key, data.version)?;
    let plaintext = cipher
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(if data.version == 1 { TOKEN_AAD } else { TOKEN_AAD_V2 }),
            &mut ciphertext,
        )
        .map_err(|_| anyhow::anyhow!("xCloud token authentication failed"))?;
    String::from_utf8(plaintext.to_vec()).context("decrypted xCloud token is not UTF-8")
}

fn token_cipher(key: &[u8; TOKEN_KEY_SIZE], version: u8) -> Result<LessSafeKey> {
    // Derive an independent AES key; do not reuse the legacy ChaCha key across algorithms.
    let derived = ring::hmac::sign(&ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key), TOKEN_AAD_V2);
    let (algorithm, material): (&aead::Algorithm, &[u8]) = match version {
        1 => (&aead::CHACHA20_POLY1305, key),
        2 => (&aead::AES_256_GCM, derived.as_ref()),
        _ => bail!("unsupported token cipher"),
    };
    let key = UnboundKey::new(algorithm, material)
        .map_err(|_| anyhow::anyhow!("failed to initialize xCloud token cipher"))?;
    Ok(LessSafeKey::new(key))
}

fn load_token_key() -> Result<[u8; TOKEN_KEY_SIZE]> {
    let record = crate::safe_memory::load::<TOKEN_KEY_RECORD_SIZE>(TOKEN_KEY_OFFSET)?;
    if &record[..TOKEN_KEY_MAGIC.len()] != TOKEN_KEY_MAGIC {
        bail!("xCloud token key is missing from Safe Memory");
    }
    let mut key = [0u8; TOKEN_KEY_SIZE];
    key.copy_from_slice(&record[TOKEN_KEY_MAGIC.len()..]);
    Ok(key)
}

fn load_or_create_token_key() -> Result<[u8; TOKEN_KEY_SIZE]> {
    if let Ok(key) = load_token_key() {
        return Ok(key);
    }
    let mut key = [0u8; TOKEN_KEY_SIZE];
    SystemRandom::new()
        .fill(&mut key)
        .map_err(|_| anyhow::anyhow!("failed to generate xCloud token key"))?;
    let mut record = [0u8; TOKEN_KEY_RECORD_SIZE];
    record[..TOKEN_KEY_MAGIC.len()].copy_from_slice(TOKEN_KEY_MAGIC);
    record[TOKEN_KEY_MAGIC.len()..].copy_from_slice(&key);
    crate::safe_memory::save(TOKEN_KEY_OFFSET, &record)?;
    Ok(key)
}

fn clear_token_key() -> Result<()> {
    crate::safe_memory::save(TOKEN_KEY_OFFSET, &[0u8; TOKEN_KEY_RECORD_SIZE])
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_hex(encoded: &str) -> Result<Vec<u8>> {
    if !encoded.len().is_multiple_of(2) {
        bail!("hex value has an odd length");
    }
    encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = decode_hex_digit(pair[0])?;
            let low = decode_hex_digit(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn decode_hex_digit(digit: u8) -> Result<u8> {
    match digit {
        b'0'..=b'9' => Ok(digit - b'0'),
        b'a'..=b'f' => Ok(digit - b'a' + 10),
        b'A'..=b'F' => Ok(digit - b'A' + 10),
        _ => bail!("invalid hex digit"),
    }
}

fn urlencode(value: &str) -> String {
    value.replace(' ', "%20")
}

#[cfg(test)]
mod vault_crypto_tests {
    use super::*;
    #[test]
    fn algorithms_roundtrip_and_reject_modified_data() {
        for version in [1,2] {
            let key = [42u8;32]; let nonce = [7u8;12];
            let aad = if version == 1 { TOKEN_AAD } else { TOKEN_AAD_V2 };
            let mut data = b"synthetic credential".to_vec();
            let cipher = token_cipher(&key,version).unwrap();
            cipher.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce),Aad::from(aad),&mut data).unwrap();
            let mut tampered = data.clone(); tampered[0] ^= 1;
            assert!(cipher.open_in_place(Nonce::assume_unique_for_key(nonce),Aad::from(aad),&mut tampered).is_err());
            assert_eq!(cipher.open_in_place(Nonce::assume_unique_for_key(nonce),Aad::from(aad),&mut data).unwrap(),b"synthetic credential");
        }
    }
}
