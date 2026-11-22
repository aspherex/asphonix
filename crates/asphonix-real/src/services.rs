//! OS integration and background services of the real backend: listen-event flush on
//! logout/exit, Windows media controls (SMTC), the MCP bridge and the updater.
//! Inherent methods only; the `Backend` trait forwarders are one-liners in the API impls.
use crate::{err, reads::conv, RealBackend};
use asphonix_api::*;
use asphonix_core::media::{BoxFut, MediaHost};
use asphonix_core::updates::Updater;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How long logout / account switch / exit wait for the listen writer.
pub(crate) const LISTEN_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
/// Final session write bound at exit.
pub(crate) const EXIT_PERSIST_TIMEOUT: Duration = Duration::from_secs(4);
/// Delay before a saved-on MCP bridge starts listening.
const MCP_START_DELAY: Duration = Duration::from_secs(3);

/// Per-service state held by `Inner`.
pub(crate) struct Services {
    pub updater: Arc<Updater>,
    /// `Settings.autoCheckUpdates` (OFF by default; set from the persisted settings at startup and on change).
    pub auto_check_updates: Arc<AtomicBool>,
    checker_started: AtomicBool,
    /// HWND currently registered with SMTC (0 = none) and its registration generation.
    media_hwnd: std::sync::atomic::AtomicIsize,
    media_generation: Arc<std::sync::atomic::AtomicU64>,
    mcp: asphonix_core::mcp_bridge::McpBridge,
    /// `Settings.mcpEnabled` / `mcpAllowControl`: both OFF by default.
    mcp_enabled: AtomicBool,
    mcp_allow_control: AtomicBool,
}

impl Default for Services {
    fn default() -> Self {
        Self { updater: Arc::new(Updater::built_in()), auto_check_updates: Arc::new(AtomicBool::new(false)), checker_started: AtomicBool::new(false), media_hwnd: Default::default(), media_generation: Default::default(), mcp: Default::default(), mcp_enabled: AtomicBool::new(false), mcp_allow_control: AtomicBool::new(false) }
    }
}

impl RealBackend {
    /// Stop playback and wait (bounded) until the listen session `Stop` closed, and any still queued,
    /// are in the DB. Must run BEFORE `Db::clear_all` on logout / account switch, so the closing listen row belongs to the old account.
    pub(crate) fn flush_listen(&self) {
        self.0.rt.block_on(self.0.engine.stop_and_flush_listen(LISTEN_FLUSH_TIMEOUT));
    }

    /// App exit: final session write (also records the `quit` listen row), then service teardown.
    pub fn shutdown(&self) {
        self.0.engine.persist_now(EXIT_PERSIST_TIMEOUT);
        // No stale endpoint/token once the app is gone.
        asphonix_core::mcp_bridge::remove_discovery_on_exit(&self.0.dir);
    }

    // --- Windows media controls (SMTC / media keys) ---------------------------------------------

    /// Registers the main window (Win32 HWND) with the OS media controls. Call it on the window's thread, delayed past
    /// the first show (about 1.5 s). The same window again is a no-op; a new window
    /// re-registers and retires the previous update loop. A no-op off Windows.
    pub fn attach_media_window(&self, hwnd: isize) {
        if self.0.svc.media_hwnd.swap(hwnd, Ordering::SeqCst) == hwnd {
            return;
        }
        let generation = self.0.svc.media_generation.fetch_add(1, Ordering::SeqCst) + 1;
        asphonix_core::media::attach(Arc::new(RealMedia(Arc::downgrade(&self.0))), hwnd, self.0.svc.media_generation.clone(), generation);
    }

    // --- updater ----------------------------------------------------------------------------------

    /// `updates_check`: the newer release, if any. Disabled channel (see `asphonix_core::updates`) = "no update", no network.
    pub(crate) fn updates_check_impl(&self) -> ApiResult<UpdateInfo> {
        let info = self.0.rt.block_on(self.0.svc.updater.check()).map_err(err)?;
        let info: UpdateInfo = conv(info)?;
        if info.available {
            self.emit(Event::UpdateAvailable(info.clone()));
        }
        Ok(info)
    }

    /// `updates_install`: download, verify the signature, persist + stop playback, run the installer. Explicit user action only.
    pub(crate) fn updates_install_impl(&self) -> ApiResult<()> {
        let (me, last) = (self.clone(), std::cell::Cell::new(std::time::Instant::now() - Duration::from_secs(1)));
        let before = self.clone();
        self.0
            .rt
            .block_on(self.0.svc.updater.install(
                move |downloaded, total| {
                    if last.get().elapsed() >= Duration::from_millis(100) {
                        last.set(std::time::Instant::now());
                        me.emit(Event::UpdateProgress(UpdateProgress { downloaded, total }));
                    }
                },
                move || {
                    // The installer path exits the process without the exit hook: persist (bounded), release the audio device.
                    before.0.engine.persist_now(EXIT_PERSIST_TIMEOUT);
                    before.0.engine.send(asphonix_core::audio::EngineCommand::Stop);
                },
            ))
            .map_err(err)
    }

    /// `Settings.autoCheckUpdates` (default OFF). Turning it on starts the background checker (first check after
    /// 10 s, then every 6 h) if an update channel is configured; with the built-in empty channel nothing runs.
    pub fn set_auto_check_updates(&self, on: bool) {
        self.0.svc.auto_check_updates.store(on, Ordering::Relaxed);
        if !on || !self.0.svc.updater.is_enabled() || self.0.svc.checker_started.swap(true, Ordering::SeqCst) {
            return;
        }
        // Weak: the checker must not keep the backend alive.
        let weak = Arc::downgrade(&self.0);
        self.0.rt.spawn(async move {
            tokio::time::sleep(asphonix_core::updates::FIRST_CHECK_DELAY).await;
            loop {
                let Some(inner) = weak.upgrade() else { break };
                let me = RealBackend(inner);
                if me.0.svc.auto_check_updates.load(Ordering::Relaxed)
                    && let Ok(info) = me.0.svc.updater.check().await
                    && info.available
                    && let Ok(i) = conv::<_, UpdateInfo>(info)
                {
                    me.emit(Event::UpdateAvailable(i));
                }
                drop(me);
                tokio::time::sleep(asphonix_core::updates::CHECK_INTERVAL).await;
            }
        });
    }
}

/// What SMTC needs from the app: engine commands, the player-state watch and encoded cover bytes.
/// Weak: the OS integration never keeps the backend alive.
struct RealMedia(std::sync::Weak<crate::Inner>);

impl RealMedia {
    fn backend(&self) -> Option<RealBackend> {
        self.0.upgrade().map(RealBackend)
    }
}

impl MediaHost for RealMedia {
    fn send(&self, cmd: asphonix_core::audio::EngineCommand) {
        if let Some(b) = self.backend() {
            b.0.engine.send(cmd);
        }
    }
    fn subscribe(&self) -> Option<tokio::sync::watch::Receiver<asphonix_core::ipc::PlayerState>> {
        self.backend().map(|b| b.0.engine.subscribe_player_state())
    }
    fn cover(&self, cover_art_id: String, size: u32) -> BoxFut<Option<Vec<u8>>> {
        let Some(b) = self.backend() else { return Box::pin(async { None }) };
        let client = b.0.client.read().unwrap_or_else(|p| p.into_inner()).clone();
        let path = crate::art::cache_path(&b.art_dir(), &cover_art_id, size);
        drop(b);
        Box::pin(async move {
            // The `art_get` cache (same file per id/bucket, same cap accounting): bytes as received, stored only once
            // they decode.
            if let Some(bytes) = crate::art::read_cached_bytes(&path) {
                return Some(bytes);
            }
            let (bytes, _) = client?.get_cover_art(&cover_art_id, Some(size)).await.ok()?;
            if crate::art::decodes(&bytes) {
                crate::art::store(&path, &bytes);
            }
            Some(bytes)
        })
    }
}

// --- Claude (MCP) bridge ---------------------------------------------------------------------------
// OFF by default: nothing listens and no `mcp.json` exists until
// `set_mcp_enabled(true)`; `settings_set` and `apply_service_settings` (startup) drive these methods.

/// Tool calls run against the backend's own library DB / engine; both gates are re-read on every call.
/// Weak: a running bridge never keeps the backend alive.
struct RealDispatch(std::sync::Weak<crate::Inner>);

impl asphonix_core::mcp_bridge::Dispatch for RealDispatch {
    fn call(&self, tool: &str, args: &serde_json::Value) -> Result<serde_json::Value, String> {
        use asphonix_core::mcp_bridge::tools;
        let Some(b) = self.0.upgrade() else { return Err("ASPHONIX is shutting down.".into()) };
        // Re-checked per call: a client connected a moment before "off" is refused.
        if !b.svc.mcp_enabled.load(Ordering::Relaxed) {
            return Err("Claude (MCP) is turned off in ASPHONIX settings.".into());
        }
        let me = b.session.lock().unwrap().cfg.as_ref().map(|c| c.username.clone());
        // The backend's own store: smart-playlist edits from the UI and from MCP share one copy.
        let ctx = tools::Ctx { db: &b.db, smart: &b.smart, player: &b.engine, me, allow_control: b.svc.mcp_allow_control.load(Ordering::Relaxed) };
        tools::call(&ctx, tool, args)
    }
}

impl RealBackend {
    /// Where `asphonix-mcp` is expected: next to the running executable.
    fn mcp_bridge_path() -> Option<std::path::PathBuf> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.parent()?.join(if cfg!(windows) { "asphonix-mcp.exe" } else { "asphonix-mcp" }))
    }

    /// `mcp_get_status`.
    pub(crate) fn mcp_status_impl(&self) -> ApiResult<McpStatus> {
        let path = Self::mcp_bridge_path();
        let s = &self.0.svc;
        Ok(McpStatus {
            enabled: s.mcp_enabled.load(Ordering::Relaxed),
            allow_control: s.mcp_allow_control.load(Ordering::Relaxed),
            listening: s.mcp.is_listening(),
            bridge_exists: path.as_ref().is_some_and(|p| p.exists()),
            register_command: path.as_ref().map(|p| format!("claude mcp add asphonix -- \"{}\"", p.display())),
            bridge_path: path.map(|p| p.display().to_string()),
            last_error: s.mcp.last_error(),
        })
    }

    /// "Claude (MCP)" master switch (default OFF). On = a fresh token, the local named pipe and `mcp.json`
    /// in the config dir; off = all of that torn down. Persisting the flag is the settings layer's job.
    pub fn set_mcp_enabled(&self, enabled: bool) -> ApiResult<McpStatus> {
        let s = &self.0.svc;
        if enabled {
            let dispatch: Arc<dyn asphonix_core::mcp_bridge::Dispatch> = Arc::new(RealDispatch(Arc::downgrade(&self.0)));
            self.0.rt.block_on(s.mcp.start(dispatch, &self.0.dir)).map_err(|m| ApiError::new(ErrorCode::Unknown, m))?;
            s.mcp_enabled.store(true, Ordering::Relaxed);
        } else {
            // Gate first: from here on no new tool call passes `RealDispatch`, even on a connection the shutdown
            // below has not closed yet. A call already running on the blocking pool still finishes.
            s.mcp_enabled.store(false, Ordering::Relaxed);
            self.0.rt.block_on(s.mcp.stop(&self.0.dir));
        }
        self.mcp_status_impl()
    }

    /// Startup: applies the persisted service flags before anything else runs (the flags are
    /// stored at once; the MCP listener starts 3 s later and only if still enabled).
    pub(crate) fn apply_service_settings(&self, s: &asphonix_core::secrets::Settings) {
        self.set_auto_check_updates(s.auto_check_updates);
        self.0.svc.mcp_allow_control.store(s.mcp_allow_control, Ordering::Relaxed);
        if !s.mcp_enabled {
            return;
        }
        self.0.svc.mcp_enabled.store(true, Ordering::Relaxed);
        // Weak: a backend dropped within the delay (tests, early exit) starts nothing.
        let weak = Arc::downgrade(&self.0);
        std::thread::spawn(move || {
            std::thread::sleep(MCP_START_DELAY);
            let Some(inner) = weak.upgrade() else { return };
            let b = RealBackend(inner);
            if b.0.svc.mcp_enabled.load(Ordering::Relaxed)
                && let Err(e) = b.set_mcp_enabled(true)
            {
                log::warn!("Claude (MCP) bridge not started: {}", e.message);
            }
        });
    }

    /// "Allow playback control" (default OFF, only meaningful while enabled). Read live on every tool call.
    pub fn set_mcp_allow_control(&self, allow: bool) -> ApiResult<McpStatus> {
        self.0.svc.mcp_allow_control.store(allow, Ordering::Relaxed);
        self.mcp_status_impl()
    }
}

#[cfg(test)]
mod tests {
    use crate::RealBackend;

    /// The shipped update channel is empty: turning the auto-check on (as the persisted setting does at startup)
    /// starts no checker task, so no request can ever go out.
    #[test]
    fn auto_check_on_with_the_empty_channel_starts_nothing() {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::var_os("ASPHONIX_TEST_TMP").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join(format!("asphonix-svc-auto-{}-{seq}", std::process::id()));
        {
            let b = RealBackend::open_in(dir.join("config"), dir.join("data"));
            assert!(asphonix_core::updates::UPDATE_ENDPOINT.is_empty() && !b.0.svc.updater.is_enabled());
            b.set_auto_check_updates(true);
            assert!(b.0.svc.auto_check_updates.load(std::sync::atomic::Ordering::Relaxed), "the flag is kept");
            assert!(!b.0.svc.checker_started.load(std::sync::atomic::Ordering::SeqCst), "no checker task, no network");
        }
        let t = std::time::Instant::now();
        while std::fs::remove_dir_all(&dir).is_err() && dir.exists() && t.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!dir.exists(), "temp dir removed");
    }
}
