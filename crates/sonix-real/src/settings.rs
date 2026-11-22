//! Settings, session continuity and the art-cache readout on `RealBackend`.
//!
//! * `settings_get`/`settings_set`: `secrets::Settings` persisted as `settings.json` in `%APPDATA%\dev.sonix.native`;
//!   every change is applied live the way the reference's `settings_set_*` commands do (`Prefs`, engine commands,
//!   the shared scrobbling flag), then the whole file is rewritten.
//! * session resume: `SessionStore` (written by the engine) + a delayed `RestoreSession` at startup (local only).
//! * `session_check_server_queue` / `session_resume_from_server`: `getPlayQueue` only (read-only), the check is
//!   gated by `sync_play_queue`. The server `savePlayQueue` push lives in `SessionStore` and is gated by the same pref.
use crate::reads::conv;
use crate::{err, RealBackend};
use sonix_api::*;
use sonix_core::audio::EngineCommand;
use sonix_core::secrets;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Applies `change` to `s` and returns the live side effects to run (pure: unit-testable without an engine).
pub(crate) fn apply_change(s: &mut secrets::Settings, change: &SettingChange) -> ApiResult<()> {
    match change {
        SettingChange::Scrobbling(v) => s.scrobbling_enabled = *v,
        SettingChange::AutoCheckUpdates(v) => s.auto_check_updates = *v,
        SettingChange::SpreadArtists(v) => s.spread_artists_in_shuffle = *v,
        SettingChange::QueueWindowSize(v) => {
            s.queue_window_size = (*v).clamp(sonix_core::audio::queue::MIN_WINDOW_SIZE as u32, sonix_core::audio::queue::MAX_WINDOW_SIZE as u32)
        }
        SettingChange::Autoplay(v) => s.autoplay_enabled = *v,
        SettingChange::SyncPlayQueue(v) => s.sync_play_queue = *v,
        SettingChange::ReplayGain(m) => s.replay_gain_mode = conv(*m)?,
        SettingChange::PlayThreshold(v) => s.play_threshold_sec = secrets::clamp_play_threshold_sec(*v),
        SettingChange::TrackOverlay(v) => s.track_overlay = *v,
        SettingChange::TrackOverlayCorner(c) => s.track_overlay_corner = conv(*c)?,
        SettingChange::FreeMemoryWhenMinimized(v) => s.free_memory_when_minimized = *v,
        SettingChange::McpEnabled(v) => s.mcp_enabled = *v,
        SettingChange::McpAllowControl(v) => s.mcp_allow_control = *v,
    }
    Ok(())
}

pub(crate) fn wire(s: &secrets::Settings) -> ApiResult<Settings> {
    Ok(Settings {
        scrobbling_enabled: s.scrobbling_enabled,
        auto_check_updates: s.auto_check_updates,
        spread_artists_in_shuffle: s.spread_artists_in_shuffle,
        queue_window_size: s.queue_window_size,
        autoplay_enabled: s.autoplay_enabled,
        sync_play_queue: s.sync_play_queue,
        play_threshold_sec: s.play_threshold_sec,
        free_memory_when_minimized: s.free_memory_when_minimized,
        replay_gain_mode: conv(s.replay_gain_mode)?,
        track_overlay: s.track_overlay,
        track_overlay_corner: conv(s.track_overlay_corner)?,
        mcp_enabled: s.mcp_enabled,
        mcp_allow_control: s.mcp_allow_control,
    })
}

impl SettingsApi for RealBackend {
    fn settings_get(&self) -> ApiResult<Settings> {
        wire(&self.0.settings.lock().unwrap_or_else(|p| p.into_inner()))
    }

    fn settings_set(&self, change: SettingChange) -> ApiResult<()> {
        // Reference `settings_set_mcp_enabled`: start/stop first (no settings lock held: it blocks on the runtime);
        // a failed start returns the error and persists nothing.
        if let SettingChange::McpEnabled(on) = change {
            self.set_mcp_enabled(on)?;
        }
        let mut s = self.0.settings.lock().unwrap_or_else(|p| p.into_inner());
        apply_change(&mut s, &change)?;
        // Live effects, like the reference's settings_set_* commands.
        match change {
            SettingChange::Scrobbling(_) => self.0.scrobbling.store(s.scrobbling_enabled, Ordering::Relaxed),
            SettingChange::SpreadArtists(_) => self.0.engine.send(EngineCommand::SetSpreadArtists(s.spread_artists_in_shuffle)),
            SettingChange::QueueWindowSize(_) => self.0.engine.send(EngineCommand::SetQueueWindowSize(s.queue_window_size)),
            SettingChange::Autoplay(_) => self.0.prefs.set_autoplay(s.autoplay_enabled),
            SettingChange::SyncPlayQueue(_) => self.0.prefs.set_sync_play_queue(s.sync_play_queue),
            SettingChange::ReplayGain(_) => {
                self.0.prefs.set_replay_gain(s.replay_gain_mode);
                if self.0.engine.is_started() {
                    self.0.engine.send(EngineCommand::RefreshGain);
                }
            }
            SettingChange::PlayThreshold(_) => self.0.prefs.set_play_threshold_sec(s.play_threshold_sec),
            SettingChange::AutoCheckUpdates(_) => self.set_auto_check_updates(s.auto_check_updates),
            SettingChange::McpAllowControl(_) => {
                self.set_mcp_allow_control(s.mcp_allow_control)?;
            }
            // Applied above, before the lock.
            SettingChange::McpEnabled(_) => {}
            // Stored only: the overlay and background-mode services are not wired in the native app yet.
            SettingChange::TrackOverlay(_) | SettingChange::TrackOverlayCorner(_) | SettingChange::FreeMemoryWhenMinimized(_) => {}
        }
        secrets::save_settings(&self.0.dir, &s).map_err(err)
    }

    /// The live bridge status (`services.rs`).
    fn mcp_status(&self) -> ApiResult<McpStatus> {
        self.mcp_status_impl()
    }

    /// Like the reference ("cached / total covers - bytes"): `total` = distinct album and artist covers in the library
    /// mirror, `cached` = how many of those have at least one size cached (the native cache stores one file per
    /// size bucket, so files are not counted), `bytes` = size of the whole art dir. `cached <= total` always.
    fn art_cache_status(&self) -> ApiResult<ArtCacheStatus> {
        let mut keys = std::collections::HashSet::new();
        let mut bytes = 0u64;
        if let Ok(rd) = std::fs::read_dir(self.art_dir()) {
            for e in rd.flatten() {
                if let Ok(m) = e.metadata()
                    && m.is_file()
                {
                    bytes += m.len();
                    // `{key}_{size}.img`
                    if let Some(stem) = e.file_name().to_string_lossy().strip_suffix(".img")
                        && let Some((key, size)) = stem.rsplit_once('_')
                        && size.chars().all(|c| c.is_ascii_digit())
                    {
                        keys.insert(key.to_string());
                    }
                }
            }
        }
        let covers: Vec<String> = self
            .0
            .db
            .read(|c| {
                let mut st = c.prepare("SELECT cover_art FROM albums WHERE cover_art IS NOT NULL UNION SELECT cover_art FROM artists WHERE cover_art IS NOT NULL")?;
                st.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()
            })
            .unwrap_or_default();
        let wanted: std::collections::HashSet<String> = covers.iter().map(|c| art_key(c)).collect();
        let cached = wanted.iter().filter(|k| keys.contains(*k)).count() as u64;
        Ok(ArtCacheStatus { cached, total: wanted.len() as u64, bytes })
    }
}

/// Cache file key of a cover id (same sanitising as `ArtApi::art_get` in `reads.rs`).
fn art_key(cover_art: &str) -> String {
    cover_art.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

impl RealBackend {
    /// Reads the saved session shortly after startup (off the caller's thread) and hands it to the engine, which
    /// rebuilds the queue paused at the saved position from the local library only. No network.
    pub(crate) fn spawn_session_restore(&self) {
        let b = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            b.offer_saved_session();
        });
    }

    /// Reads the session files and hands them to the engine as a startup (non-forced) restore. The engine ignores
    /// it once anything is queued, loading or playing, so offering it twice is harmless.
    fn offer_saved_session(&self) {
        if let Some((state, pools)) = sonix_core::audio::session::load(&self.0.dir) {
            self.0.engine.send(EngineCommand::RestoreSession { state: Box::new(state), pools, force: false });
        }
    }

    /// Native deviation from the reference (which restores only once, at 300 ms): the restore resolves ids against
    /// the local library only, so a library that is behind the server (the session was saved by another client, or
    /// the app was not opened for a while) skips the whole session ("none of the saved tracks is in the local
    /// library") although the startup sync adds those tracks seconds later. The first successful sync of the run
    /// therefore offers the saved session once more, re-read from disk. The engine's own guard (nothing queued,
    /// loading or playing) keeps it from replacing anything the user started meanwhile; when the 300 ms restore
    /// already worked, the retry is a no-op for the same reason.
    pub(crate) fn after_first_sync(&self) {
        if self.0.restore_retry.swap(false, Ordering::SeqCst) {
            self.offer_saved_session();
        }
    }

    /// Exit hook: final session write (+ `savePlayQueue` only when "Sync play queue with server" is on), bounded to 4 s.
    pub fn persist_now(&self) {
        self.0.engine.persist_now(Duration::from_secs(4));
    }

    pub(crate) fn session_check_impl(&self) -> ApiResult<Option<ServerQueueOffer>> {
        let sync = self.0.prefs.sync_play_queue();
        let client = self.0.client.read().unwrap_or_else(|p| p.into_inner()).clone();
        let offer = self
            .0
            .rt
            .block_on(sonix_core::continuity::check_server_queue(sync, client, &self.0.db, &self.0.engine, Some(&*self.0.store)))
            .map_err(err)?;
        offer.map(conv).transpose()
    }

    pub(crate) fn session_resume_impl(&self) -> ApiResult<()> {
        let client = self.0.client.read().unwrap_or_else(|p| p.into_inner()).clone();
        self.0.rt.block_on(sonix_core::continuity::resume_from_server(client, &self.0.engine)).map_err(err)
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
