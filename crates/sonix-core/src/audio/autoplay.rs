//! "Autoplay similar music when the queue ends": the engine-side pieces that
//! don't need a device — the provider trait, the trigger condition and the
//! candidate filter. The engine (`engine.rs`) owns the state machine.
//!
//! Flow: when the *last* track of the queue is playing (`Queue::is_at_end`,
//! repeat off, nothing left to draw from the source), the engine asks the
//! [`AutoplaySource`] for tracks similar to it on a helper thread (so the
//! engine thread never blocks on the network). The answer is appended behind
//! the current track as an in-order `radio` source (`Queue::continue_with_source`),
//! which the ordinary gapless hand-off then plays seamlessly. If the answer
//! comes too late for a gapless transition (slow server), the radio starts
//! right after the last track ends, with a short gap.

use std::collections::VecDeque;

/// Similar-track lookup (the production impl reuses `radio_start`'s logic:
/// `getSimilarSongs` + local fallback + 1-star filtering, see
/// `continuity::radio_pool`). Blocking; only ever called off the engine thread.
pub trait AutoplaySource: Send + Sync + 'static {
    /// `(track id, artist)` of tracks similar to `seed_track_id`, in relevance
    /// order, already restricted to tracks the local library knows. May be empty.
    fn similar(&self, seed_track_id: &str) -> Vec<(String, Option<String>)>;
}

/// How many recently played ids autoplay refuses to queue again.
pub const RECENT_LIMIT: usize = 50;

/// Label prefix of the autoplay radio source (`Autoplay · <seed title>`);
/// `listen::` recognizes autoplay-origin plays by it.
pub use crate::config::AUTOPLAY_LABEL_PREFIX as LABEL_PREFIX;

/// Inputs of the trigger decision, kept as plain data so it is unit-testable
/// without a device.
#[derive(Debug, Clone, Copy)]
pub struct WantCheck {
    pub enabled: bool,
    pub have_provider: bool,
    /// A track is actually loaded in the output (not merely queued/restored).
    pub playing_something: bool,
    /// `Queue::is_at_end`.
    pub at_end: bool,
    pub request_inflight: bool,
    /// A request was already made for this very current track.
    pub already_requested_for_current: bool,
}

pub fn autoplay_wanted(c: &WantCheck) -> bool {
    c.enabled && c.have_provider && c.playing_something && c.at_end && !c.request_inflight && !c.already_requested_for_current
}

/// Drops candidates that were played recently, are already queued, or repeat
/// within the list. Returns parallel id/artist vectors.
pub fn filter_candidates(
    items: Vec<(String, Option<String>)>,
    recent: &VecDeque<String>,
    in_queue: &dyn Fn(&str) -> bool,
) -> (Vec<String>, Vec<Option<String>>) {
    let mut seen = std::collections::HashSet::new();
    let mut ids = Vec::new();
    let mut artists = Vec::new();
    for (id, artist) in items {
        if recent.iter().any(|r| *r == id) || in_queue(&id) || !seen.insert(id.clone()) {
            continue;
        }
        ids.push(id);
        artists.push(artist);
    }
    (ids, artists)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> WantCheck {
        WantCheck {
            enabled: true,
            have_provider: true,
            playing_something: true,
            at_end: true,
            request_inflight: false,
            already_requested_for_current: false,
        }
    }

    #[test]
    fn triggers_only_when_every_condition_holds() {
        assert!(autoplay_wanted(&base()));
        assert!(!autoplay_wanted(&WantCheck { enabled: false, ..base() }), "setting off");
        assert!(!autoplay_wanted(&WantCheck { have_provider: false, ..base() }));
        assert!(!autoplay_wanted(&WantCheck { playing_something: false, ..base() }), "a restored, never-played queue does not fetch");
        assert!(!autoplay_wanted(&WantCheck { at_end: false, ..base() }), "queue still has tracks / repeat on");
        assert!(!autoplay_wanted(&WantCheck { request_inflight: true, ..base() }));
        assert!(!autoplay_wanted(&WantCheck { already_requested_for_current: true, ..base() }), "an empty answer must not loop");
    }

    #[test]
    fn candidates_exclude_recent_queued_and_duplicate_ids() {
        let recent: VecDeque<String> = ["r1".to_string(), "r2".to_string()].into();
        let items = vec![
            ("a".to_string(), Some("A".to_string())),
            ("r1".to_string(), None),
            ("q".to_string(), None),
            ("a".to_string(), Some("A".to_string())),
            ("b".to_string(), None),
        ];
        let (ids, artists) = filter_candidates(items, &recent, &|id| id == "q");
        assert_eq!(ids, vec!["a", "b"]);
        assert_eq!(artists, vec![Some("A".to_string()), None]);
    }
}
