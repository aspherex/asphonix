//! Output backends: WASAPI exclusive/shared on Windows, a `cpal`
//! shared-mode fallback everywhere else (macOS CoreAudio hog mode is
//! post-MVP per `PLAN.md`).
//!
//! Both backends expose the same tiny surface (`list_devices`, `negotiate`,
//! `spawn_render`) so `audio::engine` doesn't need any `cfg` branches of its
//! own beyond picking which module to `use`.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{default_device_id, list_devices, negotiate, shared_format, spawn_render};

/// Exclusive-mode diagnostics (`examples/probe_formats.rs`).
#[cfg(windows)]
pub mod probe {
    pub use super::windows::{hresult_name, probe_all_devices, DeviceProbe, ProbeRow};
}

#[cfg(not(windows))]
mod fallback;
#[cfg(not(windows))]
pub use fallback::{default_device_id, list_devices, negotiate, shared_format, spawn_render};

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use rtrb::{Consumer, Producer};

use crate::audio::format::{Container, NegotiatedFormat};

/// Stereo peak levels of the samples actually sent to the device (after the
/// volume stage), shared between the render thread (writer) and the spectrum
/// thread (reader, ~30 Hz). Lock-free and allocation-free: a non-negative
/// `f32`'s bit pattern orders like the value itself, so `fetch_max` on the
/// bits is a running maximum; the reader `swap`s it back to zero, so every
/// reading is "the loudest sample since the previous reading".
#[derive(Default)]
pub struct PeakMeter {
    left: AtomicU32,
    right: AtomicU32,
}

impl PeakMeter {
    /// Linear peaks (`0.0..`) of one render callback.
    pub fn record(&self, left: f32, right: f32) {
        if left.is_finite() {
            self.left.fetch_max(left.abs().to_bits(), Ordering::Relaxed);
        }
        if right.is_finite() {
            self.right.fetch_max(right.abs().to_bits(), Ordering::Relaxed);
        }
    }

    /// Takes the peaks accumulated since the last call (resets to zero).
    pub fn take(&self) -> (f32, f32) {
        (
            f32::from_bits(self.left.swap(0, Ordering::Relaxed)),
            f32::from_bits(self.right.swap(0, Ordering::Relaxed)),
        )
    }
}

/// Everything the render thread feeds the analyzer thread with: the mono
/// downmix for the FFT and the stereo peak meter. Both are only fed while
/// `active` is set (a webview is showing them; see `Prefs::spectrum_active`).
pub struct MonoTap {
    pub mono: Producer<f32>,
    pub peaks: Arc<PeakMeter>,
    pub active: Arc<AtomicBool>,
}

/// One enumerated render device.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// Result of format negotiation: the format to open, plus — when exclusive
/// mode was requested but couldn't be used — the human-readable reason
/// (named HRESULT), surfaced to the UI via `OutputStatus.exclusiveError`.
#[derive(Debug, Clone)]
pub struct Negotiation {
    pub format: NegotiatedFormat,
    pub exclusive_error: Option<String>,
}

/// Events the render thread reports back to the engine. The render thread
/// never panics on device trouble — it reports and exits cleanly so the
/// engine can update `OutputStatus`/`PlayerState` truthfully.
#[derive(Debug)]
pub enum RenderEvent {
    /// The device disappeared or stopped servicing the stream (unplugged,
    /// disabled, format changed underneath us, ...). Carries the reason.
    DeviceLost(String),
}

/// A running render thread. Dropping/stopping is explicit (`stop` then
/// `join`) rather than on `Drop`, so the engine controls exactly when the
/// thread has finished tearing down the device before opening a new one.
///
/// `paused`: while set, the render thread writes silence and does *not*
/// pop from the ring — decode's producer side naturally blocks once the
/// ring fills, so pause/resume needs no separate signalling to the decode
/// thread at all.
///
/// `played_frames`: real (ring) frames the device has actually played since
/// this render thread started — frames handed to the device minus those
/// still queued in its buffer. The engine derives the audible position and
/// the gapless track boundary from this, not from the decoder.
pub struct RenderHandle {
    pub stop: Arc<AtomicBool>,
    pub paused: Arc<AtomicBool>,
    pub played_frames: Arc<AtomicU64>,
    /// Silence frames inserted *between* real frames by underruns (audible
    /// dropouts / track-change gaps within this stream). Diagnostics only.
    pub gap_frames: Arc<AtomicU64>,
    /// Current volume (`f32` bits via `to_bits`/`from_bits`), read by the
    /// render thread on every callback and applied to the already-packed
    /// container bytes popped from the ring, right before they're written to
    /// the device (`apply_gain_in_place`). This — not the decode thread — is
    /// the only place gain is applied, so a volume change is audible within
    /// one render callback (a few ms) instead of waiting out the ring
    /// buffer's ~1.5 s. Relaxed atomic: a torn read for one callback is
    /// inaudible and self-corrects on the next one; no lock, no allocation.
    pub volume_bits: Arc<AtomicU32>,
    /// Hands the render thread a freshly-seeked ring to resume from on the
    /// *same* open device stream (`engine::Engine::seek`'s common path) —
    /// see `windows::render_loop`'s seek branch. The `cpal` fallback accepts
    /// and drains this too (for interface parity) but still reopens the
    /// stream underneath since `cpal` has no flush/reset primitive exposed.
    pub seek_tx: Sender<Consumer<u8>>,
    /// Seeks the render thread has applied (incremented, Release, right after
    /// it reset `played_frames` to 0 for a ring received on `seek_tx`). The
    /// engine compares it with the seeks it sent: until they match,
    /// `played_frames` still counts the *old* ring and must not be added to
    /// the new start offset (that showed the seek target + seconds played
    /// since the previous seek for one state report).
    pub seeks_applied: Arc<AtomicU64>,
    pub join: std::thread::JoinHandle<()>,
}

impl RenderHandle {
    pub fn stop_and_join(self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.join.thread().unpark();
        let _ = self.join.join();
    }

    /// Sets the pause flag and nudges the render thread, which may be parked
    /// with its device stream stopped after a long pause (`windows::park_stopped`).
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, std::sync::atomic::Ordering::Relaxed);
        self.join.thread().unpark();
    }

    /// Hands the render thread a new ring (`seek_tx`) and nudges it (see
    /// [`Self::set_paused`]). `Err` = the render thread is gone.
    pub fn send_ring(&self, consumer: Consumer<u8>) -> Result<(), std::sync::mpsc::SendError<Consumer<u8>>> {
        let r = self.seek_tx.send(consumer);
        self.join.thread().unpark();
        r
    }

    pub fn set_volume(&self, volume: f32) {
        self.volume_bits.store(volume.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }
}

/// Bottom of the volume taper: slider position 0+ is this many dB down,
/// position 0 itself is mute.
pub const VOLUME_RANGE_DB: f32 = 60.0;

/// Perceptual volume taper: the UI slider position `p` (0..1, what
/// `player_set_volume` carries) is linear in **dB**, `gain_db = 60 * (p - 1)`,
/// so every 1/60 of travel is 1 dB across the whole range and low listening
/// levels can actually be dialed in (a linear-amplitude slider spends most
/// of its travel above -10 dB). `p >= 1` is exactly unity (bit-perfect
/// path skips gain entirely), `p <= 0` is exactly mute.
pub fn volume_to_gain(p: f32) -> f32 {
    if p >= 1.0 {
        1.0
    } else if p <= 0.0 || !p.is_finite() {
        0.0
    } else {
        10f32.powf(volume_to_db(p).unwrap_or(-VOLUME_RANGE_DB) / 20.0)
    }
}

/// The applied attenuation in dB for slider position `p`, `None` = muted.
pub fn volume_to_db(p: f32) -> Option<f32> {
    if p <= 0.0 || !p.is_finite() {
        None
    } else {
        Some(VOLUME_RANGE_DB * (p.min(1.0) - 1.0))
    }
}

/// Multiplies the samples already packed into `buf` (in `container`'s exact
/// device byte layout) by `volume` in place. Called from the render thread's
/// hot path, right after popping bytes from the ring and before writing them
/// to the device — allocation-free (`chunks_exact_mut` over the caller's own
/// buffer), and skipped entirely by callers when `volume == 1.0` so unity
/// gain never touches a single bit (bit-perfect requirement).
///
/// Mirrors exactly what `decode::Packer` used to do at decode time (same
/// per-container math/rounding), just operating on bytes instead of
/// symphonia sample buffers, and only ever invoked when gain is non-unity.
pub(crate) fn apply_gain_in_place(buf: &mut [u8], container: Container, volume: f32) {
    match container {
        Container::I16 => {
            for chunk in buf.chunks_exact_mut(2) {
                let s = i16::from_le_bytes([chunk[0], chunk[1]]);
                let v = ((s as f32) * volume).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
                chunk.copy_from_slice(&v.to_le_bytes());
            }
        }
        Container::I24In32 | Container::I32 => {
            for chunk in buf.chunks_exact_mut(4) {
                let s = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                let mut v = ((s as f64) * volume as f64).clamp(i32::MIN as f64, i32::MAX as f64) as i32;
                if container == Container::I24In32 {
                    v &= !0xFF; // keep the 24-in-32 low byte zero after gain
                }
                chunk.copy_from_slice(&v.to_le_bytes());
            }
        }
        Container::I24Packed => {
            for chunk in buf.chunks_exact_mut(3) {
                // `chunk` is bytes 1..4 of the MSB-aligned 32-bit value (byte
                // 0, always zero, was dropped when packing) — reconstructing
                // it with a zero low byte recovers that exact value,
                // including the sign (carried in `chunk[2]`, the top byte).
                let v = i32::from_le_bytes([0, chunk[0], chunk[1], chunk[2]]);
                let scaled = ((v as f64) * volume as f64).clamp(i32::MIN as f64, i32::MAX as f64) as i32;
                let b = scaled.to_le_bytes();
                chunk.copy_from_slice(&b[1..4]);
            }
        }
        Container::F32 => {
            for chunk in buf.chunks_exact_mut(4) {
                let s = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                let v = (s * volume).clamp(-1.0, 1.0);
                chunk.copy_from_slice(&v.to_le_bytes());
            }
        }
    }
}

#[cfg(test)]
mod gain_tests {
    use super::*;

    #[test]
    fn volume_taper_is_exact_at_the_ends_and_linear_in_db() {
        assert_eq!(volume_to_gain(1.0), 1.0, "top must be exact unity (bit-perfect)");
        assert_eq!(volume_to_gain(0.0), 0.0);
        assert_eq!(volume_to_db(0.0), None);
        assert_eq!(volume_to_db(1.0), Some(0.0));
        assert!((volume_to_db(0.5).unwrap() + 30.0).abs() < 1e-4);
        // One 1/60 step anywhere on the slider is 1 dB.
        let step = 1.0 / 60.0;
        for p in [0.1f32, 0.5, 0.9] {
            let d = volume_to_db(p).unwrap() - volume_to_db(p - step).unwrap();
            assert!((d - 1.0).abs() < 1e-3, "p={p}: {d} dB per step");
        }
        // -20 dB at p = 2/3 is a tenth of the amplitude.
        assert!((volume_to_gain(2.0 / 3.0) - 0.1).abs() < 1e-4);
        assert_eq!(volume_to_gain(f32::NAN), 0.0);
    }

    #[test]
    fn i16_gain_matches_scalar_math() {
        let mut buf = 1000i16.to_le_bytes().to_vec();
        apply_gain_in_place(&mut buf, Container::I16, 0.5);
        assert_eq!(i16::from_le_bytes([buf[0], buf[1]]), 500);
    }

    #[test]
    fn i24_in_32_keeps_low_byte_zero_after_gain() {
        let v: i32 = 0x00_12_34_00; // MSB-aligned 24-bit value, low byte zero
        let mut buf = v.to_le_bytes().to_vec();
        apply_gain_in_place(&mut buf, Container::I24In32, 0.5);
        let got = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(got & 0xFF, 0, "low byte must stay zero");
        assert_eq!(got, (v as f64 * 0.5) as i32 & !0xFF);
    }

    #[test]
    fn i24_packed_round_trips_through_i24_in_32_math() {
        // A negative 24-bit value: MSB-aligned i32 = -1,000,000 << 8.
        let v: i32 = (-1_000_000i32) << 8;
        let packed = &v.to_le_bytes()[1..4];
        let mut buf = packed.to_vec();
        apply_gain_in_place(&mut buf, Container::I24Packed, 0.5);
        let reconstructed = i32::from_le_bytes([0, buf[0], buf[1], buf[2]]);
        let expected = (v as f64 * 0.5) as i32;
        assert_eq!(reconstructed, expected);
    }

    #[test]
    fn f32_gain_clamps_to_valid_range() {
        let mut buf = 0.9f32.to_le_bytes().to_vec();
        apply_gain_in_place(&mut buf, Container::F32, 2.0);
        assert_eq!(f32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]), 1.0);
    }
}

#[cfg(test)]
mod peak_meter_tests {
    use super::PeakMeter;

    #[test]
    fn keeps_the_loudest_value_until_taken_then_reads_silence() {
        let m = PeakMeter::default();
        m.record(0.25, 0.5);
        m.record(0.75, 0.1);
        m.record(f32::NAN, f32::INFINITY); // never poisons the meter
        assert_eq!(m.take(), (0.75, 0.5));
        // Nothing rendered since (paused/stopped): the next reading is silence.
        assert_eq!(m.take(), (0.0, 0.0));
    }
}
