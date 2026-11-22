//! Conservative 720p-only receiver bitrate feedback.
//! Feedback is a request to the server, not evidence that Xbox changed its encoder.
use std::time::{Duration, Instant};

const MIN_BPS: u32 = 2_500_000;
const MAX_BPS: u32 = 8_000_000;
const START_BPS: u32 = 6_000_000;
const WINDOW: Duration = Duration::from_secs(3);
const FEEDBACK_INTERVAL: Duration = Duration::from_secs(5);
const CLEAN_INCREASE_INTERVAL: Duration = Duration::from_secs(20);

pub(crate) struct AdaptiveBitrate {
    target_bps: u32,
    window_started: Instant,
    last_increase: Instant,
    last_feedback: Option<Instant>,
    packets: u64,
    damaged_frames: u64,
}

impl AdaptiveBitrate {
    pub(crate) fn new() -> Self {
        let now = Instant::now();
        Self {
            target_bps: START_BPS,
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
                let increased = self.target_bps.saturating_add(250_000).min(MAX_BPS);
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
    fn never_requests_more_than_720p_budget() {
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
