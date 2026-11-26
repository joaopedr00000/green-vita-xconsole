//! Service-neutral input state produced by the Vita UI.
//!
//! Each streaming backend is responsible for translating these values to its
//! own wire protocol.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GamepadFrame {
    pub gamepad_index: u8,
    pub nexus: f32,
    pub menu: f32,
    pub view: f32,
    pub a: f32,
    pub b: f32,
    pub x: f32,
    pub y: f32,
    pub dpad_up: f32,
    pub dpad_down: f32,
    pub dpad_left: f32,
    pub dpad_right: f32,
    pub left_shoulder: f32,
    pub right_shoulder: f32,
    pub left_thumb: f32,
    pub right_thumb: f32,
    pub left_thumb_x_axis: f32,
    pub left_thumb_y_axis: f32,
    pub right_thumb_x_axis: f32,
    pub right_thumb_y_axis: f32,
    pub left_trigger: f32,
    pub right_trigger: f32,
}

#[derive(Debug, Clone, Default)]
pub struct PointerEvent {
    pub contact_major: u16,
    pub contact_minor: u16,
    pub pressure: u8,
    pub twist: u16,
    pub x: u32,
    pub y: u32,
    pub event_type: u8,
}

pub const CONTROL_LABELS: [&str; 17] = ["A", "B", "X", "Y", "D-pad ↑", "D-pad ↓", "D-pad ←", "D-pad →", "LB", "RB", "LT", "RT", "LS click", "RS click", "Menu", "View", "Disabled"];
pub fn default_control_map() -> [u8; 16] { [0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15] }

impl GamepadFrame {
    pub fn remap(&mut self, map: &[u8; 16]) {
        let values = [self.a, self.b, self.x, self.y, self.dpad_up, self.dpad_down, self.dpad_left, self.dpad_right, self.left_shoulder, self.right_shoulder, self.left_trigger, self.right_trigger, self.left_thumb, self.right_thumb, self.menu, self.view];
        let mut out = [0.0f32; 16];
        for (index, value) in values.into_iter().enumerate() {
            if let Some(target) = out.get_mut(map[index] as usize) { *target = target.max(value); }
        }
        self.a = out[0];
        self.b = out[1];
        self.x = out[2];
        self.y = out[3];
        self.dpad_up = out[4];
        self.dpad_down = out[5];
        self.dpad_left = out[6];
        self.dpad_right = out[7];
        self.left_shoulder = out[8];
        self.right_shoulder = out[9];
        self.left_trigger = out[10];
        self.right_trigger = out[11];
        self.left_thumb = out[12];
        self.right_thumb = out[13];
        self.menu = out[14];
        self.view = out[15];
    }
}

#[cfg(test)]
mod control_tests {
    use super::*;
    #[test]
    fn shared_target_and_disabled() {
        let mut mapping = default_control_map();
        mapping[0] = 1; mapping[2] = 16;
        let mut frame = GamepadFrame { a: 1.0, b: 0.5, x: 1.0, ..Default::default() };
        frame.remap(&mapping);
        assert_eq!((frame.a, frame.b, frame.x), (0.0, 1.0, 0.0));
    }
}

pub(crate) fn native_poll_ms(changed_recently: bool) -> u64 { if changed_recently { 2 } else { 4 } }
pub(crate) fn native_axis(raw: u8, deadzone_percent: u8) -> f32 {
    let value = ((raw as f32 - 128.0) / if raw < 128 {128.0} else {127.0}).clamp(-1.0,1.0);
    let dz = f32::from(deadzone_percent.min(20)) / 100.0;
    if value.abs() <= dz { 0.0 } else { value.signum() * (value.abs()-dz) / (1.0-dz) }
}
impl GamepadFrame {
    pub(crate) fn digital_mask(&self) -> u32 {
        [self.a,self.b,self.x,self.y,self.dpad_up,self.dpad_down,self.dpad_left,self.dpad_right,self.left_shoulder,self.right_shoulder,self.left_trigger,self.right_trigger,self.left_thumb,self.right_thumb,self.menu,self.view].iter().enumerate().fold(0,|mask,(i,v)| mask | (u32::from(*v>0.5)<<i))
    }
}
#[cfg(test)]
mod native_tests {
    use super::*;
    #[test] fn native_axis_keeps_full_range() {
        for dz in [0,4,8,12,20,255] { assert_eq!(native_axis(128,dz),0.0); assert_eq!(native_axis(0,dz),-1.0); assert_eq!(native_axis(255,dz),1.0); }
        assert_eq!(native_axis(130,4),0.0);
        assert!(native_axis(150,8)>0.0);
        assert!(native_axis(150,8)<native_axis(150,0));
    }
    #[test] fn mask_ignores_stick_noise_and_preserves_remapped_button() {
        let mut f=GamepadFrame {a:1.0,..Default::default()};
        let m=f.digital_mask(); f.left_thumb_x_axis=0.3; assert_eq!(m,f.digital_mask());
        let mut map=default_control_map();map[0]=11;f.remap(&map);assert_eq!(f.digital_mask(),1<<11);
    }
    #[test] fn poll_bounds() { assert_eq!(native_poll_ms(true),2); assert_eq!(native_poll_ms(false),4); }
}

pub(crate) fn touch_zone_enabled(front_panel:bool,lower_half:bool,rear_enabled:bool,front_mirror:bool,front_sticks:bool)->bool{
    if front_sticks{
        if front_panel{lower_half}else{rear_enabled && !lower_half}
    }else if front_panel{front_mirror}else{rear_enabled}
}
#[cfg(test)]
mod split_touch_tests{
    use super::*;
    #[test]fn legacy_layouts_are_unchanged(){
        for rear in [false,true]{for mirror in [false,true]{for lower in [false,true]{
            assert_eq!(touch_zone_enabled(false,lower,rear,mirror,false),rear);
            assert_eq!(touch_zone_enabled(true,lower,rear,mirror,false),mirror);
        }}}
    }
    #[test]fn split_only_sticks_front_and_triggers_rear(){
        for rear in [false,true]{for mirror in [false,true]{
            assert!(!touch_zone_enabled(true,false,rear,mirror,true));
            assert!(touch_zone_enabled(true,true,rear,mirror,true));
            assert_eq!(touch_zone_enabled(false,false,rear,mirror,true),rear);
            assert!(!touch_zone_enabled(false,true,rear,mirror,true));
        }}
    }
    #[test]fn both_stick_clicks_survive_shoulder_swap_and_remap(){
        let mut frame=GamepadFrame{left_thumb:1.0,right_thumb:1.0,left_trigger:1.0,..Default::default()};
        std::mem::swap(&mut frame.left_shoulder,&mut frame.left_trigger);
        std::mem::swap(&mut frame.right_shoulder,&mut frame.right_trigger);
        frame.remap(&default_control_map());
        assert_eq!((frame.left_thumb,frame.right_thumb,frame.left_shoulder,frame.left_trigger),(1.0,1.0,1.0,0.0));
    }
}
