//! Independent handheld-only sampler; SDL remains the compatibility path.
use crate::streaming::input::{GamepadFrame,native_poll_ms};
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock, Condvar};
use std::sync::atomic::{AtomicU64,Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Config { pub rear: bool, pub front: bool, pub swap: bool, pub map: [u8;16], pub deadzone_percent:u8 }
#[derive(Clone)]
pub struct NativeSample { pub frame:GamepadFrame, pub sampled_at:Instant, pub changed_at:Instant, pub edge:Option<u64> }
struct State { config:Option<Config>, sample:Option<NativeSample>, edges:VecDeque<NativeSample>, generation:u64, next_id:u64 }
static STATE:Mutex<State>=Mutex::new(State {config:None,sample:None,edges:VecDeque::new(),generation:0,next_id:0});
static WAKE:Condvar=Condvar::new();
pub(crate) static INPUT_CHANGED:tokio::sync::Notify=tokio::sync::Notify::const_new();
static CHANGE_COUNT:AtomicU64=AtomicU64::new(0);
static CHANGE_SUM_US:AtomicU64=AtomicU64::new(0);
static CHANGE_MAX_US:AtomicU64=AtomicU64::new(0);
static CHANGE_LAST_US:AtomicU64=AtomicU64::new(0);
pub(crate) fn dispatch_change(us:u64){CHANGE_COUNT.fetch_add(1,Ordering::Relaxed);CHANGE_SUM_US.fetch_add(us,Ordering::Relaxed);CHANGE_MAX_US.fetch_max(us,Ordering::Relaxed);CHANGE_LAST_US.store(us,Ordering::Relaxed);}
pub(crate) fn change_metrics()->(u64,u64,u64,u64){let n=CHANGE_COUNT.load(Ordering::Relaxed);(n,CHANGE_LAST_US.load(Ordering::Relaxed),CHANGE_SUM_US.load(Ordering::Relaxed).checked_div(n).unwrap_or(0),CHANGE_MAX_US.load(Ordering::Relaxed))}
static STARTED:OnceLock<bool>=OnceLock::new();
static SUPPRESS_UNTIL:Mutex<Option<Instant>>=Mutex::new(None);
const EDGE_LIMIT:usize=8;
const MAX_AGE:Duration=Duration::from_millis(100);
static POLLS:AtomicU64=AtomicU64::new(0);
static REPEATS:AtomicU64=AtomicU64::new(0);
static INTERVAL_SUM:AtomicU64=AtomicU64::new(0);
static INTERVAL_COUNT:AtomicU64=AtomicU64::new(0);
static INTERVAL_MAX:AtomicU64=AtomicU64::new(0);
static EDGE_DROPS:AtomicU64=AtomicU64::new(0);
static NATIVE_SENT:AtomicU64=AtomicU64::new(0);
static SDL_SENT:AtomicU64=AtomicU64::new(0);
static NATIVE_DELAY:AtomicU64=AtomicU64::new(0);
static SDL_DELAY:AtomicU64=AtomicU64::new(0);
#[derive(Default)]
struct HardwareCache { ports_at:Option<Instant>,handheld:bool,panel:[Option<(i32,i32)>;2] }

pub fn suppress_for(duration:Duration) {
    if let Ok(mut until)=SUPPRESS_UNTIL.lock() { *until=Some(Instant::now()+duration); }
    if let Ok(mut s)=STATE.lock() { s.edges.clear(); s.sample=None; s.generation=s.generation.wrapping_add(1); }
    WAKE.notify_one();
}
fn suppressed()->bool { SUPPRESS_UNTIL.try_lock().map_or(true,|until|until.is_some_and(|at|at>Instant::now())) }
pub fn configure(config:Option<Config>) {
    if let Ok(mut s)=STATE.lock() {
        if s.config!=config { s.config=config;s.sample=None;s.edges.clear();s.generation=s.generation.wrapping_add(1);WAKE.notify_one(); }
    }
    if config.is_some() { let _=STARTED.get_or_init(||std::thread::Builder::new().name("green-vita-input".into()).spawn(run).is_ok()); }
}
pub fn disable(){configure(None);}
pub fn latest()->Option<NativeSample>{
    if suppressed(){return None;}
    let mut s=STATE.try_lock().ok()?;
    while s.edges.front().is_some_and(|e|e.sampled_at.elapsed()>=MAX_AGE) { s.edges.pop_front();EDGE_DROPS.fetch_add(1,Ordering::Relaxed); }
    let latest=s.sample.as_ref().filter(|x|x.sampled_at.elapsed()<MAX_AGE)?;
    Some(s.edges.front().unwrap_or(latest).clone())
}
pub fn acknowledge(edge:u64){
    if let Ok(mut s)=STATE.try_lock(){ if s.edges.front().is_some_and(|e|e.edge==Some(edge)){s.edges.pop_front();} }
}
pub fn dispatch(native:bool,age_us:u64){
    let (count,sum)=if native{(&NATIVE_SENT,&NATIVE_DELAY)}else{(&SDL_SENT,&SDL_DELAY)};
    count.fetch_add(1,Ordering::Relaxed);sum.fetch_add(age_us,Ordering::Relaxed);
}
#[derive(Default)]
pub(crate) struct InputMetrics {pub polls:u64,pub repeat_percent:u64,pub interval_us:u64,pub max_interval_us:u64,pub edge_drops:u64,pub native_count:u64,pub sdl_count:u64,pub native_avg_us:u64,pub sdl_avg_us:u64,pub native_active:bool}
pub(crate) fn metrics()->InputMetrics{
    let polls=POLLS.load(Ordering::Relaxed);let count=INTERVAL_COUNT.load(Ordering::Relaxed);
    let n=NATIVE_SENT.load(Ordering::Relaxed);let s=SDL_SENT.load(Ordering::Relaxed);
    InputMetrics{polls,repeat_percent:REPEATS.load(Ordering::Relaxed).saturating_mul(100).checked_div(polls).unwrap_or(0),interval_us:INTERVAL_SUM.load(Ordering::Relaxed).checked_div(count).unwrap_or(0),max_interval_us:INTERVAL_MAX.load(Ordering::Relaxed),edge_drops:EDGE_DROPS.load(Ordering::Relaxed),native_count:n,sdl_count:s,native_avg_us:NATIVE_DELAY.load(Ordering::Relaxed).checked_div(n).unwrap_or(0),sdl_avg_us:SDL_DELAY.load(Ordering::Relaxed).checked_div(s).unwrap_or(0),native_active:STATE.try_lock().ok().is_some_and(|x|x.sample.as_ref().is_some_and(|s|s.sampled_at.elapsed()<MAX_AGE))}
}
pub fn telemetry()->String{
    let polls=POLLS.load(Ordering::Relaxed);let repeats=REPEATS.load(Ordering::Relaxed);
    let count=INTERVAL_COUNT.load(Ordering::Relaxed);
    let n=NATIVE_SENT.load(Ordering::Relaxed);let s=SDL_SENT.load(Ordering::Relaxed);
    format!("native polls:{} repeat:{}% interval:{}/{}us edge_drop:{} dispatch N:{}us/{} S:{}us/{}",polls,repeats.saturating_mul(100).checked_div(polls).unwrap_or(0),INTERVAL_SUM.load(Ordering::Relaxed).checked_div(count).unwrap_or(0),INTERVAL_MAX.load(Ordering::Relaxed),EDGE_DROPS.load(Ordering::Relaxed),NATIVE_DELAY.load(Ordering::Relaxed).checked_div(n).unwrap_or(0),n,SDL_DELAY.load(Ordering::Relaxed).checked_div(s).unwrap_or(0),s)
}
fn run(){
    let mut cache=HardwareCache::default();let mut generation_seen=u64::MAX;
    let mut previous_poll:Option<Instant>=None;let mut previous_timestamp=None;
    let mut fast_until=Instant::now();
    loop {
        let Ok(mut s)=STATE.lock() else{return;};
        while s.config.is_none(){let Ok(waited)=WAKE.wait(s) else{return;};s=waited;}
        let config=s.config.expect("active config");let generation=s.generation;drop(s);
        if generation_seen!=generation {cache=HardwareCache::default();previous_poll=None;previous_timestamp=None;generation_seen=generation;fast_until=Instant::now();}
        let started=Instant::now();
        if let Some(old)=previous_poll {let us=started.saturating_duration_since(old).as_micros() as u64;INTERVAL_SUM.fetch_add(us,Ordering::Relaxed);INTERVAL_COUNT.fetch_add(1,Ordering::Relaxed);INTERVAL_MAX.fetch_max(us,Ordering::Relaxed);}
        previous_poll=Some(started);
        let reading=sample_hardware(config,&mut cache);
        if let Ok(mut s)=STATE.lock(){
            if s.generation==generation{
                match reading {
                    Some((frame,stamp))=>{
                        let sampled_at=Instant::now();
                        POLLS.fetch_add(1,Ordering::Relaxed);
                        if previous_timestamp==Some(stamp){REPEATS.fetch_add(1,Ordering::Relaxed);}
                        previous_timestamp=Some(stamp);
                        let changed=s.sample.as_ref().is_some_and(|old|old.frame!=frame);
                        let edge=s.sample.as_ref().is_some_and(|old|old.frame.digital_mask()!=frame.digital_mask());
                        if changed{fast_until=sampled_at+Duration::from_millis(100);}
                        let changed_at=if changed{sampled_at}else{s.sample.as_ref().map(|s|s.changed_at).unwrap_or(sampled_at)};
                        let sample=NativeSample{frame,sampled_at,changed_at,edge:None};
                        if edge && !suppressed(){
                            if s.edges.len()>=EDGE_LIMIT{EDGE_DROPS.fetch_add(s.edges.len() as u64,Ordering::Relaxed);s.edges.clear();}
                            s.next_id=s.next_id.wrapping_add(1);
                            let mut e=sample.clone();e.edge=Some(s.next_id);s.edges.push_back(e);
                        }
                        let first=s.sample.is_none();s.sample=Some(sample);
                        if (changed||first) && crate::settings::latency_mode()==2{INPUT_CHANGED.notify_one();}
                    },
                    None=>{s.sample=None;s.edges.clear();previous_timestamp=None;}
                }
            }
        }
        let period=Duration::from_millis(native_poll_ms(Instant::now()<fast_until));
        if let Ok(s)=STATE.lock(){if s.generation==generation{let _guard=WAKE.wait_timeout(s,period.saturating_sub(started.elapsed()));}}
    }
}
#[cfg(not(target_os="vita"))]
fn sample_hardware(_:Config,_:&mut HardwareCache)->Option<(GamepadFrame,u64)>{None}

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
    pub struct Panel { pub min_x:i16,pub min_y:i16,pub max_x:i16,pub max_y:i16,pub rest:[u8;40] }
    impl Default for Panel { fn default() -> Self { Self { min_x:0,min_y:0,max_x:0,max_y:0,rest:[0;40] } } }
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
fn sample_hardware(config: Config, cache: &mut HardwareCache) -> Option<(GamepadFrame,u64)> {
    use vita::*;
    // Recheck external controllers at least every 100ms; a topology event also
    // disables the sampler and clears its cache via the configuration generation.
    if cache.ports_at.is_none_or(|at| at.elapsed() >= Duration::from_millis(100)) {
        let mut ports = [0u8;16];
        cache.handheld = unsafe { sceCtrlGetControllerPortInfo(&mut ports) } >= 0 && ports[0] == 1 && ports[1..5].iter().all(|p| *p == 0);
        cache.ports_at = Some(Instant::now());
    }
    if !cache.handheld { return None; }
    let mut pad = Pad::default();
    if unsafe { sceCtrlPeekBufferPositive(0,&mut pad,1) } <= 0 || pad.buttons & 0x10000 != 0 { return None; }
    let b = |mask:u32| f32::from(pad.buttons & mask != 0);
    let axis = |raw:u8| crate::streaming::input::native_axis(raw,config.deadzone_percent);
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
        if cache.panel[port as usize].is_none() {
            let mut panel = Panel::default();
            if unsafe { sceTouchGetPanelInfo(port,&mut panel) } < 0 { return None; }
            cache.panel[port as usize] = Some(((i32::from(panel.min_x)+i32::from(panel.max_x))/2,(i32::from(panel.min_y)+i32::from(panel.max_y))/2));
        }
        let (mid_x,mid_y) = cache.panel[port as usize]?;
        let mut touch = TouchData::default();
        if unsafe { sceTouchPeek(port,&mut touch,1) } <= 0 { return None; }
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
    Some((f,pad.timestamp))
}
