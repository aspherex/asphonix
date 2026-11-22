//! `asphonix-mcp.exe`: MCP server over stdio (register it with
//! `claude mcp add asphonix -- "<path>\asphonix-mcp.exe"`). Everything on stdout is
//! JSON-RPC; diagnostics go to stderr.

use std::io::{BufReader, Write};

fn main() {
    let cfg = asphonix_mcp::Config::from_env();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if let Err(e) = asphonix_mcp::stdio::run(BufReader::new(stdin.lock()), &mut out, &cfg) {
        eprintln!("asphonix-mcp: {e}");
        let _ = out.flush();
        std::process::exit(1);
    }
}
