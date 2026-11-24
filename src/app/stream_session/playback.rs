use crate::app::stream_session::session::StreamReturnTarget;
use crate::app::{App, AppState};
use anyhow::Result;

impl App {
    pub(in crate::app) async fn pump_rtc_session(&mut self) -> Result<()> {
        let Some(streaming) = self.state.streaming_mut() else {
            return Ok(());
        };

        let (closed, error) = streaming.drain_backend_events();
        if let Some(error) = error {
            crate::diagnostics::failure("webrtc_error", &error);
            self.stop_stream_with_error("error-webrtc-session", error.to_string())
                .await;
            return Ok(());
        }
        if closed {
            crate::diagnostics::step("webrtc_connection", "CLOSED");
            // A transport close after actual playback may be a brief network drop.
            // Rejoin once only: no unbounded retries or repeated account requests.
            if self.try_reconnect_after_live_stream().await {
                return Ok(());
            }
            if !self.try_original_web_after_console_error().await {
                self.exit_stream().await;
            }
            return Ok(());
        }

        let Some(streaming) = self.state.streaming_mut() else {
            return Ok(());
        };
        if let Some(code) = streaming.maintain_backend().await {
            self.stop_stream_with_error("error-stream-ended", code)
                .await;
        }
        Ok(())
    }

    /// Inspired by XCloud4's brief-network-drop recovery, but limited to one
    /// retry per launch and to streams that actually decoded at least one frame.
    /// The original GreenVita 720p codec/output pipeline is unchanged.
    async fn try_reconnect_after_live_stream(&mut self) -> bool {
        if self.auto_reconnect_used {
            crate::diagnostics::step("network_rejoin", "RETRY_LIMIT_REACHED");
            return false;
        }
        let Some(target) = self.state.streaming()
            .and_then(|streaming| streaming.reconnect_target()) else {
            return false;
        };
        self.auto_reconnect_used = true;
        crate::diagnostics::step("network_rejoin", "ONE_RETRY_AFTER_DECODED_VIDEO");

        let state = std::mem::replace(&mut self.state, AppState::ModeSelect { selected: 0 });
        if let Some(streaming) = state.into_streaming() {
            if let Err(error) = streaming.stop().await {
                crate::diagnostics::failure("network_rejoin_cleanup", &format!("{error:#}"));
            }
        }
        let api = self.service.api.clone();
        let kind = target.kind;
        let target_id = target.target_id.clone();
        let job = Some(tokio::spawn(async move {
            // Expired cloud sessions are cleaned before starting a fresh one.
            super::connection::cleanup_active_sessions(&api, kind).await;
            api.start_stream(kind, &target_id).await
        }));
        self.set_state(AppState::StartingStream { target, job });
        true
    }

    async fn stop_stream_with_error(&mut self, reason_key: &'static str, error: String) {
        if self.try_original_web_after_console_error().await {
            crate::diagnostics::step("webrtc_fallback", "XGPUWEB_REQUESTED");
            eprintln!("Experimental console RTC error; restored xgpuweb: {error}");
            return;
        }
        let state = std::mem::replace(&mut self.state, AppState::ModeSelect { selected: 0 });
        if let Some(streaming) = state.into_streaming() {
            let _ = streaming.stop().await;
        }
        self.set_localized_error_screen(reason_key, error);
    }

    /// A console session can succeed at HTTP login/play but fail only after
    /// WebRTC starts. Keep that failure invisible and restart once using
    /// the original browser offering; web sessions never retry console.
    async fn try_original_web_after_console_error(&mut self) -> bool {
        let Some(target) = self.state.streaming()
            .and_then(|streaming| streaming.web_fallback_target()) else {
            return false;
        };
        let state = std::mem::replace(&mut self.state, AppState::ModeSelect { selected: 0 });
        if let Some(streaming) = state.into_streaming() {
            if let Err(error) = streaming.stop().await {
                eprintln!("Could not stop rejected xgpuconsole session: {error:#}");
            }
        }
        self.start_web_fallback(target);
        true
    }

    /// Manual in-game restart. Preserves game/console selection and reuses the
    /// normal clean session startup; does not retry indefinitely on its own.
    pub(in crate::app) async fn restart_stream(&mut self) {
        let Some(target) = self.state.streaming().map(|session| session.restart_target()) else {
            return;
        };
        let previous = std::mem::replace(&mut self.state, AppState::ModeSelect { selected: 0 });
        if let Some(streaming) = previous.into_streaming() {
            if let Err(error) = streaming.stop().await {
                crate::diagnostics::failure("manual_restart_cleanup", &format!("{error:#}"));
            }
        }
        self.start_stream_for_target(target);
    }

    pub(in crate::app) async fn exit_stream(&mut self) {
        let state = std::mem::replace(&mut self.state, AppState::ModeSelect { selected: 0 });
        let return_target = if let Some(streaming) = state.into_streaming() {
            let return_target = streaming.return_target;
            let description = streaming.backend_description();
            eprintln!("Exiting stream: stopping {description}...");
            match streaming.stop().await {
                Ok(()) => {}
                Err(error) => eprintln!("Failed to stop {description}: {error:#}"),
            }
            return_target
        } else {
            self.set_state(AppState::ModeSelect { selected: 0 });
            return;
        };
        self.set_state(match return_target {
            StreamReturnTarget::Titles(selected) => AppState::TitleList { selected },
            StreamReturnTarget::Consoles(selected) => AppState::ConsoleList { selected },
        });
    }
}
