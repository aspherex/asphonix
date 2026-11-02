//! Write paths against a local mock Subsonic server (127.0.0.1, random port). Never contacts a real server.
use super::*;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone)]
struct Req {
    endpoint: String,
    pairs: Vec<(String, String)>,
}

impl Req {
    fn all(&self, key: &str) -> Vec<String> {
        self.pairs.iter().filter(|(k, _)| k == key).map(|(_, v)| v.clone()).collect()
    }
}

#[derive(Default)]
struct State {
    log: Vec<Req>,
    /// id -> (name, entry track ids)
    playlists: BTreeMap<String, (String, Vec<String>)>,
    next: u32,
    /// `Some("*")` fails every endpoint with `status:"failed"`, `Some(name)` only that endpoint.
    fail: Option<&'static str>,
}

/// Stateful mock server: records every request with exactly parsed query pairs, applies playlist edits.
struct Mock {
    url: String,
    st: Arc<Mutex<State>>,
}

fn playlist_json(id: &str, name: &str, entries: &[String], with_entries: bool) -> String {
    let song = |id: &str| format!(r#"{{"id":"{id}","title":"T {id}","suffix":"flac","artist":"A"}}"#);
    let e = if with_entries { format!(r#","entry":[{}]"#, entries.iter().map(|i| song(i)).collect::<Vec<_>>().join(",")) } else { String::new() };
    format!(r#""playlist":{{"id":"{id}","name":"{name}","owner":"me","songCount":{},"duration":10.0,"readonly":false{e}}}"#, entries.len())
}

impl Mock {
    fn start() -> Mock {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let st = Arc::new(Mutex::new(State::default()));
        let s2 = st.clone();
        std::thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(mut sock) = sock else { break };
                let mut buf = [0u8; 8192];
                let n = sock.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let target = req.lines().next().unwrap_or("").split(' ').nth(1).unwrap_or("").to_string();
                let u = url::Url::parse(&format!("http://x{target}")).unwrap();
                let r = Req { endpoint: u.path().trim_start_matches("/rest/").to_string(), pairs: u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect() };
                let mut st = s2.lock().unwrap();
                st.log.push(r.clone());
                let body = if st.fail.is_some_and(|f| f == "*" || f == r.endpoint) {
                    r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":0,"message":"boom"}}}"#.to_string()
                } else {
                    let first = |k: &str| r.all(k).into_iter().next().unwrap_or_default();
                    let inner = match r.endpoint.as_str() {
                        "createPlaylist" => {
                            st.next += 1;
                            let id = format!("pl{}", st.next);
                            let name = first("name");
                            st.playlists.insert(id.clone(), (name.clone(), vec![]));
                            format!(",{}", playlist_json(&id, &name, &[], false))
                        }
                        "getPlaylist" => match st.playlists.get(&first("id")) {
                            Some((name, e)) => format!(",{}", playlist_json(&first("id"), name, e, true)),
                            None => String::new(),
                        },
                        "updatePlaylist" => {
                            if let Some(p) = st.playlists.get_mut(&first("playlistId")) {
                                if r.pairs.iter().any(|(k, _)| k == "name") {
                                    p.0 = first("name");
                                }
                                let mut rm: Vec<usize> = r.all("songIndexToRemove").iter().map(|s| s.parse().unwrap()).collect();
                                rm.sort_unstable_by(|a, b| b.cmp(a));
                                for i in rm {
                                    p.1.remove(i);
                                }
                                p.1.extend(r.all("songIdToAdd"));
                            }
                            String::new()
                        }
                        "deletePlaylist" => {
                            st.playlists.remove(&first("id"));
                            String::new()
                        }
                        _ => String::new(),
                    };
                    format!(r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1"{inner}}}}}"#)
                };
                drop(st);
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                let _ = sock.write_all(resp.as_bytes());
            }
        });
        Mock { url, st }
    }
    fn take(&self) -> Vec<Req> {
        std::mem::take(&mut self.st.lock().unwrap().log)
    }
    fn seed(&self, id: &str, name: &str, entries: &[&str]) {
        self.st.lock().unwrap().playlists.insert(id.into(), (name.into(), entries.iter().map(|s| s.to_string()).collect()));
    }
    fn entries(&self, id: &str) -> Vec<String> {
        self.st.lock().unwrap().playlists[id].1.clone()
    }
    fn fail(&self, on: Option<&'static str>) {
        self.st.lock().unwrap().fail = on;
    }
}

/// Exact pair match (`id=t1` never matches `albumId=t1`).
fn has(reqs: &[Req], endpoint: &str, pairs: &[(&str, &str)]) -> bool {
    reqs.iter().any(|r| r.endpoint == endpoint && pairs.iter().all(|(k, v)| r.pairs.iter().any(|(rk, rv)| rk == k && rv == v)))
}

fn find<'a>(reqs: &'a [Req], endpoint: &str) -> Vec<&'a Req> {
    reqs.iter().filter(|r| r.endpoint == endpoint).collect()
}

/// Temp dir on D: (C: is nearly full), removed on drop even when the test panics.
struct TmpDir(std::path::PathBuf);
impl TmpDir {
    fn new(tag: &str) -> Self {
        let base = if std::path::Path::new("<build-dir>").exists() { std::path::PathBuf::from("<build-dir>/tmp") } else { std::env::temp_dir() };
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = base.join(format!("sonix-writes-{tag}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        TmpDir(d)
    }
}
impl Drop for TmpDir {
    fn drop(&mut self) {
        // The closed backend's threads may hold the SQLite files for a moment: retry for up to 5 s.
        let t = std::time::Instant::now();
        while std::fs::remove_dir_all(&self.0).is_err() && self.0.exists() && t.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

fn exec(b: &RealBackend, sql: &str) {
    b.0.db.with_conn(|c| c.execute_batch(sql)).unwrap();
}

fn track_col(b: &RealBackend, id: &str, col: &str) -> Option<String> {
    b.0.db.read(|c| c.query_row(&format!("SELECT CAST({col} AS TEXT) FROM tracks WHERE id='{id}'"), [], |r| r.get(0))).unwrap()
}

fn local_ids(b: &RealBackend, pl: &str) -> Vec<String> {
    b.playlists_get(pl).unwrap().tracks.into_iter().map(|t| t.id).collect()
}

/// Events seen within a short window.
fn events(rx: &std::sync::mpsc::Receiver<Event>) -> Vec<Event> {
    std::iter::from_fn(|| rx.recv_timeout(Duration::from_millis(300)).ok()).collect()
}

fn changed(evs: &[Event]) -> bool {
    evs.iter().any(|e| matches!(e, Event::LibraryChanged))
}

fn connect(b: &RealBackend, mock: &Mock) {
    *b.0.client.write().unwrap() = Some(Client::new(&mock.url, "me", "pw").unwrap());
    b.0.session.lock().unwrap().cfg = Some(sonix_core::secrets::ConnectionConfig { url: mock.url.clone(), username: "me".into(), last_sync_at: None });
}

#[test]
fn map_ui_indices_cases() {
    // hidden entry X between A and B
    assert_eq!(map_ui_indices(&["a", "b"], &["a", "x", "b"], &[1]), Some(vec![2]));
    assert_eq!(map_ui_indices(&["a", "b"], &["a", "x", "b"], &[1, 0, 1]), Some(vec![0, 2]));
    // nth occurrence of a duplicate
    assert_eq!(map_ui_indices(&["a", "a"], &["a", "x", "a"], &[1]), Some(vec![2]));
    // out of range, or local list not a subsequence of the server's
    assert_eq!(map_ui_indices(&["a"], &["a"], &[1]), None);
    assert_eq!(map_ui_indices(&["a", "c"], &["a", "b"], &[0]), None);
    assert_eq!(map_ui_indices(&["b", "a"], &["a", "b"], &[0]), None);
}

#[test]
fn writes_hit_the_mock_server_and_mirror_locally() {
    let tmp = TmpDir::new("main");
    let b = RealBackend::open_in(tmp.0.join("config"), tmp.0.join("data"));
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

    connect(&b, &mock);
    assert!(mock.take().is_empty());
    events(&rx);

    // --- stars: exact parameter names per kind
    b.star_add(StarRequest { id: "t1".into(), kind: StarTargetType::Track }).unwrap();
    let reqs = mock.take();
    assert!(has(&reqs, "star", &[("id", "t1")]) && !has(&reqs, "star", &[("albumId", "t1")]));
    assert!(changed(&events(&rx)), "star must emit LibraryChanged");
    assert!(track_col(&b, "t1", "starred").is_some());
    assert_eq!(b.liked_count().unwrap(), 1);
    b.star_add(StarRequest { id: "al1".into(), kind: StarTargetType::Album }).unwrap();
    let reqs = mock.take();
    assert!(has(&reqs, "star", &[("albumId", "al1")]) && !has(&reqs, "star", &[("id", "al1")]));
    b.star_add(StarRequest { id: "ar1".into(), kind: StarTargetType::Artist }).unwrap();
    assert!(has(&mock.take(), "star", &[("artistId", "ar1")]));
    b.star_remove(StarRequest { id: "t1".into(), kind: StarTargetType::Track }).unwrap();
    assert!(has(&mock.take(), "unstar", &[("id", "t1")]));
    assert_eq!(b.liked_count().unwrap(), 0);

    // --- ratings (id checked on the clearing call too)
    b.rating_set(SetRatingRequest { id: "t2".into(), kind: StarTargetType::Track, rating: 4 }).unwrap();
    assert!(has(&mock.take(), "setRating", &[("id", "t2"), ("rating", "4")]));
    assert_eq!(track_col(&b, "t2", "rating").as_deref(), Some("4"));
    b.rating_set(SetRatingRequest { id: "t2".into(), kind: StarTargetType::Track, rating: 0 }).unwrap();
    assert!(has(&mock.take(), "setRating", &[("id", "t2"), ("rating", "0")]));
    assert_eq!(track_col(&b, "t2", "rating"), None);
    assert!(b.rating_set(SetRatingRequest { id: "t2".into(), kind: StarTargetType::Track, rating: 6 }).is_err());
    assert!(mock.take().is_empty(), "out-of-range rating must not reach the server");
    events(&rx);

    // --- a failed server write (`status:"failed"`) leaves the mirror untouched and emits nothing
    mock.fail(Some("*"));
    assert!(b.star_add(StarRequest { id: "t3".into(), kind: StarTargetType::Track }).is_err());
    assert!(b.rating_set(SetRatingRequest { id: "t3".into(), kind: StarTargetType::Track, rating: 3 }).is_err());
    assert!(b.playlists_rename(RenamePlaylistRequest { id: "mine".into(), name: "Renamed".into() }).is_err());
    assert!(b.playlists_delete(DeletePlaylistRequest { id: "mine".into() }).is_err());
    assert_eq!(mock.take().len(), 4, "each failed write did reach the server");
    assert_eq!(track_col(&b, "t3", "starred"), None);
    assert_eq!(track_col(&b, "t3", "rating"), None);
    assert_eq!(b.playlists_get("mine").unwrap().playlist.name, "Mine");
    assert!(!changed(&events(&rx)), "failed writes must not emit LibraryChanged");
    mock.fail(None);

    // --- create with tracks keeps song order
    let p = b.playlists_create(CreatePlaylistRequest { name: "Mix".into(), track_ids: vec!["t2".into(), "t1".into()] }).unwrap();
    assert_eq!(p.id, "pl1");
    assert!(!p.smart && !p.read_only);
    let reqs = mock.take();
    assert!(has(&reqs, "createPlaylist", &[("name", "Mix")]));
    let add = find(&reqs, "updatePlaylist");
    assert_eq!(add.len(), 1);
    assert_eq!(add[0].all("playlistId"), ["pl1"]);
    assert_eq!(add[0].all("songIdToAdd"), ["t2", "t1"]);
    assert_eq!(local_ids(&b, "pl1"), ["t2", "t1"]);

    b.playlists_rename(RenamePlaylistRequest { id: "pl1".into(), name: "New".into() }).unwrap();
    assert!(has(&mock.take(), "updatePlaylist", &[("playlistId", "pl1"), ("name", "New")]));
    assert_eq!(b.playlists_get("pl1").unwrap().playlist.name, "New");

    b.playlists_add_tracks(AddToPlaylistRequest { id: "pl1".into(), track_ids: vec!["t3".into()] }).unwrap();
    let reqs = mock.take();
    assert!(has(&reqs, "updatePlaylist", &[("playlistId", "pl1"), ("songIdToAdd", "t3")]) && has(&reqs, "getPlaylist", &[("id", "pl1")]));
    assert_eq!(local_ids(&b, "pl1"), ["t2", "t1", "t3"]);

    // remove by UI index: playlistId and indices, mirror follows the server
    b.playlists_remove_tracks(RemovePlaylistTracksRequest { id: "pl1".into(), indices: vec![0, 2] }).unwrap();
    let reqs = mock.take();
    let upd = find(&reqs, "updatePlaylist");
    assert_eq!(upd.len(), 1);
    assert_eq!(upd[0].all("playlistId"), ["pl1"]);
    let mut idx = upd[0].all("songIndexToRemove");
    idx.sort();
    assert_eq!(idx, ["0", "2"]);
    assert_eq!(mock.entries("pl1"), ["t1"]);
    assert_eq!(local_ids(&b, "pl1"), ["t1"]);

    // reorder: ONE updatePlaylist removing every current entry and re-adding the new order
    b.playlists_add_tracks(AddToPlaylistRequest { id: "pl1".into(), track_ids: vec!["t2".into()] }).unwrap();
    mock.take();
    b.playlists_reorder(ReorderPlaylistRequest { id: "pl1".into(), track_ids: vec!["t2".into(), "t1".into()] }).unwrap();
    let reqs = mock.take();
    let upd = find(&reqs, "updatePlaylist");
    assert_eq!(upd.len(), 1);
    let mut rm = upd[0].all("songIndexToRemove");
    rm.sort();
    assert_eq!(rm, ["0", "1"]);
    assert_eq!(upd[0].all("songIdToAdd"), ["t2", "t1"]);
    assert_eq!(mock.entries("pl1"), ["t2", "t1"]);
    assert_eq!(local_ids(&b, "pl1"), ["t2", "t1"]);

    // duplicate: new id, "<name> (copy)", same tracks in order
    let dup = b.playlists_duplicate("pl1").unwrap();
    let reqs = mock.take();
    assert_eq!(dup.id, "pl2");
    assert!(has(&reqs, "createPlaylist", &[("name", "New (copy)")]));
    let add = find(&reqs, "updatePlaylist");
    assert_eq!(add[0].all("playlistId"), ["pl2"]);
    assert_eq!(add[0].all("songIdToAdd"), ["t2", "t1"]);
    assert_eq!(mock.entries("pl2"), ["t2", "t1"]);
    assert_eq!(local_ids(&b, "pl2"), ["t2", "t1"]);
    assert_eq!(b.playlists_get("pl2").unwrap().playlist.name, "New (copy)");

    // create with tracks where only the add step fails: the playlist exists on the server, so the mirror has it
    // and the UI is told, although the call errors
    events(&rx);
    mock.fail(Some("updatePlaylist"));
    assert!(b.playlists_create(CreatePlaylistRequest { name: "Half".into(), track_ids: vec!["t1".into()] }).is_err());
    mock.fail(None);
    assert!(has(&mock.take(), "createPlaylist", &[("name", "Half")]));
    assert_eq!(b.playlists_get("pl3").unwrap().playlist.name, "Half");
    assert!(changed(&events(&rx)));

    b.playlists_delete(DeletePlaylistRequest { id: "mine".into() }).unwrap();
    assert!(has(&mock.take(), "deletePlaylist", &[("id", "mine")]));
    assert!(b.playlists_get("mine").is_err());

    // --- access flags and the read-only guard
    let lists = b.playlists_list().unwrap();
    let get = |id: &str| lists.iter().find(|p| p.id == id).unwrap().clone();
    assert!(get("ro").smart && get("ro").read_only);
    assert!(!get("theirs").smart && get("theirs").read_only);
    assert!(!get("pl1").read_only);
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

    // the definition file lives in the config dir passed to `open_in`
    assert!(tmp.0.join("config").join("smart_playlists.json").exists());
}

/// Playlists whose server entries have no local track row (the UI list is shorter than the server's).
#[test]
fn hidden_and_stale_entries_never_edit_the_wrong_row() {
    let tmp = TmpDir::new("hidden");
    let b = RealBackend::open_in(tmp.0.join("config"), tmp.0.join("data"));
    let rx = b.subscribe();
    let mock = Mock::start();
    connect(&b, &mock);
    // t1 = A, t2 = B are mirrored; "x" (X) has no local track row, so the UI list is [A, B] while the server has [A, X, B].
    exec(
        &b,
        "INSERT INTO tracks (id,title,artist,suffix,duration) VALUES ('t1','A','A','flac',10),('t2','B','A','flac',10),('t3','C','A','flac',10);
         INSERT INTO playlists (id,name,owner,readonly) VALUES ('h1','H1','me',0),('h2','H2','me',0),('st','Stale','me',0);
         INSERT INTO playlist_tracks (playlist_id,position,track_id) VALUES ('h1',0,'t1'),('h1',1,'x'),('h1',2,'t2'),
            ('h2',0,'t1'),('h2',1,'x'),('h2',2,'t2'),('st',0,'t1'),('st',1,'t3');",
    );
    mock.seed("h1", "H1", &["t1", "x", "t2"]);
    mock.seed("h2", "H2", &["t1", "x", "t2"]);
    mock.seed("st", "Stale", &["t1", "t2"]);
    assert_eq!(local_ids(&b, "h1"), ["t1", "t2"]);
    events(&rx);

    // remove UI row 1 (B): server position 2, not 1 (X)
    b.playlists_remove_tracks(RemovePlaylistTracksRequest { id: "h1".into(), indices: vec![1] }).unwrap();
    let reqs = mock.take();
    assert_eq!(find(&reqs, "updatePlaylist")[0].all("songIndexToRemove"), ["2"]);
    assert_eq!(mock.entries("h1"), ["t1", "x"]);
    assert_eq!(local_ids(&b, "h1"), ["t1"]);
    assert!(changed(&events(&rx)));

    // reorder from the shortened UI list would drop X: refused, nothing edited, mirror refreshed, UI told
    let e = b.playlists_reorder(ReorderPlaylistRequest { id: "h2".into(), track_ids: vec!["t2".into(), "t1".into()] }).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidRequest);
    let reqs = mock.take();
    assert!(find(&reqs, "updatePlaylist").is_empty(), "refused reorder must not edit");
    assert!(has(&reqs, "getPlaylist", &[("id", "h2")]), "refusal refreshes the mirror");
    assert_eq!(mock.entries("h2"), ["t1", "x", "t2"]);
    assert!(changed(&events(&rx)));

    // stale mirror (local [t1,t3], server [t1,t2]): remove is refused, mirror re-synced
    let e = b.playlists_remove_tracks(RemovePlaylistTracksRequest { id: "st".into(), indices: vec![1] }).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidRequest);
    let reqs = mock.take();
    assert!(find(&reqs, "updatePlaylist").is_empty());
    assert_eq!(mock.entries("st"), ["t1", "t2"]);
    assert_eq!(local_ids(&b, "st"), ["t1", "t2"]);
    assert!(changed(&events(&rx)));
}

/// Reference `connect_account`: switching to another account (no keychain write: `save = false`) stops the engine and
/// wipes the library mirror and the art cache before the new client is installed; nothing of the old account is pushed.
#[test]
fn account_switch_stops_the_engine_and_wipes_mirror_and_art_cache() {
    let tmp = TmpDir::new("switch");
    let b = RealBackend::open_in(tmp.0.join("config"), tmp.0.join("data"));
    let mock = Mock::start();
    b.connect(&mock.url, "me", "pw", false).unwrap();
    // let the first (empty) sync settle, then fake an old-account library + cached cover
    std::thread::sleep(Duration::from_millis(500));
    exec(&b, "INSERT INTO tracks (id,title,artist,suffix,duration) VALUES ('t1','One','A','flac',10);");
    let art = b.art_dir();
    std::fs::create_dir_all(&art).unwrap();
    std::fs::write(art.join("c1_160.img"), b"x").unwrap();
    assert_eq!(b.0.db.track_count().unwrap(), 1);
    let page = PageRequest { offset: 0, limit: 10 };
    b.player_enqueue(EnqueueRequest { track_ids: vec!["t1".into()], position: EnqueuePosition::End }).unwrap();
    let t = std::time::Instant::now();
    while b.player_get_queue(page).unwrap().total == 0 && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(b.player_get_queue(page).unwrap().total, 1, "old-account queue");
    mock.take();
    b.connect(&mock.url, "other", "pw", false).unwrap();
    assert_eq!(b.0.db.track_count().unwrap(), 0, "the other account's mirror is wiped");
    assert!(!art.join("c1_160.img").exists(), "the other account's covers are dropped");
    assert_eq!(b.player_get_queue(page).unwrap().total, 0, "the engine queue is cleared (Stop)");
    let reqs = mock.take();
    assert!(find(&reqs, "savePlayQueue").is_empty() && find(&reqs, "scrobble").is_empty(), "{reqs:?}");
}

/// Reference `auth_logout`: stop + flush the listen writer first (ordering of the Stop row vs. the flush is covered by
/// `engine::tests::stop_goes_through_the_writer_and_flush_follows_it`), drop the client, cancel the old sync (epoch),
/// then wipe the mirror (listen history included) and the art cache, so the next account inherits nothing.
/// The keychain step only deletes (an entry this test never created).
#[test]
fn logout_stops_flushes_and_wipes_everything_local() {
    let tmp = TmpDir::new("logout");
    let b = RealBackend::open_in(tmp.0.join("config"), tmp.0.join("data"));
    let mock = Mock::start();
    b.connect(&mock.url, "me", "pw", false).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    exec(
        &b,
        "INSERT INTO tracks (id,title,artist,suffix,duration) VALUES ('t1','One','A','flac',10);
         INSERT INTO listen_events (track_id,started_at,ended_at,listened_ms,duration_ms,end_reason,origin) VALUES ('t1',1,2,1,10000,'stopped','user');",
    );
    let page = PageRequest { offset: 0, limit: 10 };
    b.player_enqueue(EnqueueRequest { track_ids: vec!["t1".into()], position: EnqueuePosition::End }).unwrap();
    let t = std::time::Instant::now();
    while b.player_get_queue(page).unwrap().total == 0 && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let art = b.art_dir();
    std::fs::create_dir_all(&art).unwrap();
    std::fs::write(art.join("c1_160.img"), b"x").unwrap();
    let epoch = b.0.epoch.load(std::sync::atomic::Ordering::SeqCst);
    mock.take();
    let t = std::time::Instant::now();
    b.auth_logout().unwrap();
    assert!(t.elapsed() < Duration::from_secs(5), "bounded: {:?}", t.elapsed());
    assert!(b.0.epoch.load(std::sync::atomic::Ordering::SeqCst) > epoch, "a running sync of the old account stops writing");
    assert!(b.0.client.read().unwrap().is_none());
    assert_eq!(b.player_get_queue(page).unwrap().total, 0, "engine stopped, queue cleared");
    assert_eq!(b.0.db.track_count().unwrap(), 0);
    let listens: i64 = b.0.db.read(|c| c.query_row("SELECT COUNT(*) FROM listen_events", [], |r| r.get(0))).unwrap();
    assert_eq!(listens, 0, "listen history goes with the old account");
    assert!(!art.join("c1_160.img").exists());
    assert_eq!(b.auth_session_status().unwrap().state, ConnectionState::Disconnected);
    assert!(!tmp.0.join("config").join("connection.json").exists());
    let reqs = mock.take();
    assert!(find(&reqs, "savePlayQueue").is_empty() && find(&reqs, "scrobble").is_empty(), "{reqs:?}");
}
