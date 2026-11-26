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
