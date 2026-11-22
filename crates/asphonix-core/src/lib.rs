pub mod audio;
pub mod discovery;
pub mod ipc;
pub mod library;
pub mod secrets;
pub mod subsonic;
pub mod overlay;
pub mod listen;
pub mod rt;
pub mod scrobble;
pub mod smartlists;
pub mod updates;
// The update classification only feeds the Windows SMTC backend.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod media;
pub mod mcp_bridge;
pub mod extras;
pub mod continuity;
