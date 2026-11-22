//! The ASPHONIX command contract as Rust.
//!
//! This crate is the *seam* between the native UI (`asphonix-ui`) and whatever
//! provides data and playback (`asphonix-real`, built on `asphonix-core`).
//!
//! * Types are serde-serializable (camelCase on the wire, per-type exceptions
//!   noted in `types.rs`).
//! * [`Backend`] groups the commands into namespaces
//!   (`auth, library, browse, search, playlists, stars, player, output,
//!   settings, connection` ...) as sub-traits. Methods default to
//!   [`ApiError::unimplemented`] so a backend can grow command by command.
//! * Every list is a [`Paged`] result; nothing here ever returns "everything".
//! * Events (player state, spectrum, sync, connection) arrive through
//!   [`Backend::subscribe`].

pub mod backend;
pub mod discovery;
pub mod error;
pub mod format;
pub mod types;
pub mod types_lists;

pub use backend::*;
pub use discovery::*;
pub use error::*;
pub use types::*;
pub use types_lists::*;
