//! SONIX MCP bridge: the stdio MCP server (`stdio`), the static tool table
//! (`catalog`) and the wire format shared with the app's local IPC listener
//! (`wire`, `client`). The bridge holds no library logic: every `tools/call`
//! is forwarded to the running SONIX app, which validates and gates it.

pub use sonix_mcp_protocol::catalog;
pub mod client;
pub mod stdio;
pub use sonix_mcp_protocol::wire;

use std::path::PathBuf;
use std::time::Duration;

/// Bridge client settings.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory holding the app's `mcp.json` discovery file.
    pub config_dir: Option<PathBuf>,
    /// Upper bound for one forwarded tool call (connect + answer).
    pub call_timeout: Duration,
}

impl Config {
    /// `SONIX_MCP_CONFIG_DIR` overrides the default per-OS app config dir
    /// (used by tests and by builds with a different app identifier).
    pub fn from_env() -> Self {
        Self { config_dir: wire::config_dir(), call_timeout: Duration::from_secs(60) }
    }
}
