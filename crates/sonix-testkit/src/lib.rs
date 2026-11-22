//! Test kit for SONIX (dev-dependency only).
//!
//! * [`fixtures`]: wire-type builders (tracks, albums, artists, playlists, player state).
//! * [`FakeBackend`]: an in-memory `sonix_api::Backend` with canned data, a call log, injectable errors, an event
//!   sender for `subscribe()` and a per-call hook (UI tests put GPUI's `simulate_random_delay` there).
//! * [`MockSubsonic`]: one configurable local Subsonic HTTP server (127.0.0.1, random port) with a request log,
//!   per-endpoint failure injection and latency, and stateful playlists. Never a real server.

pub mod fake_backend;
pub mod fixtures;

pub use fake_backend::{Call, FakeBackend, FakeState};

