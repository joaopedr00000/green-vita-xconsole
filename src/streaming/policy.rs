//! Bounded playback policies, independent of Vita hardware.
pub fn pcm_cap_ms(target: u32) -> u32 { target.clamp(50,110) + 50 }
pub fn retry_delay_ms(failures: u32) -> u64 { (100u64 << failures.saturating_sub(1).min(3)).min(800) }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn caps_leave_audio_headroom() { assert_eq!(pcm_cap_ms(50),100); assert_eq!(pcm_cap_ms(110),160); assert_eq!(pcm_cap_ms(u32::MAX),160); }
    #[test] fn retry_is_bounded() { assert_eq!(retry_delay_ms(1),100); assert_eq!(retry_delay_ms(u32::MAX),800); }
}

/// Presentation-only recovery; never skips compressed references. The
/// previous tiers remain for all rates. At >=50fps allow a moderate recovery
/// with a single newer queued unit, provided the current AU is already late.
pub fn catch_up_wait_ms(age_us:u64,remaining:usize,source_duration_us:Option<u64>)->u64{
    if remaining>=2 {
        if age_us>=45_000{return 2;}
        if age_us>=30_000{return 8;}
    }
    let fast_stream=source_duration_us.is_some_and(|d|(8_000..=20_000).contains(&d));
    if fast_stream && remaining>=1 && age_us>=25_000{return 8;}
    67
}
#[cfg(test)]
mod video_tests{
    use super::*;
    #[test]fn recovery_preserves_normal_pacing_and_old_tiers(){
        assert_eq!(catch_up_wait_ms(100_000,0,Some(16_667)),67);
        assert_eq!(catch_up_wait_ms(100_000,1,Some(33_333)),67);
        assert_eq!(catch_up_wait_ms(100_000,1,None),67);
        assert_eq!(catch_up_wait_ms(29_999,5,Some(33_333)),67);
        assert_eq!(catch_up_wait_ms(30_000,2,None),8);
        assert_eq!(catch_up_wait_ms(45_000,2,Some(16_667)),2);
    }
    #[test]fn high_fps_single_newer_frame_recovers_moderately(){
        assert_eq!(catch_up_wait_ms(24_999,1,Some(16_667)),67);
        assert_eq!(catch_up_wait_ms(25_000,1,Some(16_667)),8);
        assert_eq!(catch_up_wait_ms(25_000,1,Some(20_001)),67);
        assert_eq!(catch_up_wait_ms(60_000,1,Some(0)),67);
    }
}
