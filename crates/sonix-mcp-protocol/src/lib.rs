//! The contract between the SONIX app's local MCP bridge listener (`sonix-core::mcp_bridge`) and the `sonix-mcp`
//! stdio server: discovery file, handshake and line format (`wire`) and the static tool table (`catalog`).
//! Both sides depend on this crate, so neither depends on the other.

pub mod catalog;
pub mod wire;
