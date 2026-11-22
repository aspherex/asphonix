//! [`MockSubsonic`]: a local Subsonic HTTP server for tests (127.0.0.1, random port; never a real server).
//!
//! * Every request is logged ([`Request`]: raw target, endpoint, exactly parsed query pairs) before it is answered.
//! * Answers, in order: an injected failure ([`MockSubsonic::fail`], [`MockSubsonic::fail_with`]), the per-test
//!   handler ([`MockSubsonic::with_handler`], `None` falls through), the built-in stateful playlist endpoints
//!   (`createPlaylist`, `getPlaylist`, `updatePlaylist`, `deletePlaylist`), else the empty `status:"ok"` envelope.
//! * Injectable latency (all endpoints or one), slept on the connection's own thread: one slow answer never blocks
//!   another (every connection is served on its own thread, `Connection: close`).
//! * Dropping the mock stops the accept loop.
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// One logged request.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// Request target exactly as sent: `/rest/star?u=..&id=t1`.
    pub target: String,
    /// The path's last segment without `.view`: `star`.
    pub endpoint: String,
    /// The raw query string (no `?`).
    pub query: String,
    /// Percent-decoded query pairs in order (repeated keys kept).
    pub pairs: Vec<(String, String)>,
}

impl Request {
    fn parse(target: &str) -> Request {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let endpoint = path.rsplit('/').next().unwrap_or("").trim_end_matches(".view").to_string();
        let pairs = url::form_urlencoded::parse(query.as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        Request { target: target.to_string(), endpoint, query: query.to_string(), pairs }
    }
    /// Every value of `key`, in order.
    pub fn all(&self, key: &str) -> Vec<String> {
        self.pairs.iter().filter(|(k, _)| k == key).map(|(_, v)| v.clone()).collect()
    }
    /// The first value of `key`.
    pub fn first(&self, key: &str) -> Option<String> {
        self.pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }
    pub fn has_key(&self, key: &str) -> bool {
        self.pairs.iter().any(|(k, _)| k == key)
    }
    /// `endpoint` with every pair of `pairs` (exact key and value: `id=t1` never matches `albumId=t1`).
    pub fn matches(&self, endpoint: &str, pairs: &[(&str, &str)]) -> bool {
        self.endpoint == endpoint && pairs.iter().all(|(k, v)| self.pairs.iter().any(|(rk, rv)| rk == k && rv == v))
    }
    /// `endpoint?query`.
    pub fn line(&self) -> String {
        format!("{}?{}", self.endpoint, self.query)
    }
}

/// An HTTP answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl Response {
    pub fn raw(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Response {
        Response { status, content_type: content_type.into(), body: body.into() }
    }
    /// The empty success envelope.
    pub fn ok() -> Response {
        Response::json("")
    }
    /// A success envelope with `fields` (`"playlist":{...}`) after `status` and `version`; empty = no fields.
    pub fn json(fields: &str) -> Response {
        let sep = if fields.is_empty() { "" } else { "," };
        Response::raw(200, "application/json", format!(r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1"{sep}{fields}}}}}"#))
    }
    /// A Subsonic-level failure (`status:"failed"`, HTTP 200).
    pub fn failed(code: u32, message: &str) -> Response {
        Response::raw(200, "application/json", format!(r#"{{"subsonic-response":{{"status":"failed","version":"1.16.1","error":{{"code":{code},"message":"{message}"}}}}}}"#))
    }
    pub fn not_found() -> Response {
        Response::raw(404, "text/plain", Vec::new())
    }
}

type Handler = Arc<dyn Fn(&Request) -> Option<Response> + Send + Sync>;

#[derive(Default)]
struct State {
    log: Vec<Request>,
    /// id -> (name, entry track ids)
    playlists: BTreeMap<String, (String, Vec<String>)>,
    next_playlist: u32,
    /// endpoint (or `*`) -> forced answer
    fail: HashMap<String, Response>,
    latency: Duration,
    latency_for: HashMap<String, Duration>,
}

/// Local mock Subsonic server; see the module docs.
pub struct MockSubsonic {
    url: String,
    st: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn playlist_json(id: &str, name: &str, entries: &[String], with_entries: bool) -> String {
    let song = |id: &str| format!(r#"{{"id":"{id}","title":"T {id}","suffix":"flac","artist":"A"}}"#);
    let e = if with_entries { format!(r#","entry":[{}]"#, entries.iter().map(|i| song(i)).collect::<Vec<_>>().join(",")) } else { String::new() };
    format!(r#""playlist":{{"id":"{id}","name":"{name}","owner":"me","songCount":{},"duration":10.0,"readonly":false{e}}}"#, entries.len())
}

/// The built-in endpoints: playlists are stateful, everything else answers the empty success envelope.
fn builtin(st: &mut State, r: &Request) -> Response {
    let first = |k: &str| r.first(k).unwrap_or_default();
    match r.endpoint.as_str() {
        "createPlaylist" => {
            st.next_playlist += 1;
            let id = format!("pl{}", st.next_playlist);
            let name = first("name");
            st.playlists.insert(id.clone(), (name.clone(), vec![]));
            Response::json(&playlist_json(&id, &name, &[], false))
        }
        "getPlaylist" => match st.playlists.get(&first("id")) {
            Some((name, e)) => Response::json(&playlist_json(&first("id"), name, e, true)),
            None => Response::ok(),
        },
        "updatePlaylist" => {
            if let Some(p) = st.playlists.get_mut(&first("playlistId")) {
                if r.has_key("name") {
                    p.0 = first("name");
                }
                let mut rm: Vec<usize> = r.all("songIndexToRemove").iter().filter_map(|s| s.parse().ok()).collect();
                rm.sort_unstable_by(|a, b| b.cmp(a));
                rm.retain(|i| *i < p.1.len());
                for i in rm {
                    p.1.remove(i);
                }
                p.1.extend(r.all("songIdToAdd"));
            }
            Response::ok()
        }
        "deletePlaylist" => {
            st.playlists.remove(&first("id"));
            Response::ok()
        }
        _ => Response::ok(),
    }
}

/// Reads the request head (up to the blank line).
fn read_head(s: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn serve(mut s: TcpStream, st: Arc<Mutex<State>>, handler: Option<Handler>) {
    let head = read_head(&mut s);
    let target = head.lines().next().unwrap_or("").split_whitespace().nth(1).unwrap_or("/").to_string();
    let req = Request::parse(&target);
    let fail = {
        let mut g = lock(&st);
        g.log.push(req.clone());
        g.fail.get(&req.endpoint).or_else(|| g.fail.get("*")).cloned()
    };
    // the handler runs without the state lock (it may be slow or call back into the mock)
    let resp = match fail {
        Some(f) => f,
        None => match handler.as_ref().and_then(|h| h(&req)) {
            Some(r) => r,
            None => builtin(&mut lock(&st), &req),
        },
    };
    let delay = {
        let g = lock(&st);
        g.latency_for.get(&req.endpoint).copied().unwrap_or(g.latency)
    };
    if !delay.is_zero() {
        std::thread::sleep(delay);
    }
    let reason = if resp.status == 200 { "OK" } else { "X" };
    let _ = write!(s, "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", resp.status, resp.content_type, resp.body.len());
    let _ = s.write_all(&resp.body);
    let _ = s.flush();
}

impl MockSubsonic {
    /// Built-in answers only (stateful playlists + empty success envelope).
    pub fn start() -> MockSubsonic {
        Self::spawn(None)
    }

    /// `handler` answers first; `None` falls through to the built-in endpoints.
    pub fn with_handler(handler: impl Fn(&Request) -> Option<Response> + Send + Sync + 'static) -> MockSubsonic {
        Self::spawn(Some(Arc::new(handler)))
    }

    fn spawn(handler: Option<Handler>) -> MockSubsonic {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
        let url = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let st = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (st2, stop2) = (st.clone(), stop.clone());
        std::thread::Builder::new()
            .name("mock-subsonic".into())
            .spawn(move || {
                for s in l.incoming() {
                    if stop2.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(s) = s else { continue };
                    let (st, h) = (st2.clone(), handler.clone());
                    std::thread::spawn(move || serve(s, st, h));
                }
            })
            .expect("spawn mock server");
        MockSubsonic { url, st, stop }
    }

    /// `http://127.0.0.1:<port>`
    pub fn url(&self) -> &str {
        &self.url
    }

    // ------------------------------------------------------------------ request log
    /// Every request so far.
    pub fn requests(&self) -> Vec<Request> {
        lock(&self.st).log.clone()
    }
    /// Return and clear the request log.
    pub fn take(&self) -> Vec<Request> {
        std::mem::take(&mut lock(&self.st).log)
    }
    /// Raw targets (`/rest/<endpoint>?<query>`) of every request so far.
    pub fn targets(&self) -> Vec<String> {
        lock(&self.st).log.iter().map(|r| r.target.clone()).collect()
    }
    pub fn total(&self) -> usize {
        lock(&self.st).log.len()
    }
    /// Requests to `endpoint`.
    pub fn count(&self, endpoint: &str) -> usize {
        lock(&self.st).log.iter().filter(|r| r.endpoint == endpoint).count()
    }
    /// The first request to `endpoint`.
    pub fn find(&self, endpoint: &str) -> Option<Request> {
        lock(&self.st).log.iter().find(|r| r.endpoint == endpoint).cloned()
    }

    // ------------------------------------------------------------------ playlists
    /// Server-side playlist `id` with these entries (as if created earlier).
    pub fn seed_playlist(&self, id: &str, name: &str, entries: &[&str]) {
        lock(&self.st).playlists.insert(id.into(), (name.into(), entries.iter().map(|s| s.to_string()).collect()));
    }
    /// Entry track ids of server-side playlist `id` (panics if there is none).
    pub fn playlist_entries(&self, id: &str) -> Vec<String> {
        lock(&self.st).playlists[id].1.clone()
    }

    // ------------------------------------------------------------------ faults
    /// `Some("*")` fails every endpoint with `status:"failed"`, `Some(name)` only that endpoint; `None` heals all.
    pub fn fail(&self, on: Option<&str>) {
        let mut g = lock(&self.st);
        g.fail.clear();
        if let Some(ep) = on {
            g.fail.insert(ep.to_string(), Response::failed(0, "boom"));
        }
    }
    /// `endpoint` (or `*`) answers `resp` until [`Self::fail`]`(None)`.
    pub fn fail_with(&self, endpoint: &str, resp: Response) {
        lock(&self.st).fail.insert(endpoint.to_string(), resp);
    }
    /// Every answer waits `d` before it is sent.
    pub fn set_latency(&self, d: Duration) {
        lock(&self.st).latency = d;
    }
    /// Answers of `endpoint` wait `d` (overrides [`Self::set_latency`]).
    pub fn set_latency_for(&self, endpoint: &str, d: Duration) {
        lock(&self.st).latency_for.insert(endpoint.to_string(), d);
    }
}

impl Drop for MockSubsonic {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // wake the blocking accept so the loop sees the flag
        if let Some(addr) = self.url.strip_prefix("http://") {
            let _ = TcpStream::connect(addr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal HTTP/1.1 GET (the mock closes the connection after one answer).
    fn get(m: &MockSubsonic, target: &str) -> (u16, String) {
        let mut s = TcpStream::connect(m.url().strip_prefix("http://").unwrap()).unwrap();
        write!(s, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let status = out.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, out.split("\r\n\r\n").nth(1).unwrap_or("").to_string())
    }

    #[test]
    fn logs_exact_pairs_and_keeps_playlist_state() {
        let m = MockSubsonic::start();
        let (st, body) = get(&m, "/rest/createPlaylist.view?u=me&name=Mix%20Tape");
        assert_eq!(st, 200);
        assert!(body.contains(r#""id":"pl1""#) && body.contains("Mix Tape"), "{body}");
        get(&m, "/rest/updatePlaylist?playlistId=pl1&songIdToAdd=a&songIdToAdd=b&songIdToAdd=c");
        get(&m, "/rest/updatePlaylist?playlistId=pl1&songIndexToRemove=0&songIndexToRemove=2");
        assert_eq!(m.playlist_entries("pl1"), ["b"]);
        let reqs = m.take();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[0].endpoint, "createPlaylist");
        assert_eq!(reqs[0].first("name").as_deref(), Some("Mix Tape"));
        assert_eq!(reqs[1].all("songIdToAdd"), ["a", "b", "c"]);
        assert!(reqs[1].matches("updatePlaylist", &[("playlistId", "pl1")]) && !reqs[1].matches("updatePlaylist", &[("id", "pl1")]));
        assert_eq!(reqs[2].target, "/rest/updatePlaylist?playlistId=pl1&songIndexToRemove=0&songIndexToRemove=2");
        assert!(m.requests().is_empty());
    }

    #[test]
    fn failures_handler_and_fallthrough_in_that_order() {
        let m = MockSubsonic::with_handler(|r| match r.endpoint.as_str() {
            "getCoverArt" => Some(Response::raw(200, "image/png", vec![1, 2, 3])),
            "stream" => Some(Response::not_found()),
            _ => None,
        });
        assert_eq!(get(&m, "/rest/getCoverArt?id=c1").0, 200);
        assert_eq!(get(&m, "/rest/stream?id=t1").0, 404);
        assert!(get(&m, "/rest/ping").1.contains(r#""status":"ok""#));
        m.fail(Some("ping"));
        assert!(get(&m, "/rest/ping").1.contains(r#""status":"failed""#));
        assert_eq!(get(&m, "/rest/getCoverArt?id=c1").0, 200, "only the named endpoint fails");
        m.fail(Some("*"));
        assert!(get(&m, "/rest/getCoverArt?id=c1").1.contains("failed"));
        m.fail(None);
        m.fail_with("stream", Response::raw(500, "text/plain", "x"));
        assert_eq!(get(&m, "/rest/stream").0, 500);
        assert_eq!((m.count("getCoverArt"), m.count("stream"), m.total()), (3, 2, 7));
        assert_eq!(m.find("stream").unwrap().line(), "stream?id=t1");
    }

    #[test]
    fn latency_applies_per_endpoint() {
        let m = MockSubsonic::start();
        m.set_latency_for("slow", Duration::from_millis(150));
        let t = std::time::Instant::now();
        get(&m, "/rest/fast");
        let fast = t.elapsed();
        get(&m, "/rest/slow");
        assert!(t.elapsed() - fast >= Duration::from_millis(150));
    }
}
