use super::state::CredentialsLoadResult;
use super::stream_session::cleanup_active_sessions;
use super::{App, AppState, PollJob, poll_job};
use crate::api::catalog::Game;
use crate::api::catalog::worker::ImageKind;
use crate::{DeviceCodeAuth, DeviceCodePoll, MsalAuth, StreamKind};
use anyhow::Result;
use tokio::task::JoinHandle;

const AVATAR_KEY: &str = "__avatar__";

impl App {
    pub(super) fn load_credentials(&mut self) {
        if crate::shell::netctl::blocks_sign_in(crate::shell::netctl::wifi_state()) {
            self.set_localized_error_screen(
                "error-sign-in-request",
                "PS Vita Wi-Fi is disconnected. Reconnect and retry.".to_owned()
            );
            return;
        }
        let mut auth = self.service.auth.clone();
        self.set_state(AppState::LoadingCredentials(tokio::spawn(async move {
            let credentials = match auth.fetch_streaming_credentials().await {
                Ok(credentials) => credentials,
                Err(error) => {
                    return Ok(CredentialsLoadResult {
                        result: Err(error),
                        auth,
                    });
                }
            };
            // Profile data is decorative and must not prevent sign-in.
            let profile = match auth.fetch_xbox_profile().await {
                Ok(profile) => Some(profile),
                Err(error) => {
                    eprintln!("Failed to fetch Xbox profile (avatar/gamertag): {error:#}");
                    None
                }
            };
            Ok(CredentialsLoadResult {
                result: Ok((credentials, profile)),
                auth,
            })
        })));
    }

    pub(super) fn request_device_code(&mut self) {
        if crate::shell::netctl::blocks_sign_in(crate::shell::netctl::wifi_state()) {
            self.set_localized_error_screen(
                "error-sign-in-request",
                "PS Vita Wi-Fi is disconnected. Reconnect and retry.".to_owned()
            );
            return;
        }
        let auth = self.service.auth.clone();
        self.set_state(AppState::RequestingDeviceCode(tokio::spawn(async move {
            auth.request_device_code().await
        })));
    }

    pub(super) fn choose_stream_kind(&mut self, kind: StreamKind) -> Result<()> {
        self.enter_entrypoint(kind)
    }

    fn enter_entrypoint(&mut self, kind: StreamKind) -> Result<()> {
        match kind {
            StreamKind::Cloud => {
                let catalog = self.service.catalog_backend.clone();
                self.set_state(AppState::LoadingTitles(tokio::spawn(async move {
                    catalog.load_games().await
                })));
            }
            StreamKind::Home => {
                let api = self.service.api.clone();
                self.set_state(AppState::LoadingConsoles(tokio::spawn(async move {
                    api.get_consoles().await
                })));
            }
        }
        Ok(())
    }

    pub(super) async fn pump_entry_state(&mut self) -> Result<()> {
        let state = std::mem::replace(&mut self.state, AppState::InitializeAuthentication);

        match state {
            AppState::RequestingDeviceCode(job) => self.pump_device_code_request(job).await,
            AppState::WaitingForDeviceAuthorization { device_code, job } => {
                self.pump_device_authorization_check(device_code, job).await;
            }
            AppState::LoadingCredentials(job) => self.pump_credentials(job).await,
            AppState::LoadingTitles(job) => self.pump_titles(job).await,
            AppState::LoadingConsoles(job) => self.pump_consoles(job).await,
            state => self.state = state,
        }

        Ok(())
    }

    async fn pump_device_code_request(&mut self, job: JoinHandle<Result<DeviceCodeAuth>>) {
        match poll_job(job).await {
            PollJob::Pending(job) => self.state = AppState::RequestingDeviceCode(job),
            PollJob::Done(Ok(device_code)) => {
                let job = self.spawn_device_authorization_poll(&device_code);
                self.set_state(AppState::WaitingForDeviceAuthorization { device_code, job });
            }
            PollJob::Done(Err(error)) => {
                self.set_localized_error_screen("error-sign-in-request", format!("{error:#}"));
            }
        }
    }

    async fn pump_device_authorization_check(
        &mut self,
        device_code: DeviceCodeAuth,
        job: JoinHandle<Result<MsalAuth>>,
    ) {
        match poll_job(job).await {
            PollJob::Pending(job) => {
                self.state = AppState::WaitingForDeviceAuthorization { device_code, job };
            }
            PollJob::Done(Ok(auth)) => {
                self.service.auth = auth;
                self.load_credentials();
            }
            PollJob::Done(Err(error)) => {
                eprintln!("Sign-in check failed: {error:#}");
                self.request_device_code();
            }
        }
    }

    fn spawn_device_authorization_poll(
        &self,
        device_code: &DeviceCodeAuth,
    ) -> JoinHandle<Result<MsalAuth>> {
        let mut auth = self.service.auth.clone();
        let device_code = device_code.clone();

        tokio::spawn(async move {
            let mut retry_after = device_code.poll_interval;
            loop {
                tokio::time::sleep(retry_after).await;
                if device_code.is_expired() {
                    anyhow::bail!("device code expired");
                }

                match auth.poll_device_code(&device_code).await {
                    Ok(DeviceCodePoll::Authorized) => return Ok(auth),
                    Ok(DeviceCodePoll::Pending(next_retry)) => retry_after = next_retry,
                    Ok(DeviceCodePoll::Restart) => anyhow::bail!("device code expired"),
                    Err(error) => {
                        eprintln!("Sign-in poll failed, retrying: {error:#}");
                        retry_after = device_code.poll_interval;
                    }
                }
            }
        })
    }

    async fn pump_credentials(&mut self, job: JoinHandle<Result<CredentialsLoadResult>>) {
        match poll_job(job).await {
            PollJob::Pending(job) => self.state = AppState::LoadingCredentials(job),
            PollJob::Done(Ok(CredentialsLoadResult {
                result: Ok((credentials, profile)),
                auth,
            })) => {
                self.service.api.config.home = credentials.home;
                let mut cloud = credentials.cloud;
                if let Some(preferred) = self.settings.preferred_cloud_region.as_deref() {
                    if cloud.regions.iter().any(|region| region.host == preferred) {
                        cloud.host = preferred.to_owned();
                    } else {
                        // An obsolete preference must never redirect tokens to an unknown host.
                        self.settings.preferred_cloud_region = None;
                        self.settings.save();
                    }
                }
                let regions = cloud.regions.clone();
                self.service.api.config.cloud = cloud;
                self.service.api.config.cloud_f2p = credentials.cloud_f2p;
                self.service.api.config.console_cloud = credentials.console_cloud;
                self.service.auth = auth;
                self.service.refresh_xcloud_catalog_backend();
                // HTTPS handshake RTT is a rough region-selection hint, not xCloud
                // end-to-end streaming latency. Never send a bearer token in the probe.
                let ping_slot = std::sync::Arc::clone(&self.region_ping_ms);
                tokio::spawn(async move {
                    let client = match reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(2))
                        .build() {
                        Ok(client) => client,
                        Err(_) => return,
                    };
                    for region in regions.iter().take(12) {
                        let start = std::time::Instant::now();
                        if client.head(&region.host).send().await.is_ok() {
                            let ms = start.elapsed().as_millis().min(10_000) as u64;
                            if let Ok(mut ping) = ping_slot.lock() {
                                ping.insert(region.host.clone(), ms);
                            }
                        }
                    }
                });
                if let Some(profile) = profile {
                    self.service.gamertag = profile.gamertag;
                    self.service.gamerscore = profile.gamerscore;
                    if let Some(url) = profile.avatar_url {
                        self.service.catalog_worker.request_image(
                            AVATAR_KEY.to_owned(),
                            ImageKind::Avatar,
                            url,
                            None,
                        );
                    }
                }
                let api = self.service.api.clone();
                tokio::spawn(async move {
                    cleanup_active_sessions(&api, StreamKind::Cloud).await;
                    cleanup_active_sessions(&api, StreamKind::Home).await;
                });
                self.set_state(AppState::ModeSelect { selected: 0 });
            }
            PollJob::Done(Ok(CredentialsLoadResult {
                result: Err(error),
                auth,
            })) => {
                self.service.auth = auth;
                eprintln!("Xbox sign-in failed: {error:#}");
                self.set_sign_in_error_screen(format!("{error:#}"));
            }
            PollJob::Done(Err(error)) => {
                eprintln!("Xbox sign-in task failed: {error:#}");
                self.set_sign_in_error_screen(format!("{error:#}"));
            }
        }
    }

    async fn pump_titles(&mut self, job: JoinHandle<Result<Vec<Game>>>) {
        match poll_job(job).await {
            PollJob::Pending(job) => self.state = AppState::LoadingTitles(job),
            PollJob::Done(Ok(games)) => {
                crate::diagnostics::step("catalog", "WEB_CATALOG_AVAILABLE");
                self.service.titles = games;
                self.set_state(AppState::TitleList { selected: 0 });
            }
            PollJob::Done(Err(error)) => {
                crate::diagnostics::failure("catalog", &format!("{error:#}"));
                self.set_localized_error_screen("error-title-request", format!("{error:#}"));
            }
        }
    }

    async fn pump_consoles(&mut self, job: JoinHandle<Result<crate::ConsolesResponse>>) {
        match poll_job(job).await {
            PollJob::Pending(job) => self.state = AppState::LoadingConsoles(job),
            PollJob::Done(Ok(response)) => {
                self.service.consoles = response.results;
                self.set_state(AppState::ConsoleList { selected: 0 });
            }
            PollJob::Done(Err(error)) => {
                self.set_localized_error_screen("error-console-request", format!("{error:#}"));
            }
        }
    }
}
