//! Claude (MCP) bridge, app side. Contract: `docs/MCP.md`.
//!
//! `sonix-mcp.exe` (separate crate, `tools/sonix-mcp`) speaks MCP over stdio to
//! Claude and forwards every tool call here over a local named pipe / Unix
//! socket. This module owns that listener: it exists only while
//! `Settings.mcpEnabled` is on, first proves to each client that it is SONIX
//! (HMAC over the client's nonce with `serverKey`), then accepts only a client
//! that presents the token; both secrets come from the user-only discovery
//! file (`mcp.json`, rewritten on every enable). It bounds everything a hostile client could do (line size, connections,
//! timeouts). No TCP/HTTP listener exists.

pub mod sources;
pub mod tools;
mod transport;

use std::path::Path;

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use sonix_mcp_protocol::wire::{self, Discovery};

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{watch, Semaphore};



use transport::{BoxedStream, Listener};

/// Runs one tool call (blocking; invoked on the blocking pool).
pub trait Dispatch: Send + Sync + 'static {
    fn call(&self, tool: &str, args: &Value) -> Result<Value, String>;
}

#[derive(Debug, Clone)]
pub struct Limits {
    pub max_connections: usize,
    pub hello_timeout: Duration,
    pub idle_timeout: Duration,
    pub call_timeout: Duration,
    pub max_request_line: usize,
    pub max_result_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_connections: 4,
            hello_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(60),
            call_timeout: Duration::from_secs(45),
            max_request_line: wire::MAX_REQUEST_LINE,
            max_result_bytes: 6 << 20,
        }
    }
}

use sonix_mcp_protocol::wire::ct_eq;

/// The two secrets of one enable (both also in `mcp.json`): `token` is what
/// clients must present; `server_key` is what this app proves it knows
/// (HMAC over the client's nonce) before a client sends that token.
#[derive(Clone)]
pub struct Keys {
    pub token: String,
    pub server_key: String,
}

/// Async twin of `sonix_mcp_protocol::wire::read_line_capped`.
async fn read_line_capped<R: AsyncBufRead + Unpin>(r: &mut R, max: usize) -> std::io::Result<Option<String>> {
    use std::io::{Error, ErrorKind};
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let (done, used) = {
            let chunk = r.fill_buf().await?;
            if chunk.is_empty() {
                if buf.is_empty() {
                    return Ok(None);
                }
                (true, 0)
            } else if let Some(i) = chunk.iter().position(|&b| b == b'\n') {
                if buf.len() + i > max {
                    return Err(Error::new(ErrorKind::InvalidData, "line too long"));
                }
                buf.extend_from_slice(&chunk[..i]);
                (true, i + 1)
            } else {
                if buf.len() + chunk.len() > max {
                    return Err(Error::new(ErrorKind::InvalidData, "line too long"));
                }
                buf.extend_from_slice(chunk);
                (false, chunk.len())
            }
        };
        r.consume(used);
        if done {
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return String::from_utf8(buf).map(Some).map_err(|_| Error::new(ErrorKind::InvalidData, "not UTF-8"));
        }
    }
}

async fn write_json<W: AsyncWrite + Unpin>(w: &mut W, v: &Value) -> std::io::Result<()> {
    let mut s = v.to_string();
    s.push('\n');
    w.write_all(s.as_bytes()).await?;
    w.flush().await
}

fn fail(msg: &str) -> Value {
    json!({ "ok": false, "error": msg })
}

#[derive(Deserialize)]
struct Hello {
    hello: String,
    v: u64,
    #[serde(default)]
    nonce: Option<String>,
}

#[derive(Deserialize)]
struct TokenLine {
    token: String,
}

/// A client nonce: 32-256 hex digits (the bridge sends 64).
fn valid_nonce(n: &str) -> bool {
    (32..=256).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The handshake (`sonix_mcp_protocol::wire` module doc): hello + nonce -> proof ->
/// token. `Ok(())` = authenticated; `Err(reply)` = send it and close.
async fn authenticate<R, W>(r: &mut R, w: &mut W, keys: &Keys, limits: &Limits) -> Result<(), Value>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let unauthorized = || fail("unauthorized");
    let line = match tokio::time::timeout(limits.hello_timeout, read_line_capped(r, 4096)).await {
        Ok(Ok(Some(line))) => line,
        _ => return Err(unauthorized()),
    };
    let hello = serde_json::from_str::<Hello>(&line).map_err(|_| unauthorized())?;
    if hello.hello != wire::HELLO {
        return Err(unauthorized());
    }
    if hello.v != wire::BRIDGE_VERSION {
        return Err(fail(&format!(
            "sonix-mcp protocol mismatch: this SONIX speaks version {}, the client sent version {}. Use the \
sonix-mcp that matches this SONIX.",
            wire::BRIDGE_VERSION,
            hello.v
        )));
    }
    let nonce = hello.nonce.filter(|n| valid_nonce(n)).ok_or_else(unauthorized)?;
    let proof = json!({ "ok": true, "v": wire::BRIDGE_VERSION, "proof": wire::server_proof(&keys.server_key, &nonce) });
    write_json(w, &proof).await.map_err(|_| unauthorized())?;
    let line = match tokio::time::timeout(limits.hello_timeout, read_line_capped(r, 4096)).await {
        Ok(Ok(Some(line))) => line,
        _ => return Err(unauthorized()),
    };
    let t = serde_json::from_str::<TokenLine>(&line).map_err(|_| unauthorized())?;
    if !ct_eq(t.token.as_bytes(), keys.token.as_bytes()) {
        return Err(unauthorized());
    }
    Ok(())
}

#[derive(Deserialize)]
struct Request {
    tool: String,
    #[serde(default)]
    args: Value,
}

/// Serves one authenticated-or-rejected connection until EOF, an error, the
/// idle timeout or shutdown.
pub async fn serve_connection<S>(
    stream: S,
    keys: &Keys,
    dispatch: Arc<dyn Dispatch>,
    limits: &Limits,
    mut shutdown: watch::Receiver<bool>,
) where
    S: tokio::io::AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (r, mut w) = tokio::io::split(stream);
    let mut r = BufReader::new(r);

    // --- handshake (app proof, then client token) ---
    if let Err(reply) = authenticate(&mut r, &mut w, keys, limits).await {
        let _ = write_json(&mut w, &reply).await;
        return;
    }
    if write_json(&mut w, &json!({ "ok": true })).await.is_err() {
        return;
    }

    // --- requests ---
    loop {
        let line = tokio::select! {
            _ = shutdown.changed() => return,
            l = tokio::time::timeout(limits.idle_timeout, read_line_capped(&mut r, limits.max_request_line)) => l,
        };
        let line = match line {
            Ok(Ok(Some(l))) => l,
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::InvalidData => {
                let _ = write_json(&mut w, &fail("request too large or not UTF-8")).await;
                return;
            }
            _ => return, // EOF, I/O error or idle timeout
        };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Request>(&line) {
            Err(_) => fail("malformed request"),
            Ok(req) => {
                let d = dispatch.clone();
                let work = tokio::task::spawn_blocking(move || d.call(&req.tool, &req.args));
                match tokio::time::timeout(limits.call_timeout, work).await {
                    Err(_) => fail("the tool call timed out"),
                    Ok(Err(_)) => fail("the tool call failed internally"),
                    Ok(Ok(Err(msg))) => fail(&msg),
                    Ok(Ok(Ok(result))) => {
                        let v = json!({ "ok": true, "result": result });
                        if v.to_string().len() > limits.max_result_bytes {
                            fail("result too large; ask for fewer rows")
                        } else {
                            v
                        }
                    }
                }
            }
        };
        if write_json(&mut w, &reply).await.is_err() {
            return;
        }
    }
}

/// Accept loop: at most `limits.max_connections` live connections (extra
/// ones are dropped immediately), ends when `shutdown` flips or its sender
/// goes away; open connections end with it.
pub async fn serve<L: Listener>(
    mut listener: L,
    keys: Keys,
    dispatch: Arc<dyn Dispatch>,
    limits: Limits,
    mut shutdown: watch::Receiver<bool>,
) {
    let permits = Arc::new(Semaphore::new(limits.max_connections));
    let keys = Arc::new(keys);
    let limits = Arc::new(limits);
    let mut backoff = Duration::from_millis(250);
    loop {
        let accepted = tokio::select! {
            _ = shutdown.changed() => break,
            a = listener.accept() => a,
        };
        if *shutdown.borrow() {
            break;
        }
        match accepted {
            Ok(stream) => {
                backoff = Duration::from_millis(250);
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    drop(stream); // over the connection cap
                    continue;
                };
                let (keys, dispatch, limits, sd) = (keys.clone(), dispatch.clone(), limits.clone(), shutdown.clone());
                tokio::spawn(async move {
                    serve_connection::<BoxedStream>(stream, &keys, dispatch, &limits, sd).await;
                    drop(permit);
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "mcp bridge accept failed");
                tokio::select! {
                    _ = shutdown.changed() => break,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

fn random_hex(bytes: usize) -> Result<String, String> {
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::rngs::OsRng.try_fill_bytes(&mut buf).map_err(|e| format!("OS random source failed: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Writes the discovery file (user-only, atomic).
fn write_discovery(dir: &Path, d: &Discovery) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = Discovery::path(dir);
    let tmp = dir.join("mcp.json.tmp");
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(d.to_json().as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)
}

struct Running {
    shutdown: watch::Sender<bool>,
}

#[derive(Default)]
pub struct McpBridge {
    running: tokio::sync::Mutex<Option<Running>>,
    last_error: std::sync::Mutex<Option<String>>,
}

impl McpBridge {
    pub fn is_listening(&self) -> bool {
        self.running.try_lock().map(|g| g.is_some()).unwrap_or(true)
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn set_error(&self, e: Option<String>) {
        *self.last_error.lock().unwrap_or_else(|p| p.into_inner()) = e;
    }

    /// Starts the listener (fresh token + endpoint) and publishes the discovery file. A no-op while
    /// already running. `dispatch` runs the tool calls (the backend re-checks its enabled/control gates per call).
    pub async fn start(&self, dispatch: Arc<dyn Dispatch>, config_dir: &Path) -> Result<(), String> {
        let mut guard = self.running.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        let r = self.start_inner(dispatch, config_dir, &mut guard).await;
        self.set_error(r.as_ref().err().cloned());
        r
    }

    async fn start_inner(&self, dispatch: Arc<dyn Dispatch>, config_dir: &Path, slot: &mut Option<Running>) -> Result<(), String> {
        let keys = Keys { token: random_hex(32)?, server_key: random_hex(32)? };
        // A stale file from a previous run must not point at the new endpoint before the listener exists.
        let _ = std::fs::remove_file(Discovery::path(config_dir));
        #[cfg(windows)]
        let (endpoint, listener) = {
            let endpoint = format!(r"\\.\pipe\sonix-mcp-{}", random_hex(8)?);
            let l = transport::bind(&endpoint).map_err(|e| format!("could not create the local pipe: {e}"))?;
            (endpoint, l)
        };
        #[cfg(unix)]
        let (endpoint, listener) = {
            // A fresh name per enable (like the pipe name on Windows): an old listener still shutting down
            // can never touch the new socket.
            let path = config_dir.join("mcp").join(format!("bridge-{}.sock", random_hex(8)?));
            let l = transport::bind(&path).map_err(|e| format!("could not create the local socket: {e}"))?;
            (path.to_string_lossy().into_owned(), l)
        };
        write_discovery(config_dir, &Discovery { endpoint, token: keys.token.clone(), server_key: keys.server_key.clone() })
            .map_err(|e| format!("could not write the discovery file: {e}"))?;
        let (tx, rx) = watch::channel(false);
        crate::rt::spawn(serve(listener, keys, dispatch, Limits::default(), rx));
        *slot = Some(Running { shutdown: tx });
        tracing::info!("Claude (MCP) bridge listening");
        Ok(())
    }

    /// Closes the listener and every open connection, removes the discovery file.
    pub async fn stop(&self, config_dir: &Path) {
        let mut guard = self.running.lock().await;
        if let Some(r) = guard.take() {
            let _ = r.shutdown.send(true);
            tracing::info!("Claude (MCP) bridge stopped");
        }
        let _ = std::fs::remove_file(Discovery::path(config_dir));
        self.set_error(None);
    }
}

/// App exit: remove the discovery file. Once SONIX is gone its pipe name is free, and any local account can
/// list `\.\pipe\` and create that name; a `sonix-mcp` still reading the old file would then hand its
/// request to that pipe and show its answer to Claude. (A crash still leaves the file behind; see
/// docs/MCP.md.) The next enable writes a fresh endpoint and token.
pub fn remove_discovery_on_exit(config_dir: &Path) {
    let _ = std::fs::remove_file(Discovery::path(config_dir));
}

#[cfg(test)]
mod tests;
