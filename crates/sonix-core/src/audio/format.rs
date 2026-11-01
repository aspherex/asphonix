//! Sample containers and format negotiation types shared between the decode
//! thread and the output backends.
//!
//! The negotiation rule (see `PLAN.md` / module docs): for a given source bit
//! depth we only ever try *container* variants that can represent it exactly
//! — never a smaller container that would truncate bits. Wider integer
//! containers are fine and still bit-perfect: a 16-bit sample placed
//! MSB-aligned in a 24- or 32-bit container (low bits zero) is the identical
//! value to the DAC, which is how DACs that reject 16-bit exclusive formats
//! get fed. If no exclusive-mode container is accepted by the device at the
//! source's native sample rate, we fall back to shared mode rather than
//! silently downgrading bit depth or resampling in exclusive mode.

use crate::ipc::OutputMode;

/// The exact byte layout the decode thread packs samples into and the
/// render thread copies verbatim (no conversion, no allocation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Container {
    /// 16-bit signed PCM, little-endian, 2 bytes/sample.
    I16,
    /// 24-bit audio MSB-aligned in a 32-bit signed little-endian container
    /// (the common "24-in-32" exclusive-mode layout, low byte zero).
    I24In32,
    /// 24-bit signed PCM packed into 3 bytes/sample, little-endian (some
    /// USB/HDMI drivers accept only this, not 24-in-32).
    I24Packed,
    /// 32-bit signed PCM, little-endian, 4 bytes/sample.
    I32,
    /// 32-bit IEEE float, little-endian, 4 bytes/sample.
    F32,
}

impl Container {
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Container::I16 => 2,
            Container::I24Packed => 3,
            Container::I24In32 | Container::I32 | Container::F32 => 4,
        }
    }

    /// The *valid* bits of the container (what the DAC is told), not the
    /// storage size.
    pub fn valid_bits(self) -> u8 {
        match self {
            Container::I16 => 16,
            Container::I24In32 | Container::I24Packed => 24,
            Container::I32 => 32,
            Container::F32 => 32,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Container::I16 => "PCM16",
            Container::I24In32 => "PCM24",
            Container::I24Packed => "PCM24P",
            Container::I32 => "PCM32",
            Container::F32 => "FLOAT32",
        }
    }
}

/// What the decoder found for the current track, before any output device
/// has been consulted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceFormat {
    pub sample_rate: u32,
    pub channels: u8,
    /// `None` for codecs with no fixed PCM bit depth (MP3/AAC/Vorbis — these
    /// decode to float internally); `Some(16|24|32)` for lossless PCM-like
    /// sources (FLAC/ALAC/WAV/AIFF).
    pub bit_depth: Option<u8>,
    /// symphonia's short codec name (`"flac"`, `"mp3"`, `"pcm_s24le"`...),
    /// used for the UI's format label. Empty if unknown.
    pub codec: &'static str,
    /// Total frames in the track, when the container states it (FLAC
    /// STREAMINFO, WAV data size, MP3 Xing/LAME header, MP4 sample table).
    pub total_frames: Option<u64>,
}

impl SourceFormat {
    /// Container candidates to try, in preference order, for this source.
    /// Exclusive mode must pick one of these — never a smaller one.
    pub fn candidate_containers(&self) -> &'static [Container] {
        match self.bit_depth {
            // Exact container first, then zero-padded wider ones.
            Some(b) if b <= 16 => &[
                Container::I16,
                Container::I24In32,
                Container::I24Packed,
                Container::I32,
            ],
            Some(b) if b <= 24 => &[Container::I24In32, Container::I24Packed, Container::I32],
            Some(_) => &[Container::I32],
            // Lossy codecs decode to float: prefer a float stream, then the
            // widest integer containers a DAC may insist on (the decoder's
            // own output quantised once, at >= 24 bits — no resampling).
            None => &[
                Container::F32,
                Container::I32,
                Container::I24In32,
                Container::I24Packed,
            ],
        }
    }

    /// Track duration from the container's frame count; 0.0 if unknown.
    pub fn duration_sec(&self) -> f64 {
        match self.total_frames {
            Some(n) if self.sample_rate > 0 => n as f64 / self.sample_rate as f64,
            _ => 0.0,
        }
    }

    /// Short upper-case codec label for the UI ("FLAC", "MP3", "PCM").
    pub fn codec_label(&self) -> String {
        if self.codec.starts_with("pcm") {
            "PCM".to_string()
        } else {
            self.codec.to_uppercase()
        }
    }
}

/// The format actually negotiated with an output device/mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NegotiatedFormat {
    pub sample_rate: u32,
    pub channels: u8,
    pub container: Container,
    pub mode: OutputMode,
    /// WAVEFORMATEXTENSIBLE `dwChannelMask` the device actually accepted
    /// (`None` = the standard mask for `channels`). Some drivers only take a
    /// zero mask; the render thread must initialize with exactly the variant
    /// that passed `IsFormatSupported`.
    pub channel_mask: Option<u32>,
    /// `true` when the device only accepted the plain WAVEFORMATEX
    /// representation of this format (not WAVEFORMATEXTENSIBLE).
    pub plain_wfx: bool,
}

impl NegotiatedFormat {
    /// A format with the default channel mask / WAVEFORMATEXTENSIBLE layout.
    pub fn new(sample_rate: u32, channels: u8, container: Container, mode: OutputMode) -> Self {
        Self {
            sample_rate,
            channels,
            container,
            mode,
            channel_mask: None,
            plain_wfx: false,
        }
    }

    pub fn bytes_per_frame(&self) -> usize {
        self.container.bytes_per_sample() * self.channels as usize
    }

    /// `true` only when playing back at the file's native rate/depth with no
    /// resampling and no gain applied — the UI's "BIT-PERFECT" badge.
    pub fn is_bit_perfect_capable(&self) -> bool {
        matches!(self.mode, OutputMode::Exclusive)
    }

    /// e.g. `WASAPI EXCL · 24/96000 · FLAC` — the bit depth is the
    /// container's valid bits as actually sent to the device.
    pub fn format_label(&self, codec_label: &str) -> String {
        let mut s = format!(
            "{} · {}/{}",
            match self.mode {
                OutputMode::Exclusive => "WASAPI EXCL",
                OutputMode::Shared => "SHARED",
            },
            self.container.valid_bits(),
            self.sample_rate,
        );
        if !codec_label.is_empty() {
            s.push_str(" · ");
            s.push_str(codec_label);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_never_shrink_bit_depth() {
        let fmt24 = SourceFormat {
            sample_rate: 96_000,
            channels: 2,
            bit_depth: Some(24),
            codec: "flac",
            total_frames: None,
        };
        for c in fmt24.candidate_containers() {
            assert!(
                c.valid_bits() >= 24,
                "must not truncate 24-bit source to {c:?}"
            );
            assert_ne!(*c, Container::F32);
        }

        let fmt16 = SourceFormat {
            sample_rate: 44_100,
            channels: 2,
            bit_depth: Some(16),
            codec: "flac",
            total_frames: None,
        };
        assert_eq!(fmt16.candidate_containers()[0], Container::I16);
        for c in fmt16.candidate_containers() {
            assert!(c.valid_bits() >= 16);
            assert_ne!(*c, Container::F32);
        }

        let lossy = SourceFormat {
            sample_rate: 44_100,
            channels: 2,
            bit_depth: None,
            codec: "mp3",
            total_frames: None,
        };
        assert_eq!(lossy.candidate_containers()[0], Container::F32);
    }

    #[test]
    fn container_labels_are_human_readable() {
        assert_eq!(Container::I16.label(), "PCM16");
        assert_eq!(Container::I24In32.label(), "PCM24");
        assert_eq!(Container::I24Packed.label(), "PCM24P");
        assert_eq!(Container::I32.label(), "PCM32");
        assert_eq!(Container::F32.label(), "FLOAT32");
    }

    #[test]
    fn bytes_per_frame_matches_container_and_channels() {
        let fmt = NegotiatedFormat::new(48_000, 2, Container::I24In32, OutputMode::Exclusive);
        assert_eq!(fmt.bytes_per_frame(), 8);
        let packed = NegotiatedFormat::new(48_000, 2, Container::I24Packed, OutputMode::Exclusive);
        assert_eq!(packed.bytes_per_frame(), 6);
    }

    #[test]
    fn format_label_includes_codec() {
        let fmt = NegotiatedFormat::new(96_000, 2, Container::I24In32, OutputMode::Exclusive);
        assert_eq!(fmt.format_label("FLAC"), "WASAPI EXCL · 24/96000 · FLAC");
        assert_eq!(fmt.format_label(""), "WASAPI EXCL · 24/96000");
    }
}
