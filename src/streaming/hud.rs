//! Read-only, structured diagnostic snapshots. No codec or transport policy changes.
use std::time::Instant;
#[derive(Clone,Debug,Default)]
pub(crate) struct PlaybackMetrics {
    pub decoded:u64,pub presented:u64,pub rtp_avg_us:u64,pub rtp_max_us:u64,
    pub decode_us:u64,pub queue_age_us:u64,pub input_us:u64,pub display_us:u64,pub pipeline_us:u64,
    pub audio_ms:u64,pub underruns:u64,pub audio_drops:u64,pub skipped:u64,pub replaced:u64,
    pub queue_full:u64,pub resyncs:u64,pub resets:u64,
}
#[derive(Clone,Debug)]
pub(crate) struct StreamHud {
    pub at:Instant,pub elapsed:f64,pub server:Option<(u32,u32)>,pub encoded:Option<(u32,u32)>,
    pub source_fps:Option<f64>,pub playback:PlaybackMetrics,pub rx_bps:u64,pub tx_bps:u64,
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
