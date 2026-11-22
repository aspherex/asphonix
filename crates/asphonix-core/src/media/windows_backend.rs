//! Windows SMTC backend: registers ASPHONIX's own window as a System Media
//! Transport Controls source (`ISystemMediaTransportControlsInterop::
//! GetForWindow`) and keeps it in sync with `PlayerState`.
//!
//! The window handle is passed in as a raw HWND; engine commands, player state and cover bytes come through
//! [`MediaHost`]. `GetForWindow` and the event registration run on the thread that owns the window:
//! [`attach`] is called there by the UI (after a 1.5 s delay). The `SystemMediaTransportControls` object is WinRT-agile
//! (`Send + Sync` in the `windows` crate), so later updates and the OS callbacks need no thread hops.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use windows::core::{Ref, HSTRING};
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs, SystemMediaTransportControls, SystemMediaTransportControlsButton,
    SystemMediaTransportControlsButtonPressedEventArgs, SystemMediaTransportControlsDisplayUpdater, SystemMediaTransportControlsTimelineProperties,
};
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream, RandomAccessStreamReference};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::{ISystemMediaTransportControlsInterop, RoInitialize, RO_INIT_SINGLETHREADED};

use crate::audio::EngineCommand;
use crate::ipc::{PlaybackStatus, PlayerState};

use super::{classify, MediaHost, UpdateKind};

/// Cover art is requested at this display size for the thumbnail (one of the cache's bucket sizes).
const THUMBNAIL_SIZE: u32 = 320;

struct Smtc {
    controls: SystemMediaTransportControls,
    display_updater: SystemMediaTransportControlsDisplayUpdater,
    timeline: SystemMediaTransportControlsTimelineProperties,
}

// SAFETY: every field is a WinRT-agile COM object (the `windows` crate's codegen marks
// `SystemMediaTransportControls` et al. `Send + Sync` for exactly this reason), so moving the registered
// handle between threads and sharing it with the update task is sound.
unsafe impl Send for Smtc {}
// SAFETY: see the Send impl above.
unsafe impl Sync for Smtc {}

/// Runs on the window's thread (see [`super::attach`]); the update loop reads the engine's *current* state on
/// subscribing, so a restored session still appears.
pub fn attach(host: Arc<dyn MediaHost>, hwnd: isize, generation: Arc<AtomicU64>, r#gen: u64) {
    // The UI thread normally has its apartment already (S_FALSE / RPC_E_CHANGED_MODE are fine): make sure WinRT
    // activation works here without changing an existing apartment.
    // SAFETY: RoInitialize has no memory-safety preconditions; S_FALSE / RPC_E_CHANGED_MODE are tolerated by design.
    let _ = unsafe { RoInitialize(RO_INIT_SINGLETHREADED) };
    let smtc = match register(&host, hwnd) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "SMTC registration failed");
            return;
        }
    };
    std::thread::Builder::new()
        .name("asphonix-smtc".into())
        .spawn(move || crate::rt::block_on(run_update_loop(host, smtc, generation, r#gen)))
        .ok();
}

/// OS button -> engine command (only the buttons ASPHONIX implements).
pub(crate) fn button_command(button: SystemMediaTransportControlsButton) -> Option<EngineCommand> {
    if button == SystemMediaTransportControlsButton::Play {
        Some(EngineCommand::Play)
    } else if button == SystemMediaTransportControlsButton::Pause {
        Some(EngineCommand::Pause)
    } else if button == SystemMediaTransportControlsButton::Next {
        Some(EngineCommand::Next)
    } else if button == SystemMediaTransportControlsButton::Previous {
        Some(EngineCommand::Previous)
    } else {
        None
    }
}

/// Obtains the SMTC for the window, enables the real transport controls, and wires button/seek events
/// straight to the engine.
fn register(host: &Arc<dyn MediaHost>, hwnd: isize) -> Result<Smtc, String> {
    let interop: ISystemMediaTransportControlsInterop =
        windows::core::factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>().map_err(|e| e.to_string())?;
    // SAFETY: `hwnd` is the live top-level window handle passed in by the UI for the window's lifetime.
    let controls: SystemMediaTransportControls = unsafe { interop.GetForWindow(HWND(hwnd as *mut core::ffi::c_void)) }.map_err(|e| e.to_string())?;

    controls.SetIsEnabled(true).map_err(|e| e.to_string())?;
    // Only buttons ASPHONIX actually implements - no fake affordances.
    let _ = controls.SetIsPlayEnabled(true);
    let _ = controls.SetIsPauseEnabled(true);
    let _ = controls.SetIsNextEnabled(true);
    let _ = controls.SetIsPreviousEnabled(true);
    let _ = controls.SetIsStopEnabled(false);
    let _ = controls.SetIsFastForwardEnabled(false);
    let _ = controls.SetIsRewindEnabled(false);
    let _ = controls.SetIsRecordEnabled(false);

    let display_updater = controls.DisplayUpdater().map_err(|e| e.to_string())?;
    let _ = display_updater.SetType(MediaPlaybackType::Music);
    let timeline = SystemMediaTransportControlsTimelineProperties::new().map_err(|e| e.to_string())?;

    let h = host.clone();
    let button_handler = TypedEventHandler::new(move |_sender: Ref<SystemMediaTransportControls>, args: Ref<SystemMediaTransportControlsButtonPressedEventArgs>| {
        let Some(args) = args.as_ref() else { return Ok(()) };
        if let Some(cmd) = button_command(args.Button()?) {
            h.send(cmd);
        }
        Ok(())
    });
    controls.ButtonPressed(&button_handler).map_err(|e| e.to_string())?;

    let h = host.clone();
    let seek_handler = TypedEventHandler::new(move |_sender: Ref<SystemMediaTransportControls>, args: Ref<PlaybackPositionChangeRequestedEventArgs>| {
        let Some(args) = args.as_ref() else { return Ok(()) };
        let requested = args.RequestedPlaybackPosition()?;
        h.send(EngineCommand::Seek(requested.Duration as f64 / 10_000_000.0)); // 100 ns ticks
        Ok(())
    });
    controls.PlaybackPositionChangeRequested(&seek_handler).map_err(|e| e.to_string())?;

    Ok(Smtc { controls, display_updater, timeline })
}

/// One `PlayerState` watch subscription, pushing updates to SMTC only when [`classify`] says something changed.
async fn run_update_loop(host: Arc<dyn MediaHost>, smtc: Smtc, generation: Arc<AtomicU64>, r#gen: u64) {
    let smtc = Arc::new(smtc);
    let Some(mut rx) = host.subscribe() else { return };

    let mut prev_track_id: Option<String> = None;
    let mut prev_status = PlaybackStatus::Stopped;
    let mut prev_position_sec = 0.0_f64;
    let mut last_timeline_push = Instant::now();
    let art_fetch = ArtFetchState::new();

    loop {
        let state = rx.borrow_and_update().clone();
        let kind = classify(prev_track_id.as_deref(), prev_status, prev_position_sec, &state, last_timeline_push.elapsed().as_secs_f64());

        match kind {
            UpdateKind::Metadata => {
                apply_metadata(&host, &smtc, &state, &art_fetch);
                last_timeline_push = Instant::now();
            }
            UpdateKind::Timeline => {
                apply_timeline(&smtc, &state);
                last_timeline_push = Instant::now();
            }
            UpdateKind::None => {}
        }

        prev_track_id = state.track.as_ref().map(|t| t.id.clone());
        prev_status = state.status;
        prev_position_sec = state.position_sec;

        if rx.changed().await.is_err() {
            break; // engine handle dropped: app shutting down.
        }
        if generation.load(Ordering::SeqCst) != r#gen {
            break; // a newer window registered; this one is gone.
        }
    }
}

pub(crate) fn media_playback_status(status: PlaybackStatus, has_track: bool) -> MediaPlaybackStatus {
    if !has_track {
        return MediaPlaybackStatus::Closed;
    }
    match status {
        PlaybackStatus::Playing => MediaPlaybackStatus::Playing,
        PlaybackStatus::Paused => MediaPlaybackStatus::Paused,
        PlaybackStatus::Loading => MediaPlaybackStatus::Changing,
        PlaybackStatus::Stopped => MediaPlaybackStatus::Stopped,
    }
}

fn ticks(sec: f64) -> TimeSpan {
    TimeSpan { Duration: (sec.max(0.0) * 10_000_000.0) as i64 }
}

fn apply_timeline(smtc: &Smtc, state: &PlayerState) {
    let _ = smtc.controls.SetPlaybackStatus(media_playback_status(state.status, state.track.is_some()));
    let _ = smtc.timeline.SetPosition(ticks(state.position_sec));
    let _ = smtc.controls.UpdateTimelineProperties(&smtc.timeline);
}

fn apply_metadata(host: &Arc<dyn MediaHost>, smtc: &Arc<Smtc>, state: &PlayerState, art_fetch: &ArtFetchState) {
    let Some(track) = &state.track else {
        let _ = smtc.display_updater.ClearAll();
        let _ = smtc.controls.SetPlaybackStatus(MediaPlaybackStatus::Closed);
        let _ = smtc.display_updater.Update();
        return;
    };

    if let Ok(music) = smtc.display_updater.MusicProperties() {
        let _ = music.SetTitle(&HSTRING::from(track.title.as_str()));
        let _ = music.SetArtist(&HSTRING::from(track.artist.as_str()));
        // Always set (empty when unknown) so the previous track's album never lingers.
        let _ = music.SetAlbumTitle(&HSTRING::from(track.album.as_deref().unwrap_or("")));
    }

    let zero = TimeSpan::default();
    let end = ticks(state.duration_sec);
    let _ = smtc.timeline.SetStartTime(zero);
    let _ = smtc.timeline.SetMinSeekTime(zero);
    let _ = smtc.timeline.SetEndTime(end);
    let _ = smtc.timeline.SetMaxSeekTime(end);
    let _ = smtc.timeline.SetPosition(ticks(state.position_sec));
    let _ = smtc.controls.UpdateTimelineProperties(&smtc.timeline);
    let _ = smtc.controls.SetPlaybackStatus(media_playback_status(state.status, true));
    let _ = smtc.display_updater.Update();

    match track.cover_art.clone() {
        Some(cover_art_id) => request_thumbnail(host.clone(), smtc.display_updater.clone(), cover_art_id, art_fetch.clone()),
        None => {
            // No art: drop the previous track's thumbnail (and any fetch still queued for it).
            *art_fetch.pending.lock().unwrap_or_else(|p| p.into_inner()) = None;
            let _ = smtc.display_updater.SetThumbnail(None);
            let _ = smtc.display_updater.Update();
        }
    }
}

struct Display(SystemMediaTransportControlsDisplayUpdater);
// SAFETY: the display updater is a WinRT-agile COM object (free-threaded marshaling), safe to move and share across threads.
unsafe impl Send for Display {}
// SAFETY: see the Send impl above.
unsafe impl Sync for Display {}

/// Latest-wins queue for one in-flight thumbnail fetch: a fast track skip must never leave SMTC showing a
/// *stale* thumbnail. `pending` always holds the most recently requested cover-art id; `running` is `true`
/// while a task is draining it.
#[derive(Clone)]
struct ArtFetchState {
    pending: Arc<std::sync::Mutex<Option<String>>>,
    running: Arc<AtomicBool>,
}

impl ArtFetchState {
    fn new() -> Self {
        Self { pending: Arc::new(std::sync::Mutex::new(None)), running: Arc::new(AtomicBool::new(false)) }
    }
}

fn request_thumbnail(host: Arc<dyn MediaHost>, display: SystemMediaTransportControlsDisplayUpdater, cover_art_id: String, art_fetch: ArtFetchState) {
    *art_fetch.pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(cover_art_id);
    if art_fetch.running.swap(true, Ordering::AcqRel) {
        return; // a task is already draining `pending`; it will pick up this id.
    }
    let display = Display(display);
    crate::rt::spawn(async move {
        loop {
            let Some(cover_art_id) = art_fetch.pending.lock().unwrap_or_else(|p| p.into_inner()).take() else {
                break;
            };
            fetch_and_set_thumbnail(&host, &display, cover_art_id).await;
        }
        art_fetch.running.store(false, Ordering::Release);
    });
}

async fn fetch_and_set_thumbnail(host: &Arc<dyn MediaHost>, display: &Display, cover_art_id: String) {
    let Some(bytes) = host.cover(cover_art_id, THUMBNAIL_SIZE).await else { return };
    // An in-memory WinRT stream of the encoded bytes: a `file://` URI via `CreateFromUri` is silently never
    // shown for an unpackaged app.
    let thumb = match thumbnail_stream(&bytes).await {
        Ok(t) => t,
        Err(e) => {
            tracing::debug!(error = %e, "SMTC thumbnail stream failed");
            return;
        }
    };
    let _ = display.0.SetThumbnail(&thumb);
    let _ = display.0.Update();
}

async fn thumbnail_stream(bytes: &[u8]) -> windows::core::Result<RandomAccessStreamReference> {
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(bytes)?;
    writer.StoreAsync()?.await?;
    writer.FlushAsync()?.await?;
    writer.DetachStream()?;
    stream.Seek(0)?;
    RandomAccessStreamReference::CreateFromStream(&stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_buttons_map_to_engine_commands() {
        assert!(matches!(button_command(SystemMediaTransportControlsButton::Play), Some(EngineCommand::Play)));
        assert!(matches!(button_command(SystemMediaTransportControlsButton::Pause), Some(EngineCommand::Pause)));
        assert!(matches!(button_command(SystemMediaTransportControlsButton::Next), Some(EngineCommand::Next)));
        assert!(matches!(button_command(SystemMediaTransportControlsButton::Previous), Some(EngineCommand::Previous)));
        // Buttons ASPHONIX does not implement are ignored.
        assert!(button_command(SystemMediaTransportControlsButton::Stop).is_none());
        assert!(button_command(SystemMediaTransportControlsButton::FastForward).is_none());
    }

    #[test]
    fn playback_status_follows_player_status() {
        assert_eq!(media_playback_status(PlaybackStatus::Playing, true), MediaPlaybackStatus::Playing);
        assert_eq!(media_playback_status(PlaybackStatus::Paused, true), MediaPlaybackStatus::Paused);
        assert_eq!(media_playback_status(PlaybackStatus::Loading, true), MediaPlaybackStatus::Changing);
        assert_eq!(media_playback_status(PlaybackStatus::Stopped, true), MediaPlaybackStatus::Stopped);
        assert_eq!(media_playback_status(PlaybackStatus::Playing, false), MediaPlaybackStatus::Closed, "no track = closed");
    }

    #[test]
    fn timeline_ticks_are_100ns_and_never_negative() {
        assert_eq!(ticks(1.5).Duration, 15_000_000);
        assert_eq!(ticks(-3.0).Duration, 0);
    }
}
