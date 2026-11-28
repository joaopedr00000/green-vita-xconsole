//! Bounded playback policies, independent of Vita hardware.
/// Fixed universal synchronization policy for all streaming quality profiles.
pub const UNIVERSAL_LATENCY_MODE:u8=2;
#[cfg(test)]
mod latency_default_tests{
    use super::*;
    #[test]fn universal_mode_is_extreme(){assert_eq!(UNIVERSAL_LATENCY_MODE,2);}
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
    Some(if mode>=2{if fast{10_000}else{14_000}}else if fast{14_000}else{16_000})
}
pub fn late_wait_us(mode:u8,fast:bool)->u64{
    if !fast || mode==0{0}else if mode>=2{0}else{2_000}
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
        assert_eq!(early_wake_gap_us(2,Some(60.0)),Some(10_000));
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
pub fn nack_grace_ms(mode:u8,unrequested_age_us:u64,samples:u64)->u64{
    if mode!=2 || samples<16{return 12;}
    (unrequested_age_us.saturating_add(3_000).div_ceil(1000)).clamp(12,18)
}
#[cfg(test)]
mod refinements{
    use super::*;
    #[test]fn audio_headroom(){assert_eq!(audio_start_ms(2,80),60);assert_eq!(audio_floor_ms(2,80),50);assert_eq!(audio_start_ms(0,80),80);assert_eq!(audio_next_target_ms(60,50,true,19),60);assert_eq!(audio_next_target_ms(60,50,true,20),55);assert_eq!(audio_next_target_ms(50,50,true,30),50);assert_eq!(audio_next_target_ms(60,50,false,30),60);}

    #[test]fn nack_bounds(){assert_eq!(nack_grace_ms(2,15000,15),12);assert_eq!(nack_grace_ms(0,15000,100),12);assert_eq!(nack_grace_ms(2,100,100),12);assert_eq!(nack_grace_ms(2,11100,100),15);assert_eq!(nack_grace_ms(2,100000,100),18);}
}

/// Very small rate correction only with excess queued audio. Preserve a
/// generous deadband and never accelerate startup or an underrun recovery.
pub fn audio_rate_trim_frames(mode:u8,started:bool,queued_ms:u32,target_ms:u32,frames:usize)->usize {
    if mode!=2 || !started || queued_ms<target_ms.saturating_add(8) || frames<128{return 0;}
    let percent=if queued_ms>=target_ms.saturating_add(20){2}else{1};
    frames.saturating_mul(percent)/100
}
/// In-place stereo-preserving interpolation. Read positions never precede
/// write positions. Keep both endpoints, no allocation and no packet deletion.
pub fn shorten_stereo_pcm(samples:&mut Vec<i16>,trim_frames:usize)->usize{
    let frames=samples.len()/2;
    if samples.len()%2!=0 || frames<2 || trim_frames==0 || trim_frames>frames/50{return 0;}
    let out=frames-trim_frames;
    if out<2{return 0;}
    let denom=(out-1) as i64;
    for i in 0..out {
        let pos=(i as i64)*(frames-1) as i64;
        let j=(pos/denom) as usize;
        let fraction=pos%denom;
        let next=(j+1).min(frames-1);
        for ch in 0..2 {
            let a=i64::from(samples[j*2+ch]);let b=i64::from(samples[next*2+ch]);
            samples[i*2+ch]=((a*(denom-fraction)+b*fraction)/denom) as i16;
        }
    }
    samples.truncate(out*2);
    trim_frames
}
#[cfg(test)]
mod audio_trim_tests{
    use super::*;
    #[test]fn trim_is_bounded_and_uses_headroom(){
        assert_eq!(audio_rate_trim_frames(2,true,67,60,960),0);
        assert_eq!(audio_rate_trim_frames(2,true,68,60,960),9);
        assert_eq!(audio_rate_trim_frames(2,true,79,60,960),9);
        assert_eq!(audio_rate_trim_frames(2,true,80,60,960),19);
        assert_eq!(audio_rate_trim_frames(2,false,100,60,960),0);
        assert_eq!(audio_rate_trim_frames(0,true,100,60,960),0);
    }
    #[test]fn interpolation_preserves_stereo_and_endpoints(){
        let mut s:Vec<i16>=(0..1000i16).flat_map(|i|[i,-i]).collect();
        assert_eq!(shorten_stereo_pcm(&mut s,20),20);
        assert_eq!(s.len(),1960);assert_eq!(&s[..2],&[0,0]);assert_eq!(&s[s.len()-2..],&[999,-999]);
        assert!(s.chunks_exact(2).all(|p|p[0]==-p[1]));
        assert_eq!(shorten_stereo_pcm(&mut s,100),0);
    }
}

/// Refill only with a complete negotiated device period; no claim about DAC state.
pub fn audio_refill_sufficient(queued_bytes:u32,period_bytes:u32)->bool{period_bytes>0 && queued_bytes>=period_bytes}
#[cfg(test)]
mod audio_refill_tests{
    use super::*;
    #[test]fn period_required(){assert!(!audio_refill_sufficient(0,4096));assert!(!audio_refill_sufficient(4095,4096));assert!(audio_refill_sufficient(4096,4096));assert!(audio_refill_sufficient(8192,4096));assert!(!audio_refill_sufficient(4096,0));}
}

/// Find a target not owned by the renderer or the pending picture.
pub fn free_texture(count:usize,displayed:Option<usize>,pending:Option<usize>)->Option<usize>{(0..count).find(|i|Some(*i)!=displayed && Some(*i)!=pending)}
#[cfg(test)]mod triple_tests{use super::*;#[test]fn independent_targets(){assert_eq!(free_texture(3,Some(0),Some(1)),Some(2));assert_eq!(free_texture(2,Some(0),Some(1)),None);assert_eq!(free_texture(2,Some(0),None),Some(1));}}

pub fn valid_plc_duration(frames:usize)->bool{matches!(frames,120|240|480|960|1920|2880)}
pub fn plc_packet_count(previous:u32,current:u32,frames:usize,missing:u16)->usize{
    if missing==0 || !valid_plc_duration(frames){return 0;}
    let delta=current.wrapping_sub(previous) as usize;
    if delta<=frames || delta>frames*3 || delta%frames!=0{return 0;}
    (delta/frames-1).min(missing as usize)
}
/// Peak-limited optional gain; no clipping and no change at 100%.
pub fn apply_audio_gain(pcm:&mut [i16],percent:u32){
    let percent=percent.clamp(100,150);
    if percent==100{return;}
    let peak=pcm.iter().map(|s|i32::from(*s).unsigned_abs()).max().unwrap_or(0).max(1);
    let gain=(percent*256/100).min(32767*256/peak);
    for s in pcm{*s=(i32::from(*s)*gain as i32/256).clamp(-32768,32767) as i16;}
}
#[cfg(test)]mod direct_audio_tests{
    use super::*;
    #[test]fn plc_only_confirmed_short_loss(){assert_eq!(plc_packet_count(1000,2920,960,1),1);assert_eq!(plc_packet_count(1000,3880,960,2),2);assert_eq!(plc_packet_count(1000,3880,960,0),0);assert_eq!(plc_packet_count(1000,12000,960,5),0);assert_eq!(plc_packet_count(u32::MAX-959,960,960,1),1);assert_eq!(plc_packet_count(1000,2000,441,2),0);}
    #[test]fn gain_peak_safe(){let mut s=[1000,-1000,30000,-32768];apply_audio_gain(&mut s,150);assert!(s[2]<=30000);assert!(s[3]>=-32768);let mut quiet=[1000,-1000];apply_audio_gain(&mut quiet,150);assert_eq!(quiet,[1500,-1500]);}
}

/// Warm-start hint limited to two Mbps over the ordinary startup rate.
pub fn bounded_link_hint(start:u32,cap:u32,hint:u32)->u32{hint.max(start).min(start.saturating_add(2_000_000)).min(cap)}
#[cfg(test)]mod hint_tests{use super::*;#[test]fn bounded(){assert_eq!(bounded_link_hint(6_000_000,15_000_000,0),6_000_000);assert_eq!(bounded_link_hint(6_000_000,15_000_000,u32::MAX),8_000_000);assert_eq!(bounded_link_hint(6_000_000,6_000_000,9_000_000),6_000_000);}}

// Shorten only the waiting window for healthy low-latency streams. Keep full
// recovery grace on known loss, keyframe recovery or rising jitter.
pub fn adaptive_reorder_grace_ms(base:u64,mode:u8,low_latency:bool,jitter_us:u64,recovering:bool)->u64{
    if mode>=2 && low_latency && !recovering && jitter_us<=4_000 { (8+jitter_us/500).clamp(8,16).min(base) } else {base}
}
#[cfg(test)] mod adaptive_reorder_tests{use super::*;#[test]fn safe_bounds(){assert_eq!(adaptive_reorder_grace_ms(16,2,true,0,false),8);assert_eq!(adaptive_reorder_grace_ms(18,2,true,4000,false),16);assert_eq!(adaptive_reorder_grace_ms(24,2,true,5000,false),24);assert_eq!(adaptive_reorder_grace_ms(16,2,true,0,true),16);assert_eq!(adaptive_reorder_grace_ms(32,2,false,0,false),32);}}

