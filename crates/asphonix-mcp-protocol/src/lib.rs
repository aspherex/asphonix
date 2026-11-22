//! The contract between the ASPHONIX app's local MCP bridge listener (`asphonix-core::mcp_bridge`) and the `asphonix-mcp`
//! stdio server: discovery file, handshake and line format (`wire`) and the static tool table (`catalog`).
//! Both sides depend on this crate, so neither depends on the other.

pub mod catalog;
pub mod wire;
