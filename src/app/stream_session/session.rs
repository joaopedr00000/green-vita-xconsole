use crate::api::streaming::{PlaybackBackend, PlaybackBackendEvent};
use super::StreamStartTarget;
use crate::settings::Settings;
use crate::streaming::input::{GamepadFrame, PointerEvent};
use crate::streaming::video::{DecodedFrame, DirectVideoOutput};
use crate::{Stream, StreamKind};
use anyhow::Result;
use bytes::Bytes;
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy)]
pub(super) enum StreamReturnTarget {
    Titles(usize),
    Consoles(usize),
}

pub(crate) struct StreamingSession {
    pub(crate) paused: bool,
    pub(crate) status: String,
    pub(crate) hint_started_at: Instant,
    pub(in crate::app) pause_selected: usize,
    pub(in crate::app) title_id: Option<String>,
    pub(super) return_target: StreamReturnTarget,
    /// Internal metadata for one automatic fallback; not exposed in menus.
    is_console_attempt: bool,
    launch_target_id: String,
    launch_label: String,
    backend: PlaybackBackend,
    latest_video_frame: Option<u64>,
    current_video_frame: Option<DecodedFrame>,
    stream_video_size: Option<(u32, u32)>,
    pending_audio_packets: Vec<Bytes>,
    ignore_confirm_until_release: bool,
}

impl StreamingSession {
    pub(super) fn start_xbox(
        stream: Stream,
        kind: StreamKind,
        title_id: Option<String>,
        return_selected: usize,
        launch_target_id: String,
        launch_label: String,
    ) -> Result<Self> {
        let is_console_attempt = stream.is_console;
        let backend = PlaybackBackend::start_xbox(stream)?;
        let return_target = match kind {
            StreamKind::Cloud => StreamReturnTarget::Titles(return_selected),
            StreamKind::Home => StreamReturnTarget::Consoles(return_selected),
        };
        Ok(Self {
            paused: false,
            status: "Starting streaming backend".to_owned(),
            hint_started_at: Instant::now(),
            pause_selected: 0,
            title_id,
            return_target,
            is_console_attempt,
            launch_target_id,
            launch_label,
            backend,
            latest_video_frame: None,
            current_video_frame: None,
            stream_video_size: None,
            pending_audio_packets: Vec::new(),
            ignore_confirm_until_release: false,
        })
    }

    pub(super) fn web_fallback_target(&self) -> Option<StreamStartTarget> {
        if !self.is_console_attempt {
            return None;
        }
        let StreamReturnTarget::Titles(selected) = self.return_target else {
            return None;
        };
        Some(StreamStartTarget {
            kind: StreamKind::Cloud,
            target_id: self.launch_target_id.clone(),
            game_id: self.title_id.clone(),
            label: self.launch_label.clone(),
            return_selected: selected,
        })
    }

    /// Rejoin only after a real decoded video frame. Never mistake HTTP
    /// session creation or device spoofing for playable video.
    pub(super) fn reconnect_target(&self) -> Option<StreamStartTarget> {
        self.latest_video_frame?;
        Some(self.restart_target())
    }

    /// Explicit user request: safe even when no video frame has decoded yet.
    pub(super) fn restart_target(&self) -> StreamStartTarget {
        let (kind, selected) = match self.return_target {
            StreamReturnTarget::Titles(selected) => (StreamKind::Cloud, selected),
            StreamReturnTarget::Consoles(selected) => (StreamKind::Home, selected),
        };
        StreamStartTarget {
            kind,
            target_id: self.launch_target_id.clone(),
            game_id: self.title_id.clone(),
            label: self.launch_label.clone(),
            return_selected: selected,
        }
    }

    pub(crate) fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        if paused {
            crate::native_input::disable();
            self.backend.send_gamepad_frame(GamepadFrame::default());
        }
        self.hint_started_at = Instant::now();
    }

    pub(crate) fn take_audio_packets(&mut self) -> Vec<Bytes> {
        std::mem::take(&mut self.pending_audio_packets)
    }

    pub(crate) fn video_frame(&self) -> Option<(u64, &DecodedFrame)> {
        Some((self.latest_video_frame?, self.current_video_frame.as_ref()?))
    }

    pub(crate) fn video_size(&self) -> Option<(u32, u32)> {
        self.stream_video_size
    }

    pub(crate) fn direct_video_output(&self) -> Arc<DirectVideoOutput> {
        self.backend.direct_video_output()
    }

    pub(crate) fn send_gamepad_frame(&mut self, mut frame: GamepadFrame, settings: &Settings) {
        if self.ignore_confirm_until_release {
            if frame.a > 0.0 {
                return;
            }
            self.ignore_confirm_until_release = false;
        }

        let swap_shoulders_and_triggers = self
            .title_id
            .as_deref()
            .and_then(|title_id| settings.game_profile(title_id))
            .is_some_and(|profile| profile.swap_shoulders_and_triggers);
        if swap_shoulders_and_triggers {
            std::mem::swap(&mut frame.left_shoulder, &mut frame.left_trigger);
            std::mem::swap(&mut frame.right_shoulder, &mut frame.right_trigger);
        }
        frame.remap(&settings.control_map);
        self.backend.send_gamepad_frame(frame);
    }

    pub(crate) fn native_config(&self, settings: &Settings) -> Option<crate::native_input::Config> {
        if self.ignore_confirm_until_release { return None; }
        let swap = self.title_id.as_deref().and_then(|id| settings.game_profile(id)).is_some_and(|p| p.swap_shoulders_and_triggers);
        Some(crate::native_input::Config { rear: self.rear_touch_enabled(settings), front: self.front_touch_auxiliary_buttons(settings), swap, map: settings.control_map, deadzone_percent: settings.native_deadzone_percent })
    }

    pub(crate) fn front_touch_auxiliary_buttons(&self, settings: &Settings) -> bool {
        self.title_id
            .as_deref()
            .and_then(|title_id| settings.game_profile(title_id))
            .is_some_and(|profile| profile.front_touch_auxiliary_buttons)
    }

    pub(crate) fn rear_touch_enabled(&self, settings: &Settings) -> bool {
        self.title_id
            .as_deref()
            .and_then(|title_id| settings.game_profile(title_id))
            .is_none_or(|profile| profile.rear_touch_enabled)
    }

    pub(crate) fn press_guide_button(&mut self) {
        self.ignore_confirm_until_release = true;
        self.backend.send_gamepad_pulse(GamepadFrame {
            nexus: 1.0,
            ..Default::default()
        });
    }

    pub(crate) fn send_pointer_event(&self, event: PointerEvent) {
        self.backend.send_pointer_event(event);
    }

    pub(super) fn drain_backend_events(&mut self) -> (bool, Option<String>) {
        let mut events = Vec::new();
        while let Some(event) = self.backend.try_recv_event() {
            events.push(event);
        }
        while let Some(mut packets) = self.backend.try_recv_audio_packets() {
            self.pending_audio_packets.append(&mut packets);
        }

        if let Some((frame_id, frame)) = self.backend.take_latest_frame() {
            if self.latest_video_frame.is_none() {
                crate::diagnostics::step("webrtc_video", if self.is_console_attempt {
                    "CONSOLE_FIRST_DECODED_FRAME"
                } else {
                    "WEB_FIRST_DECODED_FRAME"
                });
            }
            self.latest_video_frame = Some(frame_id);
            self.current_video_frame = Some(frame);
        }

        let mut closed = false;
        let mut error = None;
        for event in events {
            match event {
                PlaybackBackendEvent::Status(status) => self.status = status,
                PlaybackBackendEvent::VideoResolution(width, height) => {
                    crate::diagnostics::video_resolution(width, height);
                    self.stream_video_size = Some((width, height));
                }
                PlaybackBackendEvent::Closed => closed = true,
                PlaybackBackendEvent::Error(message) => error = Some(message),
            }
        }

        (closed, error)
    }

    pub(super) async fn maintain_backend(&mut self) -> Option<String> {
        self.backend.maintain().await
    }

    pub(super) fn backend_description(&self) -> String {
        self.backend.description()
    }

    pub(super) async fn stop(self) -> Result<()> {
        crate::native_input::disable();
        self.backend.send_gamepad_frame(GamepadFrame::default());
        self.backend.stop().await
    }
}
