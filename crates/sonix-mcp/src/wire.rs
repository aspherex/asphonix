//! Wire format between `sonix-mcp` and the SONIX app (newline-delimited JSON
//! over a local named pipe / Unix socket), and the discovery file.
//!
//! Flow per connection (protocol 2):
//! 1. client `{"hello":"sonix-mcp","v":2,"nonce":"<32 random bytes, hex>"}`
//! 2. app `{"ok":true,"v":2,"proof":"<hex HMAC-SHA256(serverKey, PROOF_LABEL || nonce)>"}`,
//!    or `{"ok":false,"error":..}` + close (e.g. a protocol version mismatch)
//! 3. the client checks the proof in constant time and only then sends
//!    `{"token":"<hex>"}`; app `{"ok":true}` or `{"ok":false,"error":"unauthorized"}` + close
//! 4. any number of `{"tool":"search","args":{..}}` -> `{"ok":true,"result":..}` /
//!    `{"ok":false,"error":".."}`.
//!
//! Step 2 means a pipe/socket that is not SONIX (e.g. another local account
//! that re-created a stale pipe name after a crash) never gets the token or a
//! request: it cannot answer the nonce without `serverKey`, which only the
//! user-only `mcp.json` holds.

use std::io::{self, BufRead};
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Handshake protocol version (2 = the app proves itself before the token).
pub const BRIDGE_VERSION: u64 = 2;
/// Domain separation for the server proof (used for nothing else).
pub const PROOF_LABEL: &[u8] = b"sonix-mcp server proof v2\0";
pub const HELLO: &str = "sonix-mcp";
/// The native app's identifier (`sonix_real::IDENTIFIER`): names its config dir (NOT the installed app's `dev.sonix.app`).
pub const APP_IDENTIFIER: &str = "dev.sonix.native";
/// Written by the app while "Claude (MCP)" is enabled, removed when disabled.
pub const DISCOVERY_FILE: &str = "mcp.json";
/// Largest line the app accepts from a client (hello or request).
pub const MAX_REQUEST_LINE: usize = 1 << 20;
/// Largest answer line the bridge accepts from the app.
pub const MAX_RESPONSE_LINE: usize = 8 << 20;

/// Where the app keeps `mcp.json`: `SONIX_NATIVE_MCP_CONFIG_DIR`, else the
/// per-OS equivalent of `%APPDATA%` for [`APP_IDENTIFIER`].
pub fn config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("SONIX_NATIVE_MCP_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    base.map(|b| b.join(APP_IDENTIFIER))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    /// Named pipe name (`\\.\pipe\...`) or Unix socket path.
    pub endpoint: String,
    /// Client -> app secret, sent only after the app proved itself.
    pub token: String,
    /// App -> client secret: only ever the HMAC key of the server proof.
    pub server_key: String,
}

impl Discovery {
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "v": BRIDGE_VERSION, "endpoint": self.endpoint, "token": self.token, "serverKey": self.server_key
        })
        .to_string()
    }

    pub fn parse(text: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(text).ok()?;
        let endpoint = v.get("endpoint")?.as_str()?.to_string();
        let token = v.get("token")?.as_str()?.to_string();
        // A file written by an older SONIX (protocol 1) has no serverKey.
        let server_key = v.get("serverKey")?.as_str()?.to_string();
        if endpoint.is_empty() || token.is_empty() || server_key.is_empty() {
            return None;
        }
        Some(Self { endpoint, token, server_key })
    }

    pub fn path(dir: &Path) -> PathBuf {
        dir.join(DISCOVERY_FILE)
    }
}

/// Step 1: the client's hello with a fresh random nonce (no secret in it).
pub fn hello_line(nonce: &str) -> String {
    serde_json::json!({ "hello": HELLO, "v": BRIDGE_VERSION, "nonce": nonce }).to_string()
}

/// Step 3: the token, sent only after the app's proof checked out.
pub fn token_line(token: &str) -> String {
    serde_json::json!({ "token": token }).to_string()
}

/// Constant-time equality (only the length, which is public, can leak).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `n` bytes from the OS random source, hex-encoded.
pub fn random_hex(n: usize) -> io::Result<String> {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).map_err(|e| io::Error::other(format!("OS random source failed: {e}")))?;
    Ok(hex(&buf))
}

/// HMAC-SHA256 (RFC 2104) on top of `sha2`.
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// Step 2, app side: hex HMAC-SHA256(serverKey, PROOF_LABEL || nonce).
pub fn server_proof(server_key: &str, nonce: &str) -> String {
    let mut msg = PROOF_LABEL.to_vec();
    msg.extend_from_slice(nonce.as_bytes());
    hex(&hmac_sha256(server_key.as_bytes(), &msg))
}

/// Step 2, client side: does `proof` answer `nonce` under `server_key`? Constant time.
pub fn verify_server_proof(server_key: &str, nonce: &str, proof: &str) -> bool {
    ct_eq(server_proof(server_key, nonce).as_bytes(), proof.as_bytes())
}

pub fn request_line(tool: &str, args: &Value) -> String {
    serde_json::json!({ "tool": tool, "args": args }).to_string()
}

/// Reads one `\n`-terminated line of at most `max` bytes. `Ok(None)` = EOF
/// before any byte. A longer line is an `InvalidData` error (the rest of the
/// stream is not consumed: callers drop the connection).
pub fn read_line_capped<R: BufRead + ?Sized>(r: &mut R, max: usize) -> io::Result<Option<String>> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let (done, used) = {
            let chunk = match r.fill_buf() {
                Ok(c) => c,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if chunk.is_empty() {
                if buf.is_empty() {
                    return Ok(None);
                }
                (true, 0)
            } else if let Some(i) = chunk.iter().position(|&b| b == b'\n') {
                if buf.len() + i > max {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
                }
                buf.extend_from_slice(&chunk[..i]);
                (true, i + 1)
            } else {
                if buf.len() + chunk.len() > max {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
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
            return String::from_utf8(buf)
                .map(Some)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "line is not UTF-8"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_lines_and_eof() {
        let mut c = Cursor::new(b"abc\r\ndef\n".to_vec());
        assert_eq!(read_line_capped(&mut c, 100).unwrap().as_deref(), Some("abc"));
        assert_eq!(read_line_capped(&mut c, 100).unwrap().as_deref(), Some("def"));
        assert_eq!(read_line_capped(&mut c, 100).unwrap(), None);
        // A final unterminated line is still delivered.
        let mut c = Cursor::new(b"tail".to_vec());
        assert_eq!(read_line_capped(&mut c, 100).unwrap().as_deref(), Some("tail"));
    }

    #[test]
    fn caps_line_length_and_rejects_bad_utf8() {
        let mut c = Cursor::new(vec![b'x'; 50]);
        assert!(read_line_capped(&mut c, 10).is_err());
        let mut c = Cursor::new([vec![b'x'; 11], b"\n".to_vec()].concat());
        assert!(read_line_capped(&mut c, 10).is_err());
        let mut c = Cursor::new(vec![0xff, 0xfe, b'\n']);
        assert!(read_line_capped(&mut c, 10).is_err());
        let mut c = Cursor::new([vec![b'x'; 10], b"\n".to_vec()].concat());
        assert_eq!(read_line_capped(&mut c, 10).unwrap().unwrap().len(), 10);
    }

    #[test]
    fn discovery_round_trip_and_rejects_garbage() {
        let d = Discovery { endpoint: r"\\.\pipe\x".into(), token: "ab".into(), server_key: "cd".into() };
        assert_eq!(Discovery::parse(&d.to_json()), Some(d));
        assert_eq!(Discovery::parse("nope"), None);
        assert_eq!(Discovery::parse(r#"{"endpoint":"","token":"a"}"#), None);
        assert_eq!(Discovery::parse(r#"{"endpoint":"x"}"#), None);
        // A protocol-1 file (no serverKey) is not half-used.
        assert_eq!(Discovery::parse(r#"{"v":1,"endpoint":"x","token":"t"}"#), None);
    }

    #[test]
    fn hmac_matches_rfc4231_and_proofs_check() {
        // RFC 4231 test case 2, and case 6 (key longer than the block size).
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex(&hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        let p = server_proof("key", "n1");
        assert!(verify_server_proof("key", "n1", &p));
        assert!(!verify_server_proof("other", "n1", &p));
        assert!(!verify_server_proof("key", "n2", &p));
        assert!(!verify_server_proof("key", "n1", ""));
        assert!(ct_eq(b"", b"") && !ct_eq(b"a", b"ab") && !ct_eq(b"ab", b"ac"));
        let (a, b) = (random_hex(32).unwrap(), random_hex(32).unwrap());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }
}
