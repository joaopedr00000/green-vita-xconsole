pub mod command;
mod entry;
mod image;
mod service;
mod state;
mod stream_session;
mod titles;
pub(crate) mod ui;

pub(crate) use crate::jobs::{PollJob, poll_job};
pub use command::{AppCommand, InputCommand, NavigationCommand};
pub use image::TitleImage;
pub(crate) use state::AppState;
pub(crate) use stream_session::{StreamStartTarget, StreamingSession, describe_stream_state};

use self::service::Service;
use self::ui::header::MenuState;
use crate::i18n::{I18n, arg_string};
use crate::settings::Settings;
use anyhow::Result;
use fluent_bundle::FluentArgs;
use std::fmt::Display;
use std::time::Instant;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub(crate) struct TitleInitialOverlay {
    pub(crate) label: String,
    pub(crate) shown_at: Instant,
}

pub struct App {
    pub settings: Settings,
    pub(crate) service: Service,
    pub(crate) state: AppState,
    pub(crate) menu: MenuState,
    pub(crate) title_initial_overlay: Option<TitleInitialOverlay>,
    pub(crate) title_search_query: String,
    pub(crate) title_search_requested: bool,
    /// Trigger for the Vita on-screen keyboard to edit a private LAN Xbox address.
    pub(crate) home_ip_edit_requested: bool,
    pub(crate) home_ip_invalid: bool,
    /// At most one automatic stream rejoin per user-initiated launch.
    pub(crate) auto_reconnect_used: bool,
    pub(crate) release_update: Arc<Mutex<Option<String>>>,
    pub(crate) region_ping_ms: Arc<Mutex<HashMap<String, u64>>>, 
}

impl App {
    pub fn new() -> Result<Self> {
        let mut settings = Settings::load();
        // Migrate the previous single debug toggle without losing its user preference.
        if settings.show_stream_debug_info &&
            settings.stream_debug_mode == crate::settings::StreamDebugMode::Off {
            settings.stream_debug_mode = crate::settings::StreamDebugMode::Full;
        }
        crate::settings::set_active_stream_profile(settings.stream_profile);
        crate::settings::set_active_video_options(&settings);
        let service = Service::new(settings.locale.as_str());
        let release_update = Arc::new(Mutex::new(None));
        let update_slot = Arc::clone(&release_update);
        tokio::spawn(async move {
            if let Some(version) = crate::updates::check_for_update().await {
                if let Ok(mut target) = update_slot.lock() {
                    *target = Some(version);
                }
            }
        });
        Ok(Self {
            service,
            state: AppState::InitializeAuthentication,
            menu: MenuState::default(),
            title_initial_overlay: None,
            title_search_query: String::new(),
            title_search_requested: false,
            home_ip_edit_requested: false,
            home_ip_invalid: false,
            auto_reconnect_used: false,
            release_update,
            region_ping_ms: Arc::new(Mutex::new(HashMap::new())), 
            settings,
        })
    }

    pub(crate) fn set_home_lan_ip_from_keyboard(&mut self, input: String) {
        let value = input.trim();
        if value.is_empty() {
            self.settings.home_console_lan_ip = None;
            self.home_ip_invalid = false;
            self.settings.save();
            return;
        }
        let valid = value.parse::<std::net::Ipv4Addr>().ok().filter(|ip| ip.is_private());
        if let Some(ip) = valid {
            self.settings.home_console_lan_ip = Some(ip.to_string());
            self.home_ip_invalid = false;
            self.settings.save();
        } else {
            // No arbitrary endpoints or invalid IPs accepted for candidate rewriting.
            self.home_ip_invalid = true;
        }
    }

    fn set_state(&mut self, state: AppState) {
        self.state = state;
        self.menu.open = false;
    }

    fn set_error_screen(&mut self, reason: impl Into<String>, details: impl Into<String>) {
        let reason = reason.into();
        let details = details.into();
        eprintln!("ERROR: {reason}");
        if !details.is_empty() {
            eprintln!("{details}");
        }
        self.set_state(AppState::Error {
            reason,
            details,
            retry_sign_in: false,
        });
    }

    fn localized_error(&self, reason_key: &'static str, error: impl Display) -> (String, String) {
        let i18n = I18n::new(self.settings.locale);
        let mut args = FluentArgs::new();
        args.set("error", arg_string(error.to_string()));
        (
            i18n.text(reason_key),
            i18n.text_with("error-technical-details", args),
        )
    }

    fn set_localized_error_screen(&mut self, reason_key: &'static str, error: impl Display) {
        let (reason, details) = self.localized_error(reason_key, error);
        self.set_error_screen(reason, details);
    }

    fn set_sign_in_error_screen(&mut self, error: impl Display) {
        let (reason, details) = self.localized_error("error-sign-in-request", error);
        self.set_state(AppState::Error {
            reason,
            details,
            retry_sign_in: true,
        });
    }

    fn localized_error_state(&self, reason_key: &'static str, error: impl Display) -> AppState {
        let (reason, details) = self.localized_error(reason_key, error);
        AppState::Error {
            reason,
            details,
            retry_sign_in: false,
        }
    }

    /// The active cloud title's display name - `None` for Home streams.
    fn active_title_name(&self) -> Option<String> {
        let title_id = self.state.active_title_id()?;
        Some(self.service.title_name_or_id(title_id))
    }
}
