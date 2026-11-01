//! `RealBackend`: the `Backend` trait over the vendored SONIX core (`sonix-core`).
//! Phase 2 step 1: login, profiles, keychain, session status. Everything else
//! returns `unimplemented` (see `stubs.rs`) until the later steps land.
mod reads;
mod stubs;

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
    dir: PathBuf,
    db: Db,
    epoch: AtomicU64,
    client: Mutex<Option<Client>>,
    session: Mutex<Session>,
    subs: Mutex<Vec<Sender<Event>>>,
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
        secrets::set_service_name(IDENTIFIER);
        let dir = config_dir();
        let _ = std::fs::create_dir_all(&dir);
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio");
        let cfg = secrets::load_config(&dir);
        let me = Self(Arc::new(Inner {
            rt,
            db: Db::open(&data_dir()).expect("library db"),
            epoch: AtomicU64::new(0),
            dir,
            client: Mutex::new(None),
            session: Mutex::new(Session {
                state: Some(if cfg.is_some() { ConnectionState::Connecting } else { ConnectionState::Disconnected }),
                cfg: cfg.clone(),
                ..Default::default()
            }),
            subs: Mutex::new(Vec::new()),
        }));
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

    /// Background sync (full catalog when the DB is empty or `full`, else incremental).
    pub(crate) fn start_sync(&self, full: bool) {
        let Some(client) = self.0.client.lock().unwrap().clone() else { return };
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
            self.0.db.clear_all().map_err(err)?;
        }
        *self.0.client.lock().unwrap() = Some(client);
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
        let client = self.0.client.lock().unwrap().clone().ok_or_else(|| ApiError::new(ErrorCode::NotConnected, "not connected"))?;
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
        *self.0.client.lock().unwrap() = None;
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
}
