//! Bit-perfect audio engine: fetch -> disk-backed buffer -> `symphonia`
//! decode -> ring buffer -> native output.
//!
//! Output is WASAPI exclusive, event-driven mode on Windows and a `cpal`
//! shared-mode fallback on other platforms (no CoreAudio hog mode yet),
//! both bypassing OS mixing and resampling in
//! exclusive mode so the DAC receives the file's native rate/depth
//! unmodified. The active mode is always reported truthfully to the UI via
//! `ipc::OutputStatus`/`ipc::PlayerState` — never fabricated.
//!
//! Memory discipline: compressed bytes are streamed to a disk-backed temp
//! file (`source::ProgressiveDownload`), never buffered whole in RAM; only a
//! ~1.5s PCM ring buffer (`ring::new_pcm_ring`) is kept in memory. The next
//! track is prefetched to disk (not RAM) as soon as the current one starts
//! (`engine::Engine::prefetch_next`).
//!
//! Spectrum data is computed on its own thread (`spectrum::SpectrumAnalyzer`)
//! and pushed to the UI as a small `u8` array (see
//! `ipc::SpectrumFrame`) — the render thread only does a cheap lock-free
//! push of downmixed samples into a secondary ring, never the FFT itself.
//!
//! Module map:
//! - `source`   — `TrackSource` trait + HTTP (progressive download) impl.
//! - `decode`   — the `symphonia` decode thread.
//! - `format`   — sample containers + format negotiation types.
//! - `ring`     — the decode -> render lock-free PCM ring buffer.
//! - `output`   — WASAPI (Windows) / `cpal` (fallback) render backends.
//! - `queue`    — pure queue/shuffle/repeat logic (ids only).
//! - `spectrum` — FFT spectrum analyzer.
//! - `engine`   — orchestrates all of the above; owns the `player_*`/
//!   `output_*` commands' actual behaviour via `EngineHandle`.

pub mod abloop;
pub mod autoplay;
pub mod decode;
mod engine;
pub mod format;
pub mod output;
pub use crate::prefs;
pub mod queue;
pub mod replaygain;
pub mod ring;
pub mod waveform;
pub use crate::sequence;
pub mod session;
pub mod source;
mod spectrum;

pub use engine::{EngineCommand, EngineEvents, EngineHandle, EngineRequestError};
pub use prefs::Prefs;
pub use session::SessionStore;
pub use source::{HttpTrackSource, SubsonicTrackSource, TrackSource};

/// Re-exported so engine callers can name the player/output types from `audio::`.
pub use crate::ipc::{OutputMode, OutputStatus, PlaybackStatus, PlayerState, SpectrumFrame};
