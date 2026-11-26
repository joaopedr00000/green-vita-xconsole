//! Independent handheld-only physical/touch sampler. SDL stays on its owning thread.
use crate::streaming::input::GamepadFrame;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Config { pub rear: bool, pub front: bool, pub swap: bool, pub map: [u8; 16] }
struct State { config: Option<Config>, sample: Option<(GamepadFrame, Instant)>, generation: u64 }
static STATE: Mutex<State> = Mutex::new(State { config: None, sample: None, generation: 0 });
static STARTED: OnceLock<bool> = OnceLock::new();

pub fn configure(config: Option<Config>) {
    if let Ok(mut s) = STATE.lock() {
        if s.config != config {
            s.config = config; s.sample = None; s.generation = s.generation.wrapping_add(1);
        }
    }
    if config.is_some() {
        let _ = STARTED.get_or_init(|| std::thread::Builder::new()
            .name("green-vita-input".into()).spawn(run).is_ok());
    }
}
pub fn disable() { configure(None); }
pub fn latest() -> Option<(GamepadFrame, Instant)> {
    let s = STATE.try_lock().ok()?;
    s.sample.as_ref().filter(|(_, at)| at.elapsed() < Duration::from_millis(100)).cloned()
}
fn run() {
    loop {
        let started = Instant::now();
        let (config, generation) = STATE.lock().map(|s| (s.config,s.generation)).unwrap_or((None,0));
        if let Some(config) = config {
            let sample = sample_hardware(config).map(|frame| (frame, Instant::now()));
            if let Ok(mut s) = STATE.lock() {
                if s.generation == generation { s.sample = sample; }
            }
        }
        let period = if config.is_some() { Duration::from_millis(4) } else { Duration::from_millis(100) };
        std::thread::sleep(period.saturating_sub(started.elapsed()));
    }
}
#[cfg(not(target_os="vita"))]
fn sample_hardware(_: Config) -> Option<GamepadFrame> { None }

#[cfg(target_os="vita")]
mod vita {
    #[repr(C)]
    #[derive(Default)]
    pub struct Pad { pub timestamp: u64, pub buttons: u32, pub lx:u8, pub ly:u8, pub rx:u8, pub ry:u8, pub extra:[u8;16] }
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct Touch { pub id:u8, pub force:u8, pub x:i16, pub y:i16, pub reserved:[u8;8], pub info:u16 }
    #[repr(C)]
    #[derive(Default)]
    pub struct TouchData { pub timestamp:u64, pub status:u32, pub count:u32, pub reports:[Touch;8] }
    #[repr(C)]
    #[derive(Default)]
    pub struct Panel { pub min_x:i16,pub min_y:i16,pub max_x:i16,pub max_y:i16,pub rest:[u8;40] }
    #[link(name="SceCtrl_stub", kind="static")]
    unsafe extern "C" {
        pub fn sceCtrlPeekBufferPositive(port:i32,pad:*mut Pad,count:i32)->i32;
        pub fn sceCtrlGetControllerPortInfo(info:*mut [u8;16])->i32;
    }
    #[link(name="SceTouch_stub", kind="static")]
    unsafe extern "C" {
        pub fn sceTouchPeek(port:u32,data:*mut TouchData,count:u32)->i32;
        pub fn sceTouchGetPanelInfo(port:u32,info:*mut Panel)->i32;
    }
    const _: () = assert!(std::mem::size_of::<Pad>() == 32);
    const _: () = assert!(std::mem::size_of::<TouchData>() == 144);
    const _: () = assert!(std::mem::size_of::<Panel>() == 48);
}
#[cfg(target_os="vita")]
fn sample_hardware(config: Config) -> Option<GamepadFrame> {
    use vita::*;
    let mut ports = [0u8;16];
    // Keep the SDL path for PS TV and paired external controllers.
    if unsafe { sceCtrlGetControllerPortInfo(&mut ports) } < 0 || ports[0] != 1 || ports[1..5].iter().any(|p| *p != 0) { return None; }
    let mut pad = Pad::default();
    if unsafe { sceCtrlPeekBufferPositive(0,&mut pad,1) } <= 0 || pad.buttons & 0x10000 != 0 { return None; }
    let b = |mask:u32| f32::from(pad.buttons & mask != 0);
    let axis = |raw:u8| ((raw as f32 - 128.0) / if raw < 128 {128.0} else {127.0}).clamp(-1.0,1.0);
    let mut f = GamepadFrame {
        a:b(0x4000), b:b(0x2000), x:b(0x8000), y:b(0x1000),
        dpad_up:b(0x10), dpad_down:b(0x40), dpad_left:b(0x80), dpad_right:b(0x20),
        left_shoulder:b(0x100), right_shoulder:b(0x200), menu:b(8),
        left_thumb_x_axis:axis(pad.lx), left_thumb_y_axis:axis(pad.ly),
        right_thumb_x_axis:axis(pad.rx), right_thumb_y_axis:axis(pad.ry), ..Default::default()
    };
    // View/Back hold and pointer gestures remain owned by the UI.
    for (port, enabled) in [(0,config.front),(1,config.rear)] {
        if !enabled { continue; }
        let mut panel = Panel::default();
        let mut touch = TouchData::default();
        if unsafe { sceTouchGetPanelInfo(port,&mut panel) } < 0 || unsafe { sceTouchPeek(port,&mut touch,1) } <= 0 { continue; }
        let mid_x = (i32::from(panel.min_x)+i32::from(panel.max_x))/2;
        let mid_y = (i32::from(panel.min_y)+i32::from(panel.max_y))/2;
        for t in touch.reports.iter().take((touch.count as usize).min(8)) {
            match (i32::from(t.x)<mid_x,i32::from(t.y)<mid_y) {
                (true,true)=>f.left_trigger=1.0, (false,true)=>f.right_trigger=1.0,
                (true,false)=>f.left_thumb=1.0, (false,false)=>f.right_thumb=1.0
            }
        }
    }
    if config.swap {
        std::mem::swap(&mut f.left_shoulder,&mut f.left_trigger);
        std::mem::swap(&mut f.right_shoulder,&mut f.right_trigger);
    }
    f.remap(&config.map);
    Some(f)
}
