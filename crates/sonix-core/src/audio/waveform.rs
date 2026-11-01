//! Per-track waveform for Now Playing's seek bar: [`BUCKETS`] loudness values
//! (`u8`, RMS x sqrt(2) clamped to 0..255, so a full-scale sine is 255)
//! computed from the decoded PCM. RMS, not peak: modern masters sit at 0 dBFS
//! nearly everywhere, so a peak waveform of a real library was a flat bar.
//!
//! Why a separate scan and not the playback decoder: that decoder is paced
//! by the ring buffer (it blocks once ~1.5 s are decoded ahead of the
//! speakers), so it would only ever have seen the part already played. The
//! scan instead decodes the *same compressed bytes* playback is already
//! fetching: `TrackSource::open_shared` hands out a second reader over the
//! playback download's temp file (`source::SharedReader`, its own handle and
//! cursor, blocking on the download's progress like the decoder does), so a
//! server track is never downloaded twice. A local file is just opened
//! again. A streamed track playback does not hold right now is not scanned
//! at all (honest "no waveform", plain bar) rather than fetched a second
//! time. The scan runs unpaced on its own background-priority thread and
//! keeps nothing but the bucket array: never a whole decoded track in RAM
//! (one packet at a time, a few KB), and it never touches the playback ring,
//! the render thread or the gapless logic.
//!
//! Started lazily, only when the UI asks for the waveform of a track
//! ([`WaveformStore::get`], i.e. the Now Playing view is open), so it costs
//! nothing otherwise. Entries are keyed by track id (a gapless next track
//! simply asks for its own id), kept in a tiny LRU, and a scan for a track
//! nobody is asking about any more is cancelled when another starts.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use super::source::TrackSource;

/// Buckets per waveform (the wire size: one byte each).
pub const BUCKETS: usize = 240;
/// Finished/partial waveforms kept in memory (240 bytes each).
const CACHE_ENTRIES: usize = 8;
/// A scan that ends with at least this fraction of the expected frames is
/// reported as complete (headers' frame counts are not always exact).
const COMPLETE_FRACTION: f64 = 0.97;
/// Publishing cadence from the scan thread to readers.
const PUBLISH_EVERY: Duration = Duration::from_millis(120);

/// Folds interleaved decoded samples into RMS buckets. Pure; the scan
/// thread and the tests both drive it.
pub struct WaveformAccumulator {
    total_frames: u64,
    frames_seen: u64,
    buckets: Vec<u8>,
    /// Bucket currently being filled, and the sum of squares / sample count
    /// seen in it (all channels).
    cur: usize,
    cur_sum_sq: f64,
    cur_samples: u64,
    /// First frame index that belongs to bucket `cur + 1`.
    next_boundary: u64,
    /// Buckets `0..filled` are final.
    filled: usize,
}

impl WaveformAccumulator {
    pub fn new(total_frames: u64, buckets: usize) -> Self {
        let total_frames = total_frames.max(1);
        let mut a = Self {
            total_frames,
            frames_seen: 0,
            buckets: vec![0; buckets.max(1)],
            cur: 0,
            cur_sum_sq: 0.0,
            cur_samples: 0,
            next_boundary: 0,
            filled: 0,
        };
        a.next_boundary = a.boundary(1);
        a
    }

    /// First frame of bucket `i`.
    fn boundary(&self, i: usize) -> u64 {
        (self.total_frames as u128 * i as u128 / self.buckets.len() as u128) as u64
    }

    fn close_current(&mut self) {
        let rms = if self.cur_samples == 0 { 0.0 } else { (self.cur_sum_sq / self.cur_samples as f64).sqrt() };
        self.buckets[self.cur] = ((rms * std::f64::consts::SQRT_2).min(1.0) * 255.0).round() as u8;
        self.cur_sum_sq = 0.0;
        self.cur_samples = 0;
        self.cur += 1;
        self.filled = self.cur;
        self.next_boundary = if self.cur < self.buckets.len() { self.boundary(self.cur + 1) } else { u64::MAX };
    }

    /// Adds interleaved samples (`channels` per frame). Frames past the
    /// expected total (a header that under-reported) are folded into the
    /// last bucket.
    pub fn push(&mut self, samples: &[f32], channels: usize) {
        let channels = channels.max(1);
        for frame in samples.chunks_exact(channels) {
            while self.frames_seen >= self.next_boundary && self.cur + 1 < self.buckets.len() {
                self.close_current();
            }
            for s in frame {
                self.cur_sum_sq += f64::from(*s) * f64::from(*s);
            }
            self.cur_samples += channels as u64;
            self.frames_seen += 1;
        }
    }

    /// End of stream: closes the bucket in progress. Returns `true` when
    /// (almost) every expected frame was seen, in which case all buckets
    /// are final; otherwise only the buckets actually reached are.
    pub fn finish(&mut self) -> bool {
        if self.frames_seen == 0 {
            return false;
        }
        let complete = self.frames_seen as f64 >= self.total_frames as f64 * COMPLETE_FRACTION;
        if complete {
            while self.cur < self.buckets.len() {
                self.close_current();
            }
        } else if self.cur < self.buckets.len() {
            self.close_current();
        }
        complete
    }

    pub fn buckets(&self) -> &[u8] {
        &self.buckets
    }

    pub fn filled(&self) -> usize {
        self.filled
    }
}

/// What `player_get_waveform` returns for one track.
#[derive(Debug, Clone, PartialEq)]
pub struct WaveformSnapshot {
    pub track_id: String,
    /// Always [`BUCKETS`] long; entries at and past `filled` are 0 and mean
    /// "not decoded (yet)", not silence.
    pub buckets: Vec<u8>,
    pub filled: usize,
    /// The scan ended (finished, failed or was cut off): polling can stop.
    pub done: bool,
}

struct EntryData {
    buckets: Vec<u8>,
    filled: usize,
    done: bool,
}

struct Entry {
    data: Mutex<EntryData>,
    cancel: Arc<AtomicBool>,
}

impl Entry {
    fn new(cancel: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            data: Mutex::new(EntryData { buckets: vec![0; BUCKETS], filled: 0, done: false }),
            cancel,
        })
    }

    fn publish(&self, acc: &WaveformAccumulator, done: bool) {
        let mut d = self.data.lock().unwrap_or_else(|p| p.into_inner());
        d.buckets.copy_from_slice(acc.buckets());
        // Only final buckets are reported; the one in progress is not.
        let filled = acc.filled();
        for b in d.buckets[filled..].iter_mut() {
            *b = 0;
        }
        d.filled = filled;
        d.done = done;
    }

    fn finish_empty(&self) {
        self.data.lock().unwrap_or_else(|p| p.into_inner()).done = true;
    }

    fn snapshot(&self, track_id: &str) -> WaveformSnapshot {
        let d = self.data.lock().unwrap_or_else(|p| p.into_inner());
        WaveformSnapshot { track_id: track_id.to_string(), buckets: d.buckets.clone(), filled: d.filled, done: d.done }
    }

    /// Finished, but short of the end (playback moved on before the bytes
    /// arrived, a seek left a gap, a decode error): worth one more try the
    /// next time somebody asks.
    fn is_partial(&self) -> bool {
        let d = self.data.lock().unwrap_or_else(|p| p.into_inner());
        d.done && d.filled < BUCKETS
    }
}

pub struct WaveformStore {
    source: Arc<dyn TrackSource>,
    /// Most recently used last.
    entries: Mutex<Vec<(String, Arc<Entry>)>>,
}

impl WaveformStore {
    pub fn new(source: Arc<dyn TrackSource>) -> Self {
        Self { source, entries: Mutex::new(Vec::new()) }
    }

    /// The waveform of `track_id` as far as it is known, starting the scan
    /// on first request. `duration_hint_sec` (library metadata) is only used
    /// when the container does not state its frame count.
    ///
    /// The scan reads the bytes playback is already fetching
    /// ([`TrackSource::open_shared`]); a streamed track that playback does
    /// not hold right now is *not* downloaded a second time: the answer is
    /// an empty, `done` waveform (the UI draws a plain bar) that is not
    /// cached, so asking again once the track is playing works.
    pub fn get(&self, track_id: &str, duration_hint_sec: f64) -> WaveformSnapshot {
        // Held throughout (opening a shared reader is a registry lookup plus
        // a file open), so two concurrent requests never start two scans.
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let mut stale: Option<(String, Arc<Entry>)> = None;
        if let Some(pos) = entries.iter().position(|(id, _)| id == track_id) {
            let item = entries.remove(pos);
            if !item.1.is_partial() {
                let snap = item.1.snapshot(track_id);
                entries.push(item);
                return snap;
            }
            stale = Some(item);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let media = match self.source.open_shared(track_id, cancel.clone()) {
            Ok(m) => m,
            Err(e) => {
                log::debug!("waveform: no shared source for {track_id}: {e}");
                // Keep what an earlier attempt got rather than nothing.
                return match stale {
                    Some(item) => {
                        let snap = item.1.snapshot(track_id);
                        entries.push(item);
                        snap
                    }
                    None => WaveformSnapshot { track_id: track_id.to_string(), buckets: vec![0; BUCKETS], filled: 0, done: true },
                };
            }
        };
        // One scan at a time that anybody still wants: the previous track's
        // unfinished scan is abandoned.
        for (_, e) in entries.iter() {
            e.cancel.store(true, Ordering::Relaxed);
        }
        let e = Entry::new(cancel);
        entries.push((track_id.to_string(), e.clone()));
        while entries.len() > CACHE_ENTRIES {
            entries.remove(0);
        }
        spawn_scan(media, track_id.to_string(), duration_hint_sec, e.clone());
        e.snapshot(track_id)
    }

    #[cfg(test)]
    fn cached_ids(&self) -> Vec<String> {
        self.entries.lock().unwrap().iter().map(|(id, _)| id.clone()).collect()
    }
}

fn spawn_scan(media: Box<dyn MediaSource>, id: String, hint: f64, entry: Arc<Entry>) {
    let spawned = std::thread::Builder::new().name("sonix-waveform".into()).spawn({
        let entry = entry.clone();
        move || {
            lower_thread_priority();
            scan(media, &id, hint, &entry);
            entry.finish_empty();
        }
    });
    if let Err(e) = spawned {
        log::warn!("waveform thread not started: {e}");
        entry.finish_empty();
    }
}

/// The scan is background work: it must never compete with the UI, the
/// decoder or the render thread for CPU or disk.
fn lower_thread_priority() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
        // SAFETY: plain Win32 call on the current thread's pseudo-handle.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
    }
}

/// Decodes `media` start to end into `entry`. Any failure just leaves the
/// buckets reached so far (the UI shows an honest partial/empty bar).
fn scan(media: Box<dyn MediaSource>, id: &str, duration_hint_sec: f64, entry: &Entry) {
    let mss = MediaSourceStream::new(media, MediaSourceStreamOptions::default());
    let fmt_opts = FormatOptions { enable_gapless: true, ..Default::default() };
    let probed = match symphonia::default::get_probe().format(&Hint::new(), mss, &fmt_opts, &MetadataOptions::default()) {
        Ok(p) => p,
        Err(e) => {
            log::debug!("waveform: probe {id}: {e}");
            return;
        }
    };
    let mut format = probed.format;
    let Some(track) = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .cloned()
    else {
        return;
    };
    let rate = track.codec_params.sample_rate.unwrap_or(44_100);
    let total_frames = match track.codec_params.n_frames {
        Some(n) if n > 0 => n,
        _ if duration_hint_sec > 0.0 => (duration_hint_sec * rate as f64) as u64,
        _ => return, // cannot place frames in buckets without a length
    };
    let Ok(mut decoder) = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()) else {
        return;
    };

    let mut acc = WaveformAccumulator::new(total_frames, BUCKETS);
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut last_publish = Instant::now();
    loop {
        if entry.cancel.load(Ordering::Relaxed) {
            return;
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(_) => break,
        };
        if packet.track_id() != track.id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(_) => break,
        };
        let channels = decoded.spec().channels.count();
        let buf = match &mut sample_buf {
            Some(b) if b.capacity() >= decoded.capacity() * channels => b,
            slot => slot.insert(SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec())),
        };
        buf.copy_interleaved_ref(decoded);
        acc.push(buf.samples(), channels);
        if last_publish.elapsed() >= PUBLISH_EVERY {
            last_publish = Instant::now();
            entry.publish(&acc, false);
        }
    }
    acc.finish();
    entry.publish(&acc, true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::source::LocalFileSource;

    #[test]
    fn buckets_take_the_rms_of_their_frame_range() {
        // 8 mono frames into 4 buckets of 2 frames each.
        let mut a = WaveformAccumulator::new(8, 4);
        a.push(&[0.1, -0.5, 0.2, 0.2, -1.0, 0.0, 0.0, 0.25], 1);
        assert!(a.finish());
        // rms * sqrt(2): [0.1,-0.5] -> 0.51, [0.2,0.2] -> 0.283, [-1,0] -> 1.0, [0,0.25] -> 0.25
        assert_eq!(a.buckets(), &[130, 72, 255, 64]);
        assert_eq!(a.filled(), 4);
    }

    #[test]
    fn channels_are_folded_into_one_rms_per_frame_group() {
        let mut a = WaveformAccumulator::new(2, 2);
        // frame 0: L 0.2 R -0.8, frame 1: L 0.4 R 0.1
        a.push(&[0.2, -0.8, 0.4, 0.1], 2);
        a.finish();
        assert_eq!(a.buckets(), &[210, 105]);
    }

    #[test]
    fn a_brickwalled_master_still_has_shape() {
        // Both halves peak at full scale; a peak waveform drew them identical.
        let mut a = WaveformAccumulator::new(200, 2);
        let quiet: Vec<f32> = (0..100).map(|i| if i % 10 == 0 { 1.0 } else { 0.05 }).collect();
        let loud: Vec<f32> = (0..100).map(|i| if i % 2 == 0 { 1.0 } else { -0.7 }).collect();
        a.push(&quiet, 1);
        a.push(&loud, 1);
        assert!(a.finish());
        let b = a.buckets();
        assert!(b[0] < 150 && b[1] == 255, "{b:?}");
    }

    #[test]
    fn partial_decode_reports_only_the_buckets_reached() {
        let mut a = WaveformAccumulator::new(1000, 10);
        a.push(&vec![0.5; 350], 1); // 3.5 buckets in
        // Buckets 0..3 are final; the one in progress is not yet.
        assert_eq!(a.filled(), 3);
        assert_eq!(&a.buckets()[3..], &[0; 7]);
        // The stream ends early (a truncated file): not complete.
        assert!(!a.finish());
        assert_eq!(a.filled(), 4);
        assert_eq!(a.buckets()[3], 180); // constant 0.5 -> rms*sqrt(2) = 0.707
        assert_eq!(a.buckets()[4], 0);
    }

    #[test]
    fn slightly_short_streams_count_as_complete_and_long_ones_stay_in_the_last_bucket() {
        let mut short = WaveformAccumulator::new(1000, 4);
        short.push(&vec![0.25; 985], 1);
        assert!(short.finish());
        assert_eq!(short.filled(), 4);

        let mut long = WaveformAccumulator::new(100, 4);
        long.push(&vec![0.1; 90], 1);
        long.push(&[0.6; 40], 1); // header said 100 frames, 130 arrive
        assert!(long.finish());
        // Last bucket = frames 75..130: 15 x 0.1 and 40 x 0.6.
        assert!((185..=186).contains(&long.buckets()[3]), "{}", long.buckets()[3]);
        assert_eq!(long.filled(), 4);
    }

    /// 16-bit mono WAV, `frames` long, amplitude ramping 0 -> full scale.
    fn write_ramp_wav(path: &std::path::Path, frames: u32) {
        use std::io::Write;
        let data_len = frames * 2;
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
        f.write_all(b"WAVEfmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&8000u32.to_le_bytes()).unwrap();
        f.write_all(&16000u32.to_le_bytes()).unwrap();
        f.write_all(&2u16.to_le_bytes()).unwrap();
        f.write_all(&16u16.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_len.to_le_bytes()).unwrap();
        for i in 0..frames {
            let v = (i as f32 / frames as f32 * 32_000.0) as i16;
            f.write_all(&v.to_le_bytes()).unwrap();
        }
    }

    fn wait_done(store: &WaveformStore, id: &str) -> WaveformSnapshot {
        for _ in 0..200 {
            let s = store.get(id, 0.0);
            if s.done {
                return s;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("scan never finished");
    }

    #[test]
    fn scan_fills_a_ramp_and_entries_are_keyed_by_track_id() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.wav");
        let b = dir.path().join("b.wav");
        write_ramp_wav(&a, 24_000);
        write_ramp_wav(&b, 8_000);
        let store = WaveformStore::new(Arc::new(LocalFileSource));
        let a_id = a.to_str().unwrap();
        let b_id = b.to_str().unwrap();

        let wa = wait_done(&store, a_id);
        assert_eq!(wa.track_id, a_id);
        assert_eq!(wa.filled, BUCKETS);
        assert_eq!(wa.buckets.len(), BUCKETS);
        assert!(wa.buckets.windows(2).all(|w| w[0] <= w[1]), "a ramp never gets quieter");
        assert!(wa.buckets[0] < 5 && wa.buckets[BUCKETS - 1] > 240);

        // A second (gapless-next) track is its own entry; the first stays cached.
        let wb = wait_done(&store, b_id);
        assert_eq!(wb.track_id, b_id);
        assert_eq!(wb.filled, BUCKETS);
        assert_eq!(store.get(a_id, 0.0).buckets, wa.buckets);
    }

    /// A streamed track is scanned from playback's own download (still in
    /// progress when the scan starts) and costs no second request; a track
    /// playback does not hold is not fetched at all.
    #[test]
    fn streamed_track_is_scanned_from_the_playback_download_without_a_second_request() {
        use crate::audio::source::HttpTrackSource;
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::atomic::AtomicUsize;

        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("s.wav");
        write_ramp_wav(&wav, 24_000);
        let body = std::fs::read(&wav).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(AtomicUsize::new(0));
        std::thread::spawn({
            let requests = requests.clone();
            move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { return };
                    requests.fetch_add(1, Ordering::SeqCst);
                    let body = body.clone();
                    std::thread::spawn(move || {
                        let mut buf = [0u8; 4096];
                        let _ = stream.read(&mut buf);
                        let _ = stream
                            .write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes());
                        for piece in body.chunks(4096) {
                            let _ = stream.write_all(piece);
                            let _ = stream.flush();
                            std::thread::sleep(Duration::from_millis(10));
                        }
                    });
                }
            }
        });

        let source: Arc<dyn TrackSource> = Arc::new(HttpTrackSource::new(move |id: &str| {
            url::Url::parse(&format!("http://127.0.0.1:{port}/{id}")).unwrap()
        }));
        let store = WaveformStore::new(source.clone());

        // Not playing: no waveform, no request, nothing cached.
        let none = store.get("s", 0.0);
        assert!(none.done && none.filled == 0);
        assert!(store.cached_ids().is_empty());

        let playback = source.open("s").unwrap(); // what the engine does
        let first = store.get("s", 0.0);
        assert!(!first.done, "the scan follows the download while it is still arriving");
        let w = wait_done(&store, "s");
        assert_eq!(w.filled, BUCKETS);
        assert!(w.buckets.windows(2).all(|p| p[0] <= p[1]));
        assert!(w.buckets[BUCKETS - 1] > 240);
        assert_eq!(requests.load(Ordering::SeqCst), 1, "one download for playback and waveform together");

        // Another track playback does not hold: not downloaded for the waveform.
        let other = store.get("other", 0.0);
        assert!(other.done && other.filled == 0);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(store.cached_ids(), vec!["s".to_string()]);
        drop(playback);
        // The finished waveform outlives the download.
        assert_eq!(store.get("s", 0.0).buckets, w.buckets);
    }

    #[test]
    fn unreadable_track_ends_empty_instead_of_hanging() {
        let store = WaveformStore::new(Arc::new(LocalFileSource));
        let s = wait_done(&store, "Z:/definitely/not/here.flac");
        assert_eq!(s.filled, 0);
        assert!(s.done);
    }
}
