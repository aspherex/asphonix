//! `asphonix-mcp.exe`: MCP server over stdio (register it with
//! `claude mcp add asphonix -- "<path>\asphonix-mcp.exe"`). Everything on stdout is
//! JSON-RPC; diagnostics go to stderr and to the log file
//! (`<data dir>\logs\asphonix-mcp.YYYY-MM-DD.jsonl`), never to stdout.

use std::io::{BufReader, Write};

fn main() {
    let code = run();
    // `exit` skips destructors: `run` has returned, so the log guard is dropped and flushed by now.
    std::process::exit(code);
}

fn run() -> i32 {
    let log = asphonix_logging::init(asphonix_logging::LogConfig::new(
        asphonix_mcp::wire::data_dir().map(|d| d.join("logs")),
        "asphonix-mcp",
        env!("CARGO_PKG_VERSION"),
    ));
    let _root = log.root().clone().entered();
    let cfg = asphonix_mcp::Config::from_env();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    tracing::info!(config_dir = cfg.config_dir.as_ref().map(|d| d.display().to_string()), "asphonix-mcp started");
    let code = match asphonix_mcp::stdio::run(BufReader::new(stdin.lock()), &mut out, &cfg) {
        Ok(()) => 0,
        Err(e) => {
            tracing::error!(error = %e, "stdio session failed");
            eprintln!("asphonix-mcp: {e}");
            1
        }
    };
    let _ = out.flush();
    tracing::info!(code, "asphonix-mcp exiting");
    code
}
