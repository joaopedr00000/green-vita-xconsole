//! Conservative 720p-only receiver bitrate feedback.
//! Feedback is a request to the server, not evidence that Xbox changed its encoder.
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicU32, Ordering};
use crate::settings::{StreamProfile, active_stream_profile};

const MIN_BPS: u32 = 2_500_000;
const MAX_BPS: u32 = 15_000_000;
const START_BPS: u32 = 6_000_000;
// Only aggregate bitrate from a healthy stream is retained in process memory.
static LAST_HEALTHY_RX_BPS: AtomicU32 = AtomicU32::new(0);
pub(crate) fn remember_healthy_rx(bps: u32) {
    if (2_500_000..=16_000_000).contains(&bps) {
        LAST_HEALTHY_RX_BPS.store(bps, Ordering::Relaxed);
    }
}
const WINDOW: Duration = Duration::from_secs(3);
const FEEDBACK_INTERVAL: Duration = Duration::from_secs(5);
const CLEAN_INCREASE_INTERVAL: Duration = Duration::from_secs(20);
const CLEAN_INCREASE_STEP_BPS: u32 = 1_000_000;

pub(crate) struct AdaptiveBitrate {
    target_bps: u32,
    profile: StreamProfile,
    window_started: Instant,
    last_increase: Instant,
    last_feedback: Option<Instant>,
    packets: u64,
    damaged_frames: u64,
}

impl AdaptiveBitrate {
    pub(crate) fn new() -> Self {
        Self::with_profile(active_stream_profile())
    }

    fn with_profile(profile: StreamProfile) -> Self {
        let now = Instant::now();
        let measured = LAST_HEALTHY_RX_BPS.load(Ordering::Relaxed);
        let initial = if measured > 0 {
            measured.saturating_add(500_000).max(profile.starting_bitrate())
        } else {
            profile.starting_bitrate()
        };
        Self {
            target_bps: initial.clamp(MIN_BPS, profile.bitrate_cap()),
            profile,
            window_started: now,
            last_increase: now,
            last_feedback: None,
            packets: 0,
            damaged_frames: 0,
        }
    }

    pub(crate) fn packet_received(&mut self) {
        self.packets = self.packets.saturating_add(1);
    }

    pub(crate) fn frame_damaged(&mut self, count: u64) {
        self.damaged_frames = self.damaged_frames.saturating_add(count);
    }

    /// Called from the RTC worker at frame/packet cadence, never from the UI.
    /// Returns feedback only periodically or after a meaningful congestion change.
    pub(crate) fn sample(&mut self, now: Instant) -> Option<u32> {
        if self.packets == 0 {
            return None;
        }
        let mut changed = false;
        if now.duration_since(self.window_started) >= WINDOW {
            // Multiple bad frames in a short window are stronger evidence than
            // one lost packet, which NACK may already be repairing.
            if self.damaged_frames >= 2 {
                let reduced = (self.target_bps * 4 / 5).max(MIN_BPS);
                changed = reduced != self.target_bps;
                self.target_bps = reduced;
                self.last_increase = now;
            } else if self.damaged_frames == 0
                && now.duration_since(self.last_increase) >= CLEAN_INCREASE_INTERVAL {
                let increased = self.target_bps.saturating_add(CLEAN_INCREASE_STEP_BPS).min(self.profile.bitrate_cap());
                changed = increased != self.target_bps;
                self.target_bps = increased;
                self.last_increase = now;
            }
            self.packets = 0;
            self.damaged_frames = 0;
            self.window_started = now;
        }
        if changed || self.last_feedback.is_none_or(|last|
            now.duration_since(last) >= FEEDBACK_INTERVAL) {
            self.last_feedback = Some(now);
            Some(self.target_bps)
        } else {
            None
        }
    }

    pub(crate) fn current_mbps(&self) -> f32 {
        self.target_bps as f32 / 1_000_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn never_requests_more_than_15mbps_720p_budget() {
        let start = Instant::now();
        let mut control = AdaptiveBitrate::new();
        assert_eq!(control.sample(start), None);
        control.packet_received();
        assert_eq!(control.sample(start), Some(START_BPS));
        for i in 1..16 {
            control.frame_damaged(3);
            control.packet_received();
            let rate = control.sample(start + WINDOW * i);
            if let Some(rate) = rate { assert!((MIN_BPS..=MAX_BPS).contains(&rate)); }
        }
        assert!(control.target_bps >= MIN_BPS);
        assert!(control.target_bps <= START_BPS);
    }
    #[test]
    fn reaches_15mbps_without_ever_exceeding_it_on_clean_stream() {
        let start = Instant::now();
        let mut control = AdaptiveBitrate::with_profile(StreamProfile::Quality);
        for i in 0..=100 {
            control.packet_received();
            let now = start + Duration::from_secs(i * 3);
            let _ = control.sample(now);
            assert!((MIN_BPS..=MAX_BPS).contains(&control.target_bps));
        }
        assert_eq!(control.target_bps, 15_000_000);
    }
    #[test]
    fn recovers_gradually_after_clean_period() {
        let start = Instant::now();
        let mut control = AdaptiveBitrate::new();
        control.packet_received();
        let _ = control.sample(start);
        control.frame_damaged(4);
        control.packet_received();
        let _ = control.sample(start + WINDOW);
        let low = control.target_bps;
        for i in 2..10 {
            control.packet_received();
            let _ = control.sample(start + WINDOW * i);
        }
        assert!(control.target_bps >= low);
        assert!(control.target_bps <= MAX_BPS);
    }
}
