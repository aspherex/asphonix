//! Ignored, read-only integration test (login, profiles, restore, logout) against any Navidrome/Subsonic server you point it at.
//! Run `cargo test -p sonix-real -- --ignored` with `SONIX_ENV_FILE=<path>`, a `KEY=value` file holding
//! `SONIX_DEV_SERVER`, `SONIX_DEV_USER` and `SONIX_DEV_PASS`. Credentials are read at runtime, never printed.
use sonix_api::*;
use sonix_real::RealBackend;

fn env_file() -> Vec<(String, String)> {
    let path = std::env::var("SONIX_ENV_FILE").expect("SONIX_ENV_FILE");
    std::fs::read_to_string(path).unwrap().lines().filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))).collect()
}

#[test]
#[ignore]
fn login_profiles_restore_logout() {
    let tmp = std::env::temp_dir().join(format!("sonix-real-test-{}", std::process::id()));
    // SAFETY: single-threaded test setup before any other thread reads the environment.
    unsafe { std::env::set_var("APPDATA", &tmp) };
    // SAFETY: single-threaded test setup before any other thread reads the environment.
    unsafe { std::env::set_var("LOCALAPPDATA", &tmp) };
    let kv = env_file();
    let get = |k: &str| kv.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone()).unwrap();
    // The real logging stack (verbose, JSON file in the temp dir) plus info-and-up lines on stderr.
    let mut cfg = sonix_logging::LogConfig::new(Some(tmp.join("logs")), "sonix", env!("CARGO_PKG_VERSION"));
    cfg.verbose = true;
    cfg.forward = Some(Box::new(|l| eprintln!("LOG {l}")));
    let log_guard = sonix_logging::init(cfg);
    let log_file = log_guard.file().map(std::path::Path::to_path_buf);
    let b = RealBackend::open();
    assert_eq!(b.auth_session_status().unwrap().state, ConnectionState::Disconnected);
    // wrong password must fail and change nothing
    let bad = b.auth_connect(ConnectRequest { url: get("SONIX_DEV_SERVER"), username: get("SONIX_DEV_USER"), password: "definitely-wrong".into() });
    assert!(bad.is_err());
    assert!(!RealBackend::has_session());
    let rx = b.subscribe();
    let caps = b.auth_connect(ConnectRequest { url: get("SONIX_DEV_SERVER"), username: get("SONIX_DEV_USER"), password: get("SONIX_DEV_PASS") }).expect("connect");
    println!("server={} v{} api={} ping={}ms", caps.server_name, caps.server_version, caps.api_version, caps.ping_ms);
    // wait for the initial full sync
    let t0 = std::time::Instant::now();
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(120)) {
            Ok(Event::SyncProgress(p)) if p.phase == SyncPhase::Done => break,
            Ok(Event::SyncProgress(p)) if p.phase == SyncPhase::Error => panic!("sync error: {:?}", p.message),
            Ok(_) => {}
            Err(_) => panic!("sync timeout"),
        }
    }
    println!("sync took {:?}", t0.elapsed());
    let tracks = b.browse_get_tracks(GetTracksRequest { offset: 0, limit: 5, sort: TrackSortField::Title, dir: SortDir::Asc, filter: None }).unwrap();
    println!("tracks total={} first={:?}", tracks.total, tracks.items.first().map(|t| t.title.clone()));
    assert!(tracks.total > 0);
    let albums = b.browse_get_albums(GetAlbumsRequest { offset: 0, limit: 5, sort: AlbumSort::Alphabetical, filter: None }).unwrap();
    println!("albums total={}", albums.total);
    let artists = b.browse_get_artists(GetArtistsRequest { offset: 0, limit: 5, sort: None }).unwrap();
    println!("artists total={}", artists.total);
    let pls = b.playlists_list().unwrap();
    println!("playlists={}", pls.len());
    println!("liked={}", b.liked_count().unwrap());
    if let Some(cv) = albums.items.iter().find_map(|a| a.cover_art.clone()) { let px = b.art_get(&cv, 160).unwrap(); println!("art {}x{} {}B", px.width, px.height, px.rgba.len()); assert!(px.width > 0); }
    if let Some(a) = albums.items.first() { let d = b.browse_get_album(&a.id).unwrap(); println!("album detail tracks={}", d.tracks.len()); }
    // playback: shared mode, about -45 dB (taper: gain_db = 60*(p-1) -> p = 0.25)
    b.player_set_volume(SetVolumeRequest { volume: 0.25 }).unwrap();
    let ids: Vec<String> = tracks.items.iter().take(3).map(|t| t.id.clone()).collect();
    b.player_play_queue(PlayQueueRequest { track_ids: ids, start_index: 0 }).unwrap();
    let mut played = None;
    for _ in 0..150 {
        let ps = b.player_get_state().unwrap();
        if ps.status == PlaybackStatus::Playing && ps.position_sec > 1.5 { played = Some(ps); break; }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let ps = played.unwrap_or_else(|| { let s = b.player_get_state().unwrap(); panic!("playback did not start: status={:?} err={:?} pos={} track={:?}", s.status, s.error, s.position_sec, s.track.as_ref().map(|t| t.id.clone())) });
    println!("playing {:?} pos={:.2}s mode={:?} fmt={:?} rate={:?} bits={:?} vol={:.2}", ps.track.as_ref().map(|t| t.title.clone()), ps.position_sec, ps.output_mode, ps.format_label, ps.sample_rate, ps.bit_depth, ps.volume);
    assert_eq!(ps.output_mode, OutputMode::Shared);
    b.player_next().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let ps2 = b.player_get_state().unwrap();
    println!("after next: {:?} {:?}", ps2.status, ps2.track.as_ref().map(|t| t.title.clone()));
    // rolling sources: shuffle the first playlist with tracks
    if let Some(p) = pls.iter().find(|p| p.song_count > 0) {
        b.player_play_playlist(PlaySourceRequest { id: p.id.clone(), shuffle: true, start_track_id: None, seed: 1, flow: None }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2500));
        let q = b.player_get_queue(PageRequest { offset: 0, limit: 50 }).unwrap();
        println!("playlist source: status={:?} queue total={} source={:?}", b.player_get_state().unwrap().status, q.total, q.source.as_ref().map(|s| s.label.clone()));
    }
    b.player_pause().unwrap();
    let q = b.player_get_queue(PageRequest { offset: 0, limit: 10 }).unwrap();
    println!("queue total={}", q.total);
    let out = b.output_get_status().unwrap();
    println!("output: {:?}", out);
    let st = b.auth_session_status().unwrap();
    assert_eq!(st.state, ConnectionState::Connected);
    println!("tracks={:?}", st.track_count);
    assert!(RealBackend::has_session());
    let p = b.profiles_list().unwrap();
    assert_eq!(p.len(), 1);
    assert!(p[0].active);
    // restart: a second backend reconnects from config + keychain
    drop(b);
    let b2 = RealBackend::open();
    for _ in 0..100 {
        if b2.auth_session_status().unwrap().state == ConnectionState::Connected { break; }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(b2.auth_session_status().unwrap().state, ConnectionState::Connected, "restore from keychain");
    b2.auth_logout().unwrap();
    assert_eq!(b2.auth_session_status().unwrap().state, ConnectionState::Disconnected);
    assert!(b2.profiles_list().unwrap().is_empty());
    drop(log_guard);
    let log = log_file.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    assert!(!log.is_empty(), "the session wrote a log file");
    assert!(!log.contains(&get("SONIX_DEV_PASS")), "the password never reaches the log");
    assert!(!log.contains("&t=") && !log.contains("?t="), "no Subsonic auth token in the log");
    let _ = std::fs::remove_dir_all(&tmp);
}
