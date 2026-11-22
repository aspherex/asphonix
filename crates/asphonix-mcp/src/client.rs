//! Forwarding of one tool call to the running app.
//!
//! One short-lived connection per call: it self-heals across app restarts and
//! never pins one of the app's few connection slots. A synchronous pipe read
//! can block forever, so the exchange runs on a worker thread and the caller
//! waits with a timeout.

use std::fmt;
use std::io::{self, BufReader, Read, Write};
use std::sync::mpsc;

use serde_json::Value;

use crate::wire::{self, Discovery};
use crate::Config;

#[derive(Debug)]
pub enum CallError {
    /// No discovery file: MCP is off, or ASPHONIX is not running (the app
    /// removes the file on exit).
    NotEnabled,
    /// Discovery file present but nothing answers on the endpoint.
    NotRunning,
    /// The app refused the hello or the call (message from the app).
    Refused(String),
    /// Whatever answered on the endpoint could not prove it is ASPHONIX (no
    /// token or request was sent to it).
    NotAuthentic,
    /// Anything else (timeouts, protocol errors).
    Other(String),
}

/// Old ASPHONIX (protocol 1) answering a protocol-2 hello: it finds no token in
/// it and says "unauthorized".
const VERSION_HINT: &str = "ASPHONIX refused the connection. ASPHONIX and asphonix-mcp are probably different versions: \
use the asphonix-mcp that matches this ASPHONIX, then turn Claude (MCP) off and on in ASPHONIX's settings.";

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallError::NotEnabled => f.write_str(
                "ASPHONIX is not running, or Claude (MCP) is not enabled in it. Start ASPHONIX; if this persists, \
open ASPHONIX > Settings > Claude (MCP) and turn it on.",
            ),
            CallError::NotRunning => f.write_str(
                "ASPHONIX is not running (or Claude (MCP) was just turned off). Start ASPHONIX and try again.",
            ),
            CallError::Refused(m) => f.write_str(m),
            CallError::NotAuthentic => f.write_str(
                "The local endpoint named in ASPHONIX's mcp.json is not ASPHONIX (it could not prove it), so nothing \
was sent to it. Restart ASPHONIX, or turn Claude (MCP) off and on in its settings.",
            ),
            CallError::Other(m) => f.write_str(m),
        }
    }
}

trait Conn: Read + Write + Send {}
impl<T: Read + Write + Send> Conn for T {}

#[cfg(windows)]
fn connect(endpoint: &str) -> io::Result<Box<dyn Conn>> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::time::{Duration, Instant};
    // SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION: the pipe server can
    // learn who we are but never impersonate us.
    const SQOS: u32 = 0x0010_0000 | 0x0001_0000;
    const ERROR_PIPE_BUSY: i32 = 231;
    let started = Instant::now();
    loop {
        match std::fs::OpenOptions::new().read(true).write(true).security_qos_flags(SQOS).open(endpoint) {
            Ok(f) => return Ok(Box::new(f)),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && started.elapsed() < Duration::from_secs(3) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(unix)]
fn connect(endpoint: &str) -> io::Result<Box<dyn Conn>> {
    let s = std::os::unix::net::UnixStream::connect(endpoint)?;
    Ok(Box::new(s))
}

fn io_err(e: io::Error) -> CallError {
    CallError::Other(format!("connection to ASPHONIX failed: {e}"))
}

fn send_line<S: Read + Write>(r: &mut BufReader<S>, line: &str) -> Result<(), CallError> {
    let mut line = line.to_string();
    line.push('\n');
    r.get_mut().write_all(line.as_bytes()).map_err(io_err)?;
    r.get_mut().flush().map_err(io_err)
}

/// Hello -> the app proves it knows `serverKey` -> only then the token.
/// Nothing secret (and no request) is written before the proof checks out.
pub fn handshake<S: Read + Write>(r: &mut BufReader<S>, d: &Discovery) -> Result<(), CallError> {
    let nonce = wire::random_hex(32).map_err(|e| CallError::Other(e.to_string()))?;
    send_line(r, &wire::hello_line(&nonce))?;
    let closed = || CallError::Refused("ASPHONIX closed the connection (is Claude (MCP) still enabled?)".into());
    let first = wire::read_line_capped(r, 4096).map_err(io_err)?.ok_or_else(closed)?;
    let v: Value = serde_json::from_str(&first).map_err(|_| CallError::NotAuthentic)?;
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        let msg = v.get("error").and_then(Value::as_str).unwrap_or("");
        // An old app rejects the token-less hello as "unauthorized".
        return Err(CallError::Refused(if msg.is_empty() || msg == "unauthorized" { VERSION_HINT.into() } else { msg.into() }));
    }
    let proof = v.get("proof").and_then(Value::as_str).unwrap_or("");
    if !wire::verify_server_proof(&d.server_key, &nonce, proof) {
        return Err(CallError::NotAuthentic);
    }
    send_line(r, &wire::token_line(&d.token))?;
    let ack = wire::read_line_capped(r, 4096).map_err(io_err)?.ok_or_else(closed)?;
    parse_reply(&ack).map(|_| ())
}

fn exchange(d: &Discovery, tool: &str, args: &Value) -> Result<Value, CallError> {
    let conn = connect(&d.endpoint).map_err(|_| CallError::NotRunning)?;
    let mut r = BufReader::new(conn);
    handshake(&mut r, d)?;
    let mut line = wire::request_line(tool, args);
    line.push('\n');
    r.get_mut().write_all(line.as_bytes()).map_err(io_err)?;
    r.get_mut().flush().map_err(io_err)?;
    let reply = wire::read_line_capped(&mut r, wire::MAX_RESPONSE_LINE)
        .map_err(io_err)?
        .ok_or_else(|| CallError::Other("ASPHONIX closed the connection without answering".into()))?;
    parse_reply(&reply)
}

fn parse_reply(line: &str) -> Result<Value, CallError> {
    let v: Value = serde_json::from_str(line).map_err(|_| CallError::Other("unreadable answer from ASPHONIX".into()))?;
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    } else {
        let msg = v.get("error").and_then(Value::as_str).unwrap_or("ASPHONIX refused the request");
        Err(CallError::Refused(msg.to_string()))
    }
}

/// Forwards one tool call to the app and returns its JSON result.
pub fn call_app(cfg: &Config, tool: &str, args: &Value) -> Result<Value, CallError> {
    let dir = cfg
        .config_dir
        .clone()
        .ok_or_else(|| CallError::Other("cannot locate ASPHONIX's config folder (set ASPHONIX_MCP_CONFIG_DIR)".into()))?;
    let text = std::fs::read_to_string(Discovery::path(&dir)).map_err(|_| CallError::NotEnabled)?;
    let d = Discovery::parse(&text).ok_or(CallError::NotEnabled)?;
    let (tx, rx) = mpsc::channel();
    let (tool_s, args_c) = (tool.to_string(), args.clone());
    std::thread::spawn(move || {
        let _ = tx.send(exchange(&d, &tool_s, &args_c));
    });
    match rx.recv_timeout(cfg.call_timeout) {
        Ok(r) => r,
        Err(_) => Err(CallError::Other(format!("ASPHONIX did not answer within {} s", cfg.call_timeout.as_secs()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// In-memory stand-in for an endpoint: answers the hello with a proof made
    /// from `key` (None = fixed garbage, like a squatter that lacks the key),
    /// or with a fixed first reply, and records every byte the client writes.
    struct Fake {
        key: Option<String>,
        first_reply: Option<String>,
        written: Vec<u8>,
        out: VecDeque<u8>,
    }

    impl Fake {
        fn new(key: Option<&str>, first_reply: Option<&str>) -> Self {
            Self { key: key.map(Into::into), first_reply: first_reply.map(Into::into), written: vec![], out: VecDeque::new() }
        }
        fn queue(&mut self, line: String) {
            self.out.extend(line.into_bytes());
            self.out.push_back(b'\n');
        }
    }

    impl Read for Fake {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(self.out.len());
            for b in buf.iter_mut().take(n) {
                *b = self.out.pop_front().unwrap_or(0);
            }
            Ok(n)
        }
    }

    impl Write for Fake {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let first_write = self.written.is_empty();
            self.written.extend_from_slice(buf);
            if first_write {
                let hello: Value = serde_json::from_slice(buf.strip_suffix(b"\n").unwrap_or(buf)).unwrap_or(Value::Null);
                let nonce = hello["nonce"].as_str().unwrap_or("").to_string();
                let reply = match (&self.first_reply, &self.key) {
                    (Some(r), _) => r.clone(),
                    (None, Some(k)) => serde_json::json!({ "ok": true, "v": 2, "proof": wire::server_proof(k, &nonce) }).to_string(),
                    (None, None) => serde_json::json!({ "ok": true, "v": 2, "proof": "00".repeat(32) }).to_string(),
                };
                self.queue(reply);
            } else {
                self.queue(r#"{"ok":true}"#.into());
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn disc() -> Discovery {
        Discovery { endpoint: "x".into(), token: "secret-token-abc".into(), server_key: "server-key-xyz".into() }
    }

    fn written(r: BufReader<Fake>) -> String {
        String::from_utf8(r.into_inner().written).unwrap()
    }

    #[test]
    fn an_endpoint_without_the_server_key_never_gets_the_token() {
        for fake in [
            Fake::new(None, None),                                   // garbage proof
            Fake::new(Some("some-other-key"), None),                 // valid HMAC, wrong key
            Fake::new(None, Some(r#"{"ok":true}"#)),                 // no proof at all
            Fake::new(None, Some("not json")),
        ] {
            let mut r = BufReader::new(fake);
            assert!(matches!(handshake(&mut r, &disc()), Err(CallError::NotAuthentic)));
            let w = written(r);
            assert!(!w.contains("secret-token-abc") && !w.contains("server-key-xyz"), "{w}");
            assert_eq!(w.lines().count(), 1, "only the hello was sent");
        }
    }

    #[test]
    fn the_real_key_passes_and_then_the_token_is_sent() {
        let mut r = BufReader::new(Fake::new(Some("server-key-xyz"), None));
        handshake(&mut r, &disc()).unwrap();
        let w = written(r);
        let lines: Vec<&str> = w.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(!lines[0].contains("secret-token-abc"), "the hello carries no secret");
        assert_eq!(lines[1], wire::token_line("secret-token-abc"));
        assert!(!w.contains("server-key-xyz"), "the server key never goes on the wire");
    }

    #[test]
    fn an_old_app_fails_with_a_version_message() {
        let mut r = BufReader::new(Fake::new(None, Some(r#"{"ok":false,"error":"unauthorized"}"#)));
        match handshake(&mut r, &disc()) {
            Err(CallError::Refused(m)) => assert!(m.contains("different versions"), "{m}"),
            other => panic!("{other:?}"),
        }
        assert!(!written(r).contains("secret-token-abc"));
    }
}
