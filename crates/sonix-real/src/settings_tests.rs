//! Tests for settings / continuity / scrobbling. Every server is a local 127.0.0.1 mock that records requests.
use super::*;
use sonix_core::audio::queue::{EntrySnapshot, QueueCore};
use sonix_core::audio::session::{PushKind, SaveRequest};
use sonix_core::ipc::{QueueItemOrigin, RepeatMode};
use sonix_core::subsonic::Client;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

static ENV: Mutex<()> = Mutex::new(());

/// Serializes tests (process-wide env vars) and points `APPDATA`/`LOCALAPPDATA` at a fresh temp dir.
struct Env {
    _g: MutexGuard<'static, ()>,
    root: std::path::PathBuf,
}
impl Env {
    fn new(tag: &str) -> Self {
        let g = ENV.lock().unwrap_or_else(|p| p.into_inner());
        let base = std::env::var("SONIX_TEST_TMP").map(std::path::PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
        let root = base.join(format!("sonix-real-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("roaming")).unwrap();
        std::fs::create_dir_all(root.join("local")).unwrap();
        unsafe {
            std::env::set_var("APPDATA", root.join("roaming"));
            std::env::set_var("LOCALAPPDATA", root.join("local"));
        }
        Env { _g: g, root }
    }
}
impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// 127.0.0.1 mock Subsonic server recording `path?query` of every request.
struct Mock {
    url: String,
    reqs: Arc<Mutex<Vec<String>>>,
}
impl Mock {
    fn start(body_for: fn(&str) -> String) -> Mock {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let reqs: Arc<Mutex<Vec<String>>> = Arc::default();
        let r2 = reqs.clone();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let line = String::from_utf8_lossy(&buf).lines().next().unwrap_or("").to_string();
                let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let body = body_for(&target);
                r2.lock().unwrap().push(target);
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            }
        });
        Mock { url, reqs }
    }
    fn client(&self) -> Client {
        Client::new(&self.url, "u", "p").unwrap()
    }
    fn requests(&self) -> Vec<String> {
        self.reqs.lock().unwrap().clone()
    }
    fn count(&self, endpoint: &str) -> usize {
        self.requests().iter().filter(|r| r.starts_with(&format!("/rest/{endpoint}?"))).count()
    }
}

const OK: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#;

fn play_queue_server(target: &str) -> String {
    if target.starts_with("/rest/getPlayQueue?") {
        r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playQueue":{"entry":[{"id":"t2","title":"Two","suffix":"flac"},{"id":"t3","title":"Three","suffix":"flac"}],"current":"t2","position":5000,"changed":"2026-10-11T09:30:00Z","changedBy":"phone"}}}"#.into()
    } else {
        OK.into()
    }
}
fn empty_server(_: &str) -> String {
    OK.into()
}
fn scrobble_server(target: &str) -> String {
    if target.starts_with("/rest/getOpenSubsonicExtensions") {
        r#"{"subsonic-response":{"status":"ok","version":"1.16.1","openSubsonic":true,"openSubsonicExtensions":[]}}"#.into()
    } else {
        OK.into()
    }
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(5), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn seed(b: &RealBackend) {
    b.0.db
        .with_tx(|tx| {
            tx.execute("INSERT INTO artists (id, name, album_count) VALUES ('ar1','Alpha',1)", [])?;
            tx.execute("INSERT INTO albums (id, name, artist, artist_id, genre, song_count, duration) VALUES ('al1','A1','Alpha','ar1','Rock',3,300)", [])?;
            tx.execute(
                "INSERT INTO tracks (id, title, artist, artist_id, album, album_id, genre, rating, duration, suffix, sampling_rate, channel_count) VALUES
                 ('t1','One','Alpha','ar1','A1','al1','Rock',NULL,200,'flac',44100,2),
                 ('t2','Two','Alpha','ar1','A1','al1','Rock',NULL,200,'flac',44100,2),
                 ('t3','Three','Alpha','ar1','A1','al1','Rock',NULL,200,'flac',44100,2)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
}

fn core_of(ids: &[&str], current: usize) -> QueueCore {
    QueueCore {
        entries: ids.iter().map(|id| EntrySnapshot { id: id.to_string(), origin: QueueItemOrigin::Source }).collect(),
        current: Some(current),
        shuffle: false,
        repeat: RepeatMode::Off,
        static_natural_order: ids.iter().map(|s| s.to_string()).collect(),
        source: None,
    }
}

#[test]
fn defaults_are_off_and_every_field_roundtrips_and_persists() {
    let _e = Env::new("settings");
    let b = RealBackend::open();
    let d = b.settings_get().unwrap();
    assert!(!d.scrobbling_enabled && !d.autoplay_enabled && !d.sync_play_queue && !d.auto_check_updates && !d.mcp_enabled && !d.mcp_allow_control);
    assert_eq!((d.queue_window_size, d.play_threshold_sec, d.replay_gain_mode), (20, 30, ReplayGainMode::Off));
    // the UI sends exactly these variants; each must come back from settings_get
    let changes = [
        SettingChange::Scrobbling(true),
        SettingChange::AutoCheckUpdates(true),
        SettingChange::SpreadArtists(true),
        SettingChange::QueueWindowSize(55),
        SettingChange::Autoplay(true),
        SettingChange::SyncPlayQueue(true),
        SettingChange::ReplayGain(ReplayGainMode::Album),
        SettingChange::PlayThreshold(120),
        SettingChange::TrackOverlay(false),
        SettingChange::TrackOverlayCorner(OverlayCorner::BottomRight),
        SettingChange::FreeMemoryWhenMinimized(false),
        SettingChange::McpEnabled(true),
        SettingChange::McpAllowControl(true),
    ];
    for c in changes {
        b.settings_set(c).unwrap();
    }
    let want = Settings {
        scrobbling_enabled: true,
        auto_check_updates: true,
        spread_artists_in_shuffle: true,
        queue_window_size: 55,
        autoplay_enabled: true,
        sync_play_queue: true,
        play_threshold_sec: 120,
        free_memory_when_minimized: false,
        replay_gain_mode: ReplayGainMode::Album,
        track_overlay: false,
        track_overlay_corner: OverlayCorner::BottomRight,
        mcp_enabled: true,
        mcp_allow_control: true,
    };
    assert_eq!(b.settings_get().unwrap(), want);
    // live effects
    assert!(b.0.scrobbling.load(Ordering::Relaxed) && b.0.prefs.autoplay() && b.0.prefs.sync_play_queue());
    assert_eq!(b.0.prefs.replay_gain(), sonix_core::audio::replaygain::ReplayGainMode::Album);
    assert_eq!(b.0.prefs.play_threshold_sec(), 120);
    // out-of-range values are clamped like the reference
    b.settings_set(SettingChange::QueueWindowSize(5)).unwrap();
    b.settings_set(SettingChange::PlayThreshold(5)).unwrap();
    let s = b.settings_get().unwrap();
    assert_eq!((s.queue_window_size, s.play_threshold_sec), (10, 10));
    assert_eq!(b.0.prefs.play_threshold_sec(), 10);
    b.settings_set(SettingChange::QueueWindowSize(55)).unwrap();
    b.settings_set(SettingChange::PlayThreshold(120)).unwrap();
    // persisted: a second backend over the same dirs reads the file and applies it at startup
    assert!(crate::config_dir().join("settings.json").exists());
    let b2 = RealBackend::open();
    assert_eq!(b2.settings_get().unwrap(), want);
    assert!(b2.0.prefs.autoplay() && b2.0.prefs.sync_play_queue() && b2.0.scrobbling.load(Ordering::Relaxed));
    assert!(!b2.mcp_status().unwrap().listening);
    assert!(b2.art_cache_status().is_ok());
}

#[test]
fn server_queue_check_is_gated_by_sync_and_read_only() {
    let _e = Env::new("srvq");
    let b = RealBackend::open();
    seed(&b);
    let mock = Mock::start(play_queue_server);
    *b.0.client.write().unwrap() = Some(mock.client());
    // sync OFF (default): not even a request
    assert!(b.session_check_server_queue().unwrap().is_none());
    assert!(mock.requests().is_empty(), "{:?}", mock.requests());
    // sync ON: getPlayQueue, offer built from the local library
    b.settings_set(SettingChange::SyncPlayQueue(true)).unwrap();
    let offer = b.session_check_server_queue().unwrap().expect("offer");
    assert_eq!(offer.track.id, "t2");
    assert_eq!((offer.position_sec, offer.track_count), (5.0, 2));
    assert_eq!(offer.changed_by.as_deref(), Some("phone"));
    assert_eq!(mock.count("getPlayQueue"), 1);
    assert_eq!(mock.requests().len(), 1, "read-only: {:?}", mock.requests());
}

#[test]
fn resume_from_server_reads_the_queue_and_errors_without_one() {
    let _e = Env::new("resume");
    let b = RealBackend::open();
    assert_eq!(b.session_resume_from_server().unwrap_err().code, ErrorCode::NotConnected);
    let none = Mock::start(empty_server);
    *b.0.client.write().unwrap() = Some(none.client());
    assert_eq!(b.session_resume_from_server().unwrap_err().code, ErrorCode::NotFound);
    // ids unknown to the (empty) local library are dropped by the engine: nothing plays
    let mock = Mock::start(play_queue_server);
    *b.0.client.write().unwrap() = Some(mock.client());
    b.session_resume_from_server().unwrap();
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(mock.count("getPlayQueue"), 1);
}

#[test]
fn save_play_queue_push_only_with_sync_on() {
    let _e = Env::new("push");
    let b = RealBackend::open();
    let mock = Mock::start(empty_server);
    *b.0.client.write().unwrap() = Some(mock.client());
    let req = || SaveRequest { queue: Some(core_of(&["t1", "t2", "t3"], 1)), pools: None, position_ms: 42_000, volume: 0.5, push: PushKind::Final };
    // default OFF: the exit write is local only
    assert!(b.0.store.save_final(req(), Duration::from_secs(3)));
    assert!(mock.requests().is_empty(), "{:?}", mock.requests());
    b.settings_set(SettingChange::SyncPlayQueue(true)).unwrap();
    assert!(b.0.store.save_final(req(), Duration::from_secs(3)));
    let r = mock.requests();
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].starts_with("/rest/savePlayQueue?"), "{}", r[0]);
    assert!(r[0].contains("id=t1&id=t2&id=t3") && r[0].contains("current=t2") && r[0].contains("position=42000"), "{}", r[0]);
}

#[test]
fn queue_and_position_survive_a_restart_and_persist_now_returns() {
    let _e = Env::new("restart");
    let b = RealBackend::open();
    seed(&b);
    let save = SaveRequest { queue: Some(core_of(&["t1", "t2", "t3"], 1)), pools: None, position_ms: 42_000, volume: 0.5, push: PushKind::None };
    assert!(b.0.store.save_final(save, Duration::from_secs(3)));
    let t = Instant::now();
    b.persist_now();
    assert!(t.elapsed() < Duration::from_secs(4));
    let b2 = RealBackend::open();
    let page = PageRequest { offset: 0, limit: 10 };
    wait_until("restored queue", || b2.player_get_queue(page).map(|q| q.total == 3).unwrap_or(false));
    let q = b2.player_get_queue(page).unwrap();
    assert_eq!(q.current_index, 1);
    let ids: Vec<_> = q.items.iter().map(|i| i.track.id.clone()).collect();
    assert_eq!(ids, ["t1", "t2", "t3"]);
    let st = b2.player_get_state().unwrap();
    assert_eq!(st.track.map(|t| t.id), Some("t2".to_string()));
    assert_ne!(st.status, PlaybackStatus::Playing, "restore never starts playback");
    assert!((st.position_sec - 42.0).abs() < 1.0, "{}", st.position_sec);
}

/// Listens to 0..=`to` seconds of "t1" (200 s long) through a watch channel.
fn play_t1(b: &RealBackend, tx: &tokio::sync::watch::Sender<sonix_core::ipc::PlayerState>, to: f64) {
    let track = sonix_core::library::get_tracks_by_ids(&b.0.db, &["t1".to_string()]).unwrap().remove(0);
    let mut s = b.0.engine.last_player_state();
    s.status = sonix_core::ipc::PlaybackStatus::Playing;
    s.track = Some(track);
    s.duration_sec = 200.0;
    let mut p = 0.0;
    while p <= to {
        s.position_sec = p;
        let _ = tx.send(s.clone());
        std::thread::sleep(Duration::from_millis(8));
        p += 0.5;
    }
}

fn history_rows(b: &RealBackend) -> (i64, i64) {
    b.0.db.read(|c| c.query_row("SELECT COUNT(*), COALESCE(SUM(scrobbled),0) FROM play_history", [], |r| Ok((r.get(0)?, r.get(1)?)))).unwrap()
}

#[test]
fn plays_are_recorded_locally_but_scrobbled_only_when_enabled() {
    let _e = Env::new("scrobble");
    let b = RealBackend::open();
    seed(&b);
    let mock = Mock::start(scrobble_server);
    *b.0.client.write().unwrap() = Some(mock.client());
    b.settings_set(SettingChange::PlayThreshold(10)).unwrap();
    // a private watch channel drives a second service over the same shared flags (the engine itself never plays here)
    let (tx, rx) = tokio::sync::watch::channel(b.0.engine.last_player_state());
    let ctx = sonix_core::scrobble::service::ScrobbleCtx {
        client: b.0.client.clone(),
        db: b.0.db.clone(),
        prefs: b.0.prefs.clone(),
        scrobbling: b.0.scrobbling.clone(),
        data_dir: b.0.dir.clone(),
    };
    sonix_core::scrobble::service::spawn(ctx, rx);
    // OFF (default): history row, no server traffic
    play_t1(&b, &tx, 12.0);
    wait_until("history row", || history_rows(&b).0 == 1);
    assert_eq!(history_rows(&b), (1, 0), "recorded, not marked scrobbled");
    std::thread::sleep(Duration::from_millis(300));
    assert!(mock.requests().is_empty(), "scrobbling OFF must send nothing: {:?}", mock.requests());
    // ON: stop, then a new play sends now-playing + a counted submission
    let mut idle = b.0.engine.last_player_state();
    idle.track = None;
    let _ = tx.send(idle);
    std::thread::sleep(Duration::from_millis(50));
    b.settings_set(SettingChange::Scrobbling(true)).unwrap();
    play_t1(&b, &tx, 12.0);
    wait_until("scrobble request", || mock.requests().iter().any(|r| r.starts_with("/rest/scrobble?") && r.contains("submission=true")));
    let r = mock.requests();
    let sub = r.iter().find(|r| r.contains("submission=true")).unwrap();
    assert!(sub.contains("id=t1") && sub.contains("time="), "{sub}");
    assert!(r.iter().any(|r| r.contains("submission=false") && r.contains("id=t1")), "now-playing: {r:?}");
    wait_until("second history row", || history_rows(&b).0 == 2);
    assert_eq!(history_rows(&b), (2, 1));
}
