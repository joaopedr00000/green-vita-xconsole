//! Bounded playback policies, independent of Vita hardware.
pub fn pcm_cap_ms(target: u32) -> u32 { target.clamp(50,110) + 50 }
pub fn retry_delay_ms(failures: u32) -> u64 { (100u64 << failures.saturating_sub(1).min(3)).min(800) }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn caps_leave_audio_headroom() { assert_eq!(pcm_cap_ms(50),100); assert_eq!(pcm_cap_ms(110),160); assert_eq!(pcm_cap_ms(u32::MAX),160); }
    #[test] fn retry_is_bounded() { assert_eq!(retry_delay_ms(1),100); assert_eq!(retry_delay_ms(u32::MAX),800); }
}
