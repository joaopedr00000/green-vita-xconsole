//! Bounded playback policies, independent of Vita hardware.
pub fn pcm_cap_ms(target: u32) -> u32 { target.clamp(50,110) + 50 }
pub fn retry_delay_ms(failures: u32) -> u64 { (100u64 << failures.saturating_sub(1).min(3)).min(800) }
pub fn should_catch_up(low_latency: bool, queued: bool, age_ms: u128, generation: u64) -> bool {
    low_latency && queued && age_ms >= 50 && generation == 0
}
pub fn jitter_floor(base: u64, ceiling: u64, jitter_us: u64) -> u64 {
    base.saturating_add(jitter_us / 1000).min(ceiling)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn caps_leave_audio_headroom() { assert_eq!(pcm_cap_ms(50),100); assert_eq!(pcm_cap_ms(110),160); assert_eq!(pcm_cap_ms(u32::MAX),160); }
    #[test] fn retry_is_bounded() { assert_eq!(retry_delay_ms(1),100); assert_eq!(retry_delay_ms(u32::MAX),800); }
    #[test] fn normal_quality_never_shortens_display_wait() { assert!(!should_catch_up(false,true,200,0)); assert!(!should_catch_up(true,true,200,1)); assert!(should_catch_up(true,true,50,0)); }
    #[test] fn jitter_headroom_bounded() { assert_eq!(jitter_floor(32,48,200_000),48); assert_eq!(jitter_floor(32,48,0),32); }
}
