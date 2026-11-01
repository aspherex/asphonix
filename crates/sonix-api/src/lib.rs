//! The SONIX command contract as Rust.
//!
//! This crate is the *seam* between the native UI (`sonix-ui`) and whatever
//! provides data and playback: today `sonix-mock`, in phase 2 a real backend on
//! top of the shared `sonix-core` crate extracted from SoniX's `src-tauri`.
//!
//! * Types mirror `src/lib/ipc/types.ts` / `src-tauri/src/ipc.rs` of the SoniX
//!   repo, with the same serde attributes (camelCase on the wire, per-type
//!   exceptions noted in `types.rs`), so the same JSON stays valid.
//! * [`Backend`] mirrors the namespaces of `src/lib/ipc/commands.ts`
//!   (`auth, library, browse, search, playlists, stars, player, output,
//!   settings, connection` ...) as sub-traits. Phase 1 implements what the UI
//!   uses; everything else defaults to [`ApiError::unimplemented`] so a
//!   backend can grow command by command.
//! * Every list is a [`Paged`] result; nothing here ever returns "everything".
//! * Events (player state, spectrum, sync, connection) arrive through
//!   [`Backend::subscribe`].

pub mod backend;
pub mod discovery;
pub mod error;
pub mod format;
pub mod types;

pub use backend::*;
pub use discovery::*;
pub use error::*;
pub use types::*;
