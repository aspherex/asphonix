//! `RealBackend`: the `Backend` trait over the vendored SONIX core (`sonix-core`).
//! Every `Backend` method is wired; the remaining gaps are listed in `docs/HANDOFF.md`.
mod art;
mod extras;
mod player;
mod reads;
mod settings;
mod services;
mod writes;

use sonix_api::*;
use sonix_core::ipc::{IpcError, IpcErrorCode};
use sonix_core::{secrets, subsonic::Client};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use sonix_core::library::Db;

/// Keychain service name and data-dir name: never the installed app's `dev.sonix.app`.
pub const IDENTIFIER: &str = "dev.sonix.native";

/// Library DB dir (`%LOCALAPPDATA%dev.sonix.native`).
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let d = base.join(IDENTIFIER);
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Where a profile/session lives (`%APPDATA%\dev.sonix.native`).
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join(IDENTIFIER)
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
    client: Arc<std::sync::RwLock<Option<Client>>>,
    engine: sonix_core::audio::EngineHandle,
    waveform: sonix_core::audio::waveform::WaveformStore,
    prefs: Arc<sonix_core::audio::Prefs>,
    /// Keeps the watch channel's value updating (`watch::Sender::send` is a no-op without receivers).
    _state_rx: tokio::sync::watch::Receiver<sonix_core::ipc::PlayerState>,
    session: Mutex<Session>,
    subs: Mutex<Vec<Sender<Event>>>,
    /// OS integration + background services (listen flush, SMTC, MCP bridge, updater).
    svc: services::Services,
    /// Local smart-playlist definitions (`smart_playlists.json` in the config dir).
    smart: sonix_core::smartlists::SmartStore,
    /// Persisted settings (`settings.json` in the config dir), the source of truth for `settings_get`.
    settings: Mutex<secrets::Settings>,
    /// `Settings.scrobbling_enabled`, shared with the scrobble tasks.
    scrobbling: Arc<std::sync::atomic::AtomicBool>,
    /// Session persistence + server play-queue push (gated by `Prefs::sync_play_queue`).
    store: Arc<sonix_core::audio::SessionStore>,
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

fn wire_caps(c: &sonix_core::subsonic::Capabilities) -> ServerCapabilities {
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
        let prefs = Arc::new(sonix_core::audio::Prefs::new(settings.autoplay_enabled, settings.sync_play_queue, settings.replay_gain_mode));
        prefs.set_play_threshold_sec(settings.play_threshold_sec);
        let scrobbling = Arc::new(std::sync::atomic::AtomicBool::new(settings.scrobbling_enabled));
        let store = sonix_core::audio::SessionStore::spawn(dir.clone(), client.clone(), prefs.clone());
        let autoplay: Arc<dyn sonix_core::audio::autoplay::AutoplaySource> = Arc::new(sonix_core::continuity::ServerAutoplay { client: client.clone(), db: db.clone() });
        let (ev_tx, ev_rx) = channel::<Event>();
        let source: Arc<dyn sonix_core::audio::TrackSource> = Arc::new(sonix_core::audio::SubsonicTrackSource::new(client.clone()));
        let waveform_source = source.clone();
        let engine = sonix_core::audio::EngineHandle::with_events(Arc::new(player::Events(ev_tx)), source, db.clone(), settings.spread_artists_in_shuffle, settings.queue_window_size)
            .with_continuity(prefs.clone(), Some(store.clone()), Some(autoplay))
            .with_listen(sonix_core::listen::spawn_writer(db.clone()));
        // Safety default: shared mode until the user picks exclusive in Settings.
        engine.send(sonix_core::audio::EngineCommand::SetOutputMode(sonix_core::ipc::OutputMode::Shared));
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio");
        let cfg = secrets::load_config(&dir);
        let me = Self(Arc::new(Inner {
            smart: sonix_core::smartlists::SmartStore::new(&dir),
            rt,
            db,
            waveform: sonix_core::audio::waveform::WaveformStore::new(waveform_source),
            _state_rx: engine.subscribe_player_state(),
            engine,
            prefs,
            epoch: AtomicU64::new(0),
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
            settings: Mutex::new(settings),
            scrobbling,
            store,
        }));
        // Persisted service flags first (all default OFF), before any background task or the reconnect starts.
        {
            let s = me.0.settings.lock().unwrap_or_else(|p| p.into_inner()).clone();
            me.apply_service_settings(&s);
        }
        sonix_core::scrobble::service::spawn(
            sonix_core::scrobble::service::ScrobbleCtx {
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

    /// Background sync (full catalog when the DB is empty or `full`, else incremental).
    pub(crate) fn start_sync(&self, full: bool) {
        let Some(client) = self.0.client.read().unwrap().clone() else { return };
        let me = self.clone();
        let epoch = self.0.epoch.load(std::sync::atomic::Ordering::SeqCst);
        self.0.rt.spawn(async move {
            let sink = reads::Sink(me.clone(), epoch);
            let res = if full || me.0.db.track_count().unwrap_or(0) == 0 {
                sonix_core::library::run_full_sync(&client, &me.0.db, &sink).await
            } else {
                sonix_core::library::run_incremental_sync(&client, &me.0.db, &sink, None).await
            };
            if res.is_ok() {
                let now = me.0.db.get_meta("lastSyncAt").ok().flatten();
                let mut st = me.0.session.lock().unwrap();
                if let Some(c) = st.cfg.as_mut() {
                    c.last_sync_at = now;
                    let _ = secrets::save_config(&me.0.dir, c);
                }
                st.track_count = me.0.db.track_count().ok();
            }
            me.emit(Event::LibraryChanged);
        });
    }

    pub(crate) fn emit(&self, ev: Event) {
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
        let caps = self.0.rt.block_on(client.probe_capabilities()).map_err(err)?;
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
        self.0.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if !same && previous.is_some() {
            // Stop + wait for the closing listen row before the wipe, so it goes with the old account.
            self.flush_listen();
            self.0.db.clear_all().map_err(err)?;
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

    fn auth_logout(&self) -> ApiResult<()> {
        let cfg = self.0.session.lock().unwrap().cfg.clone();
        if let Some(cfg) = &cfg {
            let _ = secrets::profiles::update(&self.0.dir, Some(cfg), |l| {
                l.retain(|p| !p.matches(&cfg.url, &cfg.username));
                Ok(())
            });
            secrets::delete_profile_password(&cfg.url, &cfg.username, false).map_err(err)?;
        }
        secrets::clear_config(&self.0.dir).map_err(err)?;
        self.flush_listen();
        *self.0.client.write().unwrap() = None;
        *self.0.session.lock().unwrap() = Session { state: Some(ConnectionState::Disconnected), ..Default::default() };
        self.set_state(ConnectionState::Disconnected, None);
        Ok(())
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
    fn attach_window(&self, hwnd: isize) {
        self.attach_media_window(hwnd)
    }
}
