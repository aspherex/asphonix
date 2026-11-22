//! The MCP server loop: JSON-RPC 2.0, one message per line, over any
//! `BufRead`/`Write` pair (real stdin/stdout in the binary, in-memory pipes
//! in tests). Implemented subset: `initialize`, `notifications/initialized`
//! (and other notifications, ignored), `ping`, `tools/list`, `tools/call`.

use std::io::{self, BufRead, Write};

use serde_json::{json, Value};

use crate::catalog;
use crate::client::call_app;
use crate::wire::read_line_capped;
use crate::Config;

pub const LATEST_PROTOCOL: &str = "2025-06-18";
const SUPPORTED_PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
/// A client message larger than this ends the session.
const MAX_MESSAGE: usize = 4 << 20;

const INSTRUCTIONS: &str = "Tools to read the user's ASPHONIX music library and listening history and (when the \
user allowed it in ASPHONIX settings) control playback. Results are compact JSON. Track ids come from search, \
get_tracks, get_album, get_playlist and get_smart_playlist_tracks. Call library_stats for the current local date.";

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn tool_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn list_tools() -> Value {
    let tools: Vec<Value> = catalog::tools()
        .into_iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.schema,
                "annotations": {
                    "readOnlyHint": !t.control,
                    "destructiveHint": catalog::DESTRUCTIVE.contains(&t.name),
                    "openWorldHint": false
                }
            })
        })
        .collect();
    json!({ "tools": tools })
}

/// Handles one parsed message; `None` = nothing to send (notification/response).
pub fn handle_message(msg: &Value, cfg: &Config) -> Option<Value> {
    let Some(obj) = msg.as_object() else {
        return Some(rpc_error(Value::Null, -32600, "Invalid Request"));
    };
    let id = obj.get("id").cloned();
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        // A response from the client (we send no requests) or garbage.
        return id.map(|id| rpc_error(id, -32600, "Invalid Request"));
    };
    let Some(id) = id.filter(|i| i.is_string() || i.is_number()) else {
        tracing::debug!(method, "notification");
        return None; // notification (or an unusable id): never answered
    };
    tracing::debug!(method, "request");
    let params = obj.get("params");
    Some(match method {
        "initialize" => {
            let asked = params.and_then(|p| p.get("protocolVersion")).and_then(Value::as_str);
            let version = asked.filter(|v| SUPPORTED_PROTOCOLS.contains(v)).unwrap_or(LATEST_PROTOCOL);
            rpc_result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "asphonix-mcp", "title": "ASPHONIX", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS
                }),
            )
        }
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(id, list_tools()),
        "tools/call" => {
            let Some(name) = params.and_then(|p| p.get("name")).and_then(Value::as_str) else {
                return Some(rpc_error(id, -32602, "tools/call needs a tool name"));
            };
            if catalog::find(name).is_none() {
                return Some(rpc_error(id, -32602, &format!("Unknown tool: {name}")));
            }
            let args = match params.and_then(|p| p.get("arguments")) {
                None | Some(Value::Null) => json!({}),
                Some(a) if a.is_object() => a.clone(),
                Some(_) => return Some(rpc_error(id, -32602, "arguments must be an object")),
            };
            // The tool name and timing only: arguments and results are library data, the token never leaves `client`.
            let span = tracing::info_span!("mcp_call", mcp_tool = name);
            let _enter = span.enter();
            let started = std::time::Instant::now();
            let r = call_app(cfg, name, &args);
            let duration_ms = started.elapsed().as_millis() as u64;
            match r {
                Ok(v) => {
                    tracing::info!(duration_ms, "tool call");
                    rpc_result(id, tool_result(v.to_string(), false))
                }
                Err(e) => {
                    tracing::warn!(duration_ms, error = %e, "tool call failed");
                    rpc_result(id, tool_result(e.to_string(), true))
                }
            }
        }
        _ => rpc_error(id, -32601, "Method not found"),
    })
}

/// Runs until `input` ends. Only JSON-RPC is ever written to `output`.
pub fn run<R: BufRead, W: Write>(mut input: R, mut output: W, cfg: &Config) -> io::Result<()> {
    loop {
        let line = match read_line_capped(&mut input, MAX_MESSAGE) {
            Ok(Some(l)) => l,
            Ok(None) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                send(&mut output, &rpc_error(Value::Null, -32700, "Parse error: message too large or not UTF-8"))?;
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(_)) => Some(rpc_error(Value::Null, -32600, "Batch requests are not supported")),
            Ok(msg) => handle_message(&msg, cfg),
            Err(_) => Some(rpc_error(Value::Null, -32700, "Parse error")),
        };
        if let Some(r) = reply {
            send(&mut output, &r)?;
        }
    }
}

fn send<W: Write>(out: &mut W, v: &Value) -> io::Result<()> {
    let mut s = v.to_string();
    s.push('\n');
    out.write_all(s.as_bytes())?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::Duration;

    fn cfg_without_app() -> Config {
        let dir = std::env::temp_dir().join("asphonix-mcp-test-no-such-dir-xyz");
        Config { config_dir: Some(dir), call_timeout: Duration::from_secs(2) }
    }

    fn session(input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        run(Cursor::new(input.as_bytes().to_vec()), &mut out, &cfg_without_app()).unwrap();
        String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    #[test]
    fn initialize_echoes_supported_version_else_latest() {
        let r = session("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-03-26\"}}\n");
        assert_eq!(r[0]["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r[0]["result"]["serverInfo"]["name"], "asphonix-mcp");
        assert!(r[0]["result"]["capabilities"]["tools"].is_object());
        let r = session("{\"jsonrpc\":\"2.0\",\"id\":\"a\",\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"1999-01-01\"}}\n");
        assert_eq!(r[0]["id"], "a");
        assert_eq!(r[0]["result"]["protocolVersion"], LATEST_PROTOCOL);
    }

    #[test]
    fn notifications_get_no_reply_and_ping_works() {
        let r = session(
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n",
        );
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["id"], 7);
        assert_eq!(r[0]["result"], json!({}));
    }

    #[test]
    fn malformed_input_never_kills_the_loop() {
        let r = session("not json\n[1,2]\n42\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"nope\"}\n{\"id\":2,\"method\":\"ping\"}\n");
        assert_eq!(r[0]["error"]["code"], -32700);
        assert_eq!(r[0]["id"], Value::Null);
        assert_eq!(r[1]["error"]["code"], -32600); // batch
        assert_eq!(r[2]["error"]["code"], -32600); // not an object
        assert_eq!(r[3]["error"]["code"], -32601);
        assert_eq!(r[4]["result"], json!({}));
    }

    #[test]
    fn oversized_or_non_utf8_message_ends_with_a_parse_error() {
        let mut big = vec![b'x'; MAX_MESSAGE + 10];
        big.push(b'\n');
        let mut out = Vec::new();
        run(Cursor::new(big), &mut out, &cfg_without_app()).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("-32700"));
    }

    #[test]
    fn tools_list_has_the_catalog_with_schemas() {
        let r = session("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n");
        let tools = r[0]["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), catalog::tools().len());
        for t in tools {
            assert!(t["name"].is_string() && t["description"].is_string());
            assert_eq!(t["inputSchema"]["type"], "object");
            assert!(t["annotations"]["readOnlyHint"].is_boolean());
        }
        let hint = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap()["annotations"]["destructiveHint"].clone();
        assert_eq!(hint("play_tracks"), true, "replaces the queue");
        assert_eq!(hint("play_source"), true, "replaces the queue");
        assert_eq!(hint("enqueue"), false);
        assert_eq!(hint("search"), false);
    }

    #[test]
    fn tools_call_validates_the_envelope_and_reports_app_state_as_tool_errors() {
        let r = session(concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"bogus\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"search\",\"arguments\":5}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"now_playing\"}}\n",
        ));
        assert_eq!(r[0]["error"]["code"], -32602);
        assert_eq!(r[1]["error"]["code"], -32602);
        assert_eq!(r[2]["error"]["code"], -32602);
        // No discovery file: a clear tool error, not a protocol error or a crash.
        assert_eq!(r[3]["result"]["isError"], true);
        let text = r[3]["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("not enabled"), "{text}");
    }
}
