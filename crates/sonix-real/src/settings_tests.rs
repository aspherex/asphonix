//! Tests for settings / continuity / scrobbling. Every server is a local 127.0.0.1 mock that records requests.
use super::*;
use sonix_core::audio::queue::{EntrySnapshot, QueueCore};
use sonix_core::audio::session::SessionState;
use sonix_core::ipc::{QueueItemOrigin, RepeatMode};
use sonix_core::subsonic::Client;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

static ENV: Mutex<()> = Mutex::new(());
static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A fresh config (`roaming`) + data (`local`) dir pair under `<build-dir>/tmp` (override: `SONIX_TEST_TMP`);
/// backends are opened with [`RealBackend::open_in`] on it, never on the process env (`APPDATA`/`LOCALAPPDATA` are
/// not touched). Tests still run one at a time (shared engine timing). Declare it FIRST in a test so backends drop
/// before it; the directory is removed on drop (also when the test panics), retrying while background threads
/// release SQLite.
struct Env {
    _g: MutexGuard<'static, ()>,
    root: std::path::PathBuf,
}
impl Env {
    fn new(tag: &str) -> Self {
        let g = ENV.lock().unwrap_or_else(|p| p.into_inner());
        let base = std::path::PathBuf::from(std::env::var("SONIX_TEST_TMP").unwrap_or_else(|_| "<build-dir>/tmp".into()));
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = base.join(format!("sonix-real-{tag}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("roaming")).unwrap();
        std::fs::create_dir_all(root.join("local")).unwrap();
        Env { _g: g, root }
    }
    /// Config dir of this test's backends (`connection.json`, `settings.json`, session, scrobble queue).
    fn cfg(&self) -> std::path::PathBuf {
        self.root.join("roaming").join(crate::IDENTIFIER)
    }
    /// Data dir (library DB, art cache).
    fn data(&self) -> std::path::PathBuf {
        self.root.join("local").join(crate::IDENTIFIER)
    }
    fn open(&self) -> RealBackend {
        RealBackend::open_in(self.cfg(), self.data())
    }
    /// Removes the dir, waiting up to 5 s for the closed backends' threads to release their files.
    fn cleanup(&self) -> bool {
        let t = Instant::now();
        loop {
            if std::fs::remove_dir_all(&self.root).is_ok() || !self.root.exists() {
                return true;
            }
            if t.elapsed() > Duration::from_secs(5) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
impl Drop for Env {
    fn drop(&mut self) {
        self.cleanup();
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
    let e = Env::new("settings");
    let b = e.open();
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
    // MCP is live, not just stored: the bridge listens in this test's config dir
    let st = b.mcp_status().unwrap();
    assert!(st.enabled && st.allow_control && st.listening, "{st:?}");
    assert!(e.cfg().join("mcp.json").exists());
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
    assert!(e.cfg().join("settings.json").exists());
    let b2 = e.open();
    assert_eq!(b2.settings_get().unwrap(), want);
    assert!(b2.0.prefs.autoplay() && b2.0.prefs.sync_play_queue() && b2.0.scrobbling.load(Ordering::Relaxed));
    // saved-on MCP: flags at once, the listener only after the startup delay
    let st = b2.mcp_status().unwrap();
    assert!(st.enabled && st.allow_control && !st.listening, "{st:?}");
    assert!(b2.art_cache_status().is_ok());
    drop(b2);
    // turning it off stops the bridge (and drops its handle on the backend) and persists the flag
    b.settings_set(SettingChange::McpEnabled(false)).unwrap();
    assert!(!b.mcp_status().unwrap().listening && !e.cfg().join("mcp.json").exists());
    assert!(!sonix_core::secrets::load_settings(&e.cfg()).mcp_enabled);
}

#[test]
fn server_queue_check_is_gated_by_sync_and_read_only() {
    let e = Env::new("srvq");
    let b = e.open();
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
    let e = Env::new("resume");
    let b = e.open();
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

/// Engine activity without audio: hands the engine a saved session (paused at `position_ms`, volume 0), waits until
/// its queue is in, then edits the queue the way the UI does (a mutating command, so the exit write has work to do).
fn restore_via_engine(b: &RealBackend, ids: &[&str], current: usize, position_ms: u64) {
    let state = SessionState { version: sonix_core::audio::session::SESSION_VERSION, saved_at_ms: 1, position_ms, volume: Some(0.0), queue: core_of(ids, current) };
    b.0.engine.send(EngineCommand::RestoreSession { state: Box::new(state), pools: None, force: false });
    let page = PageRequest { offset: 0, limit: 10 };
    wait_until("restored queue", || b.player_get_queue(page).map(|q| q.total == ids.len()).unwrap_or(false));
}

fn queue_ids(b: &RealBackend) -> Vec<String> {
    b.player_get_queue(PageRequest { offset: 0, limit: 10 }).unwrap().items.iter().map(|i| i.track.id.clone()).collect()
}

#[test]
fn exit_path_writes_the_session_and_pushes_the_play_queue_only_with_sync_on() {
    let e = Env::new("push");
    let b = e.open();
    seed(&b);
    let mock = Mock::start(empty_server);
    *b.0.client.write().unwrap() = Some(mock.client());
    restore_via_engine(&b, &["t1", "t2", "t3"], 1, 42_000);
    // default OFF: engine edit -> persist_now writes the local session, nothing goes to the server
    b.player_reorder_queue(0, 2).unwrap();
    assert_eq!(queue_ids(&b), ["t2", "t3", "t1"]);
    b.persist_now();
    let (saved, _) = sonix_core::audio::session::load(&b.0.dir).expect("session.json written by the exit path");
    let ids: Vec<_> = saved.queue.entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["t2", "t3", "t1"]);
    assert_eq!(saved.volume, Some(0.0));
    assert!(mock.requests().is_empty(), "sync OFF must not write to the server: {:?}", mock.requests());
    // ON: the same exit path pushes savePlayQueue
    b.settings_set(SettingChange::SyncPlayQueue(true)).unwrap();
    b.player_reorder_queue(0, 2).unwrap();
    assert_eq!(queue_ids(&b), ["t3", "t1", "t2"]);
    b.persist_now();
    let r = mock.requests();
    assert!(!r.is_empty() && r.iter().all(|q| q.starts_with("/rest/savePlayQueue?")), "{r:?}");
    let last = r.last().unwrap();
    assert!(last.contains("id=t3&id=t1&id=t2") && last.contains("current=t2"), "{last}");
}

#[test]
fn queue_and_position_survive_a_restart_through_the_exit_path() {
    let e = Env::new("restart");
    let b = e.open();
    seed(&b);
    restore_via_engine(&b, &["t1", "t2", "t3"], 1, 42_000);
    b.player_reorder_queue(0, 2).unwrap();
    let t = Instant::now();
    b.persist_now();
    assert!(t.elapsed() < Duration::from_secs(4));
    drop(b);
    let b2 = e.open();
    wait_until("restored queue", || queue_ids(&b2).len() == 3);
    assert_eq!(queue_ids(&b2), ["t2", "t3", "t1"]);
    let st = b2.player_get_state().unwrap();
    assert_eq!(st.track.map(|t| t.id), Some("t2".to_string()));
    assert_ne!(st.status, PlaybackStatus::Playing, "restore never starts playback");
    assert!((st.position_sec - 42.0).abs() < 1.0, "{}", st.position_sec);
    assert_eq!(st.volume, 0.0);
    drop(b2);
    assert!(e.cleanup(), "closed backends must release the temp dir");
}

#[test]
fn legacy_partial_or_broken_settings_files_leave_every_network_flag_off() {
    let e = Env::new("legacy");
    let cfg = e.cfg();
    std::fs::create_dir_all(&cfg).unwrap();
    let files = [
        "{}",
        r#"{"spread_artists_in_shuffle":true,"queue_window_size":40}"#,
        r#"{"someFutureField":1,"scrobblingEnabledTypo":true,"queue_window_size":30}"#,
        r#"{"scrobbling_enabled":"yes","autoplay_enabled":true}"#, // wrong type: the whole file is rejected
        "not json at all {{{",
        "",
    ];
    for f in files {
        std::fs::write(cfg.join("settings.json"), f).unwrap();
        let b = e.open();
        let s = b.settings_get().unwrap();
        assert!(
            !s.scrobbling_enabled && !s.autoplay_enabled && !s.sync_play_queue && !s.auto_check_updates && !s.mcp_enabled && !s.mcp_allow_control,
            "flags on for {f:?}: {s:?}"
        );
        assert!(!b.0.scrobbling.load(Ordering::Relaxed) && !b.0.prefs.autoplay() && !b.0.prefs.sync_play_queue(), "live flags on for {f:?}");
        assert!((10..=100).contains(&s.queue_window_size) && (10..=300).contains(&s.play_threshold_sec));
    }
    // a legacy file naming only some fields keeps those and defaults the rest to off
    std::fs::write(cfg.join("settings.json"), r#"{"scrobbling_enabled":true}"#).unwrap();
    let s = e.open().settings_get().unwrap();
    assert!(s.scrobbling_enabled && !s.autoplay_enabled && !s.sync_play_queue && !s.auto_check_updates);
    // and the Prefs default itself is opt-out
    let p = sonix_core::audio::Prefs::default();
    assert!(!p.autoplay() && !p.sync_play_queue());
}

#[test]
fn art_cache_status_counts_distinct_covers_not_size_buckets() {
    let e = Env::new("art");
    let b = e.open();
    b.0.db
        .with_tx(|tx| {
            tx.execute("INSERT INTO albums (id, name, artist, song_count, duration, cover_art) VALUES ('al1','A','X',1,1,'al-1'),('al2','B','X',1,1,'al-2')", [])?;
            tx.execute("INSERT INTO artists (id, name, album_count, cover_art) VALUES ('ar1','X',2,'ar-1')", [])?;
            Ok(())
        })
        .unwrap();
    let dir = e.data().join("art");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, n) in [("al-1_64.img", 10), ("al-1_160.img", 20), ("al-1_320.img", 30), ("ar-1_64.img", 5), ("playlist_x_64.img", 7)] {
        std::fs::write(dir.join(name), vec![0u8; n]).unwrap();
    }
    let s = b.art_cache_status().unwrap();
    assert_eq!((s.cached, s.total, s.bytes), (2, 3, 72));
    assert!(s.cached <= s.total);
}

#[test]
fn queued_scrobbles_replay_only_to_their_own_account() {
    let e = Env::new("acct");
    let mock = Mock::start(empty_server);
    let client = mock.client();
    let mine = sonix_core::scrobble::service::account_of(&client);
    let q = sonix_core::scrobble::queue::ScrobbleQueue::open(&e.cfg()).unwrap();
    q.enqueue_for(&mine, "mine-1", 1_000).unwrap();
    q.enqueue_for("bob@http://other.example", "theirs-1", 2_000).unwrap();
    q.enqueue_for("", "legacy-1", 3_000).unwrap();
    sonix_core::rt::block_on(sonix_core::scrobble::service::drain_for(&client, &q));
    let r = mock.requests();
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].starts_with("/rest/scrobble?") && r[0].contains("id=mine-1") && r[0].contains("time=1000"), "{}", r[0]);
    assert!(q.is_empty().unwrap(), "other accounts' rows are dropped, not replayed");
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
    let e = Env::new("scrobble");
    let b = e.open();
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

// ---------------------------------------------------------------------------------------------------------------
// "play from row N": what the queue holds afterwards (reference `queue.rs set_source_inner` / `set_queue`).
// A rolling source (playlist/liked/smart/artist/genre/all tracks, `startTrackId`) starts AT the clicked row: in order
// the pool continues from it (`pool[idx..]`, `queue.rs:323`), shuffle swaps it to the front of a fresh permutation.
// A static queue (`playQueue`: album, artist top tracks, search, home) keeps the whole list with `current = N`.
// ---------------------------------------------------------------------------------------------------------------

fn seed_five(b: &RealBackend) {
    b.0.db
        .with_tx(|tx| {
            tx.execute("INSERT INTO artists (id, name, album_count) VALUES ('ar1','Alpha',1)", [])?;
            for i in 1..=5 {
                tx.execute(&format!(
                    "INSERT INTO tracks (id, title, artist, artist_id, album, album_id, genre, duration, suffix, sampling_rate, channel_count, starred)
                     VALUES ('t{i}', 'Song {i}', 'Alpha', 'ar1', 'A1', 'al1', 'Rock', 200, 'flac', 44100, 2, '2026-01-01T00:00:00Z')"
                ), [])?;
            }
            tx.execute("INSERT INTO playlists (id,name,owner,readonly) VALUES ('pl1','Mix','me',0)", [])?;
            for i in 1..=5 {
                tx.execute(&format!("INSERT INTO playlist_tracks (playlist_id,position,track_id) VALUES ('pl1',{},'t{i}')", i - 1), [])?;
            }
            Ok(())
        })
        .unwrap();
}

/// 20 s of 16-bit mono 8 kHz silence (a decodable stream, so the engine does not skip through the queue on failed opens).
fn silent_wav() -> Vec<u8> {
    let data = 8000u32 * 2 * 20;
    let mut v = Vec::with_capacity(44 + data as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    for x in [16u32.to_le_bytes().as_slice(), &1u16.to_le_bytes(), &1u16.to_le_bytes(), &8000u32.to_le_bytes(), &16000u32.to_le_bytes(), &2u16.to_le_bytes(), &16u16.to_le_bytes()] {
        v.extend_from_slice(x);
    }
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data.to_le_bytes());
    v.resize(44 + data as usize, 0);
    v
}

/// 127.0.0.1 server: `/rest/stream` = the silent WAV, everything else the empty OK envelope.
fn wav_server() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let line = String::from_utf8_lossy(&buf).lines().next().unwrap_or("").to_string();
                let (ty, body) = if line.contains("/rest/stream") { ("audio/wav", silent_wav()) } else { ("application/json", OK.as_bytes().to_vec()) };
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(&body);
            });
        }
    });
    url
}

/// The queue once `ready` holds for it. Each wait names a property only the NEW queue has (source kind / mode, no source),
/// so it can never be satisfied by the previous step's queue.
fn queue_when(b: &RealBackend, what: &str, ready: impl Fn(&QueueState) -> bool) -> QueueState {
    let page = PageRequest { offset: 0, limit: 50 };
    wait_until(what, || b.player_get_queue(page).map(|q| ready(&q)).unwrap_or(false));
    b.player_get_queue(page).unwrap()
}

#[test]
fn rolling_sources_start_at_the_clicked_row_and_static_queues_keep_the_earlier_rows() {
    let e = Env::new("playfrom");
    let b = e.open();
    seed_five(&b);
    let url = wav_server();
    *b.0.client.write().unwrap() = Some(Client::new(&url, "u", "p").unwrap());
    b.player_set_volume(SetVolumeRequest { volume: 0.0 }).unwrap();
    let ids = |q: &QueueState| q.items.iter().map(|i| i.track.id.as_str().to_owned()).collect::<Vec<_>>();
    // the engine may start (silent) playback and advance; the rows it holds, history included, do not change.
    let playing = |b: &RealBackend| b.player_get_state().map(|s| s.status == PlaybackStatus::Playing).unwrap_or(false);
    let rx = b.subscribe();
    let queue_events = || std::iter::from_fn(|| rx.try_recv().ok()).filter(|e| matches!(e, Event::QueueChanged)).count();

    // playlist, in order, from row 3: the pool continues from t3 (no earlier rows)
    b.player_play_playlist(PlaySourceRequest { id: "pl1".into(), start_track_id: Some("t3".into()), ..Default::default() }).unwrap();
    let q = queue_when(&b, "playlist source", |q| q.source.as_ref().is_some_and(|s| s.kind == SourceKind::Playlist));
    assert_eq!(ids(&q), ["t3", "t4", "t5"]);
    let s = q.source.clone().unwrap();
    assert_eq!((s.id.as_deref(), s.mode), (Some("pl1"), SourceMode::InOrder));
    if playing(&b) {
        assert_eq!(q.current_index, 0, "the clicked row is current");
    }
    assert!(queue_events() >= 1, "a source change tells the UI (QueueChanged)");

    // liked songs: same
    b.player_play_liked(PlaySourceRequest { start_track_id: Some("t2".into()), ..Default::default() }).unwrap();
    let q = queue_when(&b, "liked source", |q| q.source.as_ref().is_some_and(|s| s.kind == SourceKind::Liked));
    assert_eq!(ids(&q), ["t2", "t3", "t4", "t5"]);

    // playlist, shuffle, from row 4: every row once, the clicked one first
    b.player_play_playlist(PlaySourceRequest { id: "pl1".into(), shuffle: true, start_track_id: Some("t4".into()), ..Default::default() }).unwrap();
    let q = queue_when(&b, "shuffle source", |q| q.source.as_ref().is_some_and(|s| s.mode == SourceMode::Shuffle));
    let got = ids(&q);
    assert_eq!(got[0], "t4");
    let mut sorted = got.clone();
    sorted.sort();
    assert_eq!(sorted, ["t1", "t2", "t3", "t4", "t5"]);

    // (shuffle is a player setting that outlives the source: a static queue honours it, so switch it off first)
    b.player_set_shuffle(false).unwrap();
    wait_until("shuffle off", || !b.player_get_state().unwrap().shuffle);
    // a static queue (album detail row, artist top tracks, search, home): the whole list, current = N
    let all: Vec<String> = (1..=5).map(|i| format!("t{i}")).collect();
    b.player_play_queue(PlayQueueRequest { track_ids: all.clone(), start_index: 2 }).unwrap();
    let q = queue_when(&b, "static queue", |q| q.source.is_none() && q.total == 5);
    assert_eq!(ids(&q), all);
    if playing(&b) {
        assert_eq!(q.current_index, 2);
    }
}

/// `session.json` as another client (or an older run) left it: t1..t3, current t2 at 112.676 s, volume 0.705.
fn write_saved_session(e: &Env) -> String {
    std::fs::create_dir_all(e.cfg()).unwrap();
    let state = SessionState {
        version: sonix_core::audio::session::SESSION_VERSION,
        saved_at_ms: 1,
        position_ms: 112_676,
        volume: Some(0.705),
        queue: core_of(&["t1", "t2", "t3"], 1),
    };
    let json = serde_json::to_string(&state).unwrap();
    std::fs::write(e.cfg().join(sonix_core::audio::session::SESSION_FILE), &json).unwrap();
    json
}

/// The library is behind the session (its tracks were added on the server after the last sync): the 300 ms restore
/// skips it, and the first successful sync, which brought the tracks in, offers it again: paused at the saved
/// position and volume, and the saved session is not rewritten by the restore alone.
#[test]
fn a_session_skipped_for_unknown_tracks_is_restored_after_the_first_sync() {
    let e = Env::new("restore-retry");
    let saved = write_saved_session(&e);
    let b = e.open();
    std::thread::sleep(Duration::from_millis(600)); // the startup restore ran against the empty library
    let st = b.player_get_state().unwrap();
    assert_eq!(st.status, PlaybackStatus::Stopped);
    assert!(st.track.is_none() && queue_ids(&b).is_empty());
    seed(&b); // what the startup sync adds
    b.after_first_sync();
    wait_until("restored queue", || queue_ids(&b).len() == 3);
    let st = b.player_get_state().unwrap();
    assert_eq!(st.status, PlaybackStatus::Paused);
    assert_eq!(st.track.map(|t| t.id), Some("t2".to_string()));
    assert!((st.position_sec - 112.676).abs() < 1e-6, "{}", st.position_sec);
    assert!((st.volume - 0.705).abs() < 1e-6, "{}", st.volume);
    std::thread::sleep(Duration::from_millis(300));
    let on_disk = std::fs::read_to_string(e.cfg().join(sonix_core::audio::session::SESSION_FILE)).unwrap();
    assert_eq!(on_disk, saved, "restoring alone must not rewrite the saved session");
    // only once per run: a later sync does not re-offer it (the user may have cleared the queue meanwhile)
    assert!(!b.0.restore_retry.load(std::sync::atomic::Ordering::SeqCst));
}

/// Whatever the user started before the first sync finished wins over the deferred restore.
#[test]
fn the_deferred_restore_never_replaces_a_queue_the_user_started() {
    let e = Env::new("restore-retry-user");
    write_saved_session(&e);
    let b = e.open();
    std::thread::sleep(Duration::from_millis(600));
    seed(&b);
    b.player_play_queue(PlayQueueRequest { track_ids: vec!["t3".into(), "t1".into()], start_index: 0 }).unwrap();
    wait_until("user queue", || queue_ids(&b).len() == 2);
    b.after_first_sync();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(queue_ids(&b), ["t3", "t1"]);
    assert_ne!(b.player_get_state().unwrap().track.map(|t| t.id), Some("t2".to_string()));
}

/// The user queued something and emptied the queue again before the first sync finished (the session file may still
/// hold the old queue: writes are debounced): the deferred restore must not bring the saved session back.
#[test]
fn the_deferred_restore_never_refills_a_queue_the_user_emptied() {
    let e = Env::new("restore-retry-emptied");
    write_saved_session(&e);
    let b = e.open();
    std::thread::sleep(Duration::from_millis(600));
    seed(&b);
    b.player_enqueue(EnqueueRequest { track_ids: vec!["t3".into()], position: EnqueuePosition::End }).unwrap();
    wait_until("enqueued", || queue_ids(&b).len() == 1);
    b.player_remove_from_queue(0).unwrap();
    wait_until("emptied", || queue_ids(&b).is_empty());
    b.after_first_sync();
    std::thread::sleep(Duration::from_millis(300));
    assert!(queue_ids(&b).is_empty(), "{:?}", queue_ids(&b));
    assert_eq!(b.player_get_state().unwrap().status, PlaybackStatus::Stopped);
}

/// Logging out disarms the deferred restore: the next account's first sync must not get the old account's session.
#[test]
fn logout_disarms_the_deferred_restore() {
    let e = Env::new("restore-retry-logout");
    write_saved_session(&e);
    let b = e.open();
    std::thread::sleep(Duration::from_millis(600));
    let _ = b.auth_logout();
    assert!(!b.0.restore_retry.load(std::sync::atomic::Ordering::SeqCst));
}
