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
                        let body = images.lock().unwrap().entry((seed, size)).or_insert_with(|| Arc::new(jpeg(size, seed))).clone();
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
        let p = PathBuf::from("<build-dir>/tmp").join(format!("art-perf-{tag}-{}", std::process::id()));
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
