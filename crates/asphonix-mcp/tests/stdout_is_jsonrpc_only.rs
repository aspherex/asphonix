//! The real binary with logging on at `trace`: stdout carries JSON-RPC and nothing else; the log lines
//! (which exist, so logging really ran) go to the file.
use std::io::{Read, Write};
use std::process::{Command, Stdio};

#[test]
fn with_logging_at_trace_stdout_carries_only_jsonrpc() {
    let tmp = std::env::temp_dir().join(format!("asphonix-mcp-stdout-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_asphonix-mcp"))
        .env("ASPHONIX_MCP_DATA_DIR", &tmp)
        .env("ASPHONIX_MCP_CONFIG_DIR", tmp.join("no-app")) // no mcp.json: tool calls fail fast
        .env("ASPHONIX_LOG_LEVEL", "trace")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/list\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"search\",\"arguments\":{\"query\":\"x\"}}}\n",
        "not json\n",
    );
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap(); // dropped: EOF ends the session
    let mut stdout = String::new();
    child.stdout.take().unwrap().read_to_string(&mut stdout).unwrap();
    let status = child.wait().unwrap();
    assert!(status.success(), "{status:?}");

    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 5, "one reply per request (none for the notification):\n{stdout}");
    for l in &lines {
        let v: serde_json::Value = serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON on stdout ({e}): {l}"));
        assert_eq!(v["jsonrpc"], "2.0", "{l}");
        assert!(v.get("result").is_some() || v.get("error").is_some(), "{l}");
        assert!(v.get("level").is_none() && v.get("ts").is_none(), "a log line on stdout: {l}");
    }
    let tool = serde_json::from_str::<serde_json::Value>(lines[3]).unwrap();
    assert_eq!(tool["id"], 4);
    assert_eq!(tool["result"]["isError"], true);

    let logs = tmp.join("logs");
    let files: Vec<_> = std::fs::read_dir(&logs).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_str().unwrap().to_string();
    assert!(name.starts_with("asphonix-mcp.") && name.ends_with(".jsonl"), "{name}");
    let text = std::fs::read_to_string(&files[0]).unwrap();
    let events: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let has = |m: &str| events.iter().any(|e| e["message"] == m);
    assert!(has("asphonix-mcp started") && has("request") && has("notification") && has("asphonix-mcp exiting"), "{text}");
    let failed = events.iter().find(|e| e["message"] == "tool call failed").expect("tool call logged");
    assert_eq!(failed["spans"].as_array().unwrap().last().unwrap()["mcp_tool"], "search");
    assert!(!text.contains("\"query\""), "tool arguments are not logged");
    let _ = std::fs::remove_dir_all(&tmp);
}
