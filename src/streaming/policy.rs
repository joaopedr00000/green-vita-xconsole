//! Bounded playback policies, independent of Vita hardware.
pub fn pcm_cap_ms(target: u32) -> u32 { target.clamp(50,110) + 50 }
pub fn retry_delay_ms(failures: u32) -> u64 { (100u64 << failures.saturating_sub(1).min(3)).min(800) }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn caps_leave_audio_headroom() { assert_eq!(pcm_cap_ms(50),100); assert_eq!(pcm_cap_ms(110),160); assert_eq!(pcm_cap_ms(u32::MAX),160); }
    #[test] fn retry_is_bounded() { assert_eq!(retry_delay_ms(1),100); assert_eq!(retry_delay_ms(u32::MAX),800); }
}

/// Global presentation-only recovery. Preserve the hardware-validated fast
/// path and introduce a gentler intermediate tier for a moderate backlog.
/// Never skip compressed access units here; when there is no backlog, retain
/// the original presentation wait to preserve smooth pacing.
pub fn catch_up_wait_ms(age_us:u64,remaining:usize)->u64{
    if remaining<2 {67}
    else if age_us>=45_000 {2}
    else if age_us>=30_000 {8}
    else {67}
}
#[cfg(test)]
mod video_tests{
    use super::*;
    #[test]fn catch_up_global_and_backlog_gated(){
        assert_eq!(catch_up_wait_ms(100_000,1),67);
        assert_eq!(catch_up_wait_ms(29_999,5),67);
        assert_eq!(catch_up_wait_ms(30_000,2),8);
        assert_eq!(catch_up_wait_ms(44_999,5),8);
        assert_eq!(catch_up_wait_ms(45_000,2),2);
        assert_eq!(catch_up_wait_ms(100_000,5),2);
    }
}
