//! The MCP bridge's timeouts on tokio's virtual clock (`start_paused`): production limits (5 s hello, 60 s idle,
//! 45 s per tool call) checked to the millisecond without waiting for them. In-memory duplex pipes, no sockets.
//! The paused clock auto-advances whenever every task is idle, so "nothing happens for 60 s" takes no real time.
use serde_json::{Value, json};
use asphonix_core::mcp_bridge::{Dispatch, Keys, Limits, serve_connection};
use asphonix_mcp::wire;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::sync::watch;
use tokio::time::Instant;

const NONCE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Echo;
impl Dispatch for Echo {
    fn call(&self, tool: &str, _args: &Value) -> Result<Value, String> {
        Ok(json!({ "tool": tool }))
    }
}

/// Blocks every call until the test lets it go (a tool that hangs).
struct Hang(Mutex<mpsc::Receiver<()>>);
impl Dispatch for Hang {
    fn call(&self, _tool: &str, _args: &Value) -> Result<Value, String> {
        let _ = self.0.lock().unwrap().recv();
        Ok(json!("late"))
    }
}

fn keys() -> Keys {
    Keys { token: "t0k3n".into(), server_key: "s3rv3r".into() }
}

struct Client {
    r: BufReader<tokio::io::ReadHalf<DuplexStream>>,
    w: tokio::io::WriteHalf<DuplexStream>,
}

impl Client {
    async fn send(&mut self, line: &str) {
        self.w.write_all(format!("{line}\n").as_bytes()).await.unwrap();
    }
    /// The next JSON line, `None` at EOF.
    async fn recv(&mut self) -> Option<Value> {
        let mut s = String::new();
        let n = self.r.read_line(&mut s).await.unwrap();
        (n > 0).then(|| serde_json::from_str(&s).unwrap())
    }
}

/// A bridge connection served on the runtime (production limits); the shutdown sender keeps it open.
fn connect(d: Arc<dyn Dispatch>) -> (Client, watch::Sender<bool>, tokio::task::JoinHandle<()>) {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (tx, rx) = watch::channel(false);
    let h = tokio::spawn(async move { serve_connection(server, &keys(), d, &Limits::default(), rx).await });
    let (r, w) = tokio::io::split(client);
    (Client { r: BufReader::new(r), w }, tx, h)
}

async fn authed(d: Arc<dyn Dispatch>) -> (Client, watch::Sender<bool>, tokio::task::JoinHandle<()>) {
    let (mut c, tx, h) = connect(d);
    c.send(&wire::hello_line(NONCE)).await;
    let proof = c.recv().await.unwrap();
    assert!(wire::verify_server_proof("s3rv3r", NONCE, proof["proof"].as_str().unwrap()));
    c.send(&wire::token_line("t0k3n")).await;
    assert_eq!(c.recv().await.unwrap(), json!({ "ok": true }));
    (c, tx, h)
}

#[tokio::test(start_paused = true)]
async fn a_silent_client_is_dropped_exactly_at_the_hello_timeout() {
    let t0 = Instant::now();
    let (mut c, _tx, h) = connect(Arc::new(Echo));
    assert_eq!(c.recv().await.unwrap(), json!({ "ok": false, "error": "unauthorized" }));
    assert_eq!(t0.elapsed(), Limits::default().hello_timeout);
    assert!(c.recv().await.is_none());
    h.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_token_step_has_its_own_hello_timeout() {
    let (mut c, _tx, h) = connect(Arc::new(Echo));
    tokio::time::sleep(Duration::from_millis(4_900)).await;
    c.send(&wire::hello_line(NONCE)).await;
    assert_eq!(c.recv().await.unwrap()["ok"], true, "proof sent: the hello was in time");
    let t = Instant::now();
    assert_eq!(c.recv().await.unwrap()["error"], "unauthorized", "no token");
    assert_eq!(t.elapsed(), Limits::default().hello_timeout);
    h.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn traffic_resets_the_idle_timeout_and_silence_ends_the_connection() {
    let (mut c, _tx, h) = authed(Arc::new(Echo)).await;
    let idle = Limits::default().idle_timeout;
    for _ in 0..3 {
        tokio::time::sleep(idle - Duration::from_millis(1)).await;
        c.send(&wire::request_line("ping", &json!({}))).await;
        assert_eq!(c.recv().await.unwrap(), json!({ "ok": true, "result": { "tool": "ping" } }), "still open just before the timeout");
    }
    let t = Instant::now();
    assert!(c.recv().await.is_none(), "closed after a full idle period");
    assert_eq!(t.elapsed(), idle);
    h.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_hanging_tool_call_answers_timed_out_at_the_call_timeout_and_the_connection_stays_usable() {
    // A running `spawn_blocking` task holds the paused clock (no auto-advance), so time is moved by hand here.
    let (release, rx) = mpsc::channel();
    let (mut c, _tx, h) = authed(Arc::new(Hang(Mutex::new(rx)))).await;
    let call = Limits::default().call_timeout;
    c.send(&wire::request_line("search", &json!({}))).await;
    for _ in 0..10 {
        tokio::task::yield_now().await; // let the server read the request and start the call
    }
    tokio::time::advance(call - Duration::from_millis(1)).await;
    assert!(tokio::time::timeout(Duration::ZERO, c.recv()).await.is_err(), "no answer 1 ms before the call timeout");
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(c.recv().await.unwrap(), json!({ "ok": false, "error": "the tool call timed out" }));
    // the hung call ends in the background; the next one is answered normally
    release.send(()).unwrap();
    release.send(()).unwrap();
    c.send(&wire::request_line("search", &json!({}))).await;
    assert_eq!(c.recv().await.unwrap(), json!({ "ok": true, "result": "late" }));
    drop(c);
    h.await.unwrap();
}
