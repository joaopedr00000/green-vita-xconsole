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
            self.stop_stream_with_error("error-webrtc-session", error.to_string())
                .await;
            return Ok(());
        }
        if closed {
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

    async fn stop_stream_with_error(&mut self, reason_key: &'static str, error: String) {
        if self.try_original_web_after_console_error().await {
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
