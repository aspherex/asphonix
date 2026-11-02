//! Write paths against a local mock Subsonic server (127.0.0.1, random port). Never contacts a real server.
use super::*;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

/// Mock server: records every request target (`/rest/<endpoint>?...`), answers with canned Subsonic JSON.
struct Mock {
    url: String,
    log: Arc<Mutex<Vec<String>>>,
}

impl Mock {
    fn start() -> Mock {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        std::thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(mut sock) = sock else { break };
                let mut buf = [0u8; 8192];
                let n = sock.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = req.lines().next().unwrap_or("").to_string();
                let target = line.split(' ').nth(1).unwrap_or("").to_string();
                l2.lock().unwrap().push(target.clone());
                let ep = target.trim_start_matches("/rest/").split('?').next().unwrap_or("").to_string();
                let song = |id: &str| format!(r#"{{"id":"{id}","title":"T {id}","suffix":"flac","artist":"A"}}"#);
                let pl = |name: &str| format!(r#""id":"pl9","name":"{name}","owner":"me","songCount":2,"duration":20.0,"readonly":false"#);
                let inner = match ep.as_str() {
                    "createPlaylist" => format!(r#","playlist":{{{}}}"#, pl("Mix")),
                    "getPlaylist" => format!(r#","playlist":{{{},"entry":[{},{}]}}"#, pl("Mix"), song("t1"), song("t2")),
                    _ => String::new(),
                };
                let body = format!(r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1"{inner}}}}}"#);
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                let _ = sock.write_all(resp.as_bytes());
            }
        });
        Mock { url, log }
    }
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }
}

fn has(reqs: &[String], endpoint: &str, params: &[&str]) -> bool {
    reqs.iter().any(|r| r.starts_with(&format!("/rest/{endpoint}?")) && params.iter().all(|p| r.contains(p)))
}

fn exec(b: &RealBackend, sql: &str) {
    b.0.db.with_conn(|c| c.execute_batch(sql)).unwrap();
}

fn track_col(b: &RealBackend, id: &str, col: &str) -> Option<String> {
    b.0.db.read(|c| c.query_row(&format!("SELECT CAST({col} AS TEXT) FROM tracks WHERE id='{id}'"), [], |r| r.get(0))).unwrap()
}

#[test]
fn writes_hit_the_mock_server_and_mirror_locally() {
    // Isolated data/config dirs; no keychain is touched (no saved connection).
    let tmp = std::env::temp_dir().join(format!("sonix-writes-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    unsafe { std::env::set_var("APPDATA", tmp.join("roaming")) };
    unsafe { std::env::set_var("LOCALAPPDATA", tmp.join("local")) };
    let b = RealBackend::open();
    let rx = b.subscribe();
    let mock = Mock::start();
    exec(
        &b,
        "INSERT INTO tracks (id,title,artist,suffix,duration) VALUES ('t1','One','A','flac',10),('t2','Two','A','flac',10),('t3','Three','A','flac',10);
         INSERT INTO albums (id,name,artist) VALUES ('al1','Alb','A');
         INSERT INTO playlists (id,name,owner,readonly) VALUES ('ro','Smart','me',1),('theirs','Theirs','other',NULL),('mine','Mine','me',0);",
    );

    // Not connected: every write is rejected before any request.
    let e = b.star_add(StarRequest { id: "t1".into(), kind: StarTargetType::Track }).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotConnected);

    *b.0.client.write().unwrap() = Some(Client::new(&mock.url, "me", "pw").unwrap());
    b.0.session.lock().unwrap().cfg = Some(sonix_core::secrets::ConnectionConfig { url: mock.url.clone(), username: "me".into(), last_sync_at: None });
    assert!(mock.take().is_empty());

    // --- stars
    b.star_add(StarRequest { id: "t1".into(), kind: StarTargetType::Track }).unwrap();
    assert!(has(&mock.take(), "star", &["id=t1"]));
    let got = std::iter::from_fn(|| rx.recv_timeout(std::time::Duration::from_secs(2)).ok()).any(|e| matches!(e, Event::LibraryChanged));
    assert!(got, "star must emit LibraryChanged");
    assert!(track_col(&b, "t1", "starred").is_some());
    assert_eq!(b.liked_count().unwrap(), 1);
    b.star_add(StarRequest { id: "al1".into(), kind: StarTargetType::Album }).unwrap();
    assert!(has(&mock.take(), "star", &["albumId=al1"]));
    b.star_remove(StarRequest { id: "t1".into(), kind: StarTargetType::Track }).unwrap();
    assert!(has(&mock.take(), "unstar", &["id=t1"]));
    assert_eq!(b.liked_count().unwrap(), 0);

    // --- ratings
    b.rating_set(SetRatingRequest { id: "t2".into(), kind: StarTargetType::Track, rating: 4 }).unwrap();
    assert!(has(&mock.take(), "setRating", &["id=t2", "rating=4"]));
    assert_eq!(track_col(&b, "t2", "rating").as_deref(), Some("4"));
    b.rating_set(SetRatingRequest { id: "t2".into(), kind: StarTargetType::Track, rating: 0 }).unwrap();
    assert!(has(&mock.take(), "setRating", &["rating=0"]));
    assert_eq!(track_col(&b, "t2", "rating"), None);
    assert!(b.rating_set(SetRatingRequest { id: "t2".into(), kind: StarTargetType::Track, rating: 6 }).is_err());
    assert!(mock.take().is_empty(), "out-of-range rating must not reach the server");

    // --- playlists
    let p = b.playlists_create(CreatePlaylistRequest { name: "Mix".into(), track_ids: vec!["t1".into(), "t2".into()] }).unwrap();
    assert_eq!(p.id, "pl9");
    assert!(!p.smart && !p.read_only);
    let reqs = mock.take();
    assert!(has(&reqs, "createPlaylist", &["name=Mix"]));
    assert!(has(&reqs, "updatePlaylist", &["playlistId=pl9", "songIdToAdd=t1", "songIdToAdd=t2"]));
    let d = b.playlists_get("pl9").unwrap();
    assert_eq!(d.tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["t1", "t2"]);

    b.playlists_rename(RenamePlaylistRequest { id: "pl9".into(), name: "New".into() }).unwrap();
    assert!(has(&mock.take(), "updatePlaylist", &["playlistId=pl9", "name=New"]));
    assert_eq!(b.playlists_get("pl9").unwrap().playlist.name, "New");

    b.playlists_add_tracks(AddToPlaylistRequest { id: "pl9".into(), track_ids: vec!["t3".into()] }).unwrap();
    let reqs = mock.take();
    assert!(has(&reqs, "updatePlaylist", &["songIdToAdd=t3"]) && has(&reqs, "getPlaylist", &["id=pl9"]));

    b.playlists_remove_tracks(RemovePlaylistTracksRequest { id: "pl9".into(), indices: vec![0, 2] }).unwrap();
    assert!(has(&mock.take(), "updatePlaylist", &["songIndexToRemove=0", "songIndexToRemove=2"]));

    // reorder: ONE updatePlaylist removing every current entry (mock has 2) and re-adding the new order
    b.playlists_reorder(ReorderPlaylistRequest { id: "pl9".into(), track_ids: vec!["t2".into(), "t1".into()] }).unwrap();
    let reqs = mock.take();
    let upd: Vec<_> = reqs.iter().filter(|r| r.starts_with("/rest/updatePlaylist")).collect();
    assert_eq!(upd.len(), 1);
    assert!(upd[0].contains("songIndexToRemove=1") && upd[0].contains("songIndexToRemove=0"));
    assert!(upd[0].find("songIdToAdd=t2").unwrap() < upd[0].find("songIdToAdd=t1").unwrap());

    let dup = b.playlists_duplicate("pl9").unwrap();
    let reqs = mock.take();
    assert_eq!(dup.id, "pl9");
    assert!(has(&reqs, "createPlaylist", &["name=Mix+%28copy%29"]) || has(&reqs, "createPlaylist", &["name=Mix%20%28copy%29"]), "{reqs:?}");

    b.playlists_delete(DeletePlaylistRequest { id: "mine".into() }).unwrap();
    assert!(has(&mock.take(), "deletePlaylist", &["id=mine"]));
    assert!(b.playlists_get("mine").is_err());

    // --- access flags and the read-only guard
    let lists = b.playlists_list().unwrap();
    let find = |id: &str| lists.iter().find(|p| p.id == id).unwrap().clone();
    assert!(find("ro").smart && find("ro").read_only);
    assert!(!find("theirs").smart && find("theirs").read_only);
    assert!(!find("pl9").read_only);
    mock.take();
    for e in [
        b.playlists_rename(RenamePlaylistRequest { id: "ro".into(), name: "x".into() }),
        b.playlists_delete(DeletePlaylistRequest { id: "theirs".into() }),
        b.playlists_add_tracks(AddToPlaylistRequest { id: "ro".into(), track_ids: vec!["t1".into()] }),
        b.playlists_reorder(ReorderPlaylistRequest { id: "ro".into(), track_ids: vec![] }),
    ] {
        assert_eq!(e.unwrap_err().code, ErrorCode::ServerError);
    }
    assert!(mock.take().is_empty(), "read-only playlists must not reach the server");

    // --- smart playlists (local only: no request) and flow coverage
    let input = SmartInput {
        name: "Four stars".into(),
        match_mode: SmartMatchMode::All,
        rules: vec![SmartRule { field: SmartRuleField::Rating, op: SmartRuleOp::Gte, value: Some(SmartValue::Num(4.0)) }],
        sort: SmartSort::Title,
        limit: None,
    };
    exec(&b, "UPDATE tracks SET rating = 5 WHERE id IN ('t1','t3')");
    let s = b.smart_create(input.clone()).unwrap();
    assert_eq!((s.track_count, s.input.name.as_str()), (2, "Four stars"));
    assert_eq!(b.smart_list().unwrap().len(), 1);
    assert_eq!(b.smart_get(&s.id, 0).unwrap().id, s.id);
    let page = b.smart_tracks(&s.id, 0, PageRequest { offset: 0, limit: 10 }).unwrap();
    assert_eq!(page.items.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["t1", "t3"]);
    assert_eq!(b.smart_preview(input.clone(), 0, PageRequest { offset: 0, limit: 10 }).unwrap().total, 2);
    let mut up = input.clone();
    up.name = "Renamed".into();
    assert_eq!(b.smart_update(&s.id, up).unwrap().input.name, "Renamed");
    let dup = b.smart_duplicate(&s.id).unwrap();
    assert_ne!(dup.id, s.id);
    assert!(!b.smart_presets().unwrap().is_empty());
    let cov = b.flow_get_coverage(FlowCoverageRequest { kind: FlowPoolKind::Smart, id: Some(s.id.clone()), filter: None, seed: None }).unwrap();
    assert_eq!(cov.total, 2);
    assert_eq!(b.flow_get_coverage(FlowCoverageRequest { kind: FlowPoolKind::AllTracks, id: None, filter: None, seed: None }).unwrap().total, 3);
    let nf = b.flow_get_coverage(FlowCoverageRequest { kind: FlowPoolKind::Playlist, id: Some("nope".into()), filter: None, seed: None });
    assert_eq!(nf.unwrap_err().code, ErrorCode::NotFound);
    b.smart_delete(&dup.id).unwrap();
    b.smart_delete(&s.id).unwrap();
    assert!(b.smart_list().unwrap().is_empty());
    assert!(mock.take().is_empty(), "smart playlists never touch the server");

    // the definition file lives in the config dir
    assert!(tmp.join("roaming").join(crate::IDENTIFIER).join("smart_playlists.json").exists());
    let _ = std::fs::remove_dir_all(&tmp);
}
