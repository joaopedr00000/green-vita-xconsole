//! User-editable settings, persisted to `ux0:data/xcloud-rust/settings.json` on each change.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};

/// Safe, user-selectable streaming policies. Quality requests never raise the Vita decoder
/// above 720p; the Xbox backend may ignore RTCP REMB feedback.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum StreamProfile {
    LowLatency,
    Balanced,
    #[default]
    Quality,
}
impl StreamProfile {
    pub const ALL: [Self; 3] = [Self::LowLatency, Self::Balanced, Self::Quality];
    pub const fn bitrate_cap(self) -> u32 {
        match self {
            Self::LowLatency => 6_000_000,
            Self::Balanced => 10_000_000,
            Self::Quality => 15_000_000,
        }
    }
    pub const fn starting_bitrate(self) -> u32 {
        match self {
            Self::LowLatency => 3_500_000,
            Self::Balanced => 6_000_000,
            Self::Quality => 6_000_000,
        }
    }
}
static ACTIVE_STREAM_PROFILE: AtomicU8 = AtomicU8::new(2);
// Hardware-tested Extreme policy is universal, independent of saved settings.
pub(crate) const fn latency_mode()->u8{crate::streaming::policy::UNIVERSAL_LATENCY_MODE}
pub(crate) fn set_active_stream_profile(profile: StreamProfile) {
    ACTIVE_STREAM_PROFILE.store(match profile {
        StreamProfile::LowLatency => 0,
        StreamProfile::Balanced => 1,
        StreamProfile::Quality => 2,
    }, Ordering::Relaxed);
}
pub(crate) fn active_stream_profile() -> StreamProfile {
    match ACTIVE_STREAM_PROFILE.load(Ordering::Relaxed) {
        0 => StreamProfile::LowLatency,
        2 => StreamProfile::Quality,
        _ => StreamProfile::Balanced,
    }
}


const SETTINGS_DIR: &str = "ux0:data/xcloud-rust";
const SETTINGS_PATH: &str = "ux0:data/xcloud-rust/settings.json";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Locale {
    #[default]
    EnUs,
    EnGb,
    PtBr,
    PtPt,
    EsEs,
    EsMx,
    FrFr,
    DeDe,
    ItIt,
    JaJp,
    KoKr,
    ZhCn,
    ZhTw,
    RuRu,
    PlPl,
    NlNl,
    SvSe,
    TrTr,
    ArSa,
}

impl Locale {
    pub const ALL: [Locale; 19] = [
        Self::EnUs,
        Self::EnGb,
        Self::PtBr,
        Self::PtPt,
        Self::EsEs,
        Self::EsMx,
        Self::FrFr,
        Self::DeDe,
        Self::ItIt,
        Self::JaJp,
        Self::KoKr,
        Self::ZhCn,
        Self::ZhTw,
        Self::RuRu,
        Self::PlPl,
        Self::NlNl,
        Self::SvSe,
        Self::TrTr,
        Self::ArSa,
    ];

    /// `(locale code, store market, native-language label)`.
    fn info(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::EnUs => ("en-US", "US", "English (US)"),
            Self::EnGb => ("en-GB", "GB", "English (UK)"),
            Self::PtBr => ("pt-BR", "BR", "Português (Brasil)"),
            Self::PtPt => ("pt-PT", "PT", "Português (Portugal)"),
            Self::EsEs => ("es-ES", "ES", "Español (España)"),
            Self::EsMx => ("es-MX", "MX", "Español (México)"),
            Self::FrFr => ("fr-FR", "FR", "Français"),
            Self::DeDe => ("de-DE", "DE", "Deutsch"),
            Self::ItIt => ("it-IT", "IT", "Italiano"),
            Self::JaJp => ("ja-JP", "JP", "日本語"),
            Self::KoKr => ("ko-KR", "KR", "한국어"),
            Self::ZhCn => ("zh-CN", "CN", "中文（简体）"),
            Self::ZhTw => ("zh-TW", "TW", "中文（繁體）"),
            Self::RuRu => ("ru-RU", "RU", "Русский"),
            Self::PlPl => ("pl-PL", "PL", "Polski"),
            Self::NlNl => ("nl-NL", "NL", "Nederlands"),
            Self::SvSe => ("sv-SE", "SE", "Svenska"),
            Self::TrTr => ("tr-TR", "TR", "Türkçe"),
            Self::ArSa => ("ar-SA", "SA", "العربية"),
        }
    }

    /// The locale code sent to xCloud, e.g. `"pt-BR"`.
    pub fn as_str(self) -> &'static str {
        self.info().0
    }

    /// The Microsoft Store catalog's `market` query param, e.g. `"BR"`.
    pub fn market(self) -> &'static str {
        self.info().1
    }

    /// Display label in the language's own native name, e.g. `"Русский"`.
    pub fn label(self) -> &'static str {
        self.info().2
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub locale: Locale,
    /// Shows internal stream/session state on the `Streaming` screen. Off by default.
    pub show_stream_debug_info: bool,
    /// 0 summary, 1 network, 2 video/audio, 3 controls, 4 overview.
    pub hud_page:u8,
    pub hud_opacity:u8,
    pub stream_profile: StreamProfile,
    /// Only a host returned in the signed-in account's regional allowlist is accepted.
    pub preferred_cloud_region: Option<String>,
    pub game_profiles: HashMap<String, GameProfile>,
    pub control_map: [u8; 16],
    pub native_deadzone_percent: u8,
    pub lab_performance_clocks:bool,
    pub lab_triple_buffer:bool,
    pub lab_low_delay_test:bool,
    pub lab_audio_gain_percent:u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GameProfile {
    pub swap_shoulders_and_triggers: bool,
    /// When enabled, front touch mirrors the rear L2/R2/L3/R3 zones instead of acting as a
    /// clickable xCloud pointer. Defaults to pointer mode for existing profiles.
    pub front_touch_auxiliary_buttons: bool,
    /// Controls only input from the physical rear touch panel. Front touch remains independent.
    pub rear_touch_enabled: bool,
    pub front_touch_stick_clicks: bool,
}

impl Default for GameProfile {
    fn default() -> Self {
        Self {
            swap_shoulders_and_triggers: false,
            front_touch_auxiliary_buttons: false,
            rear_touch_enabled: true,
            front_touch_stick_clicks: false,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            locale: Locale::default(),
            show_stream_debug_info: false,
            hud_page:0,hud_opacity:85,
            stream_profile: StreamProfile::Quality,
            preferred_cloud_region: None,
            game_profiles: HashMap::new(),
            control_map: crate::streaming::input::default_control_map(),
            native_deadzone_percent: 0,
            lab_performance_clocks:false,lab_triple_buffer:true,lab_low_delay_test:false,lab_audio_gain_percent:100,
        }
    }
}

impl Settings {
    pub fn game_profile(&self, title_id: &str) -> Option<&GameProfile> {
        self.game_profiles.get(title_id)
    }

    pub fn set_swap_shoulders_and_triggers(&mut self, title_id: String, enabled: bool) {
        self.game_profiles
            .entry(title_id)
            .or_default()
            .swap_shoulders_and_triggers = enabled;
    }

    pub fn set_front_touch_auxiliary_buttons(&mut self, title_id: String, enabled: bool) {
        self.game_profiles
            .entry(title_id)
            .or_default()
            .front_touch_auxiliary_buttons = enabled;
    }

    pub fn set_rear_touch_enabled(&mut self, title_id: String, enabled: bool) {
        self.game_profiles
            .entry(title_id)
            .or_default()
            .rear_touch_enabled = enabled;
    }

    pub fn set_front_touch_stick_clicks(&mut self,title_id:String,enabled:bool){
        self.game_profiles.entry(title_id).or_default().front_touch_stick_clicks=enabled;
    }

    /// Loads from disk, falling back to defaults on any error.
    pub fn load() -> Self {
        for path in [SETTINGS_PATH, "ux0:data/xcloud-rust/settings.backup"] {
            if let Ok(data) = std::fs::read_to_string(path) {
                if let Ok(mut settings) = serde_json::from_str::<Self>(&data) {
                    if settings.control_map.iter().any(|v| *v > 16) { settings.control_map = crate::streaming::input::default_control_map(); }
                    settings.native_deadzone_percent = settings.native_deadzone_percent.min(20);
                    return settings;
                }
            }
        }
        Self::default()
    }

    /// Saves to disk; errors are logged rather than surfaced.
    pub fn save(&self) {
        let result = std::fs::create_dir_all(SETTINGS_DIR)
            .context("failed to create settings directory")
            .and_then(|_| {
                serde_json::to_string_pretty(self).context("failed to serialize settings")
            })
            .and_then(|data| {
                use std::io::Write;
                const STAGE: &str = "ux0:data/xcloud-rust/settings.stage";
                const BACKUP: &str = "ux0:data/xcloud-rust/settings.backup";
                let _ = std::fs::remove_file(STAGE);
                { let mut out = std::fs::OpenOptions::new().write(true).create_new(true).open(STAGE)?; out.write_all(data.as_bytes())?; out.sync_all()?; }
                let _: Self = serde_json::from_str(&std::fs::read_to_string(STAGE)?)?;
                if std::fs::rename(STAGE, SETTINGS_PATH).is_err() {
                    if std::path::Path::new(SETTINGS_PATH).exists() {
                        let _ = std::fs::remove_file(BACKUP);
                        std::fs::rename(SETTINGS_PATH, BACKUP)?;
                    }
                    if let Err(error) = std::fs::rename(STAGE, SETTINGS_PATH) {
                        if !std::path::Path::new(SETTINGS_PATH).exists() { let _ = std::fs::rename(BACKUP, SETTINGS_PATH); }
                        return Err(error.into());
                    }
                }
                let _ = std::fs::remove_file(BACKUP);
                Ok(())
            });

        if let Err(error) = result {
            eprintln!("Settings: failed to save: {error:#}");
        }
    }
}
