//! Spectrum analyzer: mono-downmixed PCM -> log-spaced magnitude bands sent
//! to the UI as `SpectrumFrame { bands: Vec<u8> }` at ~20fps.
//!
//! All FFT buffers (input, complex output, scratch, window table, band bin
//! ranges) are allocated once in [`SpectrumAnalyzer::new`] and reused for
//! every frame — `process()` itself never allocates. This runs on its own
//! thread (see `audio/engine.rs`), fed by a small lock-free ring of mono
//! samples the render thread pushes to; it is never called from the
//! real-time render callback itself.

use realfft::num_complex::Complex32;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// FFT window size. 2048 samples @ 44.1kHz ≈ 46ms — plenty of frequency
/// resolution for a coarse visual spectrum, comfortably within a 20fps frame
/// budget (50ms).
const FFT_SIZE: usize = 2048;
pub const NUM_BANDS: usize = 28;
/// Frames per second sent to the UI (the UI draws time-based decay and
/// peak hold, so the rate only sets how often the targets update).
const SPECTRUM_FPS: usize = 20;

/// A full-scale sine through a periodic Hann window peaks at
/// `FFT_SIZE / 4` in the unnormalized FFT (N/2 for the one-sided
/// spectrum x 0.5 coherent gain); dividing by it puts magnitudes in dBFS.
const FULL_SCALE_MAGNITUDE: f32 = FFT_SIZE as f32 / 4.0;
/// Bottom of the displayed range: quieter content draws as an empty bar.
const FLOOR_DB: f32 = -70.0;

pub struct SpectrumAnalyzer {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    input: Vec<f32>,
    spectrum: Vec<Complex32>,
    scratch: Vec<Complex32>,
    /// Ring of the most recent `FFT_SIZE` mono samples (overwritten in
    /// place, no reallocation).
    history: Vec<f32>,
    history_pos: usize,
    /// How many *new* samples have arrived since the last computed frame;
    /// used to pace output to ~20fps regardless of input push granularity.
    samples_since_frame: usize,
    frame_stride: usize,
    /// (first_bin, last_bin_exclusive) per band, precomputed from the
    /// sample rate so `process()` only does integer bin lookups.
    band_bins: Vec<(usize, usize)>,
}

impl SpectrumAnalyzer {
    pub fn new(sample_rate: u32) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);

        // Periodic Hann window.
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FFT_SIZE as f32).cos()
            })
            .collect();

        let band_bins = log_spaced_bands(sample_rate, FFT_SIZE, NUM_BANDS);

        Self {
            input: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            fft,
            window,
            history: vec![0.0; FFT_SIZE],
            history_pos: 0,
            samples_since_frame: 0,
            frame_stride: (sample_rate as usize / SPECTRUM_FPS).max(1),
            band_bins,
        }
    }

    /// Feeds newly-decoded mono samples into the rolling history. Cheap: a
    /// fixed number of writes into a pre-allocated `Vec`, no allocation.
    pub fn push_samples(&mut self, samples: &[f32]) {
        for &s in samples {
            self.history[self.history_pos] = s;
            self.history_pos = (self.history_pos + 1) % FFT_SIZE;
        }
        self.samples_since_frame += samples.len();
    }

    /// Returns a new frame's bands if enough samples have accumulated since
    /// the last one (paces to ~20fps independent of caller cadence),
    /// otherwise `None`.
    pub fn maybe_compute(&mut self) -> Option<Vec<u8>> {
        if self.samples_since_frame < self.frame_stride {
            return None;
        }
        self.samples_since_frame = 0;
        Some(self.compute())
    }

    fn compute(&mut self) -> Vec<u8> {
        // Unroll the ring into `input`, oldest-first, applying the window.
        for i in 0..FFT_SIZE {
            let idx = (self.history_pos + i) % FFT_SIZE;
            self.input[i] = self.history[idx] * self.window[i];
        }

        if self
            .fft
            .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
            .is_err()
        {
            return vec![0u8; NUM_BANDS];
        }

        let mut bands = Vec::with_capacity(NUM_BANDS);
        for &(start, end) in &self.band_bins {
            let mut peak = 0f32;
            for bin in start..end.max(start + 1) {
                if let Some(c) = self.spectrum.get(bin) {
                    peak = peak.max(c.norm());
                }
            }
            // Rough log-magnitude -> 0..255 mapping. Not calibrated to any
            // absolute SPL/dBFS reference — it's a visual VU-style readout,
            // not a measurement, so a fixed, generous dynamic-range floor is
            // fine here.
            // Without the FULL_SCALE_MAGNITUDE normalization every band of
            // ordinary mastered music sat 30-50 dB above "0" and the whole
            // analyzer drew as a solid pegged block.
            let db = 20.0 * (peak / FULL_SCALE_MAGNITUDE).max(1e-7).log10();
            let normalized = ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);
            bands.push((normalized * 255.0) as u8);
        }
        bands
    }
}

/// Lowest level the peak meter reports (silence).
pub const PEAK_FLOOR_DB: f32 = -90.0;

/// Linear peak (`1.0` = full scale) -> dBFS rounded to 0.1 dB, floored at
/// [`PEAK_FLOOR_DB`].
pub fn peak_to_dbfs(linear: f32) -> f32 {
    if linear.is_nan() || linear <= 0.0 {
        return PEAK_FLOOR_DB;
    }
    let db = 20.0 * linear.min(1.0).log10();
    ((db.max(PEAK_FLOOR_DB)) * 10.0).round() / 10.0
}

/// Precomputes log-spaced FFT bin ranges for `num_bands` bands covering
/// ~40Hz to just under Nyquist.
fn log_spaced_bands(sample_rate: u32, fft_size: usize, num_bands: usize) -> Vec<(usize, usize)> {
    let nyquist = sample_rate as f32 / 2.0;
    let min_hz = 40f32.min(nyquist * 0.9);
    let max_hz = nyquist * 0.98;
    let bin_hz = sample_rate as f32 / fft_size as f32;
    let max_bin = fft_size / 2;

    let log_min = min_hz.ln();
    let log_max = max_hz.ln();
    let mut bands = Vec::with_capacity(num_bands);
    for i in 0..num_bands {
        let t0 = i as f32 / num_bands as f32;
        let t1 = (i + 1) as f32 / num_bands as f32;
        let f0 = (log_min + (log_max - log_min) * t0).exp();
        let f1 = (log_min + (log_max - log_min) * t1).exp();
        let b0 = ((f0 / bin_hz) as usize).min(max_bin.saturating_sub(1));
        let b1 = ((f1 / bin_hz) as usize).clamp(b0 + 1, max_bin);
        bands.push((b0, b1));
    }
    bands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_the_requested_band_count() {
        let mut a = SpectrumAnalyzer::new(44_100);
        let samples = vec![0.0f32; FFT_SIZE];
        a.push_samples(&samples);
        let frame = a.compute();
        assert_eq!(frame.len(), NUM_BANDS);
    }

    #[test]
    fn paces_output_to_the_configured_stride() {
        let mut a = SpectrumAnalyzer::new(44_100);
        assert!(a.maybe_compute().is_none());
        a.push_samples(&vec![0.1f32; a.frame_stride]);
        assert!(a.maybe_compute().is_some());
        // Immediately after, not enough new samples have arrived yet.
        assert!(a.maybe_compute().is_none());
    }

    #[test]
    fn silence_yields_low_bands_without_panicking() {
        let mut a = SpectrumAnalyzer::new(48_000);
        a.push_samples(&vec![0.0f32; FFT_SIZE]);
        let frame = a.compute();
        assert!(frame.iter().all(|&b| b <= 10));
    }

    fn sine(sample_rate: u32, hz: f32, amplitude: f32) -> Vec<f32> {
        (0..FFT_SIZE)
            .map(|i| amplitude * (2.0 * std::f32::consts::PI * hz * i as f32 / sample_rate as f32).sin())
            .collect()
    }

    #[test]
    fn full_scale_sine_peaks_one_band_without_pegging_the_rest() {
        let mut a = SpectrumAnalyzer::new(44_100);
        a.push_samples(&sine(44_100, 1_000.0, 1.0));
        let frame = a.compute();
        let max = *frame.iter().max().unwrap();
        assert!(max >= 240, "a 0 dBFS tone should reach the top: {frame:?}");
        let pegged = frame.iter().filter(|&&b| b >= 240).count();
        assert!(pegged <= 2, "only the tone band(s) may be at full scale: {frame:?}");
    }

    #[test]
    fn quieter_signal_draws_proportionally_lower() {
        let mut loud = SpectrumAnalyzer::new(44_100);
        loud.push_samples(&sine(44_100, 1_000.0, 1.0));
        let mut quiet = SpectrumAnalyzer::new(44_100);
        quiet.push_samples(&sine(44_100, 1_000.0, 0.01)); // -40 dBFS
        let l = *loud.compute().iter().max().unwrap();
        let q = *quiet.compute().iter().max().unwrap();
        // -40 dB on a 70 dB scale is ~146 counts lower.
        assert!(l > q && (l - q) > 120 && (l - q) < 170, "loud {l} quiet {q}");
    }

    #[test]
    fn peak_dbfs_math() {
        assert_eq!(peak_to_dbfs(1.0), 0.0);
        assert_eq!(peak_to_dbfs(2.0), 0.0, "clamped at full scale");
        assert_eq!(peak_to_dbfs(0.5), -6.0);
        assert_eq!(peak_to_dbfs(0.1), -20.0);
        assert_eq!(peak_to_dbfs(0.0), PEAK_FLOOR_DB);
        assert_eq!(peak_to_dbfs(f32::NAN), PEAK_FLOOR_DB);
        assert_eq!(peak_to_dbfs(1e-9), PEAK_FLOOR_DB);
    }

    #[test]
    fn band_bins_are_monotonic_and_in_range() {
        let bins = log_spaced_bands(44_100, FFT_SIZE, NUM_BANDS);
        let max_bin = FFT_SIZE / 2;
        let mut last_end = 0usize;
        for (start, end) in bins {
            assert!(start < end);
            assert!(end <= max_bin);
            assert!(start >= last_end.saturating_sub(1));
            last_end = end;
        }
    }
}
