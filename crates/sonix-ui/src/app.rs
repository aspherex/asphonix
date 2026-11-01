//! Shared app state: services (backend + art cache) and the player model every view
//! observes. Backend calls always run on GPUI's background executor, never in render.
use crate::art::ArtHandle;
use crate::theme::Theme;
use gpui::{App, Context, EventEmitter, Global};
use sonix_api::{Backend, PlaybackStatus, PlayerState, SpectrumFrame};
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
    pub spectrum_at: std::time::Instant,
}

impl EventEmitter<PlayerEvent> for PlayerModel {}

impl Default for PlayerModel {
    fn default() -> Self {
        Self { state: PlayerState::idle(), spectrum: None, spectrum_at: std::time::Instant::now() }
    }
}

impl PlayerModel {
    pub fn current_id(&self) -> Option<&str> {
        self.state.track.as_ref().map(|t| t.id.as_str())
    }
    pub fn playing(&self) -> bool {
        self.state.status == PlaybackStatus::Playing
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
        self.spectrum_at = std::time::Instant::now();
    }
}
