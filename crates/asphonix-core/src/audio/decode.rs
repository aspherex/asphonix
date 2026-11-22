//! The decode thread: symphonia `FormatReader`/`Decoder` -> packed PCM bytes
//! pushed into the lock-free ring buffer the render thread drains.
//!
//! Two-phase handshake with the engine (so output-device negotiation, which
//! needs to know the source format, can happen between probing and decoding):
//! 1. Thread opens the `MediaSource`, probes the container, and sends
//!    [`DecodeEvent::FormatReady`] with the source's native format.
//! 2. It then blocks waiting for [`DecodeCommand::Begin`], which carries the
//!    negotiated [`Container`] to pack samples into and the ring buffer
//!    [`Producer`] to push them to.
//!
//! Gapless hand-off: when a track finishes, the thread sends the `Producer`
//! *back* to the engine in [`DecodeEvent::Eof`] (with the exact number of
//! frames it pushed) instead of dropping it. If the next track can use the
//! same output format, the engine immediately hands that same `Producer` to
//! the next track's already-probed decode thread via `Begin` — the ring
//! buffer, and therefore the render thread, never sees a gap; the frame
//! count tells the engine where in the ring the track boundary is, so the
//! UI switches tracks when the boundary is *heard*, not when it's decoded.
//!
//! Encoder delay/padding (MP3 LAME/Xing, AAC iTunSMPB) is trimmed by
//! symphonia itself (`enable_gapless`), and seeks are sample-accurate: the
//! packets before the requested timestamp that `SeekMode::Accurate` lands
//! on are decoded and discarded here.

use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use rtrb::Producer;
use symphonia::core::audio::{AudioBufferRef, SampleBuffer};
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};

use super::format::{Container, SourceFormat};

pub enum DecodeCommand {
    /// Start (or resume, after a seek) decoding into `producer`, packing
    /// samples into `container`.
    Begin {
        container: Container,
        producer: Producer<u8>,
    },
    /// Seek to `seconds` and keep decoding into a new (empty) producer.
    Seek {
        seconds: f64,
        producer: Producer<u8>,
    },
    Stop,
}

pub enum DecodeEvent {
    /// The source's native format, sent once probing succeeds.
    FormatReady(SourceFormat),
    /// Track finished decoding; hands the ring producer back to the engine
    /// so it can be reused for gapless hand-off to the next track, along
    /// with how many frames this decoder pushed into it (since `Begin` or
    /// the last `Seek`).
    Eof {
        producer: Producer<u8>,
        frames_pushed: u64,
    },
    Error(String),
}

/// Spawns the decode thread for one track. Returns immediately; progress is
/// reported via `events`.
///
/// Volume is deliberately *not* a parameter here (or anywhere in this
/// module): gain is applied at render time, on the already-packed container
/// bytes, right before they reach the device (`output::apply_gain_in_place`)
/// — not here at decode time — so a volume change is audible within one
/// render callback instead of waiting out the whole ~1.5 s ring buffer. This
/// also means the decode -> ring path below is always the unity/bit-perfect
/// conversion, never a lossy gain multiply.
pub fn spawn_decoder(
    media_source: Box<dyn MediaSource>,
    extension_hint: Option<String>,
    commands: Receiver<DecodeCommand>,
    events: Sender<DecodeEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("asphonix-decode".into())
        .spawn(move || run(media_source, extension_hint, commands, events))
        .expect("spawn decode thread")
}

/// Per-track decode state the command handlers mutate.
struct Session {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    container: Container,
    producer: Producer<u8>,
    frames_pushed: u64,
    /// Sample-accurate seek trimming.
    trim: SeekTrim,
}

enum Flow {
    /// Abandon the current packet's remaining bytes (new producer/seek).
    Restart,
    Exit,
}

impl Session {
    fn apply(&mut self, cmd: DecodeCommand, events: &Sender<DecodeEvent>) -> Flow {
        match cmd {
            DecodeCommand::Stop => Flow::Exit,
            DecodeCommand::Seek { seconds, producer } => {
                self.producer = producer;
                self.frames_pushed = 0;
                match self.format.seek(
                    SeekMode::Accurate,
                    SeekTo::Time {
                        time: Time::from(seconds.max(0.0)),
                        track_id: Some(self.track_id),
                    },
                ) {
                    Ok(seeked) => {
                        self.decoder.reset();
                        self.trim.until_ts =
                            (seeked.required_ts > seeked.actual_ts).then_some(seeked.required_ts);
                        Flow::Restart
                    }
                    Err(e) => {
                        // Typically a seek past the end: treat as end of track.
                        log::warn!("seek to {seconds:.3}s failed: {e}");
                        Flow::Exit.then_eof(self, events)
                    }
                }
            }
            DecodeCommand::Begin {
                container,
                producer,
            } => {
                // A renegotiation mid-track (shouldn't normally happen, but
                // handle it rather than panic).
                self.container = container;
                self.producer = producer;
                self.frames_pushed = 0;
                Flow::Restart
            }
        }
    }
}

/// After an accurate seek: decoded audio before `until_ts` is discarded.
struct SeekTrim {
    until_ts: Option<u64>,
    time_base: Option<TimeBase>,
    sample_rate: u32,
}

impl SeekTrim {
    /// Frames of the decoded packet at `ts` (with `frames` frames) to drop
    /// to honour a pending accurate seek.
    fn frames_to_skip(&mut self, ts: u64, frames: usize) -> usize {
        let Some(target) = self.until_ts else {
            return 0;
        };
        if ts >= target {
            self.until_ts = None;
            return 0;
        }
        let delta_ts = target - ts;
        let delta_frames = match self.time_base {
            Some(tb) if self.sample_rate > 0 => {
                (delta_ts as u128 * tb.numer as u128 * self.sample_rate as u128 / tb.denom as u128)
                    as usize
            }
            _ => delta_ts as usize,
        };
        if delta_frames >= frames {
            frames
        } else {
            self.until_ts = None;
            delta_frames
        }
    }
}

impl Flow {
    fn then_eof(self, s: &mut Session, events: &Sender<DecodeEvent>) -> Flow {
        // Hand the real producer back to the engine; a zero-capacity dummy
        // takes its place in the session (never used afterwards).
        let (dummy, _) = rtrb::RingBuffer::<u8>::new(0);
        let producer = std::mem::replace(&mut s.producer, dummy);
        let _ = events.send(DecodeEvent::Eof {
            producer,
            frames_pushed: s.frames_pushed,
        });
        self
    }
}

fn run(
    media_source: Box<dyn MediaSource>,
    extension_hint: Option<String>,
    commands: Receiver<DecodeCommand>,
    events: Sender<DecodeEvent>,
) {
    let mss = MediaSourceStream::new(media_source, MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let Some(ext) = extension_hint {
        hint.with_extension(&ext);
    }

    // `enable_gapless`: the reader reports encoder delay/padding and the
    // decoders trim it, so MP3/AAC albums play without inter-track gaps.
    let format_opts = FormatOptions {
        enable_gapless: true,
        ..Default::default()
    };
    let probed = match symphonia::default::get_probe().format(
        &hint,
        mss,
        &format_opts,
        &MetadataOptions::default(),
    ) {
        Ok(p) => p,
        Err(e) => {
            let _ = events.send(DecodeEvent::Error(format!("probe failed: {e}")));
            return;
        }
    };
    let format: Box<dyn FormatReader> = probed.format;

    let track = match format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
    {
        Some(t) => t.clone(),
        None => {
            let _ = events.send(DecodeEvent::Error("no decodable track found".into()));
            return;
        }
    };

    let codecs = symphonia::default::get_codecs();
    let source_format = SourceFormat {
        sample_rate: track.codec_params.sample_rate.unwrap_or(44_100),
        channels: track
            .codec_params
            .channels
            .map(|c| c.count() as u8)
            .unwrap_or(2),
        bit_depth: track.codec_params.bits_per_sample.map(|b| b as u8),
        codec: codecs
            .get_codec(track.codec_params.codec)
            .map(|d| d.short_name)
            .unwrap_or(""),
        total_frames: track.codec_params.n_frames,
    };

    let decoder: Box<dyn Decoder> =
        match codecs.make(&track.codec_params, &DecoderOptions::default()) {
            Ok(d) => d,
            Err(e) => {
                let _ = events.send(DecodeEvent::Error(format!("no decoder for track: {e}")));
                return;
            }
        };

    if events
        .send(DecodeEvent::FormatReady(source_format))
        .is_err()
    {
        return; // engine gone
    }

    // Block until the engine tells us which container to pack into and
    // hands us somewhere to write.
    let (container, producer) = match commands.recv() {
        Ok(DecodeCommand::Begin {
            container,
            producer,
        }) => (container, producer),
        _ => return, // Stop, engine gone, or protocol error
    };

    let mut s = Session {
        format,
        decoder,
        track_id: track.id,
        trim: SeekTrim {
            until_ts: None,
            time_base: track.codec_params.time_base,
            sample_rate: source_format.sample_rate,
        },
        container,
        producer,
        frames_pushed: 0,
    };
    let mut packer = Packer::new();
    let mut scratch = Vec::with_capacity(64 * 1024);

    'decode: loop {
        // Drain any pending control messages without blocking.
        loop {
            match commands.try_recv() {
                Ok(cmd) => match s.apply(cmd, &events) {
                    Flow::Exit => return,
                    Flow::Restart => {}
                },
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            }
        }

        let packet = match s.format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                Flow::Exit.then_eof(&mut s, &events);
                return;
            }
            Err(SymphoniaError::ResetRequired) => {
                s.decoder.reset();
                continue 'decode;
            }
            Err(e) => {
                let _ = events.send(DecodeEvent::Error(format!("demux error: {e}")));
                Flow::Exit.then_eof(&mut s, &events);
                return;
            }
        };

        if packet.track_id() != s.track_id {
            continue;
        }

        let decoded = match s.decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(e)) => {
                log::debug!("skipping undecodable packet: {e}");
                continue;
            }
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                Flow::Exit.then_eof(&mut s, &events);
                return;
            }
            Err(e) => {
                let _ = events.send(DecodeEvent::Error(format!("decode error: {e}")));
                Flow::Exit.then_eof(&mut s, &events);
                return;
            }
        };

        let frames = decoded.frames();
        let skip = s.trim.frames_to_skip(packet.ts(), frames);
        if skip >= frames {
            continue;
        }
        let channels = decoded.spec().channels.count();
        packer.pack(decoded, s.container, &mut scratch);
        if skip > 0 {
            scratch.drain(..skip * channels * s.container.bytes_per_sample());
        }
        s.frames_pushed += (frames - skip) as u64;

        // Push into the ring, blocking (with periodic control-channel
        // checks) while it's full. The decode thread is not the real-time
        // path; it waits until about half the ring has drained (clamped), on
        // the command channel so a command wakes it at once.
        let bytes_per_sec = s.trim.sample_rate as usize * channels * s.container.bytes_per_sample();
        let mut offset = 0usize;
        while offset < scratch.len() {
            let (_, remainder) = s.producer.push_partial_slice(&scratch[offset..]);
            offset = scratch.len() - remainder.len();
            if offset < scratch.len() {
                match commands.recv_timeout(ring_full_wait(
                    s.producer.buffer().capacity() - s.producer.slots(),
                    s.producer.buffer().capacity(),
                    bytes_per_sec,
                )) {
                    Ok(cmd) => match s.apply(cmd, &events) {
                        Flow::Exit => return,
                        Flow::Restart => continue 'decode,
                    },
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        }
    }
}

/// How long to wait before retrying a push into a full ring: the time the
/// bytes above 5/6 of the ring's capacity take to play (i.e. wake when the
/// ring has dropped to ~5/6 full), clamped to 10-250 ms. The ring holds ~1.5 s,
/// so it never drops below ~1.25 s: the decoder still reaches EOF with plenty
/// of margin for the gapless next-track probe, while a full ring (e.g.
/// paused) costs ~4-10 wake-ups/s instead of 20-100.
fn ring_full_wait(fill: usize, capacity: usize, bytes_per_sec: usize) -> Duration {
    let above = fill.saturating_sub(capacity * 5 / 6);
    let ms = (above as u128 * 1000 / bytes_per_sec.max(1) as u128).clamp(10, 250);
    Duration::from_millis(ms as u64)
}

/// Reusable scratch `SampleBuffer`s (one per numeric path) so we never
/// allocate a fresh conversion buffer per packet — only when a packet is
/// larger than any seen before.
struct Packer {
    i16_buf: Option<SampleBuffer<i16>>,
    i32_buf: Option<SampleBuffer<i32>>,
    f32_buf: Option<SampleBuffer<f32>>,
}

fn ensure_capacity<S: symphonia::core::sample::Sample>(
    slot: &mut Option<SampleBuffer<S>>,
    decoded: &AudioBufferRef<'_>,
) {
    let needed = decoded.capacity() * decoded.spec().channels.count();
    if slot.as_ref().map(|b| b.capacity() < needed).unwrap_or(true) {
        *slot = Some(SampleBuffer::new(
            decoded.capacity() as u64,
            *decoded.spec(),
        ));
    }
}

impl Packer {
    fn new() -> Self {
        Self {
            i16_buf: None,
            i32_buf: None,
            f32_buf: None,
        }
    }

    /// Converts a decoded packet into `container`'s exact byte layout. Always
    /// the unity/bit-perfect conversion (see `spawn_decoder`'s doc comment —
    /// gain is applied at render time, not here).
    ///
    /// This is a pure re-typing of symphonia's exact integer sample
    /// conversion: FLAC/ALAC/WAV samples arrive MSB-aligned (`i24 -> i32` and
    /// `i16 -> i32` are lossless left shifts), so `I24In32` is the
    /// left-justified WASAPI 24-in-32 layout, `I24Packed` is its top three
    /// bytes, and a 16-bit source in either 24-bit container gets zero low
    /// bits — bit-identical to the source.
    pub(crate) fn pack(&mut self, decoded: AudioBufferRef<'_>, container: Container, out: &mut Vec<u8>) {
        out.clear();
        match container {
            Container::I16 => {
                ensure_capacity(&mut self.i16_buf, &decoded);
                let buf = self.i16_buf.as_mut().expect("sized above");
                buf.copy_interleaved_ref(decoded);
                out.reserve(buf.samples().len() * 2);
                for &s in buf.samples() {
                    out.extend_from_slice(&s.to_le_bytes());
                }
            }
            Container::I24In32 | Container::I24Packed | Container::I32 => {
                ensure_capacity(&mut self.i32_buf, &decoded);
                let buf = self.i32_buf.as_mut().expect("sized above");
                buf.copy_interleaved_ref(decoded);
                let packed = container == Container::I24Packed;
                out.reserve(buf.samples().len() * container.bytes_per_sample());
                for &s in buf.samples() {
                    let b = s.to_le_bytes();
                    if packed {
                        out.extend_from_slice(&b[1..4]);
                    } else {
                        out.extend_from_slice(&b);
                    }
                }
            }
            Container::F32 => {
                ensure_capacity(&mut self.f32_buf, &decoded);
                let buf = self.f32_buf.as_mut().expect("sized above");
                buf.copy_interleaved_ref(decoded);
                out.reserve(buf.samples().len() * 4);
                for &s in buf.samples() {
                    out.extend_from_slice(&s.to_le_bytes());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Bit-perfection: decode a WAV with known PCM through the real decode
    //! thread + ring and compare the packed container bytes against the
    //! reference samples, for every container the negotiation can pick.
    use super::*;
    use std::io::Write;

    #[test]
    fn ring_full_wait_targets_five_sixths_full_and_is_clamped() {
        let bps = 48_000 * 2 * 4; // 48 kHz stereo, 32-bit container
        let cap = bps * 3 / 2; // 1.5 s ring
        // At or below 5/6 full: minimum wait.
        assert_eq!(ring_full_wait(cap * 5 / 6, cap, bps), Duration::from_millis(10));
        assert_eq!(ring_full_wait(0, cap, bps), Duration::from_millis(10));
        // 100 ms above 5/6: proportional.
        assert_eq!(ring_full_wait(cap * 5 / 6 + bps / 10, cap, bps), Duration::from_millis(100));
        // Full ring: 250 ms above 5/6 (1.5 s / 6), exactly the cap; >= 1.25 s stays buffered.
        assert_eq!(ring_full_wait(cap, cap, bps), Duration::from_millis(250));
        assert_eq!(ring_full_wait(100, 100, 0), Duration::from_millis(250)); // no div by zero
    }

    /// Deterministic pseudo-random full-scale samples (xorshift), so every
    /// bit position — including the LSBs — is exercised.
    fn reference_samples(n: usize, bits: u32) -> Vec<i32> {
        let mut x: u32 = 0x9E37_79B9;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as i32) >> (32 - bits) // sign-extended `bits`-bit value
            })
            .collect()
    }

    fn write_wav(path: &std::path::Path, rate: u32, bits: u16, channels: u16, samples: &[i32]) {
        let bps = (bits / 8) as u32;
        let data_len = samples.len() as u32 * bps;
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
        f.write_all(b"WAVEfmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&channels.to_le_bytes()).unwrap();
        f.write_all(&rate.to_le_bytes()).unwrap();
        f.write_all(&(rate * channels as u32 * bps).to_le_bytes())
            .unwrap();
        f.write_all(&((channels as u32 * bps) as u16).to_le_bytes())
            .unwrap();
        f.write_all(&bits.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_len.to_le_bytes()).unwrap();
        for &s in samples {
            f.write_all(&s.to_le_bytes()[..bps as usize]).unwrap();
        }
    }

    /// Runs the real decode thread on `path` into a ring and returns
    /// everything it pushed, plus the reported frame count.
    fn decode_all(
        path: &std::path::Path,
        container: Container,
        seek: Option<f64>,
    ) -> (SourceFormat, Vec<u8>, u64) {
        let file = std::fs::File::open(path).unwrap();
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let handle = spawn_decoder(Box::new(file), None, cmd_rx, evt_tx);
        let fmt = match evt_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            DecodeEvent::FormatReady(f) => f,
            _ => panic!("expected FormatReady"),
        };
        let (prod, mut cons) = rtrb::RingBuffer::<u8>::new(1 << 16);
        cmd_tx
            .send(DecodeCommand::Begin {
                container,
                producer: prod,
            })
            .unwrap();
        if let Some(sec) = seek {
            let (p2, c2) = rtrb::RingBuffer::<u8>::new(1 << 16);
            cmd_tx
                .send(DecodeCommand::Seek {
                    seconds: sec,
                    producer: p2,
                })
                .unwrap();
            cons = c2;
        }
        let mut out = Vec::new();
        let mut buf = vec![0u8; 4096];
        loop {
            let (popped, _) = cons.pop_partial_slice(&mut buf);
            out.extend_from_slice(popped);
            match evt_rx.try_recv() {
                Ok(DecodeEvent::Eof { frames_pushed, .. }) => {
                    loop {
                        let (popped, _) = cons.pop_partial_slice(&mut buf);
                        if popped.is_empty() {
                            break;
                        }
                        out.extend_from_slice(popped);
                    }
                    handle.join().unwrap();
                    return (fmt, out, frames_pushed);
                }
                Ok(DecodeEvent::Error(e)) => panic!("decode error {e}"),
                _ => std::thread::sleep(Duration::from_millis(1)),
            }
        }
    }

    fn expected_bytes(samples: &[i32], src_bits: u32, container: Container) -> Vec<u8> {
        let mut out = Vec::new();
        for &s in samples {
            let msb_aligned = s << (32 - src_bits); // what the DAC must see
            match container {
                Container::I16 => {
                    out.extend_from_slice(&((msb_aligned >> 16) as i16).to_le_bytes())
                }
                Container::I24In32 | Container::I32 => {
                    out.extend_from_slice(&msb_aligned.to_le_bytes())
                }
                Container::I24Packed => out.extend_from_slice(&msb_aligned.to_le_bytes()[1..4]),
                Container::F32 => unreachable!(),
            }
        }
        out
    }

    /// MSB-first bit writer for the FLAC test encoder.
    struct Bits {
        out: Vec<u8>,
        acc: u64,
        n: u32,
    }

    impl Bits {
        fn new() -> Self {
            Self { out: Vec::new(), acc: 0, n: 0 }
        }
        fn put(&mut self, value: u64, bits: u32) {
            for i in (0..bits).rev() {
                self.acc = (self.acc << 1) | ((value >> i) & 1);
                self.n += 1;
                if self.n == 8 {
                    self.out.push(self.acc as u8);
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }
        fn align(&mut self) {
            while self.n != 0 {
                self.put(0, 1);
            }
        }
    }

    fn crc8(data: &[u8]) -> u8 {
        let mut crc = 0u8;
        for &b in data {
            crc ^= b;
            for _ in 0..8 {
                crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
            }
        }
        crc
    }

    fn crc16(data: &[u8]) -> u16 {
        let mut crc = 0u16;
        for &b in data {
            crc ^= (b as u16) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
            }
        }
        crc
    }

    /// Minimal spec-conformant FLAC encoder (VERBATIM subframes, fixed
    /// 4096-frame blocks, stereo) — no encoder binary needed, and the
    /// decoder still runs its real FLAC path (frame parsing, CRCs, the
    /// MSB-alignment shift).
    fn write_flac(path: &std::path::Path, rate: u32, bits: u32, samples: &[i32]) {
        const BLOCK: usize = 4096;
        let frames = samples.len() / 2;
        let mut out = b"fLaC".to_vec();
        let mut si = Bits::new();
        si.put(1, 1); // last metadata block
        si.put(0, 7); // STREAMINFO
        si.put(34, 24);
        si.put(BLOCK as u64, 16);
        si.put(BLOCK as u64, 16);
        si.put(0, 24);
        si.put(0, 24);
        si.put(rate as u64, 20);
        si.put(1, 3); // 2 channels
        si.put(bits as u64 - 1, 5);
        si.put(frames as u64, 36);
        si.put(0, 64); // MD5 unknown
        si.put(0, 64);
        out.extend_from_slice(&si.out);
        for (frame_no, chunk) in samples.chunks(BLOCK * 2).enumerate() {
            let n = chunk.len() / 2;
            let mut b = Bits::new();
            b.put(0b11111111111110, 14);
            b.put(0, 1);
            b.put(0, 1); // fixed blocksize
            b.put(0b0111, 4); // 16-bit (blocksize-1) follows
            b.put(0b0000, 4); // rate from STREAMINFO
            b.put(0b0001, 4); // independent stereo
            b.put(if bits == 16 { 0b100 } else { 0b110 }, 3);
            b.put(0, 1);
            assert!(frame_no < 128);
            b.put(frame_no as u64, 8); // UTF-8 coded frame number (< 128)
            b.put(n as u64 - 1, 16);
            let crc = crc8(&b.out);
            b.put(crc as u64, 8);
            for ch in 0..2 {
                b.put(0, 1);
                b.put(0b000001, 6); // VERBATIM
                b.put(0, 1);
                for i in 0..n {
                    b.put((chunk[i * 2 + ch] as u32 as u64) & ((1u64 << bits) - 1), bits);
                }
            }
            b.align();
            let crc = crc16(&b.out);
            b.put(crc as u64, 16);
            out.extend_from_slice(&b.out);
        }
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn pcm_is_bit_identical_in_every_lossless_container() {
        let dir = tempfile::tempdir().unwrap();
        for (bits, containers) in [
            (16u32, &[Container::I16, Container::I24In32, Container::I24Packed, Container::I32][..]),
            (24u32, &[Container::I24In32, Container::I24Packed, Container::I32][..]),
        ] {
            let samples = reference_samples(2 * 30_000, bits);
            let wav = dir.path().join(format!("ref{bits}.wav"));
            write_wav(&wav, 44_100, bits as u16, 2, &samples);
            let flac = dir.path().join(format!("ref{bits}.flac"));
            write_flac(&flac, 44_100, bits, &samples);
            for path in [&wav, &flac] {
                for &container in containers {
                    let (fmt, got, frames) = decode_all(path, container, None);
                    let what = format!("{bits}-bit {} in {container:?}", fmt.codec);
                    assert_eq!(fmt.bit_depth, Some(bits as u8), "{what}");
                    assert_eq!(frames, 30_000, "{what}: frame count");
                    let want = expected_bytes(&samples, bits, container);
                    assert_eq!(got.len(), want.len(), "{what}: length");
                    assert!(got == want, "{what}: not bit-identical");
                }
            }
        }
    }

    #[test]
    fn accurate_seek_starts_on_the_exact_sample() {
        let dir = tempfile::tempdir().unwrap();
        let rate = 48_000u32;
        let samples = reference_samples(2 * rate as usize * 2, 24); // 2 s stereo
        let path = dir.path().join("seek.wav");
        write_wav(&path, rate, 24, 2, &samples);
        let (_, got, frames) = decode_all(&path, Container::I24In32, Some(0.75));
        let start_frame = (0.75 * rate as f64) as usize;
        let want = expected_bytes(&samples[start_frame * 2..], 24, Container::I24In32);
        assert_eq!(frames as usize, samples.len() / 2 - start_frame);
        assert_eq!(&got[..64], &want[..64], "first samples after seek");
        assert!(got == want);
    }
}
