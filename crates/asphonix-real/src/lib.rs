//! `RealBackend`: the `Backend` trait over the ASPHONIX core (`asphonix-core`).
//! Every `Backend` method is wired.
mod art;
mod extras;
mod player;
mod reads;
mod settings;
mod services;
mod writes;

use asphonix_api::*;
use asphonix_core::ipc::{IpcError, IpcErrorCode};
use asphonix_core::{secrets, subsonic::Client};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use asphonix_core::library::Db;

/// Keychain service name and data-dir name.
pub const IDENTIFIER: &str = "dev.asphonix.app";

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

#[cfg(not(windows))]
fn home_join(rel: &str) -> Option<PathBuf> {
    env_dir("HOME").map(|h| h.join(rel))
}

/// Library DB, caches and logs: `%LOCALAPPDATA%\dev.asphonix.app` on Windows, `~/Library/Application Support/dev.asphonix.app`
/// on macOS, `$XDG_DATA_HOME/dev.asphonix.app` (default `~/.local/share`) elsewhere.
pub fn data_dir() -> PathBuf {
    #[cfg(windows)]
    let base = env_dir("LOCALAPPDATA");
    #[cfg(target_os = "macos")]
    let base = home_join("Library/Application Support");
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let base = env_dir("XDG_DATA_HOME").or_else(|| home_join(".local/share"));
    let d = base.unwrap_or_else(std::env::temp_dir).join(IDENTIFIER);
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Profiles, settings and the session: `%APPDATA%\dev.asphonix.app` on Windows, `~/Library/Application Support/dev.asphonix.app`
/// on macOS, `$XDG_CONFIG_HOME/dev.asphonix.app` (default `~/.config`) elsewhere. Same rule as `asphonix_mcp::wire::config_dir`,
/// which finds the bridge's `mcp.json` here.
pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    let base = env_dir("APPDATA");
    #[cfg(target_os = "macos")]
    let base = home_join("Library/Application Support");
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let base = env_dir("XDG_CONFIG_HOME").or_else(|| home_join(".config"));
    base.unwrap_or_else(std::env::temp_dir).join(IDENTIFIER)
}

#[derive(Clone, Default)]
struct Session {
    state: Option<ConnectionState>,
    cfg: Option<secrets::ConnectionConfig>,
    caps: Option<ServerCapabilities>,
    track_count: Option<u64>,
}

struct Inner {
    rt: tokio::runtime::Runtime,
    /// Config dir (`connection.json`, settings, session, smart playlists, MCP discovery, scrobble queue).
    dir: PathBuf,
    /// Data dir (library DB, art cache).
    data: PathBuf,
    db: Arc<Db>,
    epoch: AtomicU64,
    /// Held for a whole library sync, so at most one runs at a time (background
    /// resyncs skip while one runs, full syncs wait). Two overlapping catalog syncs get different
    /// generations and the later one's stale-row sweep deletes pages the earlier one re-wrote.
    sync_lock: tokio::sync::Mutex<()>,
    /// `epoch + 1` while a full sync of this session runs or waits, so
    /// `auth_connect` + the login screen's `library_sync_start { full: true }` fetch the catalog once.
    full_sync_pending: AtomicU64,
    client: Arc<std::sync::RwLock<Option<Client>>>,
    engine: asphonix_core::audio::EngineHandle,
    waveform: asphonix_core::audio::waveform::WaveformStore,
    prefs: Arc<asphonix_core::audio::Prefs>,
    /// Keeps the watch channel's value updating (`watch::Sender::send` is a no-op without receivers).
    _state_rx: tokio::sync::watch::Receiver<asphonix_core::ipc::PlayerState>,
    session: Mutex<Session>,
    subs: Mutex<Vec<Sender<Event>>>,
    /// OS integration + background services (listen flush, SMTC, MCP bridge, updater).
    svc: services::Services,
    /// Cover fetch state (gates, single-flight, prefetch control): `art.rs`.
    art: art::ArtState,
    /// Local smart-playlist definitions (`smart_playlists.json` in the config dir).
    smart: asphonix_core::smartlists::SmartStore,
    /// Persisted settings (`settings.json` in the config dir), the source of truth for `settings_get`.
    settings: Mutex<secrets::Settings>,
    /// `Settings.scrobbling_enabled`, shared with the scrobble tasks.
    scrobbling: Arc<std::sync::atomic::AtomicBool>,
    /// Session persistence + server play-queue push (gated by `Prefs::sync_play_queue`).
    store: Arc<asphonix_core::audio::SessionStore>,
    /// Track id + status of the last `PlayerState` event (`emit` drops position-only ticks while suspended).
    last_ui_state: Mutex<Option<(Option<String>, PlaybackStatus)>>,
    /// True until the first successful library sync of this run has re-offered the saved session to the engine
    /// (`settings.rs: after_first_sync`).
    restore_retry: std::sync::atomic::AtomicBool,
}

#[derive(Clone)]
pub struct RealBackend(Arc<Inner>);

pub(crate) fn err(e: IpcError) -> ApiError {
    let code = match e.code {
        IpcErrorCode::NotConnected => ErrorCode::NotConnected,
        IpcErrorCode::Network => ErrorCode::Network,
        IpcErrorCode::AuthFailed => ErrorCode::AuthFailed,
        IpcErrorCode::NotFound => ErrorCode::NotFound,
        IpcErrorCode::ServerError => ErrorCode::ServerError,
        IpcErrorCode::Io => ErrorCode::Io,
        IpcErrorCode::Decode => ErrorCode::Decode,
        IpcErrorCode::OutputUnavailable => ErrorCode::OutputUnavailable,
        IpcErrorCode::Unimplemented => ErrorCode::Unimplemented,
        IpcErrorCode::InvalidRequest => ErrorCode::InvalidRequest,
        IpcErrorCode::Unknown => ErrorCode::Unknown,
    };
    ApiError::new(code, e.message)
}

fn wire_caps(c: &asphonix_core::subsonic::Capabilities) -> ServerCapabilities {
    ServerCapabilities {
        server_name: c.server_type.clone(),
        server_version: c.server_version.clone(),
        api_version: c.api_version.clone(),
        open_subsonic_extensions: c.extensions.clone(),
        ping_ms: c.ping_ms,
    }
}

impl RealBackend {
    /// True when a saved connection exists, i.e. the real backend should start.
    pub fn has_session() -> bool {
        secrets::load_config(&config_dir()).is_some()
    }

    /// Opens the backend; with a saved connection it reconnects in the background
    /// using the keychain password (state `Connecting` until the probe answers).
    pub fn open() -> Self {
        Self::open_in(config_dir(), data_dir())
    }

    /// `open` with explicit config (`connection.json`, profiles, smart playlists) and data (library DB) dirs.
    pub fn open_in(dir: PathBuf, data: PathBuf) -> Self {
        secrets::set_service_name(IDENTIFIER);
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::create_dir_all(&data);
        let db = Arc::new(Db::open(&data).expect("library db"));
        let client: Arc<std::sync::RwLock<Option<Client>>> = Arc::new(std::sync::RwLock::new(None));
        let settings = secrets::load_settings(&dir);
        if settings.verbose_logging {
            asphonix_logging::set_verbose(true); // the stored "Verbose logging" setting (no-op before `asphonix_logging::init`)
        }
        let prefs = Arc::new(asphonix_core::audio::Prefs::new(settings.autoplay_enabled, settings.sync_play_queue, settings.replay_gain_mode));
        prefs.set_play_threshold_sec(settings.play_threshold_sec);
        let scrobbling = Arc::new(std::sync::atomic::AtomicBool::new(settings.scrobbling_enabled));
        let store = asphonix_core::audio::SessionStore::spawn(dir.clone(), client.clone(), prefs.clone());
        let autoplay: Arc<dyn asphonix_core::audio::autoplay::AutoplaySource> = Arc::new(asphonix_core::continuity::ServerAutoplay { client: client.clone(), db: db.clone() });
        let (ev_tx, ev_rx) = channel::<Event>();
        let source: Arc<dyn asphonix_core::audio::TrackSource> = Arc::new(asphonix_core::audio::SubsonicTrackSource::new(client.clone()));
        let waveform_source = source.clone();
        let engine = asphonix_core::audio::EngineHandle::with_events(Arc::new(player::Events(ev_tx)), source, db.clone(), settings.spread_artists_in_shuffle, settings.queue_window_size)
            .with_continuity(prefs.clone(), Some(store.clone()), Some(autoplay))
            .with_listen(asphonix_core::listen::spawn_writer(db.clone()));
        // Safety default: shared mode until the user picks exclusive in Settings.
        engine.send(asphonix_core::audio::EngineCommand::SetOutputMode(asphonix_core::ipc::OutputMode::Shared));
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio");
        let cfg = secrets::load_config(&dir);
        let me = Self(Arc::new(Inner {
            smart: asphonix_core::smartlists::SmartStore::new(&dir),
            rt,
            db,
            waveform: asphonix_core::audio::waveform::WaveformStore::new(waveform_source),
            _state_rx: engine.subscribe_player_state(),
            engine,
            prefs,
            epoch: AtomicU64::new(0),
            sync_lock: tokio::sync::Mutex::new(()),
            full_sync_pending: AtomicU64::new(0),
            dir,
            data,
            client,
            session: Mutex::new(Session {
                state: Some(if cfg.is_some() { ConnectionState::Connecting } else { ConnectionState::Disconnected }),
                cfg: cfg.clone(),
                ..Default::default()
            }),
            subs: Mutex::new(Vec::new()),
            svc: Default::default(),
            art: Default::default(),
            settings: Mutex::new(settings),
            scrobbling,
            store,
            last_ui_state: Mutex::new(None),
            restore_retry: std::sync::atomic::AtomicBool::new(true),
        }));
        // Persisted service flags first (all default OFF), before any background task or the reconnect starts.
        {
            let s = me.0.settings.lock().unwrap_or_else(|p| p.into_inner()).clone();
            me.apply_service_settings(&s);
        }
        asphonix_core::scrobble::service::spawn(
            asphonix_core::scrobble::service::ScrobbleCtx {
                client: me.0.client.clone(),
                db: me.0.db.clone(),
                prefs: me.0.prefs.clone(),
                scrobbling: me.0.scrobbling.clone(),
                data_dir: me.0.dir.clone(),
            },
            me.0.engine.subscribe_player_state(),
        );
        me.spawn_session_restore();
        {
            // Weak: the pump must not keep the backend (engine, DB) alive once every handle is dropped.
            let weak = Arc::downgrade(&me.0);
            std::thread::spawn(move || {
                while let Ok(ev) = ev_rx.recv() {
                    let Some(inner) = weak.upgrade() else { break };
                    RealBackend(inner).emit(ev);
                }
            });
        }
        if let Some(cfg) = cfg {
            let b = me.clone();
            std::thread::spawn(move || match secrets::load_profile_password(&cfg.url, &cfg.username) {
                Ok(Some(pw)) => {
                    if let Err(e) = b.connect(&cfg.url, &cfg.username, &pw, false) {
                        b.set_state(ConnectionState::Error, Some(e.message));
                    }
                }
                _ => b.set_state(ConnectionState::Disconnected, Some("no saved password; sign in again".into())),
            });
        }
        me
    }

    /// Cover / mosaic disk cache (`<data dir>\art`).
    pub(crate) fn art_dir(&self) -> PathBuf {
        self.0.data.join("art")
    }

    /// Drops every cached cover/mosaic (account switch / logout: the ids belong to the other library).
    pub(crate) fn clear_art_cache(&self) {
        let _ = std::fs::remove_dir_all(self.art_dir());
        crate::art::reset_cache_bytes();
    }

    /// Background sync (full catalog when the DB is empty or `full`, else incremental).
    pub(crate) fn start_sync(&self, full: bool) {
        let Some(client) = self.0.client.read().unwrap().clone() else { return };
        let me = self.clone();
        let epoch = self.0.epoch.load(std::sync::atomic::Ordering::SeqCst);
        self.0.rt.spawn(async move {
            use std::sync::atomic::Ordering::SeqCst;
            // Coalesce duplicate full requests within one session.
            let pending_marker = epoch + 1;
            if full && me.0.full_sync_pending.swap(pending_marker, SeqCst) == pending_marker {
                return;
            }
            struct ClearPending<'a>(&'a AtomicU64, u64, bool);
            impl Drop for ClearPending<'_> {
                fn drop(&mut self) {
                    if self.2 {
                        let _ = self.0.compare_exchange(self.1, 0, SeqCst, SeqCst);
                    }
                }
            }
            let _clear_pending = ClearPending(&me.0.full_sync_pending, pending_marker, full);
            let _guard = if full {
                me.0.sync_lock.lock().await
            } else {
                match me.0.sync_lock.try_lock() {
                    Ok(g) => g,
                    Err(_) => return,
                }
            };
            // the session may have changed while this sync waited for the lock
            if me.0.epoch.load(SeqCst) != epoch {
                return;
            }
            let sink = reads::Sink(me.clone(), epoch);
            let res = if full || me.0.db.track_count().unwrap_or(0) == 0 {
                asphonix_core::library::run_full_sync(&client, &me.0.db, &sink).await
            } else {
                asphonix_core::library::run_incremental_sync(&client, &me.0.db, &sink, None).await
            };
            if res.is_ok() && me.0.epoch.load(SeqCst) == epoch {
                let now = me.0.db.get_meta("lastSyncAt").ok().flatten();
                let mut st = me.0.session.lock().unwrap();
                if let Some(c) = st.cfg.as_mut() {
                    c.last_sync_at = now;
                    let _ = secrets::save_config(&me.0.dir, c);
                }
                st.track_count = me.0.db.track_count().ok();
            }
            me.emit(Event::LibraryChanged);
            if res.is_ok() {
                if me.0.epoch.load(SeqCst) == epoch {
                    me.after_first_sync();
                }
                me.schedule_art_prefetch(epoch);
            }
        });
    }

    pub(crate) fn emit(&self, ev: Event) {
        // Minimized (`set_suspended`): nobody renders the 4 Hz position ticks, and every one wakes the UI threads and
        // re-touches pages the trim just released. Only a track/status change is forwarded (the UI re-reads
        // `player_get_state` on restore anyway).
        if let Event::PlayerState(s) = &ev {
            let key = (s.track.as_ref().map(|t| t.id.clone()), s.status);
            let mut last = self.0.last_ui_state.lock().unwrap_or_else(|p| p.into_inner());
            let same = last.as_ref() == Some(&key);
            *last = Some(key);
            if same && self.0.art.suspended.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
        }
        if matches!(ev, Event::Spectrum(_)) && self.0.art.suspended.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        self.0.subs.lock().unwrap().retain(|s| s.send(ev.clone()).is_ok());
    }

    fn set_state(&self, state: ConnectionState, message: Option<String>) {
        let ping = {
            let mut s = self.0.session.lock().unwrap();
            s.state = Some(state);
            s.caps.as_ref().map(|c| c.ping_ms)
        };
        self.emit(Event::Connection(ConnectionStatusEvent { state, message, ping_ms: ping }));
    }

    /// Probe, store the password (only after a successful probe), persist config + profile list, install the client.
    fn connect(&self, url: &str, username: &str, password: &str, save: bool) -> ApiResult<ServerCapabilities> {
        let client = Client::new(url, username, password).map_err(err)?;
        // No user name or server URL in the log: the user travels as the `u` auth parameter.
        let caps = match self.0.rt.block_on(client.probe_capabilities()) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e.message, "login failed");
                return Err(err(e));
            }
        };
        tracing::info!(
            server_type = %caps.server_type,
            server_version = %caps.server_version,
            api_version = %caps.api_version,
            ping_ms = caps.ping_ms,
            track_count = caps.track_count,
            "login: connected"
        );
        let wire = wire_caps(&caps);
        let previous = self.0.session.lock().unwrap().cfg.clone();
        if save {
            secrets::save_profile_password(url, username, password).map_err(err)?;
        }
        let last_sync_at = previous.as_ref().filter(|p| p.url == url && p.username == username).and_then(|p| p.last_sync_at.clone());
        let cfg = secrets::ConnectionConfig { url: url.into(), username: username.into(), last_sync_at };
        secrets::save_config(&self.0.dir, &cfg).map_err(err)?;
        let _ = secrets::profiles::update(&self.0.dir, previous.as_ref(), |l| {
            secrets::profiles::upsert(l, url, username);
            Ok(())
        });
        let same = previous.as_ref().is_some_and(|p| p.url == url && p.username == username);
        if !same {
            // The saved session belongs to the previous account: its first sync must not re-offer it to this one.
            self.0.restore_retry.store(false, std::sync::atomic::Ordering::SeqCst);
        }
        self.0.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if !same {
            // A different server/user (also the first login after a logout) starts clean.
            // Stop + wait for the closing listen row before the wipe, so it goes with the old account; the art cache
            // ids belong to the other library too.
            self.flush_listen();
            self.0.db.clear_all().map_err(err)?;
            self.clear_art_cache();
        }
        *self.0.client.write().unwrap() = Some(client);
        {
            let mut s = self.0.session.lock().unwrap();
            s.cfg = Some(cfg);
            s.caps = Some(wire.clone());
            s.track_count = caps.track_count;
        }
        self.set_state(ConnectionState::Connected, None);
        self.start_sync(false);
        Ok(wire)
    }
}

impl AuthApi for RealBackend {
    fn auth_connect(&self, req: ConnectRequest) -> ApiResult<ServerCapabilities> {
        self.connect(req.url.trim(), req.username.trim(), &req.password, true)
    }

    fn auth_probe(&self) -> ApiResult<ServerCapabilities> {
        let client = self.0.client.read().unwrap().clone().ok_or_else(|| ApiError::new(ErrorCode::NotConnected, "not connected"))?;
        let caps = self.0.rt.block_on(client.probe_capabilities()).map_err(err)?;
        Ok(wire_caps(&caps))
    }

    /// Stop the engine and flush the listen writer first, drop the client, cancel a running
    /// sync, then clear everything local (profile entry, keychain, `connection.json`, library mirror, art cache) even if
    /// one step fails; the first error is reported.
    fn auth_logout(&self) -> ApiResult<()> {
        self.0.restore_retry.store(false, std::sync::atomic::Ordering::SeqCst);
        self.flush_listen();
        *self.0.client.write().unwrap() = None;
        self.0.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let cfg = self.0.session.lock().unwrap().cfg.clone();
        let keychain = match &cfg {
            Some(cfg) => {
                let _ = secrets::profiles::update(&self.0.dir, Some(cfg), |l| {
                    l.retain(|p| !p.matches(&cfg.url, &cfg.username));
                    Ok(())
                });
                secrets::delete_profile_password(&cfg.url, &cfg.username, false)
            }
            None => Ok(()),
        };
        let config = secrets::clear_config(&self.0.dir);
        let db = self.0.db.clear_all();
        self.clear_art_cache();
        *self.0.session.lock().unwrap() = Session { state: Some(ConnectionState::Disconnected), ..Default::default() };
        self.set_state(ConnectionState::Disconnected, None);
        self.emit(Event::LibraryChanged);
        keychain.and(config).and(db).map_err(err)
    }

    fn auth_session_status(&self) -> ApiResult<SessionStatus> {
        let s = self.0.session.lock().unwrap().clone();
        Ok(SessionStatus {
            state: s.state.unwrap_or(ConnectionState::Disconnected),
            server_url: s.cfg.as_ref().map(|c| c.url.clone()),
            username: s.cfg.as_ref().map(|c| c.username.clone()),
            server_version: s.caps.as_ref().map(|c| c.server_version.clone()),
            api_version: s.caps.as_ref().map(|c| c.api_version.clone()),
            ping_ms: s.caps.as_ref().map(|c| c.ping_ms),
            last_sync_at: s.cfg.as_ref().and_then(|c| c.last_sync_at.clone()),
            track_count: s.track_count,
        })
    }

    fn profiles_list(&self) -> ApiResult<Vec<Profile>> {
        let active = self.0.session.lock().unwrap().cfg.clone();
        let list = secrets::profiles::load(&self.0.dir, active.as_ref());
        Ok(list
            .iter()
            .map(|p| Profile { id: p.id(), url: p.url.clone(), username: p.username.clone(), active: active.as_ref().is_some_and(|a| p.matches(&a.url, &a.username)) })
            .collect())
    }

    fn profiles_switch(&self, id: &str) -> ApiResult<ServerCapabilities> {
        let active = self.0.session.lock().unwrap().cfg.clone();
        let list = secrets::profiles::load(&self.0.dir, active.as_ref());
        let p = secrets::profiles::find(&list, id).map_err(err)?.clone();
        let pw = secrets::load_profile_password(&p.url, &p.username)
            .map_err(err)?
            .ok_or_else(|| ApiError::new(ErrorCode::AuthFailed, format!("no saved password for {} on {}; sign in again", p.username, p.url)))?;
        self.connect(&p.url, &p.username, &pw, true)
    }

    fn profiles_remove(&self, id: &str) -> ApiResult<()> {
        let active = self.0.session.lock().unwrap().cfg.clone();
        secrets::profiles::update(&self.0.dir, active.as_ref(), |l| {
            if let Some(i) = l.iter().position(|p| p.id() == id) {
                let p = l[i].clone();
                if active.as_ref().is_some_and(|a| p.matches(&a.url, &a.username)) {
                    return Err(IpcError { code: IpcErrorCode::InvalidRequest, message: "cannot remove the active profile; sign out instead".into() });
                }
                let _ = secrets::delete_profile_password(&p.url, &p.username, false);
                l.remove(i);
            }
            Ok(())
        })
        .map_err(err)?;
        Ok(())
    }
}

impl Backend for RealBackend {
    fn subscribe(&self) -> Receiver<Event> {
        let (tx, rx) = channel();
        self.0.subs.lock().unwrap().push(tx);
        rx
    }
    fn shutdown(&self) {
        RealBackend::shutdown(self)
    }
    fn set_suspended(&self, suspended: bool) {
        self.0.art.suspended.store(suspended, std::sync::atomic::Ordering::SeqCst);
    }
    fn attach_window(&self, hwnd: isize) {
        self.attach_media_window(hwnd)
    }
}
