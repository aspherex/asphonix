//! Test kit for ASPHONIX (dev-dependency only).
//!
//! * [`fixtures`]: wire-type builders (tracks, albums, artists, playlists, player state).
//! * [`FakeBackend`]: an in-memory `asphonix_api::Backend` with canned data, a call log, injectable errors, an event
//!   sender for `subscribe()` and a per-call hook (UI tests put GPUI's `simulate_random_delay` there).
//! * [`MockSubsonic`]: one configurable local Subsonic HTTP server (127.0.0.1, random port) with a request log,
//!   per-endpoint failure injection and latency, and stateful playlists. Never a real server.

pub mod fake_backend;
pub mod fixtures;
pub mod mock_subsonic;

pub use fake_backend::{Call, FakeBackend, FakeState};
pub use mock_subsonic::{MockSubsonic, Request, Response};
