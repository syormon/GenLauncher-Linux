//! Holds drawing to 60 frames a second.
//!
//! egui draws a whole frame for every input event, so moving the mouse over
//! the window redraws it as fast as the monitor refreshes: 144 or more times a
//! second on a fast one, which a launcher has no use for. Frames that come
//! closer together than 1/60 s wait for their turn instead; events that arrive
//! meanwhile are handled together in the next frame.
//!
//! Frame starts are kept on a fixed 60 Hz schedule rather than spaced 1/60 s
//! after the previous one. With vsync, a frame can only be shown on a refresh,
//! so "1/60 s after the last" would often just miss one and wait for the next,
//! settling well below 60. On the schedule, a frame that was shown late is
//! followed by one that waits less, and the rate averages out at 60. A 60 Hz
//! monitor already keeps to it, so there frames wait at most a moment.

use std::time::{Duration, Instant};

pub const FRAME: Duration = Duration::from_nanos(1_000_000_000 / 60);

#[derive(Default)]
pub struct FrameLimiter {
    /// When the next frame may start.
    next: Option<Instant>,
}

impl FrameLimiter {
    /// Wait until the frame about to be drawn is due.
    pub fn wait(&mut self) {
        let wait = self.delay(Instant::now());
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }

    /// How long a frame asked for at `now` must wait, booking its slot.
    fn delay(&mut self, now: Instant) -> Duration {
        let wait = self.next.map_or(Duration::ZERO, |next| next.saturating_duration_since(now));
        let start = now + wait;
        self.next = Some(match self.next {
            // On schedule, or behind it by less than a frame.
            Some(next) if next + FRAME > start => next + FRAME,
            // After a pause, or a slow frame: begin a new schedule rather
            // than catch up with a burst of frames.
            _ => start + FRAME,
        });
        wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn the_first_frame_and_one_after_a_pause_do_not_wait() {
        let mut limiter = FrameLimiter::default();
        let t = Instant::now();
        assert_eq!(limiter.delay(t), Duration::ZERO);
        assert_eq!(limiter.delay(t + Duration::from_secs(2)), Duration::ZERO);
    }

    #[test]
    fn frames_faster_than_60_wait_for_their_slot() {
        let mut limiter = FrameLimiter::default();
        let t = Instant::now();
        limiter.delay(t);
        // A mouse move 2 ms after the last frame started.
        assert_eq!(limiter.delay(t + 2 * MS), FRAME - 2 * MS);
    }

    #[test]
    fn a_144_hz_monitor_still_gets_60_frames_a_second() {
        // With vsync a frame starts only just after a refresh. Ask for one at
        // every refresh after the slot opens, as constant mouse movement would.
        let refresh = Duration::from_nanos(1_000_000_000 / 144);
        let paint = 2 * MS;
        let mut limiter = FrameLimiter::default();
        let t0 = Instant::now();
        let mut now = t0;
        let mut frames = 0;
        while now < t0 + Duration::from_secs(10) {
            let start = now + limiter.delay(now);
            frames += 1;
            // Shown on the first refresh after it is painted; the next frame
            // is asked for right after.
            let painted = start + paint - t0;
            let refreshes = painted.as_nanos().div_ceil(refresh.as_nanos()) as u32;
            now = t0 + refresh * refreshes;
        }
        assert!((595..=601).contains(&frames), "{frames} frames in 10 s");
    }

    #[test]
    fn a_60_hz_monitor_is_not_held_back() {
        let refresh = Duration::from_nanos(1_000_000_000 / 60);
        let mut limiter = FrameLimiter::default();
        let t0 = Instant::now();
        for i in 0..600 {
            // A frame just after each refresh, with a little jitter either way.
            let jitter = if i % 2 == 0 { Duration::from_micros(300) } else { Duration::ZERO };
            let wait = limiter.delay(t0 + refresh * i + jitter);
            assert!(wait < MS, "frame {i} waited {wait:?}");
        }
    }
}
