//! OS integration and background services of the real backend: listen-event flush on
//! logout/exit, Windows media controls (SMTC), the MCP bridge and the updater.
//! Inherent methods only; the `Backend` trait forwarders are one-liners in the API impls.
use crate::{err, reads::conv, RealBackend};
use sonix_api::*;
use sonix_core::media::{BoxFut, MediaHost};
use sonix_core::updates::Updater;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How long logout / account switch / exit wait for the listen writer (reference `LISTEN_FLUSH_TIMEOUT`).
pub(crate) const LISTEN_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
/// Final session write bound at exit (reference lib.rs `persist_now(4 s)`).
pub(crate) const EXIT_PERSIST_TIMEOUT: Duration = Duration::from_secs(4);

/// Per-service state held by `Inner`.
pub(crate) struct Services {
    pub updater: Arc<Updater>,
    /// `Settings.autoCheckUpdates` (OFF by default; the merge connects it to the persisted setting).
    pub auto_check_updates: Arc<AtomicBool>,
    checker_started: AtomicBool,
    media_attached: AtomicBool,
}

impl Default for Services {
    fn default() -> Self {
        Self { updater: Arc::new(Updater::built_in()), auto_check_updates: Arc::new(AtomicBool::new(false)), checker_started: AtomicBool::new(false), media_attached: AtomicBool::new(false) }
    }
}

impl RealBackend {
    /// Stop playback and wait (bounded) until the listen session `Stop` closed, and any still queued,
    /// are in the DB. Must run BEFORE `Db::clear_all` on logout / account switch (reference c777bc0).
    pub(crate) fn flush_listen(&self) {
        self.0.rt.block_on(self.0.engine.stop_and_flush_listen(LISTEN_FLUSH_TIMEOUT));
    }

    /// App exit: final session write (also records the `quit` listen row), then service teardown.
    pub fn shutdown(&self) {
        self.0.engine.persist_now(EXIT_PERSIST_TIMEOUT);
    }

    // --- Windows media controls (SMTC / media keys) ---------------------------------------------

    /// Registers the main window (Win32 HWND) with the OS media controls. Once per backend; a no-op off Windows.
    pub fn attach_media_window(&self, hwnd: isize) {
        if self.0.svc.media_attached.swap(true, Ordering::SeqCst) {
            return;
        }
        sonix_core::media::spawn(Arc::new(RealMedia(self.clone())), hwnd);
    }

    // --- updater ----------------------------------------------------------------------------------

    /// `updates_check`: the newer release, if any. Disabled channel (see `sonix_core::updates`) = "no update", no network.
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
                    before.0.engine.send(sonix_core::audio::EngineCommand::Stop);
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
        let me = self.clone();
        self.0.rt.spawn(async move {
            tokio::time::sleep(sonix_core::updates::FIRST_CHECK_DELAY).await;
            loop {
                if me.0.svc.auto_check_updates.load(Ordering::Relaxed) {
                    if let Ok(info) = me.0.svc.updater.check().await {
                        if info.available {
                            if let Ok(i) = conv::<_, UpdateInfo>(info) {
                                me.emit(Event::UpdateAvailable(i));
                            }
                        }
                    }
                }
                tokio::time::sleep(sonix_core::updates::CHECK_INTERVAL).await;
            }
        });
    }
}

/// What SMTC needs from the app: engine commands, the player-state watch and encoded cover bytes.
struct RealMedia(RealBackend);

impl MediaHost for RealMedia {
    fn send(&self, cmd: sonix_core::audio::EngineCommand) {
        self.0 .0.engine.send(cmd);
    }
    fn subscribe(&self) -> tokio::sync::watch::Receiver<sonix_core::ipc::PlayerState> {
        self.0 .0.engine.subscribe_player_state()
    }
    fn cover(&self, cover_art_id: String, size: u32) -> BoxFut<Option<Vec<u8>>> {
        let client = self.0 .0.client.read().unwrap().clone();
        let dir = crate::data_dir().join("art");
        Box::pin(async move {
            // Same cache layout as `art_get`: `<id>_<bucket>.img`, bytes as received from the server.
            let key: String = cover_art_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
            let path = dir.join(format!("{key}_{size}.img"));
            if let Ok(b) = tokio::fs::read(&path).await {
                if !b.is_empty() {
                    return Some(b);
                }
            }
            let (b, _) = client?.get_cover_art(&cover_art_id, Some(size)).await.ok()?;
            let _ = tokio::fs::create_dir_all(&dir).await;
            let tmp = path.with_extension("tmp");
            if tokio::fs::write(&tmp, &b).await.is_ok() {
                let _ = tokio::fs::rename(&tmp, &path).await;
            }
            Some(b)
        })
    }
}
