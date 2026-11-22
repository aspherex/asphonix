//! OS media integration: Windows System Media Transport Controls (SMTC) —
//! the "Now Playing" card in the volume flyout/Action Center — plus hardware
//! media key handling. macOS (`MPNowPlayingInfoCenter`) is a cfg stub,
//! not implemented yet.
//!
//! Driven entirely by `audio::EngineHandle::subscribe_player_state()` (the
//! in-process hook in `engine.rs`) — the exact same
//! `PlayerState` the UI gets, never a second source of truth. Button/media-key
//! presses are translated straight back into `EngineCommand`s.
//!
//! Registration (`GetForWindow` + event handlers) happens in `windows_backend`
//! on the window's own thread via [`attach`] (the UI calls it there, delayed past
//! the first show); later updates run on a background thread — never on the
//! window-show critical path. [`classify`] below is the cheap,
//! cross-platform, unit-tested decision for *when* to push an update to the
//! OS, kept separate from the `windows`-only plumbing so it needs no COM/WinRT
//! to test.

#[cfg(windows)]
mod windows_backend;

use crate::ipc::{PlaybackStatus, PlayerState};

/// Everything the OS media integration needs from the app (engine commands, player-state watch, cover bytes).
pub trait MediaHost: Send + Sync + 'static {
    /// Transport commands from the OS (play/pause/next/previous/seek) go to the engine.
    fn send(&self, cmd: crate::audio::EngineCommand);
    /// The same `PlayerState` watch the UI is fed from (never a second source of truth); `None` once the app is gone.
    fn subscribe(&self) -> Option<tokio::sync::watch::Receiver<PlayerState>>;
    /// Encoded (jpeg/png) cover bytes for the thumbnail at about `size` px (cache first, then server).
    fn cover(&self, cover_art_id: String, size: u32) -> BoxFut<Option<Vec<u8>>>;
}

pub type BoxFut<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;

/// Registers ASPHONIX's window (`hwnd`, a Win32 HWND) as a System Media Transport Controls source and keeps it
/// in sync with the player state.
///
/// MUST be called on the thread that owns the window, and not on the
/// window-show path: the caller delays it (about 1.5 s). `GetForWindow` and the event
/// registration run right here; the update loop then runs on its own thread and ends when `generation` no
/// longer equals `gen` (the window was re-attached) or the engine is gone. Non-Windows: no-op (macOS
/// `MPNowPlayingInfoCenter` is not implemented yet).
#[cfg(windows)]
pub fn attach(host: std::sync::Arc<dyn MediaHost>, hwnd: isize, generation: std::sync::Arc<std::sync::atomic::AtomicU64>, gen: u64) {
    windows_backend::attach(host, hwnd, generation, gen);
}
#[cfg(not(windows))]
pub fn attach(_host: std::sync::Arc<dyn MediaHost>, _hwnd: isize, _generation: std::sync::Arc<std::sync::atomic::AtomicU64>, _gen: u64) {}

/// Largest position delta between two consecutive samples of the *same*
/// track at the *same* status that still counts as ordinary playback rather
/// than a seek (mirrors `scrobble::threshold`'s reasoning, independently —
/// this module only cares about display accuracy, not scrobble eligibility).
const MAX_NATURAL_DELTA_SEC: f64 = 2.0;

/// Minimum spacing between two timeline-only refreshes during otherwise
/// uneventful playback — Windows interpolates the scrubber itself between
/// `UpdateTimelineProperties` calls (from `Position` + the playback status),
/// so there is no need to push on every ~250ms `player-state` tick.
const PERIODIC_REFRESH_SEC: f64 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdateKind {
    /// Nothing worth telling the OS about (steady mid-track playback,
    /// refreshed less than `PERIODIC_REFRESH_SEC` ago).
    None,
    /// Track changed (or playback became idle): refresh title/artist/album/
    /// artwork and the timeline/status.
    Metadata,
    /// Same track: just the playback status and/or timeline position
    /// (status edge, a seek, or the periodic low-frequency refresh).
    Timeline,
}

/// Decides what (if anything) to push to the OS for one `player-state`
/// sample, given the last snapshot actually pushed.
pub(crate) fn classify(
    prev_track_id: Option<&str>,
    prev_status: PlaybackStatus,
    prev_position_sec: f64,
    new: &PlayerState,
    seconds_since_last_timeline_push: f64,
) -> UpdateKind {
    let new_track_id = new.track.as_ref().map(|t| t.id.as_str());
    if new_track_id != prev_track_id {
        return UpdateKind::Metadata;
    }
    if new_track_id.is_none() {
        return UpdateKind::None; // still idle; already cleared when it first became so.
    }
    if new.status != prev_status {
        return UpdateKind::Timeline;
    }
    let delta = new.position_sec - prev_position_sec;
    let is_seek = delta.abs() > MAX_NATURAL_DELTA_SEC;
    if is_seek || seconds_since_last_timeline_push >= PERIODIC_REFRESH_SEC {
        return UpdateKind::Timeline;
    }
    UpdateKind::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{OutputMode, RepeatMode, Track, TrackFormat};

    fn state(track_id: Option<&str>, status: PlaybackStatus, position_sec: f64) -> PlayerState {
        PlayerState {
            status,
            track: track_id.map(|id| Track {
                id: id.to_string(),
                title: "T".into(),
                artist: "A".into(),
                artist_id: None,
                album: None,
                album_id: None,
                album_artist: None,
                track_number: None,
                disc_number: None,
                year: None,
                genre: None,
                duration: 200.0,
                format: TrackFormat { suffix: "flac".into(), bit_rate: None, bit_depth: Some(16), sampling_rate: 44100, channel_count: 2 },
                cover_art: None,
                starred: None,
                rating: None,
                size: None,
                replay_gain: None,
                path: None,
                date_added: None,
            }),
            position_sec,
            duration_sec: 200.0,
            output_mode: OutputMode::Exclusive,
            sample_rate: None,
            bit_depth: None,
            float_mix: false,
            format_label: None,
            volume: 1.0,
            bit_perfect: true,
            shuffle: false,
            repeat: RepeatMode::Off,
            queue_length: 1,
            error: None,
            volume_db: Some(0.0),
            replay_gain_db: None,
            loop_a: None,
            loop_b: None,
        }
    }

    #[test]
    fn new_track_is_metadata_update() {
        let s = state(Some("b"), PlaybackStatus::Playing, 0.0);
        assert_eq!(classify(Some("a"), PlaybackStatus::Playing, 50.0, &s, 0.0), UpdateKind::Metadata);
    }

    #[test]
    fn becoming_idle_from_a_track_is_metadata_update() {
        let s = state(None, PlaybackStatus::Stopped, 0.0);
        assert_eq!(classify(Some("a"), PlaybackStatus::Playing, 50.0, &s, 0.0), UpdateKind::Metadata);
    }

    #[test]
    fn staying_idle_is_a_no_op() {
        let s = state(None, PlaybackStatus::Stopped, 0.0);
        assert_eq!(classify(None, PlaybackStatus::Stopped, 0.0, &s, 999.0), UpdateKind::None);
    }

    #[test]
    fn status_edge_is_timeline_update() {
        let s = state(Some("a"), PlaybackStatus::Paused, 50.0);
        assert_eq!(classify(Some("a"), PlaybackStatus::Playing, 50.0, &s, 0.0), UpdateKind::Timeline);
    }

    #[test]
    fn ordinary_position_tick_is_a_no_op_before_the_periodic_refresh() {
        let s = state(Some("a"), PlaybackStatus::Playing, 50.25);
        assert_eq!(classify(Some("a"), PlaybackStatus::Playing, 50.0, &s, 1.0), UpdateKind::None);
    }

    #[test]
    fn periodic_refresh_fires_after_the_interval_even_with_no_seek() {
        let s = state(Some("a"), PlaybackStatus::Playing, 50.25);
        assert_eq!(classify(Some("a"), PlaybackStatus::Playing, 50.0, &s, 5.0), UpdateKind::Timeline);
    }

    #[test]
    fn a_seek_is_a_timeline_update_even_before_the_periodic_refresh() {
        let s = state(Some("a"), PlaybackStatus::Playing, 120.0);
        assert_eq!(classify(Some("a"), PlaybackStatus::Playing, 50.0, &s, 0.1), UpdateKind::Timeline);
    }
}
