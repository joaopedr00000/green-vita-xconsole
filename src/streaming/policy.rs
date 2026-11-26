//! Bounded playback policies, independent of Vita hardware.
/// Global default for all streaming quality profiles.
pub const DEFAULT_LATENCY_MODE:u8=2;
pub fn normalize_latency_mode(mode:u8)->u8{if mode<=2{mode}else{DEFAULT_LATENCY_MODE}}
#[cfg(test)]
mod latency_default_tests{
    use super::*;
    #[test]fn global_default_is_extreme(){assert_eq!(DEFAULT_LATENCY_MODE,2);assert_eq!(normalize_latency_mode(255),2);assert_eq!(normalize_latency_mode(0),0);assert_eq!(normalize_latency_mode(1),1);assert_eq!(normalize_latency_mode(2),2);}
}
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

/// A static overlay can reuse prepared geometry for at most 250ms.
pub fn reuse_overlay(age_ms:u64)->bool{age_ms<250}
#[cfg(test)]
mod overlay_tests{
    use super::*;
    #[test]fn overlay_refresh_is_bounded(){assert!(reuse_overlay(0));assert!(reuse_overlay(249));assert!(!reuse_overlay(250));assert!(!reuse_overlay(u64::MAX));}
}

/// Adaptive output waits affect only a pending, not-yet-displayed texture.
/// The compressed AU order and capacity are unchanged.
pub fn experimental_texture_wait_ms(mode:u8,age_us:u64,remaining:usize,source_duration_us:Option<u64>)->u64{
    let baseline=catch_up_wait_ms(age_us,remaining,source_duration_us);
    if mode==0{return baseline;}
    let fast=source_duration_us.is_some_and(|d|(8_000..=20_000).contains(&d));
    if mode>=2 && ((fast && age_us>=16_000)||(remaining>=1 && age_us>=33_000)){return baseline.min(2);}
    if fast && remaining>=1 && age_us>=18_000{return baseline.min(4);}
    baseline
}
/// Minimum interval before an early wake, relative to the last presentation.
/// This is CPU-side scheduling, not a claim about panel vblank or scanout.
pub fn early_wake_gap_us(mode:u8,source_fps:Option<f64>)->Option<u64>{
    if mode==0{return None;}
    let fast=source_fps.is_some_and(|f|f.is_finite()&&(50.0..=125.0).contains(&f));
    Some(if mode>=2{if fast{12_000}else{14_000}}else if fast{14_000}else{16_000})
}
pub fn late_wait_us(mode:u8,fast:bool)->u64{
    if !fast || mode==0{0}else if mode>=2{3_000}else{2_000}
}
#[cfg(test)]
mod latency_lab_tests{
    use super::*;
    #[test]fn classic_matches_release(){
        for age in [0,16_000,18_000,25_000,45_000,100_000]{for remaining in 0..6{for duration in [None,Some(16_667),Some(33_333)]{
            assert_eq!(experimental_texture_wait_ms(0,age,remaining,duration),catch_up_wait_ms(age,remaining,duration));
        }}}
        assert_eq!(early_wake_gap_us(0,Some(60.0)),None);
    }
    #[test]fn adaptive_avoids_single_frame_replacement(){
        assert_eq!(experimental_texture_wait_ms(1,20_000,0,Some(16_667)),67);
        assert_eq!(experimental_texture_wait_ms(1,18_000,1,Some(16_667)),4);
        assert_eq!(experimental_texture_wait_ms(1,18_000,1,Some(33_333)),67);
    }
    #[test]fn extreme_is_bounded_and_only_for_stale_work(){
        assert_eq!(experimental_texture_wait_ms(2,15_999,0,Some(16_667)),67);
        assert_eq!(experimental_texture_wait_ms(2,16_000,0,Some(16_667)),2);
        assert_eq!(experimental_texture_wait_ms(2,33_000,1,Some(33_333)),2);
        assert_eq!(experimental_texture_wait_ms(2,33_000,0,Some(33_333)),67);
    }
    #[test]fn pacing_limits(){
        assert_eq!(early_wake_gap_us(1,Some(59.94)),Some(14_000));
        assert_eq!(early_wake_gap_us(2,Some(60.0)),Some(12_000));
        assert_eq!(early_wake_gap_us(1,Some(30.0)),Some(16_000));
        assert_eq!(late_wait_us(2,false),0);
        assert_eq!(late_wait_us(1,true),2000);
    }
}

pub fn audio_start_ms(mode:u8,baseline:u32)->u32{if mode==2{baseline.saturating_sub(20).max(40)}else{baseline}}
pub fn audio_floor_ms(mode:u8,baseline:u32)->u32{if mode==2{baseline.saturating_sub(30).max(40)}else{baseline}}
pub fn audio_next_target_ms(current:u32,floor:u32,stable:bool,low_water_ms:u32)->u32{
    if !stable || low_water_ms<20{return current;}current.saturating_sub(5).max(floor)
}
pub fn pacing_guard_us(mode:u8,source_fps:Option<f64>,elapsed_us:u64,ready:bool)->u64{
    if mode!=2 || !ready || !source_fps.is_some_and(|f|f.is_finite()&&(50.0..=75.0).contains(&f)){return 0;}
    12_000u64.saturating_sub(elapsed_us).min(2_000)
}
pub fn nack_grace_ms(mode:u8,unrequested_age_us:u64,samples:u64)->u64{
    if mode!=2 || samples<16{return 12;}
    (unrequested_age_us.saturating_add(3_000).div_ceil(1000)).clamp(12,18)
}
#[cfg(test)]
mod refinements{
    use super::*;
    #[test]fn audio_headroom(){assert_eq!(audio_start_ms(2,80),60);assert_eq!(audio_floor_ms(2,80),50);assert_eq!(audio_start_ms(0,80),80);assert_eq!(audio_next_target_ms(60,50,true,19),60);assert_eq!(audio_next_target_ms(60,50,true,20),55);assert_eq!(audio_next_target_ms(50,50,true,30),50);assert_eq!(audio_next_target_ms(60,50,false,30),60);}
    #[test]fn pacing_bounds(){assert_eq!(pacing_guard_us(2,Some(60.0),9000,true),2000);assert_eq!(pacing_guard_us(2,Some(60.0),11500,true),500);assert_eq!(pacing_guard_us(2,Some(60.0),12000,true),0);assert_eq!(pacing_guard_us(0,Some(60.0),9000,true),0);assert_eq!(pacing_guard_us(2,Some(30.0),9000,true),0);assert_eq!(pacing_guard_us(2,Some(60.0),9000,false),0);}
    #[test]fn nack_bounds(){assert_eq!(nack_grace_ms(2,15000,15),12);assert_eq!(nack_grace_ms(0,15000,100),12);assert_eq!(nack_grace_ms(2,100,100),12);assert_eq!(nack_grace_ms(2,11100,100),15);assert_eq!(nack_grace_ms(2,100000,100),18);}
}
