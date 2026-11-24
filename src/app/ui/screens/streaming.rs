use crate::App;
use crate::app::ui::theme::Theme;
use crate::app::ui::widgets::draw_hold_progress_ring;
use crate::i18n::I18n;
use crate::settings::StreamDebugMode;
use std::sync::atomic::Ordering;

/// Fullscreen video view with a one-time "Hold Back to pause" hint, fading out over `HINT_FADE`.
pub(crate) fn show(ctx: &egui::Context, app: &App, hold_progress: Option<f32>) {
    const HINT_VISIBLE: std::time::Duration = std::time::Duration::from_secs(2);
    const HINT_FADE: std::time::Duration = std::time::Duration::from_secs(1);

    let theme = Theme::dark();
    let i18n = I18n::new(app.settings.locale);
    let streaming = match &app.state {
        crate::AppState::Streaming(streaming) => streaming,
        _ => return,
    };
    let mut frame = egui::Frame::central_panel(&ctx.style());
    frame.fill = egui::Color32::TRANSPARENT;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        let elapsed = streaming.hint_started_at.elapsed();
        let alpha = if elapsed < HINT_VISIBLE {
            1.0
        } else if elapsed < HINT_VISIBLE + HINT_FADE {
            1.0 - (elapsed - HINT_VISIBLE).as_secs_f32() / HINT_FADE.as_secs_f32()
        } else {
            0.0
        };

        if alpha > 0.0 {
            ui.vertical_centered(|ui| {
                ui.add_space(16.0);
                ui.colored_label(
                    theme.text.gamma_multiply(alpha),
                    i18n.text("streaming-hold-back"),
                );
            });
        }

        if app.settings.show_stream_debug_info
            || app.settings.stream_debug_mode == StreamDebugMode::Full {
            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                ui.add_space(12.0);
                ui.colored_label(
                    theme.text.gamma_multiply(0.75),
                    egui::RichText::new(&streaming.status).size(12.0),
                );
                ui.add_space(12.0);
            });
        }
    });

    if app.settings.stream_debug_mode == StreamDebugMode::Minimal && !streaming.status.is_empty() {
        let metrics = &crate::streaming::video::metrics::METRICS;
        let bw = metrics.video_bandwidth_kbps.load(Ordering::Relaxed);
        let fps = streaming.status.split_whitespace()
            .find_map(|x| x.strip_prefix("srcfps:")).unwrap_or("?");
        egui::Area::new(egui::Id::new("stream_mini_hud"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 8.0))
            .show(ctx, |ui| {
                egui::Frame::NONE.fill(egui::Color32::from_black_alpha(195))
                    .inner_margin(egui::Margin::same(5))
                    .show(ui, |ui| ui.label(format!("RX {} kbps  FPS {}", bw, fps)));
            });
    }

    if app.settings.latency_test_mode {
        let blink = crate::latency_test::BLINK.load(Ordering::Relaxed);
        let age_ms = crate::latency_test::last_press_age_ms();
        egui::Area::new(egui::Id::new("vita_latency_camera_marker"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::LEFT_TOP, egui::vec2(12.0, 12.0))
            .show(ctx, |ui| {
                let square = egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0), egui::vec2(140.0, 140.0));
                let painter = ui.painter();
                let foreground = if blink { egui::Color32::WHITE } else { egui::Color32::BLACK };
                let border = if blink { egui::Color32::BLACK } else { egui::Color32::WHITE };
                painter.rect_filled(square, 0.0, foreground);
                painter.rect_stroke(square, 0.0,
                    egui::Stroke::new(2.0, border), egui::StrokeKind::Inside);
                let text = age_ms.map(|ms| format!("+{ms:.0} ms"))
                    .unwrap_or_else(|| "Press a button".to_owned());
                ui.add_space(148.0);
                ui.label(egui::RichText::new(text).color(theme.text_bright).monospace());
            });
    }

    if let Some(progress) = hold_progress {
        egui::Area::new(egui::Id::new("pause_hold_indicator"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::LEFT_TOP, egui::vec2(16.0, 16.0))
            .show(ctx, |ui| {
                draw_hold_progress_ring(ui, progress);
            });
    }
}
