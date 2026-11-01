//! Read-only login test against the dev server. `cargo test -p sonix-real -- --ignored`
//! with SONIX_ENV_FILE=<path to .env.local>. Credentials are read at runtime, never printed.
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
    unsafe { std::env::set_var("APPDATA", &tmp) };
    unsafe { std::env::set_var("LOCALAPPDATA", &tmp) };
    let kv = env_file();
    let get = |k: &str| kv.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone()).unwrap();
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
    let _ = std::fs::remove_dir_all(&tmp);
}
