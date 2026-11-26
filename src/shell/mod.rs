#[link(name = "SDL2", kind = "static")]
#[link(name = "SceAudio_stub", kind = "static")]
unsafe extern "C" {}

mod egui_painter;
mod surface;

use crate::app::ui::build_ui;
use crate::input::{
    RearTouchButtons, back_button_held, held_menu_direction, map_controller_button_event,
    map_keyboard_event, map_pointer_event, map_stream_pointer_event, open_first_controller,
    read_gamepad_frame,
};
use crate::streaming::audio::AudioRenderer;
use crate::streaming::video::{STREAM_HEIGHT, STREAM_WIDTH};
use crate::{App, AppState, InputCommand, NavigationCommand};
use anyhow::{Context, Result};
use sdl2::event::Event;
use std::time::{Duration, Instant};
use surface::{HEIGHT, VitaSurface, WIDTH};
use tokio::time::sleep;

const PAUSE_HOLD_DURATION: Duration = Duration::from_millis(1500);
/// Scales `pixels_per_point` up so the UI reads legibly on the Vita's small screen.
const UI_SCALE: f32 = 1.3;
// D-Pad/left-stick auto-repeat: immediate on press, then repeating once held past the delay.
const DIRECTION_REPEAT_INITIAL_DELAY: Duration = Duration::from_millis(350);
const DIRECTION_REPEAT_INTERVAL: Duration = Duration::from_millis(90);
const STREAM_INPUT_POLL_INTERVAL: Duration = Duration::from_millis(4);

pub(crate) const TARGET_FRAME_TIME: Duration = Duration::from_millis(16);

pub async fn run(mut app: App) -> Result<()> {
    crate::experimental::configure(&app.settings);
    crate::experimental::pin_render();
    crate::streaming::video::reserve_decoder_cdram();
    let mut performance_mode=None;

    let sdl = sdl2::init().map_err(anyhow::Error::msg)?;
    let video = sdl.video().map_err(anyhow::Error::msg)?;
    let audio = sdl.audio().map_err(anyhow::Error::msg)?;
    crate::input::register_vita_controller_mapping(&sdl).map_err(anyhow::Error::msg)?;
    let game_controller_subsystem = sdl.game_controller().map_err(anyhow::Error::msg)?;
    let mut controller = open_first_controller(&game_controller_subsystem);
    let joystick_subsystem = sdl.joystick().map_err(anyhow::Error::msg)?;
    let mut raw_joystick = joystick_subsystem.open(0).ok();
    let mut event_pump = sdl.event_pump().map_err(anyhow::Error::msg)?;
    let mut surface = VitaSurface::new(&video)?;
    let mut audio_renderer =
        AudioRenderer::new(&audio).context("failed to set up audio renderer")?;
    let egui_ctx = egui::Context::default();
    crate::app::ui::fonts::configure(&egui_ctx);
    let start_time = Instant::now();
    let mut pointer_pos = egui::Pos2::ZERO;
    let mut back_hold_since: Option<Instant> = None;
    let mut rear_touch_buttons = RearTouchButtons::default();
    let mut held_direction: Option<InputCommand> = None;
    let mut held_direction_since = Instant::now();
    let mut last_direction_repeat_at = Instant::now();
    let mut vita_ime_active = false;
    let mut vita_ime_pending = None;
    let mut last_network_check = Instant::now();
    let mut network_was_connected = false;
    let mut backgrounded = false;
    let mut audio_session_output: Option<std::sync::Arc<crate::streaming::video::DirectVideoOutput>> = None;
    // A stable, noninteractive playback overlay needs only four UI updates/s.
    // All video, input, audio and backend work still runs on every loop.
    let mut overlay_cache:Option<(Instant,(bool,u8,u8,crate::settings::Locale))>=None;
    let mut last_present=Instant::now();

    loop {
        let loop_started_at = Instant::now();
        let mut egui_events = Vec::new();
        let mut direct_commands = Vec::new();
        let mut interrupted = false;
        if app.title_search_requested && !vita_ime_active {
            app.title_search_requested = false;
            vita_ime_pending = None;
            video.text_input().start();
            vita_ime_active = true;
        }

        for event in event_pump.poll_iter() {
            match &event {
                Event::AppWillEnterBackground { .. } | Event::AppDidEnterBackground { .. } => { backgrounded = true; interrupted = true; },
                Event::AppWillEnterForeground { .. } | Event::AppDidEnterForeground { .. } => { backgrounded = false; interrupted = true; },
                _ => {}
            }
            rear_touch_buttons.handle_event(&event);
            let ime_owned_event = vita_ime_active;
            if ime_owned_event {
                match &event {
                    Event::TextInput { text, .. } => vita_ime_pending = Some(text.clone()),
                    Event::KeyDown {
                        keycode: Some(sdl2::keyboard::Keycode::Return),
                        ..
                    } => {
                        if let Some(text) = vita_ime_pending.take() {
                            app.set_title_search_query(text);
                        }
                        video.text_input().stop();
                        vita_ime_active = false;
                    }
                    _ => {}
                }
            }
            // SDL may emit both events for one press.
            if !ime_owned_event
                && let Some(command) = map_keyboard_event(&event)
                && !direct_commands.contains(&command)
            {
                direct_commands.push(command);
            }
            if !ime_owned_event
                && let Some(command) = map_controller_button_event(&event)
                && !direct_commands.contains(&command)
            {
                direct_commands.push(command);
            }
            let split_gameplay=matches!(&app.state,AppState::Streaming(s) if !s.paused && s.front_touch_stick_clicks(&app.settings));
            let split_touch_event=split_gameplay && matches!(&event,Event::FingerDown{touch_id:1,..}|Event::FingerMotion{touch_id:1,..}|Event::FingerUp{touch_id:1,..});
            if !split_touch_event && let Some(egui_event) = map_pointer_event(
                &event,
                (WIDTH as f32 / UI_SCALE, HEIGHT as f32 / UI_SCALE),
                UI_SCALE,
                &mut pointer_pos,
            ) {
                egui_events.push(egui_event);
            }
            if let AppState::Streaming(streaming) = &app.state
                && !streaming.paused
                && !streaming.front_touch_auxiliary_buttons(&app.settings)
                && !streaming.front_touch_stick_clicks(&app.settings)
                && let Some(pointer_event) = map_stream_pointer_event(
                    &event,
                    (WIDTH as f32, HEIGHT as f32),
                    surface.video_rect(),
                    // Scale input into the active stream resolution.
                    streaming
                        .video_size()
                        .unwrap_or((STREAM_WIDTH, STREAM_HEIGHT)),
                )
            {
                streaming.send_pointer_event(pointer_event);
            }
            if let Event::ControllerDeviceAdded { .. } = event {
                crate::native_input::disable();
                if controller.is_none(){
                    controller = open_first_controller(&game_controller_subsystem);
                    raw_joystick = joystick_subsystem.open(0).ok();
                }
            }
            if let Event::ControllerDeviceRemoved { which, .. } = event {
                if controller.as_ref().is_some_and(|c| c.instance_id() == which) {
                    controller = open_first_controller(&game_controller_subsystem);
                    raw_joystick = joystick_subsystem.open(0).ok();
                    rear_touch_buttons = RearTouchButtons::default();
                    crate::native_input::disable();
                }
            }
        }

        if last_network_check.elapsed() >= Duration::from_millis(500) {
            last_network_check = Instant::now();
            if let Some(connected) = network_connected() {
                if network_was_connected && !connected { interrupted = true; }
                network_was_connected = connected;
            }
        }
        if interrupted {
            performance_mode=None;
            overlay_cache=None;
            crate::native_input::disable();
            rear_touch_buttons = RearTouchButtons::default();
            back_hold_since = None;
            audio_renderer.clear_session();
            crate::diagnostics::step("session_lifecycle", "INTERRUPTED");
            app.interrupt_stream().await?;
            direct_commands.clear();
        }
        if backgrounded {
            performance_mode=None;
            overlay_cache=None;
            surface.reset_benchmark();
            crate::native_input::disable();
            sleep(Duration::from_millis(50)).await;
            continue;
        }
        if vita_ime_active
            && !video
                .text_input()
                .is_screen_keyboard_shown(surface.canvas.window())
        {
            video.text_input().stop();
            vita_ime_pending = None;
            vita_ime_active = false;
        }

        match (!vita_ime_active)
            .then(|| held_menu_direction(controller.as_ref()))
            .flatten()
        {
            Some(direction) if held_direction == Some(direction) => {
                if held_direction_since.elapsed() >= DIRECTION_REPEAT_INITIAL_DELAY
                    && last_direction_repeat_at.elapsed() >= DIRECTION_REPEAT_INTERVAL
                {
                    direct_commands.push(direction.into());
                    last_direction_repeat_at = Instant::now();
                }
            }
            Some(direction) => {
                direct_commands.push(direction.into());
                held_direction = Some(direction);
                held_direction_since = Instant::now();
                last_direction_repeat_at = Instant::now();
            }
            None => held_direction = None,
        }

        // Drives the top-left hold-progress ring in `build_ui`.
        let mut hold_progress: Option<f32> = None;
        // Back doubles as game input (View) and as the hold-to-pause gesture: withheld while
        // held, replayed as a single View tap only if released before the pause fired.
        let mut relay_back_as_view = false;
        if matches!(&app.state, AppState::Streaming(streaming) if !streaming.paused) {
            if back_button_held(controller.as_ref()) {
                let held_since = *back_hold_since.get_or_insert_with(Instant::now);
                let elapsed = held_since.elapsed();
                hold_progress = Some(
                    (elapsed.as_secs_f32() / PAUSE_HOLD_DURATION.as_secs_f32()).clamp(0.0, 1.0),
                );
                if elapsed >= PAUSE_HOLD_DURATION {
                    direct_commands.push(NavigationCommand::OpenPauseOverlay.into());
                    back_hold_since = None;
                }
            } else if let Some(held_since) = back_hold_since.take() {
                relay_back_as_view = held_since.elapsed() < PAUSE_HOLD_DURATION;
            }
        } else {
            back_hold_since = None;
        }

        for command in direct_commands {
            app.handle_command(command).await?;
        }

        send_stream_gamepad_state(
            &mut app,
            controller.as_ref(),
            raw_joystick.as_ref(),
            &rear_touch_buttons,
            relay_back_as_view,
        );

        app.tick().await?;
        let output = app.state.streaming().map(|s| s.direct_video_output());
        let perf=output.is_some() && app.settings.lab_performance_clocks;
        if perf && performance_mode.is_none(){performance_mode=Some(crate::experimental::PerformanceMode::engage());}
        else if !perf{performance_mode=None;}
        let same = match (&audio_session_output, &output) {
            (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a,b),
            (None,None) => true, _ => false,
        };
        if !same { if let Some(s)=app.state.streaming(){audio_renderer.bind_session(s.audio_receiver());}else{audio_renderer.clear_session();} overlay_cache=None; last_present=Instant::now(); surface.reset_benchmark(); }
        audio_session_output = output;
        let native_config = match &app.state {
            AppState::Streaming(s) if !s.paused => s.native_config(&app.settings),
            _ => None,
        };
        crate::native_input::configure(native_config);
        surface.prepare_video_output(app.state.streaming())?;

        let cacheable=matches!(&app.state,AppState::Streaming(s) if !s.paused && s.hint_started_at.elapsed()>=Duration::from_secs(3))
            && hold_progress.is_none() && !vita_ime_active;
        let key=(app.settings.show_stream_debug_info,app.settings.hud_page,app.settings.hud_opacity,app.settings.locale);
        let reuse=cacheable && egui_events.is_empty() && overlay_cache.as_ref().is_some_and(|(at,k)|*k==key && crate::streaming::policy::reuse_overlay(at.elapsed().as_millis() as u64));
        let textures_delta=if reuse {
            // Reuse only GPU geometry, never repeat a texture upload/free delta.
            egui::TexturesDelta::default()
        }else{
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(WIDTH as f32 / UI_SCALE, HEIGHT as f32 / UI_SCALE),
            )),
            viewport_id: egui::ViewportId::ROOT,
            viewports: std::iter::once((
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    native_pixels_per_point: Some(UI_SCALE),
                    ..Default::default()
                },
            ))
            .collect(),
            time: Some(start_time.elapsed().as_secs_f64()),
            predicted_dt: TARGET_FRAME_TIME.as_secs_f32(),
            events: egui_events,
            ..Default::default()
        };

        let mut ui_commands = Vec::new();
        let full_output = egui_ctx.run(raw_input, |ctx| {
            ui_commands = build_ui(ctx, &app, hold_progress);
        });

        for command in ui_commands {
            app.handle_command(command).await?;
        }
        if !matches!(&app.state, AppState::Streaming(s) if !s.paused) { crate::native_input::disable(); }

        // Complete UI uploads and vertex conversion before claiming a video texture.
        let clipped_primitives =
            egui_ctx.tessellate(full_output.shapes, full_output.pixels_per_point);
        surface.prepare_egui(full_output.pixels_per_point,&clipped_primitives,&full_output.textures_delta)?;
        // Cache only an unchanged, noninteractive stream. Pause/hold/menu/hint
        // always render live, and every input event still reaches egui.
        overlay_cache=if cacheable && matches!(&app.state,AppState::Streaming(s) if !s.paused) && full_output.textures_delta.free.is_empty(){
            Some((Instant::now(),key))
        }else{None};
        full_output.textures_delta
        };
        let mode=crate::settings::latency_mode();
        let bench_active=matches!(&app.state,AppState::Streaming(s) if !s.paused);
        surface.benchmark_tick(bench_active,mode,app.settings.stream_profile as u8);
        let source_fps=app.state.streaming().and_then(|s|s.hud.as_ref()).filter(|h|h.at.elapsed()<Duration::from_secs(3)).and_then(|h|h.source_fps);
        let live_pacing=mode!=0 && cacheable && matches!(&app.state,AppState::Streaming(s) if !s.paused);
        let gap=crate::streaming::policy::early_wake_gap_us(mode,source_fps).map(Duration::from_micros);
        // Near a presentation opportunity, give an in-flight decode a bounded
        // chance to finish. Only for cached noninteractive playback, never menus.
        if live_pacing && reuse && gap.is_some_and(|g|last_present.elapsed()>=g){
            if let Some(output)=audio_session_output.as_ref(){
                let wait=crate::streaming::policy::late_wait_us(mode,source_fps.is_some_and(|f|(50.0..=125.0).contains(&f)));
                if wait>0 && !output.has_pending() {
                    if output.wait_ready(Duration::from_micros(wait)).await{
                        crate::streaming::video::metrics::FRAME_WAKES.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
        // Claim only at the final draw, never while the old texture is being used.
        let claimed=surface.sync_video_frame(app.state.streaming())?;
        if !live_pacing || !reuse || claimed{
            surface.draw_scene(matches!(&app.state, AppState::Streaming(_)))?;
            surface.paint_egui(&textures_delta)?;
            last_present=Instant::now();
        }else{
            // Cached overlay + no new picture: no redundant swap. Backend,
            // audio, events and input servicing are still on their usual loop.
            crate::streaming::video::metrics::IDLE_SWAPS_SAVED.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
        }
        // Refresh controls after potentially expensive video/UI presentation,
        // rather than making an input change wait for the next render iteration.
        // pump_events updates SDL state; events are consumed by the regular loop.
        // Keep short Back/View release taps visible to the RTC worker: they
        // are a one-shot pulse, so don't overwrite them in the same UI frame.
        if !relay_back_as_view
            && matches!(&app.state, AppState::Streaming(streaming) if !streaming.paused)
        {
            event_pump.pump_events();
            if mode!=2 || !crate::native_input::fresh_for_redundant_read(){
                send_stream_gamepad_state(
                    &mut app,
                    controller.as_ref(),
                    raw_joystick.as_ref(),
                    &rear_touch_buttons,
                    false,
                );
            }
        }
        // Only reuse a prepared overlay whose texture delta has already been
        // consumed. At most one direct presentation before servicing the app.
        let direct_wake=mode==2 && live_pacing && reuse;
        let frame_deadline = loop_started_at + TARGET_FRAME_TIME;
        if Instant::now() < frame_deadline {
            while Instant::now() < frame_deadline {
                let remaining = frame_deadline.saturating_duration_since(Instant::now());
                // Menus do not need four-millisecond input refreshes.
                let live = matches!(&app.state, AppState::Streaming(s) if !s.paused);
                let slice=if live { remaining.min(STREAM_INPUT_POLL_INTERVAL) } else { remaining };
                let mut early=false;
                if live && live_pacing {
                    if let (Some(gap),Some(output))=(gap,audio_session_output.as_ref()){
                        let until_gap=if direct_wake{Duration::ZERO}else{gap.saturating_sub(last_present.elapsed())};
                        if until_gap.is_zero(){
                            early=output.wait_ready(slice).await;
                        }else{
                            sleep(slice.min(until_gap)).await;
                        }
                    }else{sleep(slice).await;}
                }else{sleep(slice).await;}
                if early{
                    crate::streaming::video::metrics::FRAME_WAKES.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
                    if direct_wake && surface.try_sync_ready_video_frame()?{
                        // Decoder owns the other texture. No application tick,
                        // SDL event processing or UI rebuild before this draw.
                        surface.draw_scene(true)?;
                        surface.paint_egui(&egui::TexturesDelta::default())?;
                        last_present=Instant::now();
                    }
                    // Always service the full application after a single wake.
                    break;
                }
                if Instant::now() >= frame_deadline {
                    break;
                }

                // SDL controller state is refreshed independently of rendering. Events remain
                // queued for the normal UI pass at the start of the next frame.
                event_pump.pump_events();
                send_stream_gamepad_state(
                    &mut app,
                    controller.as_ref(),
                    raw_joystick.as_ref(),
                    &rear_touch_buttons,
                    false,
                );
            }
        } else {
            tokio::task::yield_now().await;
        }
    }
}

fn send_stream_gamepad_state(
    app: &mut App,
    controller: Option<&sdl2::controller::GameController>,
    raw_joystick: Option<&sdl2::joystick::Joystick>,
    rear_touch_buttons: &RearTouchButtons,
    relay_back_as_view: bool,
) {
    let settings = &app.settings;
    let (rear_touch_enabled, front_touch_auxiliary_buttons, front_touch_stick_clicks) = match &app.state {
        AppState::Streaming(streaming) if !streaming.paused => (
            streaming.rear_touch_enabled(settings),
            streaming.front_touch_auxiliary_buttons(settings),
            streaming.front_touch_stick_clicks(settings),
        ),
        _ => return,
    };
    let AppState::Streaming(streaming) = &mut app.state else {
        return;
    };
    let Some(mut frame) = read_gamepad_frame(
        controller,
        raw_joystick,
        rear_touch_buttons,
        rear_touch_enabled,
        front_touch_auxiliary_buttons,
        front_touch_stick_clicks,
    ) else {
        return;
    };

    // Back is relayed separately after the hold gesture resolves.
    if relay_back_as_view { crate::native_input::suppress_for(Duration::from_millis(20)); }
    frame.view = f32::from(relay_back_as_view);
    streaming.send_gamepad_frame(frame, settings);
}

#[cfg(target_os="vita")]
fn network_connected() -> Option<bool> {
    #[link(name="SceNetCtl_stub", kind="static")]
    unsafe extern "C" { fn sceNetCtlInetGetState(state:*mut i32)->i32; }
    let mut state = 0;
    (unsafe { sceNetCtlInetGetState(&mut state) } >= 0).then_some(state == 3)
}
#[cfg(not(target_os="vita"))]
fn network_connected() -> Option<bool> { None }
