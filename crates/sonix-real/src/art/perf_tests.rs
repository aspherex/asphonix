//! Cover fetch behaviour and timing against a local mock Subsonic server (127.0.0.1, random port; never a real server).
//! The mock serves real-sized JPEGs from a small "server CPU" pool with a per-request delay (the server-side resize
//! of a NAS). The ignored `bench_grid` test prints time-to-all-covers: `cargo test -p sonix-real --release bench_grid
//! -- --ignored --nocapture`.
use super::*;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

type ImageMap = Mutex<HashMap<(u32, u32), Arc<Vec<u8>>>>;

struct Server {
    url: String,
    hits: Arc<Mutex<HashMap<String, usize>>>,
    requests: Arc<AtomicUsize>,
    conns: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

fn jpeg(size: u32, seed: u32) -> Vec<u8> {
    // photographic-ish content so the file is a realistic size (noise + gradient)
    let mut x = seed.wrapping_mul(2654435761).wrapping_add(1);
    let img = RgbImage::from_fn(size, size, |px, py| {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        let n = (x & 31) as u8;
        Rgb([(px * 255 / size) as u8 ^ n, (py * 255 / size) as u8 ^ n, ((px + py) * 127 / size) as u8 ^ n])
    });
    let mut out = Vec::new();
    img.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)).unwrap();
    out
}

impl Server {
    /// `slots` = concurrent resizes the "server" can do; `delay_ms` = (min, max) per request, by id hash.
    fn start(slots: usize, delay_ms: (u64, u64)) -> Server {
        Server::start_opts(slots, delay_ms, false)
    }
    /// `garbage_master`: the 640 request returns a webp-looking payload no decoder can read.
    fn start_opts(slots: usize, delay_ms: (u64, u64), garbage_master: bool) -> Server {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let s = Server {
            url,
            hits: Default::default(),
            requests: Default::default(),
            conns: Default::default(),
            max_active: Default::default(),
        };
        let (hits, requests, conns, max_active) = (s.hits.clone(), s.requests.clone(), s.conns.clone(), s.max_active.clone());
        let gate: Arc<(Mutex<usize>, Condvar)> = Arc::new((Mutex::new(slots), Condvar::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let images: Arc<ImageMap> = Default::default();
        std::thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(mut sock) = sock else { break };
                conns.fetch_add(1, Ordering::SeqCst);
                let (hits, requests, max_active, gate, active, images) = (hits.clone(), requests.clone(), max_active.clone(), gate.clone(), active.clone(), images.clone());
                std::thread::spawn(move || {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            match sock.read(&mut chunk) {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        }
                        let end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                        let head = String::from_utf8_lossy(&buf[..end]).to_string();
                        buf.drain(..end);
                        let target = head.lines().next().unwrap_or("").split(' ').nth(1).unwrap_or("").to_string();
                        let u = url::Url::parse(&format!("http://x{target}")).unwrap();
                        let q: HashMap<String, String> = u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
                        let id = q.get("id").cloned().unwrap_or_default();
                        let size: u32 = q.get("size").and_then(|s| s.parse().ok()).unwrap_or(640);
                        requests.fetch_add(1, Ordering::SeqCst);
                        *hits.lock().unwrap().entry(format!("{id}@{size}")).or_default() += 1;
                        // server CPU slot
                        {
                            let (m, cv) = &*gate;
                            let mut free = m.lock().unwrap();
                            while *free == 0 {
                                free = cv.wait(free).unwrap();
                            }
                            *free -= 1;
                        }
                        let a = active.fetch_add(1, Ordering::SeqCst) + 1;
                        max_active.fetch_max(a, Ordering::SeqCst);
                        let seed: u32 = id.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
                        let span = delay_ms.1 - delay_ms.0 + 1;
                        std::thread::sleep(Duration::from_millis(delay_ms.0 + u64::from(seed) % span));
                        let body = if garbage_master && size == 640 {
                            Arc::new(b"RIFF\x20\0\0\0WEBPVP8 not-a-real-image-payload".to_vec())
                        } else {
                            images.lock().unwrap().entry((seed, size)).or_insert_with(|| Arc::new(jpeg(size, seed))).clone()
                        };
                        active.fetch_sub(1, Ordering::SeqCst);
                        {
                            let (m, cv) = &*gate;
                            *m.lock().unwrap() += 1;
                            cv.notify_one();
                        }
                        let resp = format!("HTTP/1.1 200 OK\r\ncontent-type: image/jpeg\r\ncontent-length: {}\r\n\r\n", body.len());
                        if sock.write_all(resp.as_bytes()).is_err() || sock.write_all(&body).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        s
    }
    fn hits_for(&self, id: &str) -> usize {
        self.hits.lock().unwrap().iter().filter(|(k, _)| k.split('@').next() == Some(id)).map(|(_, v)| *v).sum()
    }
}

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Tmp {
        let base = std::env::var_os("SONIX_TEST_TMP").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let p = base.join(format!("art-perf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn backend(tmp: &Tmp, srv: &Server) -> RealBackend {
    let b = RealBackend::open_in(tmp.0.join("config"), tmp.0.join("data"));
    *b.0.client.write().unwrap() = Some(Client::new(&srv.url, "me", "pw").unwrap());
    b
}

fn ids(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("al-{i:03}")).collect()
}

/// Old UI shape: one thread per cover, all at once.
fn all_at_once(b: &RealBackend, ids: &[String], size: u32) -> Duration {
    let t = Instant::now();
    std::thread::scope(|s| {
        for id in ids {
            s.spawn(move || b.art_pixels(id, size).unwrap());
        }
    });
    t.elapsed()
}

/// New UI shape: `workers` threads pulling from one queue.
fn queued(b: &RealBackend, ids: &[String], size: u32, workers: usize) -> Duration {
    let next = AtomicUsize::new(0);
    let t = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                while let Some(id) = ids.get(next.fetch_add(1, Ordering::SeqCst)) {
                    b.art_pixels(id, size).unwrap();
                }
            });
        }
    });
    t.elapsed()
}

#[test]
fn identical_requests_share_one_fetch() {
    let srv = Server::start(4, (40, 40));
    let tmp = Tmp::new("sf");
    let b = backend(&tmp, &srv);
    std::thread::scope(|s| {
        for _ in 0..12 {
            s.spawn(|| {
                let p = b.art_pixels("sf-1", 240).unwrap();
                assert_eq!((p.width, p.height), (240, 240));
            });
        }
    });
    assert_eq!(srv.hits_for("sf-1"), 1, "12 concurrent identical requests -> one server request");
}

#[test]
fn every_bucket_derives_from_one_master_fetch() {
    let srv = Server::start(4, (10, 10));
    let tmp = Tmp::new("master");
    let b = backend(&tmp, &srv);
    for size in [64, 160, 240, 320, 640] {
        let p = b.art_pixels("m-1", size).unwrap();
        assert_eq!((p.width, p.height), (size, size));
    }
    assert_eq!(srv.hits_for("m-1"), 1);
    assert_eq!(srv.hits.lock().unwrap().keys().cloned().collect::<Vec<_>>(), vec!["m-1@640".to_string()]);
    // warm: a fresh backend on the same dir never touches the server
    let b2 = backend(&tmp, &srv);
    let before = srv.requests.load(Ordering::SeqCst);
    b2.art_pixels("m-1", 160).unwrap();
    assert_eq!(srv.requests.load(Ordering::SeqCst), before);
}

#[test]
fn foreground_fetches_are_capped() {
    let srv = Server::start(32, (40, 40));
    let tmp = Tmp::new("cap");
    let b = backend(&tmp, &srv);
    all_at_once(&b, &ids(24), 240);
    assert!(srv.max_active.load(Ordering::SeqCst) <= MAX_CONCURRENT_FETCHES, "max active {}", srv.max_active.load(Ordering::SeqCst));
    assert_eq!(srv.requests.load(Ordering::SeqCst), 24);
}

#[test]
fn prefetch_fetches_only_missing_masters() {
    let srv = Server::start(4, (5, 5));
    let tmp = Tmp::new("pre");
    let b = backend(&tmp, &srv);
    let list = ids(6);
    b.art_pixels(&list[0], 160).unwrap();
    let n = b.art_prefetch_pass(&list, b.0.epoch.load(Ordering::SeqCst));
    assert_eq!(n, 5, "the cached master is skipped");
    assert_eq!(srv.requests.load(Ordering::SeqCst), 6);
    assert_eq!(b.art_prefetch_pass(&list, b.0.epoch.load(Ordering::SeqCst)), 0, "second pass is a no-op");
    let before = srv.requests.load(Ordering::SeqCst);
    for id in &list {
        b.art_pixels(id, 240).unwrap();
    }
    assert_eq!(srv.requests.load(Ordering::SeqCst), before, "grid after prefetch is served from disk");
}

fn reqs(s: &Server) -> usize {
    s.requests.load(Ordering::SeqCst)
}

fn epoch(b: &RealBackend) -> u64 {
    b.0.epoch.load(Ordering::SeqCst)
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let t = Instant::now();
    while !cond() {
        assert!(t.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn decode_and_fetch_limits_hold() {
    let srv = Server::start(32, (30, 30));
    let tmp = Tmp::new("limits");
    let b = backend(&tmp, &srv);
    all_at_once(&b, &ids(24), 240);
    assert!(b.0.art.fetch_gate.peak.load(Ordering::Relaxed) <= MAX_CONCURRENT_FETCHES);
    assert!(b.0.art.decode_gate.peak.load(Ordering::Relaxed) <= MAX_CONCURRENT_DECODES);
}

#[test]
fn prefetch_runs_two_at_a_time() {
    let srv = Server::start(32, (40, 40));
    let tmp = Tmp::new("pre2");
    let b = backend(&tmp, &srv);
    assert_eq!(b.art_prefetch_pass(&ids(12), epoch(&b)), 12);
    assert_eq!(srv.max_active.load(Ordering::SeqCst), PREFETCH_CONCURRENCY);
    assert_eq!(b.0.art.bg_gate.peak.load(Ordering::Relaxed), PREFETCH_CONCURRENCY);
}

#[test]
fn prefetch_stops_when_the_epoch_moves_and_frees_the_slot() {
    let srv = Server::start(32, (40, 40));
    let tmp = Tmp::new("epoch");
    let b = backend(&tmp, &srv);
    let list = ids(30);
    let e0 = epoch(&b);
    std::thread::scope(|s| {
        let h = s.spawn(|| b.art_prefetch_pass(&list, e0));
        wait_until("first prefetch requests", || reqs(&srv) >= 2);
        b.0.epoch.fetch_add(1, Ordering::SeqCst); // logout / re-login
        let done = h.join().unwrap();
        assert!(done < 10, "the old pass stopped early (fetched {done})");
    });
    assert!(b.0.art.prefetch_run.lock().unwrap().is_none(), "the slot was released");
    // the new session's pass runs at once and only fetches what is still missing
    let before = reqs(&srv);
    let n = b.art_prefetch_pass(&list, epoch(&b));
    assert_eq!(reqs(&srv) - before, n);
    assert!(list.iter().all(|id| cache_path(&b.art_dir(), id, MASTER_SIZE).exists()));
    // a pass that is stale from the start fetches nothing
    assert_eq!(b.art_prefetch_pass(&ids(3), e0), 0);
}

#[test]
fn prefetch_yields_to_foreground_requests() {
    let srv = Server::start(8, (5, 5));
    let tmp = Tmp::new("fg");
    let b = backend(&tmp, &srv);
    let list = ids(6);
    let guard = ForegroundGuard::new(&b.0.art.foreground);
    std::thread::scope(|s| {
        let h = s.spawn(|| b.art_prefetch_pass(&list, epoch(&b)));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(reqs(&srv), 0, "paused while a foreground request is running");
        drop(guard);
        assert_eq!(h.join().unwrap(), 6);
    });
}

#[test]
fn prefetch_pauses_while_suspended_and_cancels_there() {
    let srv = Server::start(8, (5, 5));
    let tmp = Tmp::new("susp");
    let b = backend(&tmp, &srv);
    let list = ids(6);
    b.0.art.suspended.store(true, Ordering::SeqCst);
    std::thread::scope(|s| {
        let h = s.spawn(|| b.art_prefetch_pass(&list, epoch(&b)));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(reqs(&srv), 0, "no network while minimized");
        b.0.art.suspended.store(false, Ordering::SeqCst);
        assert_eq!(h.join().unwrap(), 6, "resumes on restore");
    });
    // a pass paused by minimize ends when the session changes
    b.0.art.suspended.store(true, Ordering::SeqCst);
    let list2: Vec<String> = (0..4).map(|i| format!("other-{i}")).collect();
    let e = epoch(&b);
    std::thread::scope(|s| {
        let h = s.spawn(|| b.art_prefetch_pass(&list2, e));
        std::thread::sleep(Duration::from_millis(100));
        b.0.epoch.fetch_add(1, Ordering::SeqCst);
        assert_eq!(h.join().unwrap(), 0);
    });
    assert_eq!(reqs(&srv), 6);
}

#[test]
fn prefetch_gives_up_on_an_unreachable_server() {
    let srv = Server::start(4, (1, 1));
    let tmp = Tmp::new("down");
    let b = backend(&tmp, &srv);
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    *b.0.client.write().unwrap() = Some(Client::new(&dead, "me", "pw").unwrap());
    assert_eq!(b.art_prefetch_pass(&ids(60), epoch(&b)), 0);
    let attempts = b.0.art.bg_attempts.load(Ordering::Relaxed);
    assert!((PREFETCH_MAX_NET_ERRORS..PREFETCH_MAX_NET_ERRORS + PREFETCH_CONCURRENCY).contains(&attempts), "attempts {attempts}");
}

#[test]
fn undecodable_master_falls_back_to_a_sized_request_once() {
    let srv = Server::start_opts(4, (5, 5), true);
    let tmp = Tmp::new("bad");
    let b = backend(&tmp, &srv);
    let p = b.art_pixels("odd-1", 240).unwrap();
    assert_eq!((p.width, p.height), (240, 240));
    assert_eq!(srv.hits_for("odd-1"), 2, "master (kept) + the direct 240 request");
    assert_eq!(b.art_pixels("odd-1", 240).unwrap().width, 240);
    assert_eq!(reqs(&srv), 2, "second read is the bucket file");
    b.art_pixels("odd-1", 160).unwrap();
    assert_eq!(reqs(&srv), 3, "known-undecodable: straight to the sized request, the master is not decoded again");
    assert_eq!(b.art_pixels("odd-1", 640).unwrap_err().code, ErrorCode::Decode);
    assert_eq!(reqs(&srv), 3);
    // the stored master is a cache hit for later syncs: not re-requested
    assert_eq!(b.art_prefetch_pass(&["odd-1".to_string()], epoch(&b)), 0);
    assert_eq!(reqs(&srv), 3);
}

#[test]
fn sniffing_accepts_the_supported_formats() {
    assert!(looks_like_image(&[0xFF, 0xD8, 0xFF, 0xE0, 0]));
    assert!(looks_like_image(&[0x89, b'P', b'N', b'G', 0, 0]));
    assert!(looks_like_image(b"GIF89a......"));
    assert!(looks_like_image(b"RIFF\x20\0\0\0WEBPVP8 xx"));
    assert!(looks_like_image(b"\0\0\0\x1cftypavif\0\0\0\0"));
    assert!(!looks_like_image(b"{\"subsonic-response\":{}}"));
}

#[test]
fn single_flight_survives_a_panicking_leader() {
    let flight: SingleFlight<u32> = SingleFlight::new();
    let key = PathBuf::from("k");
    let runs = AtomicUsize::new(0);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    std::thread::scope(|s| {
        let leader = s.spawn(|| {
            flight.run(&key, || {
                started_tx.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(150));
                panic!("leader dies");
            })
        });
        started_rx.recv().unwrap();
        let waiters: Vec<_> = (0..3)
            .map(|_| {
                s.spawn(|| {
                    flight.run(&key, || {
                        runs.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(30));
                        7
                    })
                })
            })
            .collect();
        assert!(leader.join().is_err());
        for w in waiters {
            assert_eq!(w.join().unwrap(), 7);
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1, "one waiter took over, the others shared its result");
    });
}

#[test]
#[ignore = "benchmark"]
fn bench_grid() {
    let list = ids(60);
    for (label, slots) in [("server 4 slots, 30-80 ms", 4usize), ("server 2 slots, 30-80 ms", 2)] {
        println!("== {label}, 60 covers at bucket 240");
        let srv = Server::start(slots, (30, 80));
        let tmp = Tmp::new("bench");
        let b = backend(&tmp, &srv);
        let cold = all_at_once(&b, &list, 240);
        let c_conns = srv.conns.load(Ordering::SeqCst);
        println!("cold, 60 parallel callers : {cold:?} ({} requests, {} connections, max active {})", srv.requests.load(Ordering::SeqCst), c_conns, srv.max_active.load(Ordering::SeqCst));
        let warm = all_at_once(&b, &list, 240);
        println!("warm, 60 parallel callers : {warm:?}");
        let tmp2 = Tmp::new("bench2");
        let srv2 = Server::start(slots, (30, 80));
        let b2 = backend(&tmp2, &srv2);
        let cold4 = queued(&b2, &list, 240, 4);
        println!("cold, 4 queued workers    : {cold4:?} ({} requests, {} connections)", srv2.requests.load(Ordering::SeqCst), srv2.conns.load(Ordering::SeqCst));
        let warm4 = queued(&b2, &list, 240, 4);
        println!("warm, 4 queued workers    : {warm4:?}");
        // second bucket for the same covers (album header 320 after the 240 grid)
        let r = srv2.requests.load(Ordering::SeqCst);
        let other = queued(&b2, &list, 320, 4);
        println!("other bucket (320), 4 w.  : {other:?} (+{} requests)", srv2.requests.load(Ordering::SeqCst) - r);
    }
}
