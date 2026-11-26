//! Read-only, structured diagnostic snapshots. No codec or transport policy changes.
use std::time::{Duration,Instant};
#[derive(Clone,Debug,Default)]
pub(crate) struct PlaybackMetrics {
    pub decoded:u64,pub presented:u64,pub rtp_avg_us:u64,pub rtp_max_us:u64,
    pub decode_us:u64,pub queue_age_us:u64,pub input_us:u64,pub display_us:u64,pub pipeline_us:u64,
    pub audio_ms:u64,pub underruns:u64,pub audio_drops:u64,pub skipped:u64,pub replaced:u64,
    pub queue_full:u64,pub resyncs:u64,pub resets:u64,
    pub stages:Option<PresentedStages>,pub pipeline_avg_us:u64,pub pipeline_max_us:u64,pub stage_samples:u64,pub queue_depth:u64,pub stale_handoffs:u64,
}
#[derive(Clone,Debug)]
pub(crate) struct StreamHud {
    pub at:Instant,pub elapsed:f64,pub server:Option<(u32,u32)>,pub encoded:Option<(u32,u32)>,
    pub source_fps:Option<f64>,pub playback:PlaybackMetrics,pub rx_bps:u64,pub tx_bps:u64,
    pub rtt:Option<RttReading>,
    pub rx_total:u64,pub tx_total:u64,pub rx_packets:u64,pub tx_packets:u64,pub socket_errors:u64,
    pub video_bps:u64,pub requested_bps:u64,pub jitter_us:u64,pub gaps:u64,pub late:u64,
    pub nacks:u64,pub nack_packets:u64,pub waiting:bool,pub dropped:u64,pub errors:u64,
}
pub(crate) fn rate(bytes:u64, seconds:f64)->u64 {
    if !seconds.is_finite() || seconds<=0.0 {return 0;}
    (bytes as f64*8.0/seconds).round() as u64
}
pub(crate) fn fps(count:u64, seconds:f64)->f64 { if seconds>0.0 && seconds.is_finite(){count as f64/seconds}else{0.0} }
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn actual_elapsed_rates(){assert_eq!(rate(125000,1.0),1000000);assert_eq!(rate(125000,2.0),500000);assert_eq!(rate(100,0.0),0);assert_eq!(fps(45,1.5),30.0);}
}

#[derive(Clone,Copy,Debug,Default)]
pub(crate) struct DecodeStages {pub queue_us:u64,pub texture_us:u64,pub decode_us:u64}
#[derive(Clone,Copy,Debug,Default)]
pub(crate) struct PresentedStages {pub decode:DecodeStages,pub display_us:u64,pub total_us:u64}
#[derive(Clone,Copy,Debug)]
pub(crate) struct RttReading {pub current_us:u64,pub average_us:u64,pub responses:u64,pub observed_at:Instant}
impl RttReading {
    pub fn age(&self)->Duration{self.observed_at.elapsed()}
}
pub(crate) fn rtt_values(current:f64,total:f64,responses:u64)->Option<(u64,u64)>{
    if responses==0 || !current.is_finite() || current<=0.0 || current>60.0 || !total.is_finite() || total<=0.0 {return None;}
    Some(((current*1_000_000.0).round() as u64,(total*1_000_000.0/responses as f64).round() as u64))
}
#[derive(Default)]
pub(crate) struct SourceCadence { durations:std::collections::VecDeque<u64> }
impl SourceCadence {
    pub fn observe(&mut self,duration:u64){
        if !(8_000..=100_000).contains(&duration){return;}
        if self.durations.len()>=31{self.durations.pop_front();}
        self.durations.push_back(duration);
    }
    pub fn estimate(&self)->Option<f64>{
        if self.durations.len()<3{return None;}
        let mut a:Vec<_>=self.durations.iter().copied().collect();a.sort_unstable();
        Some(1_000_000.0/a[a.len()/2] as f64)
    }
}
#[cfg(test)]
mod new_tests {
    use super::*;
    #[test]fn rtt_requires_real_response(){assert_eq!(rtt_values(0.025,0.060,2),Some((25000,30000)));assert_eq!(rtt_values(0.025,0.060,0),None);assert_eq!(rtt_values(f64::NAN,1.0,1),None);}
    #[test]fn cadence_rejects_single_short_or_long_interval(){let mut c=SourceCadence::default();for d in [16667,16667,14900,33334,16667]{c.observe(d);}assert!((c.estimate().unwrap()-60.0).abs()<0.01);}
}
