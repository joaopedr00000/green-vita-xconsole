//! Bounded playback policies, independent of Vita hardware.
pub fn pcm_cap_ms(target: u32) -> u32 { target.clamp(50,110) + 50 }
pub fn retry_delay_ms(failures: u32) -> u64 { (100u64 << failures.saturating_sub(1).min(3)).min(800) }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn caps_leave_audio_headroom() { assert_eq!(pcm_cap_ms(50),100); assert_eq!(pcm_cap_ms(110),160); assert_eq!(pcm_cap_ms(u32::MAX),160); }
    #[test] fn retry_is_bounded() { assert_eq!(retry_delay_ms(1),100); assert_eq!(retry_delay_ms(u32::MAX),800); }
}

/// Only shorten output wait when a real backlog exists. Compressed access units
/// continue to decode in order; this is a presentation-only catch-up policy.
pub fn catch_up_wait_ms(enabled:bool,age_us:u64,remaining:usize)->u64{
    if enabled && age_us>=45_000 && remaining>=2{2}else{67}
}
#[cfg(test)]
mod video_tests{
    use super::*;
    #[test]fn catch_up_is_opt_in_and_backlog_gated(){
        assert_eq!(catch_up_wait_ms(false,100_000,5),67);
        assert_eq!(catch_up_wait_ms(true,44_999,5),67);
        assert_eq!(catch_up_wait_ms(true,100_000,1),67);
        assert_eq!(catch_up_wait_ms(true,45_000,2),2);
    }
}
