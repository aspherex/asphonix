//! `sonix-mcp.exe`: MCP server over stdio (register it with
//! `claude mcp add sonix -- "<path>\sonix-mcp.exe"`). Everything on stdout is
//! JSON-RPC; diagnostics go to stderr and to the log file
//! (`<data dir>\logs\sonix-mcp.YYYY-MM-DD.jsonl`), never to stdout.

use std::io::{BufReader, Write};

fn main() {
    let code = run();
    // `exit` skips destructors: `run` has returned, so the log guard is dropped and flushed by now.
    std::process::exit(code);
}

fn run() -> i32 {
    let log = sonix_logging::init(sonix_logging::LogConfig::new(
        sonix_mcp::wire::data_dir().map(|d| d.join("logs")),
        "sonix-mcp",
        env!("CARGO_PKG_VERSION"),
    ));
    let _root = log.root().clone().entered();
    let cfg = sonix_mcp::Config::from_env();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    tracing::info!(config_dir = cfg.config_dir.as_ref().map(|d| d.display().to_string()), "sonix-mcp started");
    let code = match sonix_mcp::stdio::run(BufReader::new(stdin.lock()), &mut out, &cfg) {
        Ok(()) => 0,
        Err(e) => {
            tracing::error!(error = %e, "stdio session failed");
            eprintln!("sonix-mcp: {e}");
            1
        }
    };
    let _ = out.flush();
    tracing::info!(code, "sonix-mcp exiting");
    code
}
