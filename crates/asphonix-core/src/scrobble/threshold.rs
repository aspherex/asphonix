//! Pure "does this play count yet?" logic — no I/O, no `windows`,
//! so it's cheap to unit test exhaustively (pause, seek, repeat-one, short
//! tracks) without a server or an engine. One verdict drives both the local
//! listening history and the server scrobble, so the two always agree.
//!
//! Rules:
//! - A track must be at least 30 seconds long to count at all.
//! - It must be *listened to* for the "count a play after" setting
//!   (`Settings.playThresholdSec`, default 30 s), or 90% of the track if
//!   that's shorter (so a 4-minute setting still counts a 3-minute track
//!   played through).
//! - "Listened to" is real playback time: pausing doesn't advance it, and
//!   jumping the position (seeking, forward or backward) doesn't either —
//!   only genuinely contiguous, forward, real-time-paced playback does. This
//!   matters because `PlayerState.position_sec` is the engine's render-side
//!   *position*, not a measure of how much was actually heard: a seek to 90%
//!   changes the position instantly without the listener having heard
//!   anything in between.

// `listened_sec()` is a debugging/observability accessor (used by tests);
// not read by the one real caller (`should_count`) which has its own
// private field access.
#![allow(dead_code)]

/// Largest position delta between two consecutive samples that still counts
/// as "normal playback" rather than a seek. Must comfortably exceed the
/// engine's own `player-state` tick (250ms) plus scheduling jitter, while
/// staying well under any plausible intentional seek.
const MAX_NATURAL_DELTA_SEC: f64 = 2.0;

/// Tracks shorter than this never count (Last.fm rule), regardless of how
/// much of them was played.
pub const MIN_COUNTABLE_DURATION_SEC: f64 = 30.0;

/// "Count a play after" setting range and default, in seconds.
pub const MIN_PLAY_THRESHOLD_SEC: u32 = 10;
pub const MAX_PLAY_THRESHOLD_SEC: u32 = 300;
pub const DEFAULT_PLAY_THRESHOLD_SEC: u32 = 30;

/// A track shorter than the setting counts once this share of it was heard
/// (position ticks stop a little before the real end, so not 100%).
const SHORT_TRACK_SHARE: f64 = 0.9;

/// Accumulates real listened-to seconds for one play of one track, from a
/// stream of `(status, position_sec)` samples (exactly what `PlayerState`
/// ticks provide). Construct a fresh one per "play instance" — a new track,
/// or the same track looping via repeat-one (see `service::is_new_play`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayThresholdTracker {
    listened_sec: f64,
    last_position_sec: Option<f64>,
    scrobbled: bool,
}

impl PlayThresholdTracker {
    pub fn new() -> Self {
        Self { listened_sec: 0.0, last_position_sec: None, scrobbled: false }
    }

    /// Feed one playback sample. `playing` is whether the engine's status is
    /// `Playing` *for this sample* (paused/stopped/loading samples never
    /// advance the accumulator, but still update the last-seen position so
    /// the next real sample's delta is measured from the right place).
    pub fn observe(&mut self, playing: bool, position_sec: f64) {
        if playing {
            if let Some(last) = self.last_position_sec {
                let delta = position_sec - last;
                // Only a small *forward* delta is genuine elapsed playback;
                // anything else (backward, or a jump bigger than one tick
                // could plausibly advance) is a seek and contributes nothing.
                if delta > 0.0 && delta <= MAX_NATURAL_DELTA_SEC {
                    self.listened_sec += delta;
                }
            }
        }
        self.last_position_sec = Some(position_sec);
    }

    /// Real seconds of this track actually heard so far (never inflated by
    /// pauses or seeks).
    pub fn listened_sec(&self) -> f64 {
        self.listened_sec
    }

    /// Whether this play has earned its history row + scrobble:
    /// `threshold_sec` of real listening (or 90% of a shorter track), and
    /// the track must clear the minimum countable duration at all.
    pub fn should_count(&self, duration_sec: f64, threshold_sec: u32) -> bool {
        !self.scrobbled && Self::counts(self.listened_sec, duration_sec, threshold_sec)
    }

    /// The threshold rule alone (no "already counted" flag): shared with the
    /// listen-event recorder, which decides `counted` once per finished
    /// session from the same accumulated listening time.
    pub fn counts(listened_sec: f64, duration_sec: f64, threshold_sec: u32) -> bool {
        if duration_sec < MIN_COUNTABLE_DURATION_SEC {
            return false;
        }
        let threshold = f64::from(threshold_sec).min(duration_sec * SHORT_TRACK_SHARE);
        listened_sec >= threshold
    }

    /// One count per play: call once the play was counted so repeated
    /// ticks on the same play never double-submit.
    pub fn mark_scrobbled(&mut self) {
        self.scrobbled = true;
    }

    pub fn already_scrobbled(&self) -> bool {
        self.scrobbled
    }
}

impl Default for PlayThresholdTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulates steady playback at `tick_sec`-second intervals for
    /// `total_sec` seconds (a track played straight through, no pauses).
    fn play_straight_through(tracker: &mut PlayThresholdTracker, total_sec: f64, tick_sec: f64) {
        let mut t = 0.0;
        while t <= total_sec {
            tracker.observe(true, t);
            t += tick_sec;
        }
    }

    /// Continues playback from `from` to `to` in 250 ms ticks.
    fn play_range(tracker: &mut PlayThresholdTracker, from: f64, to: f64) {
        let mut t = from;
        while t <= to {
            tracker.observe(true, t);
            t += 0.25;
        }
    }

    #[test]
    fn counts_after_the_configured_listening_time() {
        let mut t = PlayThresholdTracker::new();
        play_range(&mut t, 0.0, 29.5);
        assert!(!t.should_count(420.0, 30), "under 30 s heard");
        play_range(&mut t, 29.75, 30.25);
        assert!(t.should_count(420.0, 30), "30 s heard");
        assert!(!t.should_count(420.0, 60), "a 60 s setting needs 60 s");
        play_range(&mut t, 30.5, 60.25);
        assert!(t.should_count(420.0, 60));
    }

    #[test]
    fn a_setting_longer_than_the_track_counts_it_once_mostly_played() {
        let mut t = PlayThresholdTracker::new();
        // 3-minute track, 4-minute setting: 90% (162 s) is enough.
        play_range(&mut t, 0.0, 160.0);
        assert!(!t.should_count(180.0, 240));
        play_range(&mut t, 160.25, 163.0);
        assert!(t.should_count(180.0, 240));
    }

    #[test]
    fn pausing_does_not_advance_listened_time() {
        let mut t = PlayThresholdTracker::new();
        // Play 10s, then "pause" for what would be another 50s of wall time
        // (position never changes while paused) by feeding non-playing
        // samples at a frozen position, then resume.
        play_straight_through(&mut t, 10.0, 0.25);
        let listened_before_pause = t.listened_sec();
        for _ in 0..20 {
            t.observe(false, 10.0); // paused: status != Playing
        }
        assert_eq!(t.listened_sec(), listened_before_pause, "paused ticks must not add listened time");
        // Resuming from the same position continues normally.
        t.observe(true, 10.25);
        assert!(t.listened_sec() > listened_before_pause);
    }

    #[test]
    fn seeking_forward_past_the_threshold_does_not_count() {
        let mut t = PlayThresholdTracker::new();
        play_straight_through(&mut t, 5.0, 0.25); // 5s genuinely listened
        // User drags the scrubber to 90% of a 200s track.
        t.observe(true, 180.0);
        // The seek itself must not have been counted as listened time.
        assert!(t.listened_sec() < 10.0, "a seek must not inflate listened time: {}", t.listened_sec());
        assert!(!t.should_count(200.0, 30), "seeking past the threshold must not itself count a play");
    }

    #[test]
    fn seeking_backward_does_not_subtract_or_corrupt_listened_time() {
        let mut t = PlayThresholdTracker::new();
        play_straight_through(&mut t, 50.0, 0.25);
        let listened = t.listened_sec();
        t.observe(true, 5.0); // seek back near the start
        assert_eq!(t.listened_sec(), listened, "a backward seek must not change listened time");
        // Playback continues naturally from the new position afterwards.
        t.observe(true, 5.25);
        assert!(t.listened_sec() > listened);
    }

    #[test]
    fn short_tracks_never_count_even_fully_played() {
        let mut t = PlayThresholdTracker::new();
        play_straight_through(&mut t, 20.0, 0.25); // fully "played", 20s track
        assert!(!t.should_count(20.0, 10), "tracks under 30s must never count");
    }

    #[test]
    fn one_count_per_play_even_if_polled_again_after_marking() {
        let mut t = PlayThresholdTracker::new();
        play_straight_through(&mut t, 31.0, 0.25);
        assert!(t.should_count(200.0, 30));
        t.mark_scrobbled();
        assert!(t.already_scrobbled());
        // Even though the underlying listened time still clears the bar,
        // a tracker that already counted never says so again.
        assert!(!t.should_count(200.0, 30));
    }

    #[test]
    fn repeat_one_uses_a_fresh_tracker_per_loop() {
        // `is_new_play` is what decides a loop needs a new tracker; this
        // just documents/confirms a fresh tracker starts at zero so a second
        // play of the same track is independently eligible.
        let mut first = PlayThresholdTracker::new();
        play_straight_through(&mut first, 31.0, 0.25);
        assert!(first.should_count(200.0, 30));
        first.mark_scrobbled();

        let mut second = PlayThresholdTracker::new();
        assert!(!second.should_count(200.0, 30));
        play_straight_through(&mut second, 31.0, 0.25);
        assert!(second.should_count(200.0, 30), "a fresh loop must count independently");
    }
}
