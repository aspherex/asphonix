//! Shared app state: services (backend + art cache) and the player model every view
//! observes. Backend calls always run on GPUI's background executor, never in render.
use crate::art::ArtHandle;
use crate::theme::Theme;
use gpui::{App, Context, EventEmitter, Global};
use sonix_api::{Backend, PlaybackStatus, PlayerState, QueueSourceInfo, SourceKind, SpectrumFrame};
use std::sync::Arc;

pub struct Services {
    pub backend: Arc<dyn Backend>,
    pub art: ArtHandle,
}
impl Global for Services {}
impl Global for Theme {}

pub fn backend(cx: &App) -> Arc<dyn Backend> {
    cx.global::<Services>().backend.clone()
}

pub fn theme(cx: &App) -> Theme {
    *cx.global::<Theme>()
}

/// Fire-and-forget backend command off the UI thread. Results arrive as events.
pub fn call(cx: &App, f: impl FnOnce(&dyn Backend) + Send + 'static) {
    let b = backend(cx);
    cx.background_executor()
        .spawn(async move {
            f(&*b);
        })
        .detach();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerEvent {
    /// Track id or playback status changed (not position ticks).
    TrackOrStatus,
}

/// Latest player state + spectrum frame. `notify()` (observers re-render) fires for every
/// state event; the spectrum frame is stored without notifying, Now Playing reads it per frame.
pub struct PlayerModel {
    pub state: PlayerState,
    pub spectrum: Option<SpectrumFrame>,
    /// Counts frames: consumers that keep their own smoothing detect "a new frame arrived" by comparing it.
    pub spectrum_seq: u64,
    /// The queue's source identity (kind/id/label; `None` = a static queue), refreshed whenever the track, the status or
    /// the queue changes. Drives the "this source is playing" state of every play button (`PlayTarget`).
    pub source: Option<QueueSourceInfo>,
    /// A static queue (no source) whose rows all belong to one album: `(album id, row count)`.
    pub queue_album: Option<(String, u32)>,
}

/// A thing with a "play it" button: the active state of that button asks the player whether it is the one loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlayTarget {
    /// Albums play as a static queue (`playQueue`, no source identity): the album is "playing" when the static queue
    /// is exactly that album (every row from it, `tracks` of them) - not when one of its tracks plays from a search.
    Album { id: String, tracks: u32 },
    Playlist(String),
    Smart(String),
    Artist(String),
    /// Genre sources carry the genre name as their label (no id).
    Genre(String),
    Liked,
}

/// How a play button looks and acts for its target: `Idle` = accent fill, play icon, click starts it;
/// `Active { playing }` = outline, pause icon while playing (play icon while paused), click pauses / resumes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayState {
    Idle,
    Active { playing: bool },
}

impl PlayState {
    pub fn active(self) -> bool {
        matches!(self, PlayState::Active { .. })
    }
    pub fn icon(self) -> &'static str {
        if self == (PlayState::Active { playing: true }) { "pause" } else { "play" }
    }
}

impl EventEmitter<PlayerEvent> for PlayerModel {}

impl Default for PlayerModel {
    fn default() -> Self {
        Self { state: PlayerState::idle(), spectrum: None, spectrum_seq: 0, source: None, queue_album: None }
    }
}

impl PlayerModel {
    pub fn current_id(&self) -> Option<&str> {
        self.state.track.as_ref().map(|t| t.id.as_str())
    }
    pub fn playing(&self) -> bool {
        self.state.status == PlaybackStatus::Playing
    }
    /// Is `t` the source the player has loaded (and not stopped)? Loading counts as playing (the track is on its way).
    pub fn play_state(&self, t: &PlayTarget) -> PlayState {
        let loaded = match self.state.status {
            PlaybackStatus::Stopped => return PlayState::Idle,
            PlaybackStatus::Playing | PlaybackStatus::Loading => true,
            PlaybackStatus::Paused => false,
        };
        let src = self.source.as_ref();
        let hit = match t {
            PlayTarget::Album { id, tracks } => match src {
                None => self.queue_album.as_ref().is_some_and(|(a, n)| a == id && n == tracks),
                Some(s) => s.kind == SourceKind::Album && s.id.as_deref() == Some(id.as_str()),
            },
            PlayTarget::Playlist(id) => src.is_some_and(|s| s.kind == SourceKind::Playlist && s.id.as_deref() == Some(id.as_str())),
            PlayTarget::Smart(id) => src.is_some_and(|s| s.kind == SourceKind::Smart && s.id.as_deref() == Some(id.as_str())),
            PlayTarget::Artist(id) => src.is_some_and(|s| s.kind == SourceKind::Artist && s.id.as_deref() == Some(id.as_str())),
            PlayTarget::Genre(name) => src.is_some_and(|s| s.kind == SourceKind::Genre && s.label.to_lowercase() == name.to_lowercase()),
            PlayTarget::Liked => src.is_some_and(|s| s.kind == SourceKind::Liked),
        };
        if hit { PlayState::Active { playing: loaded } } else { PlayState::Idle }
    }
    /// Store the freshly fetched queue source; emits `TrackOrStatus` when it changed so play buttons repaint.
    pub fn set_source(&mut self, s: Option<QueueSourceInfo>, album: Option<(String, u32)>, notify: bool, cx: &mut Context<Self>) {
        let album_same = self.queue_album == album;
        self.queue_album = album;
        // `remaining` moves with every refill; only the identity matters for the buttons
        let same = match (&self.source, &s) {
            (Some(a), Some(b)) => a.kind == b.kind && a.id == b.id && a.label == b.label,
            (None, None) => true,
            _ => false,
        };
        self.source = s;
        if notify && (!same || !album_same) {
            cx.emit(PlayerEvent::TrackOrStatus);
            cx.notify();
        }
    }
    /// Apply a state event. `notify` is false while the window is minimized: the state is
    /// still kept current, but nothing is re-rendered.
    pub fn apply_state(&mut self, s: PlayerState, notify: bool, cx: &mut Context<Self>) {
        let changed = self.current_id() != s.track.as_ref().map(|t| t.id.as_str()) || self.state.status != s.status;
        let star_changed = self.state.track.as_ref().map(|t| t.starred.is_some()) != s.track.as_ref().map(|t| t.starred.is_some());
        self.state = s;
        if notify {
            if changed {
                cx.emit(PlayerEvent::TrackOrStatus);
            }
            if changed || star_changed || self.state.status != PlaybackStatus::Stopped || true {
                cx.notify();
            }
        }
    }
    pub fn apply_spectrum(&mut self, f: SpectrumFrame) {
        self.spectrum = Some(f);
        self.spectrum_seq += 1;
    }
}

/// Gate for a cached view that shows the player position (`crate::part`): the player notifies 4x a second, but a
/// readout only changes with the whole second (`format_duration` floors) and a progress bar only with its pixel.
/// `changed` is true when anything else in the state changed, the second changed, or the progress moved by one of
/// `steps` positions (the bar width in device px; 0 = no bar).
#[derive(Default)]
pub struct PosGate(Option<(PlayerState, u64, i64)>);

impl PosGate {
    pub fn changed(&mut self, s: &PlayerState, steps: f32) -> bool {
        let mut rest = s.clone();
        rest.position_sec = 0.0;
        let sec = if s.position_sec.is_finite() && s.position_sec >= 0.0 { s.position_sec.floor() as u64 } else { u64::MAX };
        let step = if steps > 0.0 && s.duration_sec > 0.0 { ((s.position_sec / s.duration_sec).clamp(0.0, 1.0) * steps as f64).round() as i64 } else { 0 };
        let key = (rest, sec, step);
        if self.0.as_ref() == Some(&key) {
            return false;
        }
        self.0 = Some(key);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonix_api::SourceMode;

    fn src(kind: SourceKind, id: Option<&str>, label: &str) -> Option<QueueSourceInfo> {
        Some(QueueSourceInfo { kind, label: label.into(), id: id.map(str::to_string), mode: SourceMode::InOrder, remaining: 0, arc: None, flow_coverage: None })
    }

    #[test]
    fn play_buttons_are_active_only_for_the_loaded_source() {
        let mut m = PlayerModel { source: src(SourceKind::Playlist, Some("pl1"), "Mix"), ..Default::default() };
        let al = |id: &str, n: u32| PlayTarget::Album { id: id.into(), tracks: n };
        // stopped: nothing is active, whatever the source says
        assert_eq!(m.play_state(&PlayTarget::Playlist("pl1".into())), PlayState::Idle);
        m.state.status = PlaybackStatus::Playing;
        assert_eq!(m.play_state(&PlayTarget::Playlist("pl1".into())), PlayState::Active { playing: true });
        assert_eq!(m.play_state(&PlayTarget::Playlist("pl2".into())), PlayState::Idle);
        assert_eq!(m.play_state(&PlayTarget::Smart("pl1".into())), PlayState::Idle);
        assert_eq!(m.play_state(&PlayTarget::Liked), PlayState::Idle);
        // paused: still the active source (outline, play icon), click resumes
        m.state.status = PlaybackStatus::Paused;
        let st = m.play_state(&PlayTarget::Playlist("pl1".into()));
        assert_eq!((st, st.icon()), (PlayState::Active { playing: false }, "play"));
        m.state.status = PlaybackStatus::Playing;
        assert_eq!(m.play_state(&PlayTarget::Playlist("pl1".into())).icon(), "pause");
        // genre sources are matched by name, liked / all tracks by kind
        m.source = src(SourceKind::Genre, None, "Rock");
        assert!(m.play_state(&PlayTarget::Genre("Rock".into())).active() && !m.play_state(&PlayTarget::Genre("Jazz".into())).active());
        m.source = src(SourceKind::Liked, None, "Liked Songs");
        assert!(m.play_state(&PlayTarget::Liked).active());
        // a static queue has no source: no source target is active
        m.source = None;
        // an album is playing only when the static queue IS that album: one search result of it does not count
        m.queue_album = Some(("al1".into(), 1));
        assert_eq!(m.play_state(&al("al1", 12)), PlayState::Idle, "one track of a 12 track album from a search");
        m.queue_album = Some(("al1".into(), 12));
        assert_eq!(m.play_state(&al("al1", 12)), PlayState::Active { playing: true });
        assert_eq!(m.play_state(&al("al2", 12)), PlayState::Idle);
        m.queue_album = None;
        assert_eq!(m.play_state(&al("al1", 12)), PlayState::Idle);
        assert!(!m.play_state(&PlayTarget::Liked).active() && !m.play_state(&PlayTarget::Artist("a".into())).active());
    }
}
