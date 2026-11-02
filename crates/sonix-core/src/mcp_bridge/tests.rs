use std::io::BufReader;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sonix_mcp::wire::{self, Discovery};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::watch;

use super::tools::tests::{seed, Recorder};
use super::tools::{self, Ctx};
use super::transport::{BoxedStream, Listener};
use super::*;
use crate::audio::EngineCommand;
use crate::library::Db;
use crate::smartlists::SmartStore;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const SERVER_KEY: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
const NONCE: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn keys() -> Keys {
    Keys { token: TOKEN.into(), server_key: SERVER_KEY.into() }
}

/// Real tool handlers over an in-memory DB and a recording player.
struct TestDispatch {
    db: Db,
    smart: SmartStore,
    player: Recorder,
    allow_control: AtomicBool,
    calls: AtomicUsize,
    _dir: tempfile::TempDir,
}

impl TestDispatch {
    fn new() -> Arc<Self> {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        let dir = tempfile::tempdir().unwrap();
        Arc::new(Self {
            smart: SmartStore::new(dir.path()),
            db,
            player: Recorder::default(),
            allow_control: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            _dir: dir,
        })
    }
}

impl Dispatch for TestDispatch {
    fn call(&self, tool: &str, args: &Value) -> Result<Value, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let ctx = Ctx {
            db: &self.db,
            smart: &self.smart,
            player: &self.player,
            me: Some("me".into()),
            allow_control: self.allow_control.load(Ordering::SeqCst),
        };
        tools::call(&ctx, tool, args)
    }
}

struct SlowDispatch(Duration);
impl Dispatch for SlowDispatch {
    fn call(&self, _tool: &str, _args: &Value) -> Result<Value, String> {
        std::thread::sleep(self.0);
        Ok(json!({}))
    }
}

struct BigDispatch;
impl Dispatch for BigDispatch {
    fn call(&self, _tool: &str, _args: &Value) -> Result<Value, String> {
        Ok(json!({ "blob": "x".repeat(10_000) }))
    }
}

fn fast_limits() -> Limits {
    Limits {
        hello_timeout: Duration::from_millis(300),
        idle_timeout: Duration::from_millis(500),
        call_timeout: Duration::from_millis(300),
        max_request_line: 2048,
        max_result_bytes: 4096,
        ..Limits::default()
    }
}

type Client = (tokio::io::BufReader<tokio::io::ReadHalf<DuplexStream>>, tokio::io::WriteHalf<DuplexStream>);

/// Starts `serve_connection` on one end of a duplex pipe, returns the client end.
fn connect(dispatch: Arc<dyn Dispatch>, limits: Limits) -> (Client, watch::Sender<bool>, tokio::task::JoinHandle<()>) {
    let (a, b) = tokio::io::duplex(1 << 16);
    let (tx, rx) = watch::channel(false);
    let h = tokio::spawn(async move { serve_connection(a, &keys(), dispatch, &limits, rx).await });
    let (r, w) = tokio::io::split(b);
    ((tokio::io::BufReader::new(r), w), tx, h)
}

async fn send(c: &mut Client, line: &str) {
    c.1.write_all(line.as_bytes()).await.unwrap();
    c.1.write_all(b"\n").await.unwrap();
}

async fn recv(c: &mut Client) -> Option<Value> {
    let mut s = String::new();
    let n = tokio::time::timeout(Duration::from_secs(3), c.0.read_line(&mut s)).await.expect("server answered in time").unwrap();
    (n > 0).then(|| serde_json::from_str(&s).unwrap())
}

/// The client half of the handshake: checks the app's proof, then sends the token.
async fn handshake(c: &mut Client) {
    send(c, &wire::hello_line(NONCE)).await;
    let r = recv(c).await.unwrap();
    assert_eq!(r["ok"], true);
    assert!(wire::verify_server_proof(SERVER_KEY, NONCE, r["proof"].as_str().unwrap()), "the app proves it knows the server key");
    send(c, &wire::token_line(TOKEN)).await;
    assert_eq!(recv(c).await.unwrap(), json!({ "ok": true }));
}

async fn authed(dispatch: Arc<dyn Dispatch>, limits: Limits) -> (Client, watch::Sender<bool>, tokio::task::JoinHandle<()>) {
    let (mut c, tx, h) = connect(dispatch, limits);
    handshake(&mut c).await;
    (c, tx, h)
}

#[test]
fn constant_time_compare() {
    assert!(ct_eq(b"abc", b"abc"));
    assert!(!ct_eq(b"abc", b"abd"));
    assert!(!ct_eq(b"abc", b"abcd"));
    assert!(!ct_eq(b"", b"a"));
    assert!(ct_eq(b"", b""));
}

#[tokio::test]
async fn bad_hello_is_rejected_and_nothing_runs() {
    let d = TestDispatch::new();
    // Rejected at the hello (no proof is handed out).
    for hello in [
        wire::hello_line(""),
        wire::hello_line("abc"),            // nonce too short
        wire::hello_line(&"zz".repeat(32)), // not hex
        json!({ "hello": "sonix-mcp", "v": 2 }).to_string(),
        json!({ "hello": "other", "v": 2, "nonce": NONCE }).to_string(),
        json!({ "tool": "next", "args": {} }).to_string(),
        "not json".to_string(),
        "[]".to_string(),
    ] {
        let (mut c, _tx, h) = connect(d.clone(), fast_limits());
        send(&mut c, &hello).await;
        send(&mut c, &wire::request_line("search", &json!({ "query": "a" }))).await;
        let r = recv(&mut c).await.unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "unauthorized" }), "{hello}");
        assert!(recv(&mut c).await.is_none(), "connection is closed after rejection");
        h.await.unwrap();
    }
    // A valid hello, then a wrong or missing token: proof given, then rejected.
    for token_line in [wire::token_line("wrong-token"), wire::token_line(""), wire::request_line("search", &json!({}))] {
        let (mut c, _tx, h) = connect(d.clone(), fast_limits());
        send(&mut c, &wire::hello_line(NONCE)).await;
        assert_eq!(recv(&mut c).await.unwrap()["ok"], true);
        send(&mut c, &token_line).await;
        send(&mut c, &wire::request_line("search", &json!({ "query": "a" }))).await;
        assert_eq!(recv(&mut c).await.unwrap(), json!({ "ok": false, "error": "unauthorized" }), "{token_line}");
        assert!(recv(&mut c).await.is_none());
        h.await.unwrap();
    }
    // An old (protocol 1) client gets a clear version message, not a hang.
    let (mut c, _tx, h) = connect(d.clone(), fast_limits());
    send(&mut c, &json!({ "hello": "sonix-mcp", "v": 1, "token": TOKEN }).to_string()).await;
    let r = recv(&mut c).await.unwrap();
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("protocol mismatch"), "{r}");
    assert!(recv(&mut c).await.is_none());
    h.await.unwrap();
    assert_eq!(d.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn silent_or_oversized_hello_is_dropped() {
    let d = TestDispatch::new();
    let (mut c, _tx, h) = connect(d.clone(), fast_limits());
    // Says nothing: dropped after the hello timeout.
    assert_eq!(recv(&mut c).await.unwrap()["error"], "unauthorized");
    h.await.unwrap();
    let (mut c, _tx, h) = connect(d, fast_limits());
    let big = "x".repeat(10_000);
    send(&mut c, &big).await;
    assert_eq!(recv(&mut c).await.unwrap()["error"], "unauthorized");
    h.await.unwrap();
}

#[tokio::test]
async fn requests_malformed_oversized_slow_and_big() {
    let d = TestDispatch::new();
    let (mut c, _tx, h) = authed(d.clone(), fast_limits()).await;
    send(&mut c, "garbage").await;
    assert_eq!(recv(&mut c).await.unwrap(), json!({ "ok": false, "error": "malformed request" }));
    send(&mut c, "{}").await;
    assert_eq!(recv(&mut c).await.unwrap()["error"], "malformed request");
    send(&mut c, &wire::request_line("search", &json!({ "query": "alph" }))).await;
    let r = recv(&mut c).await.unwrap();
    assert_eq!(r["ok"], true);
    assert_eq!(r["result"]["tracks"]["items"][0]["id"], "t1");
    // Tool errors come back as `ok:false` and keep the connection usable.
    send(&mut c, &wire::request_line("get_album", &json!({ "id": "zzz" }))).await;
    assert!(recv(&mut c).await.unwrap()["error"].as_str().unwrap().contains("not found"));
    // An oversized line ends the session.
    send(&mut c, &"y".repeat(5000)).await;
    assert!(recv(&mut c).await.unwrap()["error"].as_str().unwrap().contains("too large"));
    assert!(recv(&mut c).await.is_none());
    h.await.unwrap();

    // A tool that takes too long.
    let (mut c, _tx, h) = authed(Arc::new(SlowDispatch(Duration::from_millis(900))), fast_limits()).await;
    send(&mut c, &wire::request_line("search", &json!({}))).await;
    assert_eq!(recv(&mut c).await.unwrap()["error"], "the tool call timed out");
    drop(c);
    h.await.unwrap();

    // A result over the cap is refused instead of sent.
    let (mut c, _tx, _h) = authed(Arc::new(BigDispatch), fast_limits()).await;
    send(&mut c, &wire::request_line("search", &json!({}))).await;
    assert!(recv(&mut c).await.unwrap()["error"].as_str().unwrap().contains("too large"));
}

#[tokio::test]
async fn idle_connections_are_closed_and_shutdown_ends_open_ones() {
    let d = TestDispatch::new();
    let (mut c, _tx, h) = authed(d.clone(), fast_limits()).await;
    assert!(recv(&mut c).await.is_none(), "idle timeout closes the connection");
    h.await.unwrap();

    let mut limits = fast_limits();
    limits.idle_timeout = Duration::from_secs(30);
    let (mut c, tx, h) = authed(d, limits).await;
    tx.send(true).unwrap();
    assert!(recv(&mut c).await.is_none(), "disabling closes open connections");
    h.await.unwrap();
}

/// Hands out pre-made duplex streams as "accepted connections".
struct ChannelListener(tokio::sync::mpsc::Receiver<DuplexStream>);

impl Listener for ChannelListener {
    fn accept(&mut self) -> Pin<Box<dyn std::future::Future<Output = std::io::Result<BoxedStream>> + Send + '_>> {
        Box::pin(async move {
            match self.0.recv().await {
                Some(s) => Ok(Box::new(s) as BoxedStream),
                None => std::future::pending().await,
            }
        })
    }
}

#[tokio::test]
async fn connection_cap_drops_the_fifth_client() {
    let d = TestDispatch::new();
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let (sd_tx, sd_rx) = watch::channel(false);
    let mut limits = fast_limits();
    limits.idle_timeout = Duration::from_secs(30);
    limits.hello_timeout = Duration::from_secs(30);
    let server = tokio::spawn(serve(ChannelListener(rx), keys(), d, limits, sd_rx));
    let mut clients = Vec::new();
    for _ in 0..4 {
        let (a, b) = tokio::io::duplex(1 << 16);
        tx.send(a).await.unwrap();
        let (r, w) = tokio::io::split(b);
        let mut c: Client = (tokio::io::BufReader::new(r), w);
        handshake(&mut c).await;
        clients.push(c);
    }
    let (a, b) = tokio::io::duplex(1 << 16);
    tx.send(a).await.unwrap();
    let (r, _w) = tokio::io::split(b);
    let mut fifth: Client = (tokio::io::BufReader::new(r), _w);
    assert!(recv(&mut fifth).await.is_none(), "the 5th connection is closed without being served");
    // Freeing a slot lets a new client in.
    drop(clients.pop());
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (a, b) = tokio::io::duplex(1 << 16);
    tx.send(a).await.unwrap();
    let (r, w) = tokio::io::split(b);
    let mut again: Client = (tokio::io::BufReader::new(r), w);
    handshake(&mut again).await;
    sd_tx.send(true).unwrap();
    server.await.unwrap();
}

#[test]
fn discovery_file_is_written_and_removed() {
    let dir = tempfile::tempdir().unwrap();
    let d = Discovery { endpoint: "e".into(), token: random_hex(32).unwrap(), server_key: random_hex(32).unwrap() };
    assert_eq!(d.token.len(), 64);
    assert_ne!(random_hex(32).unwrap(), random_hex(32).unwrap(), "tokens are random");
    write_discovery(dir.path(), &d).unwrap();
    let back = Discovery::parse(&std::fs::read_to_string(Discovery::path(dir.path())).unwrap()).unwrap();
    assert_eq!(back, d);
    assert!(!dir.path().join("mcp.json.tmp").exists());
    // Rewriting replaces the token.
    let d2 = Discovery { endpoint: "e2".into(), token: random_hex(32).unwrap(), server_key: random_hex(32).unwrap() };
    write_discovery(dir.path(), &d2).unwrap();
    assert_eq!(Discovery::parse(&std::fs::read_to_string(Discovery::path(dir.path())).unwrap()).unwrap(), d2);
}

// ---------------------------------------------------------------------------
// End to end: the real `sonix-mcp` stdio loop -> real pipe/socket -> real
// tool handlers over an in-memory DB.
// ---------------------------------------------------------------------------

fn bind_test(dir: &std::path::Path) -> (String, impl Listener) {
    #[cfg(windows)]
    {
        let _ = dir;
        let endpoint = format!(r"\\.\pipe\sonix-mcp-test-{}", random_hex(8).unwrap());
        let l = transport::bind(&endpoint).unwrap();
        (endpoint, l)
    }
    #[cfg(unix)]
    {
        let path = dir.join("mcp").join("t.sock");
        let l = transport::bind(&path).unwrap();
        (path.to_string_lossy().into_owned(), l)
    }
}

/// Drives `sonix_mcp::stdio::run` with `lines`, returning its replies.
async fn run_client(cfg: &sonix_mcp::Config, lines: Vec<Value>) -> Vec<Value> {
    let cfg = cfg.clone();
    tokio::task::spawn_blocking(move || {
        let input: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let mut out = Vec::new();
        sonix_mcp::stdio::run(BufReader::new(input.as_bytes()), &mut out, &cfg).unwrap();
        String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    })
    .await
    .unwrap()
}

fn call_line(id: u64, name: &str, args: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": args } })
}

fn tool_text(reply: &Value) -> (bool, String) {
    let r = &reply["result"];
    (r["isError"].as_bool().unwrap(), r["content"][0]["text"].as_str().unwrap().to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn end_to_end_initialize_list_read_and_gated_control() {
    let dir = tempfile::tempdir().unwrap();
    let d = TestDispatch::new();
    let (endpoint, listener) = bind_test(dir.path());
    let k = Keys { token: random_hex(32).unwrap(), server_key: random_hex(32).unwrap() };
    write_discovery(dir.path(), &Discovery { endpoint, token: k.token.clone(), server_key: k.server_key.clone() }).unwrap();
    let (sd_tx, sd_rx) = watch::channel(false);
    let server = tokio::spawn(serve(listener, k.clone(), d.clone(), Limits::default(), sd_rx));
    let cfg = sonix_mcp::Config { config_dir: Some(dir.path().to_path_buf()), call_timeout: Duration::from_secs(10) };

    let r = run_client(
        &cfg,
        vec![
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "0" } } }),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            call_line(3, "search", json!({ "query": "alph", "type": "track" })),
            call_line(4, "next", json!({})),
            call_line(5, "get_tracks", json!({ "limit": 999999 })),
            call_line(6, "play_pause", json!({ "action": "bogus" })),
        ],
    )
    .await;
    assert_eq!(r.len(), 6, "the notification gets no reply");
    assert_eq!(r[0]["result"]["protocolVersion"], "2025-06-18");
    let listed: Vec<String> =
        r[1]["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(listed.len(), sonix_mcp::catalog::tools().len());

    let (err, text) = tool_text(&r[2]);
    assert!(!err, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["tracks"]["items"][0]["id"], "t1");

    // Control while "Allow playback control" is off: a tool error, nothing sent.
    let (err, text) = tool_text(&r[3]);
    assert!(err);
    assert!(text.contains("Allow playback control"), "{text}");
    assert_eq!(d.player.cmds.lock().unwrap().len(), 0);

    // Paging cap is enforced end to end.
    let v: Value = serde_json::from_str(&tool_text(&r[4]).1).unwrap();
    assert_eq!(v["total"], 3);

    // Flip the setting live: the same tool now works (read per call).
    d.allow_control.store(true, Ordering::SeqCst);
    let r = run_client(&cfg, vec![call_line(1, "next", json!({})), call_line(2, "seek", json!({ "positionSec": 61.5 }))]).await;
    assert!(!tool_text(&r[0]).0, "{}", tool_text(&r[0]).1);
    assert!(!tool_text(&r[1]).0);
    {
        let cmds = d.player.cmds.lock().unwrap();
        assert!(matches!(cmds[0], EngineCommand::Next));
        assert!(matches!(cmds[1], EngineCommand::Seek(s) if s == 61.5));
    }

    // Drift check: every advertised tool reaches a handler (never "unknown tool").
    let calls: Vec<Value> = listed.iter().enumerate().map(|(i, n)| call_line(i as u64 + 1, n, json!({}))).collect();
    let r = run_client(&cfg, calls).await;
    for (reply, name) in r.iter().zip(&listed) {
        let (_, text) = tool_text(reply);
        assert!(!text.starts_with(tools::UNKNOWN_TOOL_PREFIX), "{name}: {text}");
    }

    // A wrong token in the discovery file is refused by the app.
    let endpoint = Discovery::parse(&std::fs::read_to_string(Discovery::path(dir.path())).unwrap()).unwrap().endpoint;
    let with = |token: &str, server_key: &str| Discovery { endpoint: endpoint.clone(), token: token.into(), server_key: server_key.into() };
    write_discovery(dir.path(), &with("bad", &k.server_key)).unwrap();
    let r = run_client(&cfg, vec![call_line(1, "library_stats", json!({}))]).await;
    let (err, text) = tool_text(&r[0]);
    assert!(err);
    assert_eq!(text, "unauthorized");
    // A wrong server key: the client rejects the app's proof and sends nothing more.
    let before = d.calls.load(Ordering::SeqCst);
    write_discovery(dir.path(), &with(&k.token, "not-the-key")).unwrap();
    let r = run_client(&cfg, vec![call_line(1, "library_stats", json!({}))]).await;
    let (err, text) = tool_text(&r[0]);
    assert!(err && text.contains("is not SONIX"), "{text}");
    assert_eq!(d.calls.load(Ordering::SeqCst), before);

    sd_tx.send(true).unwrap();
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_reports_disabled_and_not_running() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = sonix_mcp::Config { config_dir: Some(dir.path().to_path_buf()), call_timeout: Duration::from_secs(5) };
    // No discovery file: MCP is not enabled.
    let r = run_client(&cfg, vec![call_line(1, "library_stats", json!({}))]).await;
    let (err, text) = tool_text(&r[0]);
    assert!(err && text.contains("not enabled"), "{text}");
    // File present but nobody listens (app not running): a clear message.
    let endpoint = if cfg!(windows) { r"\\.\pipe\sonix-mcp-test-nobody-home".to_string() } else { dir.path().join("nobody.sock").to_string_lossy().into_owned() };
    write_discovery(dir.path(), &Discovery { endpoint, token: "t".into(), server_key: "k".into() }).unwrap();
    let r = run_client(&cfg, vec![call_line(1, "library_stats", json!({}))]).await;
    let (err, text) = tool_text(&r[0]);
    assert!(err && text.contains("not running"), "{text}");
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn windows_pipe_name_cannot_be_pre_created_by_someone_else() {
    // first_pipe_instance(true): binding the same name again must fail.
    let endpoint = format!(r"\\.\pipe\sonix-mcp-test-{}", random_hex(8).unwrap());
    let _first = transport::bind(&endpoint).unwrap();
    assert!(transport::bind(&endpoint).is_err(), "a second first-instance on the same name must be refused");
}

/// A client that connects and vanishes must not leave the listener stuck on a dead
/// instance: a real client afterwards is still served.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn windows_listener_survives_a_client_that_connects_and_drops() {
    let endpoint = format!(r"\\.\pipe\sonix-mcp-test-{}", random_hex(8).unwrap());
    let mut listener = transport::bind(&endpoint).unwrap();
    // Connect and drop right away, then let the listener see it (any result is fine).
    drop(std::fs::OpenOptions::new().read(true).write(true).open(&endpoint).unwrap());
    let _ = tokio::time::timeout(Duration::from_secs(2), listener.accept()).await;
    // A second, well-behaved client must be accepted within a few attempts.
    let ep = endpoint.clone();
    let client = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().read(true).write(true).open(&ep).unwrap();
        f.write_all(b"hi\n").unwrap();
        f
    });
    let mut served = false;
    for _ in 0..4 {
        match tokio::time::timeout(Duration::from_secs(2), listener.accept()).await {
            Ok(Ok(mut s)) => {
                let mut buf = [0u8; 3];
                tokio::io::AsyncReadExt::read_exact(&mut s, &mut buf).await.unwrap();
                served = &buf == b"hi\n";
                break;
            }
            Ok(Err(_)) => continue,
            Err(_) => break,
        }
    }
    let _keep = client.await.unwrap();
    assert!(served, "the listener must recover and serve the next client");
}

#[test]
fn discovery_file_is_removed_on_exit() {
    let dir = tempfile::tempdir().unwrap();
    write_discovery(dir.path(), &Discovery { endpoint: "e".into(), token: random_hex(32).unwrap(), server_key: "k".into() }).unwrap();
    remove_discovery_on_exit(dir.path());
    assert!(!Discovery::path(dir.path()).exists(), "no stale endpoint/token after exit");
    // A second call (nothing to remove) is harmless.
    remove_discovery_on_exit(dir.path());
}

/// A squatter on the endpoint (it does not know `serverKey`) answers the hello
/// with a made-up proof: the real `sonix-mcp` client must stop right there,
/// so the squatter sees only the nonce, never the token or the request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn end_to_end_a_fake_server_never_receives_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let (endpoint, mut listener) = bind_test(dir.path());
    let token = random_hex(32).unwrap();
    write_discovery(dir.path(), &Discovery { endpoint, token: token.clone(), server_key: random_hex(32).unwrap() }).unwrap();
    let fake = tokio::spawn(async move {
        let stream = listener.accept().await.unwrap();
        let (r, mut w) = tokio::io::split(stream);
        let mut r = tokio::io::BufReader::new(r);
        let mut seen = String::new();
        let mut line = String::new();
        // Answer the hello with a proof made from a key it has to guess.
        r.read_line(&mut line).await.unwrap();
        seen.push_str(&line);
        let nonce = serde_json::from_str::<Value>(&line).unwrap()["nonce"].as_str().unwrap().to_string();
        let bogus = json!({ "ok": true, "v": wire::BRIDGE_VERSION, "proof": wire::server_proof("guessed-key", &nonce) });
        w.write_all(format!("{bogus}\n").as_bytes()).await.unwrap();
        // Record anything else the client sends until it hangs up.
        loop {
            line.clear();
            match tokio::time::timeout(Duration::from_secs(5), r.read_line(&mut line)).await {
                Ok(Ok(n)) if n > 0 => seen.push_str(&line),
                _ => break,
            }
        }
        seen
    });
    let cfg = sonix_mcp::Config { config_dir: Some(dir.path().to_path_buf()), call_timeout: Duration::from_secs(10) };
    let r = run_client(&cfg, vec![call_line(1, "search", json!({ "query": "private query" }))]).await;
    let (err, text) = tool_text(&r[0]);
    assert!(err && text.contains("is not SONIX"), "{text}");
    let seen = fake.await.unwrap();
    assert_eq!(seen.lines().count(), 1, "only the hello reached the fake server: {seen}");
    assert!(!seen.contains(&token) && !seen.contains("private query"), "{seen}");
}

/// Unix: an old listener dropped after a new one bound the same path must
/// not delete the new socket (it only removes the file it created itself).
#[cfg(unix)]
#[tokio::test]
async fn unix_drop_removes_only_its_own_socket() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp").join("same.sock");
    let old = transport::bind(&path).unwrap();
    let new = transport::bind(&path).unwrap();
    drop(old);
    assert!(path.exists(), "the newer listener's socket survives the old one's drop");
    drop(new);
    assert!(!path.exists(), "a listener removes its own socket");
}
