//! Snapshot tests (`insta`) of what MCP clients see: the tool catalog as `tools/list` returns it and the JSON-RPC
//! replies for the protocol paths. A changed snapshot fails the test; review it with `cargo insta review` (or
//! `INSTA_UPDATE=always cargo test -p asphonix-mcp --test snapshots` and read the diff) before committing it.
//! No app is contacted: the discovery folder points nowhere, so `tools/call` answers "not running" itself.
use serde_json::{Value, json};
use asphonix_mcp::{Config, catalog, stdio};
use std::time::Duration;

fn cfg() -> Config {
    Config { config_dir: Some(std::env::temp_dir().join(format!("asphonix-mcp-snapshots-none-{}", std::process::id()))), call_timeout: Duration::from_secs(5) }
}

/// Object keys sorted: a workspace build turns on `serde_json/preserve_order` (feature unification), a
/// `-p asphonix-mcp` build does not, and the snapshot must not depend on which one ran.
fn snap(f: impl FnOnce()) {
    let mut s = insta::Settings::clone_current();
    s.set_sort_maps(true);
    s.bind(f);
}

fn reply(msg: Value) -> Value {
    stdio::handle_message(&msg, &cfg()).expect("a request gets a reply")
}

fn rpc(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

#[test]
fn tool_catalog() {
    let tools: Vec<Value> = catalog::tools()
        .into_iter()
        .map(|t| json!({ "name": t.name, "control": t.control, "description": t.description, "schema": t.schema }))
        .collect();
    snap(|| insta::assert_json_snapshot!("tool_catalog", tools));
}

#[test]
fn tools_list_reply() {
    snap(|| insta::assert_json_snapshot!("tools_list", reply(rpc(2, "tools/list", json!({})))));
}

#[test]
fn initialize_reply() {
    let r = reply(rpc(1, "initialize", json!({ "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "t", "version": "0" } })));
    snap(|| insta::assert_json_snapshot!("initialize", r, { ".result.serverInfo.version" => "[version]" }));
    // an unknown protocol version gets the latest one
    let r = reply(rpc(1, "initialize", json!({ "protocolVersion": "1999-01-01" })));
    assert_eq!(r["result"]["protocolVersion"], stdio::LATEST_PROTOCOL);
}

#[test]
fn protocol_and_error_replies() {
    let cases = json!({
        "ping": reply(rpc(3, "ping", json!({}))),
        "unknown_method": reply(rpc(4, "resources/list", json!({}))),
        "not_an_object": reply(json!([1, 2])),
        "client_response": reply(json!({ "jsonrpc": "2.0", "id": 5, "result": {} })),
        "call_without_name": reply(rpc(6, "tools/call", json!({}))),
        "call_unknown_tool": reply(rpc(7, "tools/call", json!({ "name": "rm_rf" }))),
        "call_bad_arguments": reply(rpc(8, "tools/call", json!({ "name": "search", "arguments": [1] }))),
        "call_app_not_running": reply(rpc(9, "tools/call", json!({ "name": "library_stats" }))),
    });
    snap(|| insta::assert_json_snapshot!("protocol_replies", cases));
    // notifications are never answered
    assert_eq!(stdio::handle_message(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }), &cfg()), None);
}
