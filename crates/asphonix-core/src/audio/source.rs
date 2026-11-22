//! Track sources: where the compressed bytes for a track id come from, and
//! how they get onto disk.
//!
//! This module defines its own minimal [`TrackSource`] trait so the engine
//! does not depend on a concrete client. [`HttpTrackSource`] is a generic
//! HTTP implementation that takes a URL-builder closure returning [`Url`],
//! e.g. `HttpTrackSource::new(move |id| client.stream_url(id))` with a
//! (cheaply cloned) `subsonic::Client` captured by the closure.
//!
//! Download is progressive and disk-backed (never buffered whole in RAM):
//! [`ProgressiveDownload`] streams the HTTP response straight to a temp
//! file while symphonia reads from that same file through
//! [`ProgressiveMediaSource`], which blocks short reads until more bytes
//! land on disk. A read far past the download head (see
//! `ProgressiveDownload::wait_for`) re-issues the request with an HTTP
//! `Range: bytes=<offset>-` header (Navidrome supports Range on the raw
//! stream endpoint); nearer reads wait for the running request.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use symphonia::core::io::MediaSource;
use url::Url;

/// Minimal abstraction over "give me the bytes for this track id". The real
/// Subsonic client only needs to implement this (or be wrapped by
/// [`HttpTrackSource`]'s URL-builder closure).
pub trait TrackSource: Send + Sync + 'static {
    /// Begin streaming `track_id`'s compressed audio to a local temp file,
    /// returning a seekable [`MediaSource`] that can be handed to symphonia
    /// immediately (it blocks reads until enough bytes have arrived).
    fn open(&self, track_id: &str) -> io::Result<Box<dyn MediaSource>>;

    /// A second, independent reader over bytes that are *already* being
    /// fetched for playback (Now Playing's waveform scan), so analysis never
    /// costs a second download. Local files are simply opened again. For a
    /// streamed track this only succeeds while playback still holds that
    /// track's [`ProgressiveDownload`] (the current or the gapless-next
    /// track); otherwise `ErrorKind::NotFound`, and the caller does without.
    /// `cancel` makes a reader blocked on download progress give up.
    fn open_shared(&self, track_id: &str, cancel: Arc<AtomicBool>) -> io::Result<Box<dyn MediaSource>> {
        let _ = (track_id, cancel);
        Err(io::Error::new(io::ErrorKind::Unsupported, "no shared reader for this source"))
    }

    /// A fresh playback reader (from byte 0) over the download playback
    /// already holds for `track_id`, so reopening the output mid-track
    /// decodes the same bytes again instead of fetching them again. `None`
    /// when there is nothing to reuse (not streamed, no live download, or it
    /// failed): the caller then falls back to [`Self::open`].
    fn reattach(&self, track_id: &str) -> Option<Box<dyn MediaSource>> {
        let _ = track_id;
        None
    }
}

/// The downloads playback currently holds, by track id, so
/// [`TrackSource::open_shared`] can hand out a second reader over the same
/// temp file. Weak references only: an entry dies with its download.
#[derive(Default)]
pub struct DownloadRegistry {
    entries: Mutex<Vec<(String, Weak<SharedDownload>)>>,
}

impl DownloadRegistry {
    fn register(&self, track_id: &str, shared: &Arc<SharedDownload>) {
        let mut e = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        e.retain(|(id, w)| w.strong_count() > 0 && id != track_id);
        e.push((track_id.to_string(), Arc::downgrade(shared)));
    }

    /// A new playback reader over the newest live download of `track_id`.
    pub fn attach(&self, track_id: &str) -> Option<ProgressiveDownload> {
        let shared = {
            let e = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            e.iter().rev().find(|(id, _)| id == track_id).and_then(|(_, w)| w.upgrade())
        }?;
        ProgressiveDownload::attach(&shared)
    }

    /// A reader over the newest live download of `track_id`.
    pub fn reader(&self, track_id: &str, cancel: Arc<AtomicBool>) -> io::Result<SharedReader> {
        let shared = {
            let e = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            e.iter().rev().find(|(id, _)| id == track_id).and_then(|(_, w)| w.upgrade())
        };
        let shared =
            shared.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "track is not being downloaded"))?;
        SharedReader::new(&shared, cancel)
    }
}

/// Generic HTTP [`TrackSource`]: builds the request URL for a track id via a
/// closure, so it has zero knowledge of the Subsonic API shape.
///
/// Holds an *async* `reqwest::Client` (not `reqwest::blocking::Client`):
/// `ProgressiveDownload`'s background download worker runs its own
/// single-thread tokio runtime so it can apply a true per-chunk read/idle
/// timeout with automatic Range-resume retry (see `spawn_download_worker`) —
/// a plain blocking client's `timeout()` would instead cap the *whole*
/// download's duration, which would kill long tracks on a slow connection.
pub struct HttpTrackSource<F> {
    url_builder: F,
    client: reqwest::Client,
    downloads: DownloadRegistry,
}

impl<F> HttpTrackSource<F>
where
    F: Fn(&str) -> Url + Send + Sync + 'static,
{
    pub fn new(url_builder: F) -> Self {
        Self {
            url_builder,
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
            downloads: DownloadRegistry::default(),
        }
    }
}

impl<F> TrackSource for HttpTrackSource<F>
where
    F: Fn(&str) -> Url + Send + Sync + 'static,
{
    fn open(&self, track_id: &str) -> io::Result<Box<dyn MediaSource>> {
        let url = (self.url_builder)(track_id).to_string();
        let download = ProgressiveDownload::start(self.client.clone(), url)?;
        self.downloads.register(track_id, &download.shared);
        Ok(Box::new(download))
    }

    fn open_shared(&self, track_id: &str, cancel: Arc<AtomicBool>) -> io::Result<Box<dyn MediaSource>> {
        Ok(Box::new(self.downloads.reader(track_id, cancel)?))
    }

    fn reattach(&self, track_id: &str) -> Option<Box<dyn MediaSource>> {
        self.downloads.attach(track_id).map(|d| Box::new(d) as Box<dyn MediaSource>)
    }
}

/// A `TrackSource` that treats the "track id" as a local filesystem path —
/// for local testing without a Subsonic server.
pub struct LocalFileSource;

impl TrackSource for LocalFileSource {
    fn open(&self, track_id: &str) -> io::Result<Box<dyn MediaSource>> {
        Ok(Box::new(File::open(track_id)?))
    }

    fn open_shared(&self, track_id: &str, _cancel: Arc<AtomicBool>) -> io::Result<Box<dyn MediaSource>> {
        self.open(track_id) // a local file costs nothing to open twice
    }
}

/// The app's `TrackSource`: streams from whichever `subsonic::Client`
/// is currently logged in, read live out of a lock shared with the backend
/// (same `Arc`, set by `auth_connect`, cleared by `auth_logout`). This is the
/// one place `audio/` depends on `crate::subsonic`: the engine must track
/// login/logout/account-switch without restarting.
///
/// `open()` reads the lock once (a cheap `Option<Client>::clone`, never held
/// across the HTTP request) to build the fully-authenticated stream URL, then
/// starts a [`ProgressiveDownload`] exactly like [`HttpTrackSource`]. If
/// nobody is logged in, it fails fast with `ErrorKind::NotConnected` instead
/// of issuing a request — the backend also stops the engine synchronously on
/// logout, so this path is a defence in depth, not the primary mechanism.
pub struct SubsonicTrackSource {
    client: std::sync::Arc<std::sync::RwLock<Option<crate::subsonic::Client>>>,
    http: reqwest::Client,
    downloads: DownloadRegistry,
}

impl SubsonicTrackSource {
    pub fn new(client: std::sync::Arc<std::sync::RwLock<Option<crate::subsonic::Client>>>) -> Self {
        Self {
            client,
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                // The read/idle timeout that matters for a stalled stream is
                // enforced per-chunk by `ProgressiveDownload`'s download
                // worker (`spawn_download_worker`), not here: a blanket
                // `reqwest` timeout would also cap very long/slow full-album
                // downloads that are otherwise still trickling in fine.
                .build()
                .expect("reqwest client"),
            downloads: DownloadRegistry::default(),
        }
    }
}

impl TrackSource for SubsonicTrackSource {
    fn open(&self, track_id: &str) -> io::Result<Box<dyn MediaSource>> {
        let client = self
            .client
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let client = client.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "not logged in to a Subsonic server")
        })?;
        let url = crate::subsonic::TrackSource::stream_url(&client, track_id).to_string();
        let download = ProgressiveDownload::start(self.http.clone(), url)?;
        self.downloads.register(track_id, &download.shared);
        Ok(Box::new(download))
    }

    fn open_shared(&self, track_id: &str, cancel: Arc<AtomicBool>) -> io::Result<Box<dyn MediaSource>> {
        Ok(Box::new(self.downloads.reader(track_id, cancel)?))
    }

    fn reattach(&self, track_id: &str) -> Option<Box<dyn MediaSource>> {
        self.downloads.attach(track_id).map(|d| Box::new(d) as Box<dyn MediaSource>)
    }
}

/// Shared state between the background download thread(s) and the reader.
struct DownloadState {
    /// Bytes downloaded so far, contiguous from `base_offset` (the offset the
    /// *current* download generation started at). We only ever track one
    /// contiguous range at a time: a seek past it starts a new generation
    /// from the new offset and abandons tracking of the old region (the
    /// bytes already on disk there are harmless leftovers, not reread).
    base_offset: u64,
    downloaded_to: u64,
    total_len: Option<u64>,
    finished: bool,
    error: Option<String>,
    /// Bumped every time a seek restarts the download at a new offset; a
    /// stale worker thread checks this and exits instead of writing further.
    generation: u64,
    /// `0..valid_prefix` is known to be on disk from an earlier generation
    /// that started at offset 0 (a seek far ahead restarts the download but
    /// does not invalidate what was already written). Only the shared
    /// analysis reader uses it; playback keeps its single-range logic.
    valid_prefix: u64,
    /// The owning [`ProgressiveDownload`] is gone: no more bytes will come,
    /// so a [`SharedReader`] waiting for them sees EOF instead of hanging.
    abandoned: bool,
    /// When the current generation's request was issued, for the throughput
    /// estimate behind the "wait instead of restart" decision.
    gen_started: Instant,
    /// Last measured throughput (bytes/s) of an earlier generation, so a
    /// fresh one has an estimate before its own sample is meaningful.
    rate: Option<f64>,
}

impl DownloadState {
    /// How far past the download head a read may land and still be worth
    /// waiting for (the head is expected to get there within
    /// `policy.wait_secs`) rather than abandoning the request for a new
    /// `Range` one. Without a throughput sample yet: `policy.min_gap`.
    fn restart_gap(&self, policy: &SeekPolicy) -> u64 {
        match self.throughput() {
            Some(rate) => ((rate * policy.wait_secs) as u64).clamp(policy.min_gap, MAX_RESTART_GAP),
            None => policy.min_gap,
        }
    }

    /// Bytes/s of the current generation once it has a meaningful sample,
    /// else the last one measured before it.
    fn throughput(&self) -> Option<f64> {
        let secs = self.gen_started.elapsed().as_secs_f64();
        let got = self.downloaded_to.saturating_sub(self.base_offset);
        if secs >= 0.25 && got >= 64 * 1024 {
            Some(got as f64 / secs)
        } else {
            self.rate
        }
    }

    /// End of the on-disk prefix (`0..prefix_end`) when it lies wholly before
    /// `offset`, else 0: a new request for `offset` never needs to start
    /// earlier, and starting exactly there joins it to the prefix. (The
    /// current window can't end within `back_slack` before a restarting read:
    /// such a read is within `restart_gap` and waits instead.)
    fn readable_before(&self, offset: u64) -> u64 {
        let prefix_end = if self.base_offset <= self.valid_prefix {
            self.valid_prefix.max(self.downloaded_to)
        } else {
            self.valid_prefix
        };
        if prefix_end <= offset {
            prefix_end
        } else {
            0
        }
    }

    /// End of the contiguous on-disk range that contains `offset` (equal to
    /// `offset` when the bytes there are still to come), or `None` when
    /// `offset` falls into a gap nobody is downloading.
    fn readable_until(&self, offset: u64) -> Option<u64> {
        let prefix_end = if self.base_offset <= self.valid_prefix {
            self.valid_prefix.max(self.downloaded_to)
        } else {
            self.valid_prefix
        };
        if offset < prefix_end {
            Some(prefix_end)
        } else if offset >= self.base_offset {
            Some(self.downloaded_to.max(offset))
        } else {
            None
        }
    }
}

/// The temp file's path; the file is deleted when the last holder (the
/// playback reader or a [`SharedReader`]) drops it. Every handle to it is
/// opened with std's default Windows share mode (read | write | delete), and
/// `tempfile` creates it the same way, so a second reader never conflicts
/// with the writer or with the delete.
struct TempPath(PathBuf);

impl Drop for TempPath {
    fn drop(&mut self) {
        // Best effort (not fatal if it fails: the stale sweep reaps it).
        let _ = std::fs::remove_file(&self.0);
    }
}

/// What a download shares with analysis readers: the progress state +
/// condvar (also used by the worker threads) and the temp file's lifetime.
pub struct SharedDownload {
    state: Arc<Mutex<DownloadState>>,
    cond: Arc<Condvar>,
    temp: Arc<TempPath>,
    generation: Arc<AtomicU64>,
    client: reqwest::Client,
    url: String,
    idle_timeout: Duration,
    policy: SeekPolicy,
    /// Epoch of the playback reader allowed to move the download (issue
    /// `Range` requests): the newest one (see `ProgressiveDownload::attach`).
    steer: AtomicU64,
}

impl Drop for SharedDownload {
    /// The last playback reader is gone: stop the worker and wake any
    /// analysis reader (it then sees EOF). The temp file itself goes with the
    /// last `Arc<TempPath>`.
    fn drop(&mut self) {
        {
            let mut g = self.state.lock().unwrap_or_else(|p| p.into_inner());
            g.generation += 1;
            g.abandoned = true;
            self.cond.notify_all();
        }
        self.generation.store(u64::MAX, Ordering::SeqCst);
    }
}

/// When a read beyond the download head is worth a new `Range` request.
#[derive(Clone, Copy)]
struct SeekPolicy {
    /// Reads at most this far past the head just wait for it.
    min_gap: u64,
    /// ... or as far as the head is expected to travel in this many seconds
    /// at the measured throughput (capped by `MAX_RESTART_GAP`).
    wait_secs: f64,
}

impl SeekPolicy {
    /// How far before a restarting read the new `Range` request starts (see
    /// `ProgressiveDownload::wait_for`). Half a seek-table interval is what a
    /// FLAC seek steps back after its first probe; encoders put a point every
    /// ~10 s, i.e. ~0.5 MB back at 16/44.1, a few MB at hi-res rates.
    ///
    /// The read waits for those slack bytes first, so the slack is what the
    /// link delivers in `SLACK_SECS` (a FLAC without a seek table bisects the
    /// whole file and restarts several times; each must stay cheap on a slow
    /// link), within 256 KB..`min_gap / 2` (2 MB). Always below `min_gap`, so
    /// the read that caused the restart then waits for the new request
    /// instead of restarting it again.
    fn back_slack(&self, rate: Option<f64>) -> u64 {
        const SLACK_SECS: f64 = 0.25;
        let max = self.min_gap / 2;
        let min = max.min(256 * 1024);
        rate.map_or(min, |r| ((r * SLACK_SECS) as u64).clamp(min, max))
    }
}

impl Default for SeekPolicy {
    fn default() -> Self {
        Self { min_gap: 4 * 1024 * 1024, wait_secs: 3.0 }
    }
}

const MAX_RESTART_GAP: u64 = 512 * 1024 * 1024;

/// A track's compressed bytes, streamed progressively to a disk temp file.
/// Implements `Read + Seek` (and symphonia's [`MediaSource`]) by blocking
/// reads until the requested bytes have landed on disk.
pub struct ProgressiveDownload {
    file: File,
    path: PathBuf,
    pos: u64,
    state: Arc<Mutex<DownloadState>>,
    cond: Arc<Condvar>,
    /// Owns the download (worker, temp file); handed (weakly) to the
    /// [`DownloadRegistry`] for analysis readers. Several playback readers
    /// can hold it one after the other (see [`Self::attach`]).
    shared: Arc<SharedDownload>,
    /// This reader's epoch; only the reader whose epoch is `shared.steer`
    /// may restart the download.
    epoch: u64,
}

impl ProgressiveDownload {
    pub fn start(client: reqwest::Client, url: String) -> io::Result<Self> {
        Self::start_with_idle_timeout(client, url, READ_IDLE_TIMEOUT)
    }

    /// Same as [`Self::start`] with a configurable idle timeout — the
    /// production entry point above always uses `READ_IDLE_TIMEOUT`; tests
    /// use a much shorter one so a deliberately-stalled connection doesn't
    /// make `cargo test -- --ignored` take 15+ seconds per test.
    fn start_with_idle_timeout(client: reqwest::Client, url: String, idle_timeout: Duration) -> io::Result<Self> {
        Self::start_inner(client, url, idle_timeout, SeekPolicy::default())
    }

    fn start_inner(client: reqwest::Client, url: String, idle_timeout: Duration, policy: SeekPolicy) -> io::Result<Self> {
        sweep_stale_temp_files();
        let tmp = tempfile::Builder::new()
            .prefix("asphonix-stream-")
            .suffix(".tmp")
            .tempfile()
            .map_err(|e| temp_file_error(&e))?;
        // `.keep()` disarms the temp file's own delete-on-drop guard and
        // hands us the path; we manage its lifetime ourselves (removed in
        // `Drop` below) so a second handle can be opened by path from the
        // download worker thread.
        let (file, path) = tmp.keep().map_err(|e| temp_file_error(&e.error))?;

        let state = Arc::new(Mutex::new(DownloadState {
            base_offset: 0,
            downloaded_to: 0,
            total_len: None,
            finished: false,
            error: None,
            generation: 0,
            valid_prefix: 0,
            abandoned: false,
            gen_started: Instant::now(),
            rate: None,
        }));
        let cond = Arc::new(Condvar::new());
        let generation = Arc::new(AtomicU64::new(0));
        let shared = Arc::new(SharedDownload {
            state: state.clone(),
            cond: cond.clone(),
            temp: Arc::new(TempPath(path.clone())),
            generation: generation.clone(),
            client: client.clone(),
            url: url.clone(),
            idle_timeout,
            policy,
            steer: AtomicU64::new(0),
        });

        spawn_download_worker(
            client.clone(),
            url.clone(),
            0,
            path.clone(),
            state.clone(),
            cond.clone(),
            generation.clone(),
            0,
            idle_timeout,
        );

        Ok(Self { file, path, pos: 0, state, cond, shared, epoch: 0 })
    }

    /// A new playback reader over a download that is already running (or
    /// done), from position 0, without any request: used when the output is
    /// reopened mid-track so the same bytes are decoded again instead of
    /// being fetched again. `None` if the download failed (the caller then
    /// opens a fresh one).
    ///
    /// The new reader takes over steering: an older playback reader still
    /// alive (a torn-down decoder that is still finishing a read or a seek)
    /// can no longer restart the download, and reads EOF where it would have.
    /// Two readers steering one download towards different offsets would
    /// abandon each other's request forever.
    fn attach(shared: &Arc<SharedDownload>) -> Option<Self> {
        let g = shared.state.lock().unwrap_or_else(|p| p.into_inner());
        if g.error.is_some() {
            return None;
        }
        let file = File::open(&shared.temp.0).ok()?;
        let epoch = shared.steer.fetch_add(1, Ordering::SeqCst) + 1;
        shared.cond.notify_all(); // an older reader waiting re-checks and yields
        drop(g);
        Some(Self {
            file,
            path: shared.temp.0.clone(),
            pos: 0,
            state: shared.state.clone(),
            cond: shared.cond.clone(),
            shared: shared.clone(),
            epoch,
        })
    }

    /// Blocks until at least one byte at `offset` is available (or the
    /// download has finished / errored), returning how many of the
    /// requested `len` bytes can be read right now (0 means EOF).
    ///
    /// This is also where a new `Range` request is decided, lazily, on a
    /// read and never on a `seek`: a format reader's seek (symphonia's FLAC
    /// bisection, MP3/Ogg resync) hops around the file, and only reads need
    /// bytes. A read
    ///  - on bytes already on disk is served from disk (no request),
    ///  - a little past the head (within `restart_gap`: what the head will
    ///    cover in a few seconds at the measured throughput) just waits for
    ///    the running request,
    ///  - far past the head, or in a gap behind the window that was never
    ///    downloaded, abandons the running request for one new `Range` one.
    fn wait_for(&self, offset: u64, len: u64) -> io::Result<u64> {
        let mut guard = self.state.lock().unwrap();
        loop {
            if let Some(err) = &guard.error {
                return Err(io::Error::other(err.clone()));
            }
            if guard.total_len.is_some_and(|t| offset >= t) {
                return Ok(0); // at/after the end: EOF, never a request
            }
            let restart = match guard.readable_until(offset) {
                Some(end) if end > offset => return Ok((end - offset).min(len)),
                Some(_) => {
                    if guard.finished || guard.abandoned {
                        return Ok(0);
                    }
                    offset - guard.downloaded_to > guard.restart_gap(&self.shared.policy)
                }
                None => true, // a gap nobody is downloading
            };
            if restart {
                if self.shared.steer.load(Ordering::SeqCst) != self.epoch {
                    return Ok(0); // superseded reader (see `attach`): never steers
                }
                // Start a little *before* the read: a FLAC seek bisects
                // between two SEEKTABLE points and then scans from the lower
                // one, so after its first probe the next reads land just
                // behind it. Starting at the probe itself made every one of
                // those a gap read and a new request (5 extra on a seek
                // table FLAC). Never re-fetch what is already readable.
                let slack = self.shared.policy.back_slack(guard.throughput());
                let start = offset.saturating_sub(slack).max(guard.readable_before(offset));
                self.restart_at(&mut guard, start);
                continue;
            }
            guard = self.cond.wait_timeout(guard, Duration::from_millis(500)).unwrap().0;
        }
    }

    /// Abandons the running request and starts a new `Range` one at `offset`.
    fn restart_at(&self, g: &mut DownloadState, offset: u64) {
        if g.base_offset <= g.valid_prefix {
            g.valid_prefix = g.valid_prefix.max(g.downloaded_to);
        }
        g.rate = g.throughput();
        g.generation += 1;
        g.base_offset = offset;
        g.downloaded_to = offset;
        g.finished = false;
        g.error = None;
        g.gen_started = Instant::now();
        let gen = g.generation;
        self.shared.generation.store(gen, Ordering::SeqCst);
        spawn_download_worker(
            self.shared.client.clone(),
            self.shared.url.clone(),
            offset,
            self.path.clone(),
            self.state.clone(),
            self.cond.clone(),
            self.shared.generation.clone(),
            gen,
            self.shared.idle_timeout,
        );
    }
}

/// No bytes at all for this long on an open response body is treated as a
/// stalled connection (connect itself is bounded by the client's
/// `connect_timeout`, set where `HttpTrackSource`/`SubsonicTrackSource` build
/// their `reqwest::Client`).
const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(15);
/// How many times a stalled/dropped connection is retried (resuming with an
/// HTTP `Range` request from the last confirmed byte) before the download is
/// given up on and surfaced as an error to the decoder.
const MAX_RETRIES: u32 = 5;
const RETRY_BACKOFF_BASE: Duration = Duration::from_millis(500);
const RETRY_BACKOFF_MAX: Duration = Duration::from_secs(8);

/// Runs the download on its own thread with its own minimal (current-thread)
/// tokio runtime — kept separate from the app's main runtime so a stalled
/// track download can never starve it, and so this stays usable from
/// `examples/` and tests that have no runtime of their own running.
///
/// Uses the *async* `reqwest::Client` specifically so each chunk read can be
/// wrapped in `tokio::time::timeout` (`READ_IDLE_TIMEOUT`): a connection that
/// stops sending bytes (but doesn't close) is detected and, per the audio
/// reviewer's requirement, retried with `Range: bytes=<downloaded_to>-` to
/// resume rather than restarting the whole file or hanging the decoder
/// thread (which blocks on `wait_for` waiting for exactly this worker to
/// make progress). A `reqwest::blocking::Client`'s `timeout()` can't do this:
/// it caps the *entire* request's duration from the first byte, which would
/// also kill a long track on a merely slow (not stalled) connection.
#[allow(clippy::too_many_arguments)]
fn spawn_download_worker(
    client: reqwest::Client,
    url: String,
    start_offset: u64,
    path: PathBuf,
    state: Arc<Mutex<DownloadState>>,
    cond: Arc<Condvar>,
    generation: Arc<AtomicU64>,
    my_gen: u64,
    idle_timeout: Duration,
) {
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                set_error(&state, &generation, my_gen, &cond, format!("could not start download runtime: {e}"));
                return;
            }
        };
        rt.block_on(download_with_retries(client, url, start_offset, path, state, cond, generation, my_gen, idle_timeout));
    });
}

#[allow(clippy::too_many_arguments)]
async fn download_with_retries(
    client: reqwest::Client,
    url: String,
    start_offset: u64,
    path: PathBuf,
    state: Arc<Mutex<DownloadState>>,
    cond: Arc<Condvar>,
    generation: Arc<AtomicU64>,
    my_gen: u64,
    idle_timeout: Duration,
) {
    let mut offset = start_offset;
    let mut attempt = 0u32;
    loop {
        if generation.load(Ordering::SeqCst) != my_gen {
            return; // superseded by a seek
        }
        match download_once(&client, &url, offset, &path, &state, &cond, &generation, my_gen, idle_timeout).await {
            Ok(()) => return, // finished (or superseded, which also returns Ok)
            Err(stalled_at) => {
                attempt += 1;
                if attempt > MAX_RETRIES {
                    set_error(&state, &generation, my_gen, &cond, format!(
                        "stream stalled after {MAX_RETRIES} retries (last resumed at byte {stalled_at})"
                    ));
                    return;
                }
                offset = stalled_at;
                let backoff = (RETRY_BACKOFF_BASE * 2u32.pow(attempt - 1)).min(RETRY_BACKOFF_MAX);
                log::warn!(
                    "stream download stalled/dropped at byte {offset}; retrying with Range resume in {backoff:?} (attempt {attempt}/{MAX_RETRIES})"
                );
                tokio::time::sleep(backoff).await;
            }
        }
    }
}

/// A temp-file I/O error without its path: `tempfile` appends the file's
/// path (`at path "C:\Users\<name>\AppData\Local\Temp\..."`), and these messages reach
/// `PlayerState.error` (the dock, and through MCP, Claude). Keeps the error
/// kind and the OS error code, found anywhere in the source chain.
fn temp_file_error(e: &io::Error) -> io::Error {
    let mut code = e.raw_os_error();
    let mut src: Option<&(dyn std::error::Error + 'static)> = e.get_ref().map(|r| r as &(dyn std::error::Error + 'static));
    while code.is_none() {
        let Some(s) = src else { break };
        code = s.downcast_ref::<io::Error>().and_then(io::Error::raw_os_error);
        src = s.source();
    }
    if code.is_none() {
        // `tempfile`'s wrapper keeps its inner io::Error out of the source
        // chain but prints it: "... (os error 3) at path ...".
        let text = e.to_string();
        code = text.split("(os error ").nth(1).and_then(|rest| rest.split(')').next()).and_then(|n| n.trim().parse().ok());
    }
    let msg = match code {
        Some(c) => format!("stream temp file: {:?} (os error {c})", e.kind()),
        None => format!("stream temp file: {:?}", e.kind()),
    };
    io::Error::new(e.kind(), msg)
}

fn set_error(state: &Mutex<DownloadState>, generation: &AtomicU64, my_gen: u64, cond: &Condvar, message: String) {
    let mut g = state.lock().unwrap();
    if generation.load(Ordering::SeqCst) == my_gen {
        g.error = Some(message);
        cond.notify_all();
    }
}

/// One connection attempt starting at `offset`. Returns `Ok(())` once the
/// stream finishes (or is superseded by a seek); `Err(resume_from)` if the
/// connection drops or goes idle for `idle_timeout`, with the byte offset
/// the caller should resume from on retry.
#[allow(clippy::too_many_arguments)]
async fn download_once(
    client: &reqwest::Client,
    url: &str,
    offset: u64,
    path: &std::path::Path,
    state: &Mutex<DownloadState>,
    cond: &Condvar,
    generation: &AtomicU64,
    my_gen: u64,
    idle_timeout: Duration,
) -> Result<(), u64> {
    use futures_util::StreamExt;

    let mut req = client.get(url);
    if offset > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={offset}-"));
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            // `without_url`: the stream URL carries Subsonic auth params (u/t/s).
            log::warn!("stream connect failed at byte {offset}: {}", e.without_url());
            return Err(offset); // resumable: caller retries with backoff
        }
    };
    if !resp.status().is_success() {
        set_error(state, generation, my_gen, cond, format!("HTTP {}", resp.status()));
        return Ok(()); // a non-transient failure (e.g. 404): don't retry
    }
    let total = resp.content_length().map(|len| offset + len);
    {
        let mut g = state.lock().unwrap();
        if generation.load(Ordering::SeqCst) == my_gen && total.is_some() {
            g.total_len = total;
        }
    }

    // Open a private write handle at the right offset: a separate OS handle
    // from the reader's, so there's no cursor contention between the
    // download task (sequential appends) and the reader (random pread-style
    // access via explicit `seek` + `read`).
    let mut write_file = match OpenOptions::new().write(true).open(path) {
        Ok(f) => f,
        Err(e) => {
            // Not retryable; but say so, or a reader waiting for these bytes
            // would wait forever.
            set_error(state, generation, my_gen, cond, temp_file_error(&e).to_string());
            return Ok(());
        }
    };
    if let Err(e) = write_file.seek(SeekFrom::Start(offset)) {
        set_error(state, generation, my_gen, cond, temp_file_error(&e).to_string());
        return Ok(());
    }

    let mut stream = resp.bytes_stream();
    let mut written = offset;
    loop {
        if generation.load(Ordering::SeqCst) != my_gen {
            return Ok(()); // superseded by a seek
        }
        match tokio::time::timeout(idle_timeout, stream.next()).await {
            Ok(None) => {
                let mut g = state.lock().unwrap();
                if generation.load(Ordering::SeqCst) == my_gen {
                    g.finished = true;
                    cond.notify_all();
                }
                return Ok(());
            }
            Ok(Some(Ok(chunk))) => {
                use std::io::Write;
                if let Err(e) = write_file.write_all(&chunk) {
                    // Disk error (full?): don't retry, but surface it.
                    set_error(state, generation, my_gen, cond, temp_file_error(&e).to_string());
                    return Ok(());
                }
                written += chunk.len() as u64;
                let mut g = state.lock().unwrap();
                if generation.load(Ordering::SeqCst) != my_gen {
                    return Ok(());
                }
                g.downloaded_to = written;
                cond.notify_all();
            }
            Ok(Some(Err(_))) | Err(_) => {
                // Mid-stream error or no bytes for `READ_IDLE_TIMEOUT`:
                // resumable — let the caller retry with `Range` from here.
                return Err(written);
            }
        }
    }
}

impl Read for ProgressiveDownload {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let avail = self.wait_for(self.pos, buf.len() as u64)?;
        if avail == 0 {
            return Ok(0); // EOF
        }
        self.file.seek(SeekFrom::Start(self.pos))?;
        let n = self.file.read(&mut buf[..avail as usize])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for ProgressiveDownload {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(p) => p,
            SeekFrom::End(delta) => {
                let len = {
                    let g = self.state.lock().unwrap();
                    g.total_len.ok_or_else(|| {
                        io::Error::new(io::ErrorKind::Unsupported, "length unknown, cannot seek from end")
                    })?
                };
                (len as i64 + delta).max(0) as u64
            }
            SeekFrom::Current(delta) => (self.pos as i64 + delta).max(0) as u64,
        };
        // No request here: see `wait_for`.
        self.pos = target;
        Ok(self.pos)
    }
}

impl MediaSource for ProgressiveDownload {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        self.state.lock().unwrap().total_len
    }
}

/// A second, read-only reader over a [`ProgressiveDownload`]'s temp file
/// (its own OS handle and cursor), for analysis. Reads block until the
/// playback download has written the bytes and a seek only moves the cursor;
/// it reads EOF once the download is abandoned (playback moved on).
///
/// When a playback seek restarted the download further ahead, the bytes in
/// between (`[valid_prefix, base_offset)`) are a gap nobody downloads. The
/// reader then fetches exactly that missing range itself (one `Range`
/// request, into a private anonymous temp file, never the playback one),
/// so a track playback resumed mid-way (a restored session, an early seek)
/// still gets its whole waveform. The reader never loops on a gap: a failed
/// fetch is an I/O error, which ends the scan with what it has.
pub struct SharedReader {
    file: File,
    pos: u64,
    state: Arc<Mutex<DownloadState>>,
    cond: Arc<Condvar>,
    cancel: Arc<AtomicBool>,
    /// Keeps the temp file on disk while this reader needs it.
    _temp: Arc<TempPath>,
    client: reqwest::Client,
    url: String,
    gap: Option<GapFill>,
}

/// Where the next [`SharedReader::wait_for`] answer says to read from.
enum Avail {
    /// `n` bytes at the cursor are in the playback file (0 = EOF).
    Main(u64),
    /// The cursor is in a gap that ends at this offset.
    Gap(u64),
}

/// Progress of one private range fetch.
#[derive(Default)]
struct GapProgress {
    written: u64,
    done: bool,
}

/// `[start, end)` fetched into `file` (offset 0 of the file = byte `start`).
struct GapFill {
    start: u64,
    end: u64,
    file: File,
    progress: Arc<(Mutex<GapProgress>, Condvar)>,
    stop: Arc<AtomicBool>,
}

impl Drop for GapFill {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl GapFill {
    fn start(client: reqwest::Client, url: String, start: u64, end: u64) -> io::Result<Self> {
        // `asphonix-stream-*.tmp`: the stale sweep reaps it after a crash; no path in the error.
        let mut named =
            tempfile::Builder::new().prefix("asphonix-stream-gap-").suffix(".tmp").tempfile().map_err(|e| temp_file_error(&e))?;
        let file = File::open(named.path())?; // own cursor: the writer and the reader must not share one

        let progress: Arc<(Mutex<GapProgress>, Condvar)> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (p, s) = (progress.clone(), stop.clone());
        std::thread::Builder::new().name("asphonix-waveform-gap".into()).spawn(move || {
            use futures_util::StreamExt;
            use std::io::Write;
            let finish = |p: &(Mutex<GapProgress>, Condvar)| {
                let mut g = p.0.lock().unwrap_or_else(|e| e.into_inner());
                g.done = true;
                p.1.notify_all();
            };
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return finish(&p) };
            rt.block_on(async {
                let req = client.get(&url).header(reqwest::header::RANGE, format!("bytes={start}-{}", end - 1)).send();
                let resp = until_stopped(req, &s).await?.ok()?;

                // 206 = the range; a 200 would be the whole file from byte 0 (server ignores Range): not usable.
                if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                    return None;
                }
                let mut stream = resp.bytes_stream();
                while let Some(chunk) = until_stopped(stream.next(), &s).await? {
                    if s.load(Ordering::Relaxed) {
                        return None;
                    }
                    let chunk = chunk.ok()?;
                    named.write_all(&chunk).ok()?;
                    let mut g = p.0.lock().unwrap_or_else(|e| e.into_inner());
                    g.written += chunk.len() as u64;
                    p.1.notify_all();
                    if g.written >= end - start {
                        break;
                    }
                }
                Some(())
            });
            finish(&p);
        })?;
        Ok(Self { start, end, file, progress, stop })
    }

    /// Bytes available at `offset` (blocks until some arrive).
    fn read(&mut self, offset: u64, buf: &mut [u8], cancel: &AtomicBool) -> io::Result<usize> {
        let rel = offset - self.start;
        let want = (buf.len() as u64).min(self.end - offset);
        let mut g = self.progress.0.lock().unwrap_or_else(|e| e.into_inner());
        while g.written <= rel {
            if cancel.load(Ordering::Relaxed) {
                return Err(io::Error::other("analysis cancelled"));
            }
            if g.done {
                return Err(io::Error::other("could not fetch the missing part of the track"));
            }
            g = self.progress.1.wait_timeout(g, Duration::from_millis(200)).unwrap_or_else(|e| e.into_inner()).0;
        }
        let n = (g.written - rel).min(want) as usize;
        drop(g);
        self.file.seek(SeekFrom::Start(rel))?;
        self.file.read(&mut buf[..n])
    }
}

/// `fut`'s output, or `None` once `stop` is set (the scan is gone: checked
/// every 200 ms, so a cancelled fill drops its connection promptly) or after
/// [`READ_IDLE_TIMEOUT`] without an answer (also bounds the response headers,
/// which the client's `connect_timeout` does not).
async fn until_stopped<F: std::future::Future>(fut: F, stop: &AtomicBool) -> Option<F::Output> {
    let mut fut = std::pin::pin!(fut);
    let t0 = Instant::now();
    while !stop.load(Ordering::Relaxed) && t0.elapsed() < READ_IDLE_TIMEOUT {
        if let Ok(v) = tokio::time::timeout(Duration::from_millis(200), &mut fut).await {
            return Some(v);
        }
    }
    None
}

impl SharedReader {
    fn new(shared: &SharedDownload, cancel: Arc<AtomicBool>) -> io::Result<Self> {
        let file = File::open(&shared.temp.0)?;
        Ok(Self {
            file,
            pos: 0,
            state: shared.state.clone(),
            cond: shared.cond.clone(),
            cancel,
            _temp: shared.temp.clone(),
            client: shared.client.clone(),
            url: shared.url.clone(),
            gap: None,
        })
    }

    /// Bytes readable at `offset` right now, waiting for the download while
    /// they are still to come.
    fn wait_for(&self, offset: u64, len: u64) -> io::Result<Avail> {
        let mut g = self.state.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(io::Error::other("analysis cancelled"));
            }
            match g.readable_until(offset) {
                Some(end) if end > offset => return Ok(Avail::Main((end - offset).min(len))),
                Some(_) => {}
                // A gap: it ends where the current request starts. It is fetched only once playback's own
                // request has finished, so the extra request never competes with playback for bandwidth
                // (right after a resume/seek its buffer is thinnest). Playback gone, or its download failed:
                // an error, never `Ok(0)` (which symphonia's FLAC reader re-reads forever) and never a fetch.
                None if g.abandoned || g.error.is_some() => {
                    return Err(io::Error::other("the rest of the track is not being downloaded"));
                }
                None if g.finished => return Ok(Avail::Gap(g.base_offset)),
                None => {
                    g = self.cond.wait_timeout(g, Duration::from_millis(200)).unwrap_or_else(|p| p.into_inner()).0;
                    continue;
                }
            }
            if g.finished || g.abandoned {
                return Ok(Avail::Main(0));
            }
            if let Some(err) = &g.error {
                return Err(io::Error::other(err.clone()));
            }
            g = self.cond.wait_timeout(g, Duration::from_millis(200)).unwrap_or_else(|p| p.into_inner()).0;
        }
    }
}

impl Read for SharedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match self.wait_for(self.pos, buf.len() as u64)? {
            Avail::Main(0) => Ok(0),
            Avail::Main(avail) => {
                self.file.seek(SeekFrom::Start(self.pos))?;

                let n = self.file.read(&mut buf[..avail as usize])?;
                self.pos += n as u64;
                Ok(n)
            }
            Avail::Gap(end) => {
                if !self.gap.as_ref().is_some_and(|g| (g.start..g.end).contains(&self.pos)) {
                    self.gap = Some(GapFill::start(self.client.clone(), self.url.clone(), self.pos, end)?);
                }

                let n = self.gap.as_mut().expect("just set").read(self.pos, buf, &self.cancel)?;
                self.pos += n as u64;
                Ok(n)
            }
        }
    }
}

impl Seek for SharedReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.pos = match pos {
            SeekFrom::Start(p) => p,
            SeekFrom::Current(d) => (self.pos as i64 + d).max(0) as u64,
            SeekFrom::End(d) => {
                let len = self.state.lock().unwrap_or_else(|p| p.into_inner()).total_len.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::Unsupported, "length unknown, cannot seek from end")
                })?;
                (len as i64 + d).max(0) as u64
            }
        };
        Ok(self.pos)
    }
}

impl MediaSource for SharedReader {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).total_len
    }
}

/// Deletes `asphonix-stream-*.tmp` files left behind by an earlier process that
/// didn't get to run `Drop` (crash, kill, `panic = "abort"`), bounding disk
/// use across sessions. Runs once per process, before our first download.
/// Only files untouched for 6 h are removed, so a concurrently running
/// instance's in-use streams are never yanked from under it.
fn sweep_stale_temp_files() {
    use std::sync::Once;
    use std::time::SystemTime;
    const STALE_AFTER: Duration = Duration::from_secs(6 * 3600);
    static SWEPT: Once = Once::new();
    SWEPT.call_once(|| {
        let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !(name.starts_with("asphonix-stream-") && name.ends_with(".tmp")) {
                continue;
            }
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > STALE_AFTER);
            if stale {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    //! Verifies the stalled-connection -> Range-resume retry path
    //! (`download_with_retries`/`download_once`) end to end against a real
    //! TCP server that deliberately stalls mid-response, using a short
    //! `idle_timeout` (`start_with_idle_timeout`) so this runs in ~1s instead
    //! of waiting out the real `READ_IDLE_TIMEOUT` (15s).
    use super::*;
    use std::io::Write as _;
    use std::net::TcpListener;

    /// A tiny single-connection HTTP/1.1 server: reads (and discards) one
    /// request, writes `Content-Length: {body.len()}` + `body`, stopping
    /// after `stall_after` bytes and then doing nothing further (no more
    /// writes, no close) — simulating a connection that stopped sending
    /// without actually dropping.
    fn serve_once_stalling(listener: &TcpListener, body: &[u8], stall_after: usize) {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf); // discard the request line/headers
        let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
        stream.write_all(header.as_bytes()).unwrap();
        stream.write_all(&body[..stall_after]).unwrap();
        stream.flush().unwrap();
        // Deliberately never send the rest or close: the client must give up
        // waiting (idle timeout) rather than hang forever. Only needs to
        // outlast the test's short idle_timeout, not a realistic stall.
        std::thread::sleep(Duration::from_millis(800));
    }

    /// Serves exactly the bytes of `body` for a `Range: bytes=<offset>-`
    /// request (offset parsed out of the raw request text), closing cleanly
    /// afterwards.
    fn serve_once_range_resume(listener: &TcpListener, full_body: &[u8]) {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap();
        let request = String::from_utf8_lossy(&buf[..n]);
        let offset: usize = request
            .lines()
            .find_map(|l| l.to_ascii_lowercase().strip_prefix("range: bytes=").map(|r| r.trim_end_matches('-').to_string()))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let remaining = &full_body[offset..];
        let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", remaining.len());
        stream.write_all(header.as_bytes()).unwrap();
        stream.write_all(remaining).unwrap();
        stream.flush().unwrap();
    }

    /// Serves `body` (one connection) in `chunk`-byte pieces, `delay` apart,
    /// then sets `done`.
    fn serve_slowly(listener: TcpListener, body: Vec<u8>, chunk: usize, delay: Duration, done: Arc<AtomicBool>) {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
        stream.write_all(header.as_bytes()).unwrap();
        for piece in body.chunks(chunk) {
            stream.write_all(piece).unwrap();
            stream.flush().unwrap();
            std::thread::sleep(delay);
        }
        done.store(true, Ordering::SeqCst);
    }

    fn local_url(listener: &TcpListener) -> String {
        format!("http://127.0.0.1:{}/track", listener.local_addr().unwrap().port())
    }

    fn read_all(r: &mut impl Read) -> io::Result<Vec<u8>> {
        let mut got = Vec::new();
        let mut buf = [0u8; 3000];
        loop {
            match r.read(&mut buf)? {
                0 => return Ok(got),
                n => got.extend_from_slice(&buf[..n]),
            }
        }
    }

    #[test]
    fn shared_reader_follows_a_download_in_progress_and_keeps_the_file_until_it_is_done() {
        let body: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 251) as u8).collect();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = local_url(&listener);
        let served = Arc::new(AtomicBool::new(false));
        let server = std::thread::spawn({
            let body = body.clone();
            let served = served.clone();
            move || serve_slowly(listener, body, 4096, Duration::from_millis(15), served)
        });

        let registry = DownloadRegistry::default();
        let client = reqwest::Client::new();
        let playback = ProgressiveDownload::start(client, url).unwrap();
        let path = playback.path.clone();
        registry.register("t1", &playback.shared);

        let mut reader = registry.reader("t1", Arc::new(AtomicBool::new(false))).unwrap();
        let mut first = [0u8; 16];
        let n = reader.read(&mut first).unwrap();
        assert!(n > 0);
        assert!(!served.load(Ordering::SeqCst), "the first bytes arrive while the download is still running");
        let mut got = first[..n].to_vec();
        got.extend(read_all(&mut reader).unwrap());
        assert_eq!(got, body, "the second reader sees exactly the downloaded bytes");
        server.join().unwrap();

        // Playback lets go first: the reader keeps the file, the registry forgets it.
        drop(playback);
        assert!(path.exists(), "a live shared reader keeps the temp file");
        assert_eq!(registry.reader("t1", Arc::new(AtomicBool::new(false))).err().map(|e| e.kind()), Some(io::ErrorKind::NotFound));
        reader.seek(SeekFrom::Start(100)).unwrap();
        let mut again = [0u8; 4];
        reader.read_exact(&mut again).unwrap();
        assert_eq!(again, body[100..104]);
        drop(reader);
        assert!(!path.exists(), "the last holder deletes the temp file");
    }

    #[test]
    fn shared_reader_ends_instead_of_hanging_when_playback_abandons_or_it_is_cancelled() {
        let body: Vec<u8> = (0u8..40).collect();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = local_url(&listener);
        let server = std::thread::spawn(move || serve_once_stalling(&listener, &body, 10));

        let registry = DownloadRegistry::default();
        let playback = ProgressiveDownload::start(reqwest::Client::new(), url).unwrap();
        let path = playback.path.clone();
        registry.register("t", &playback.shared);
        let mut reader = registry.reader("t", Arc::new(AtomicBool::new(false))).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut cancelled = registry.reader("t", cancel.clone()).unwrap();

        let mut buf = [0u8; 64];
        let mut got = 0;
        while got < 10 {
            got += reader.read(&mut buf[got..]).unwrap();
        }
        // The stream stalls; playback moves on to another track.
        let t0 = std::time::Instant::now();
        let dropper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(playback);
        });
        assert_eq!(reader.read(&mut buf).unwrap(), 0, "abandoned download reads as EOF");
        assert!(t0.elapsed() < Duration::from_millis(700), "woken by the drop, not a timeout");
        dropper.join().unwrap();

        cancelled.seek(SeekFrom::Start(20)).unwrap(); // past the data: would wait
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(cancelled.read(&mut buf).unwrap_err().kind(), io::ErrorKind::Other, "not Interrupted: symphonia retries that");
        drop((reader, cancelled));
        assert!(!path.exists());
        server.join().unwrap();
    }

    #[test]
    fn readable_range_survives_a_restart_further_ahead() {
        let mut s = DownloadState {
            base_offset: 0,
            downloaded_to: 1000,
            total_len: Some(10_000),
            finished: false,
            error: None,
            generation: 0,
            valid_prefix: 0,
            abandoned: false,
            gen_started: Instant::now(),
            rate: None,
        };
        assert_eq!(s.readable_until(10), Some(1000));
        assert_eq!(s.readable_until(1000), Some(1000), "still to come");
        // A seek restarts the download at 5000 (what restart_at does).
        s.valid_prefix = s.downloaded_to;
        s.base_offset = 5000;
        s.downloaded_to = 5200;
        assert_eq!(s.readable_until(10), Some(1000), "the first range is still on disk");
        assert_eq!(s.readable_until(1500), None, "nobody downloads the gap");
        assert_eq!(s.readable_until(5100), Some(5200));
        // A later restart below the prefix end joins the two ranges.
        s.base_offset = 800;
        s.downloaded_to = 1200;
        assert_eq!(s.readable_until(10), Some(1200));
    }

    #[test]
    fn stalled_connection_resumes_with_range_and_completes() {
        let full_body: Vec<u8> = (0u8..40).collect();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/track");

        let server = std::thread::spawn(move || {
            serve_once_stalling(&listener, &full_body, 10); // stalls after 10/40 bytes
            serve_once_range_resume(&listener, &full_body); // resumes from byte 10
        });

        let client = reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).build().unwrap();
        let mut download =
            ProgressiveDownload::start_with_idle_timeout(client, url, Duration::from_millis(300)).unwrap();

        let mut got = Vec::new();
        let mut buf = [0u8; 64];
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            assert!(std::time::Instant::now() < deadline, "download did not complete in time");
            match download.read(&mut buf) {
                Ok(0) => break, // EOF: finished
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) => panic!("unexpected read error: {e}"),
            }
        }
        assert_eq!(got, (0u8..40).collect::<Vec<u8>>(), "resumed download must reconstruct the full file");
        server.join().unwrap();
    }

    /// A throttled multi-connection HTTP server with `Range: bytes=N-`
    /// support that records the start offset of every request it receives
    /// (`0` for a plain GET). Stops when dropped.
    struct RangeServer {
        url: String,
        requests: Arc<Mutex<Vec<u64>>>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl RangeServer {
        fn start(body: Vec<u8>, chunk: usize, delay: Duration) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = local_url(&listener);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let body = Arc::new(body);
            let thread = std::thread::spawn({
                let (requests, stop) = (requests.clone(), stop.clone());
                move || {
                    while !stop.load(Ordering::SeqCst) {
                        let Ok((mut stream, _)) = listener.accept() else {
                            std::thread::sleep(Duration::from_millis(2));
                            continue;
                        };
                        let _ = stream.set_nonblocking(false);
                        let (body, requests, stop) = (body.clone(), requests.clone(), stop.clone());
                        std::thread::spawn(move || {
                            let mut req = Vec::new();
                            let mut buf = [0u8; 1024];
                            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                                match stream.read(&mut buf) {
                                    Ok(0) | Err(_) => return,
                                    Ok(n) => req.extend_from_slice(&buf[..n]),
                                }
                            }
                            let text = String::from_utf8_lossy(&req).to_ascii_lowercase();
                            let range = text.lines().find_map(|l| l.strip_prefix("range: bytes=").map(str::to_string));
                            // `a-` (playback) or `a-b` (a bounded gap fetch, answered 206 like a real server).
                            let (start, last): (usize, Option<usize>) = match range.as_deref().and_then(|r| r.split_once('-')) {
                                Some((a, b)) => (a.trim().parse().unwrap_or(0), b.trim().parse().ok()),
                                None => (0, None),
                            };
                            requests.lock().unwrap().push(start as u64);
                            let rest = &body[start..last.map_or(body.len(), |l| (l + 1).min(body.len()))];
                            let status = if last.is_some() { "206 Partial Content" } else { "200 OK" };
                            let header = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n", rest.len());
                            if stream.write_all(header.as_bytes()).is_err() {
                                return;
                            }
                            for piece in rest.chunks(chunk) {
                                if stop.load(Ordering::SeqCst) || stream.write_all(piece).is_err() {
                                    return;
                                }
                                let _ = stream.flush();
                                std::thread::sleep(delay);
                            }
                        });
                    }
                }
            });
            Self { url, requests, stop, thread: Some(thread) }
        }

        fn requests(&self) -> Vec<u64> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for RangeServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * 7 + i / 251) % 256) as u8).collect()
    }

    fn read_at<T: Read + Seek>(d: &mut T, pos: u64, n: usize) -> Vec<u8> {
        d.seek(SeekFrom::Start(pos)).unwrap();
        let mut out = vec![0u8; n];
        d.read_exact(&mut out).unwrap();
        out
    }

    #[test]
    fn shared_reader_fills_the_gap_a_playback_seek_leaves_instead_of_spinning_or_hanging() {
        // Playback resumes mid-track (restored session / early seek): the download restarts far ahead and
        // `[prefix, restart)` is never fetched by it. The analysis reader must still see every byte.
        let body = pattern(3 * 1024 * 1024);
        let server = RangeServer::start(body.clone(), 16 * 1024, Duration::from_millis(5));
        let policy = SeekPolicy { min_gap: 256 * 1024, wait_secs: 1.0 };
        let mut d =
            ProgressiveDownload::start_inner(reqwest::Client::new(), server.url.clone(), READ_IDLE_TIMEOUT, policy).unwrap();
        assert_eq!(read_at(&mut d, 0, 16), body[..16]);
        let far = 2_621_440u64;
        assert_eq!(read_at(&mut d, far, 16), body[far as usize..far as usize + 16]);
        assert_eq!(server.requests().len(), 2, "playback restarted the download at the seek target");

        let registry = DownloadRegistry::default();
        registry.register("t", &d.shared);
        let mut reader = registry.reader("t", Arc::new(AtomicBool::new(false))).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut all = Vec::new();
            let r = read_all(&mut reader).map(|v| all = v);
            let _ = tx.send((r, all));
        });
        let (r, all) = rx.recv_timeout(Duration::from_secs(30)).expect("the analysis reader must not hang or spin on the gap");
        r.unwrap();
        assert!(all == body, "the whole file, gap included ({} of {} bytes)", all.len(), body.len());
        assert_eq!(server.requests().len(), 3, "exactly one extra request, for the gap only");
    }

    #[test]
    fn bisection_style_probes_near_the_head_cause_no_new_request() {
        // 2 MB served at ~6 MB/s. The decoder's seek (symphonia's FLAC
        // bisection) hops around the file with tiny reads; every probe is
        // within the default 4 MB gap of the head, so the one running request
        // simply catches up: still exactly the initial request.
        let body = pattern(2 * 1024 * 1024);
        let server = RangeServer::start(body.clone(), 64 * 1024, Duration::from_millis(10));
        let mut d = ProgressiveDownload::start(reqwest::Client::new(), server.url.clone()).unwrap();
        assert_eq!(read_at(&mut d, 0, 16), body[..16]);
        for pos in [1_048_576u64, 524_288, 786_432, 1_572_864, 1_310_720, 100] {
            assert_eq!(read_at(&mut d, pos, 16), body[pos as usize..pos as usize + 16], "probe at {pos}");
        }
        assert_eq!(server.requests(), vec![0], "one download, no per-probe Range request");
    }

    #[test]
    fn one_far_seek_issues_one_range_request_however_many_probes_follow() {
        // 3 MB at ~1.6 MB/s with a tight policy so a far target is out of
        // reach of the head (the slow-link case).
        let body = pattern(3 * 1024 * 1024);
        let server = RangeServer::start(body.clone(), 16 * 1024, Duration::from_millis(10));
        let policy = SeekPolicy { min_gap: 256 * 1024, wait_secs: 1.0 };
        let mut d =
            ProgressiveDownload::start_inner(reqwest::Client::new(), server.url.clone(), READ_IDLE_TIMEOUT, policy).unwrap();
        assert_eq!(read_at(&mut d, 0, 16), body[..16]);

        // Seeks alone (a reader looking up the length, hopping around) cost nothing.
        d.seek(SeekFrom::End(0)).unwrap();
        d.seek(SeekFrom::Start(1_000_000)).unwrap();
        assert_eq!(server.requests(), vec![0]);

        // The first read at the far target is the one new request, starting
        // `back_slack` (half of `min_gap`) before the target.
        let far = 2_621_440u64;
        assert_eq!(read_at(&mut d, far, 16), body[far as usize..far as usize + 16]);
        let start = far - policy.back_slack(None); // fixed 128 KB with this policy
        assert_eq!(server.requests(), vec![0, start]);

        // Probes around the target inside the new window just wait for it, and
        // a probe back into the already-downloaded prefix is read from disk.
        // So does one a little *behind* the target (a FLAC seek steps back).
        for pos in [far + 100_000, far - 50_000, far + 20_000, far + 300_000, 8] {
            assert_eq!(read_at(&mut d, pos, 16), body[pos as usize..pos as usize + 16], "probe at {pos}");
        }
        assert_eq!(server.requests(), vec![0, start], "one user seek = one new request");
    }

    #[test]
    fn a_seek_just_past_the_head_does_not_abandon_the_download() {
        let body = pattern(1024 * 1024);
        let server = RangeServer::start(body.clone(), 16 * 1024, Duration::from_millis(5));
        let mut d = ProgressiveDownload::start(reqwest::Client::new(), server.url.clone()).unwrap();
        assert_eq!(read_at(&mut d, 0, 16), body[..16]);
        let target = 600_000u64; // beyond the head right now, well within the gap
        assert_eq!(read_at(&mut d, target, 64), body[target as usize..target as usize + 64]);
        assert_eq!(server.requests(), vec![0]);
        // And the download still completes through to the end.
        let mut rest = Vec::new();
        d.seek(SeekFrom::Start(0)).unwrap();
        d.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, body);
        assert_eq!(server.requests(), vec![0]);
    }

    #[test]
    fn a_superseded_reader_never_steers_the_download() {
        // Device switch while the old decoder is still mid-seek: before, the
        // old and the new reader each restarted the download towards their
        // own offset, abandoning each other's request forever (hundreds of
        // Range requests, playback stuck). Now only the newest reader steers;
        // the old one reads EOF where it would have restarted.
        let body = pattern(4 * 1024 * 1024);
        let server = RangeServer::start(body.clone(), 16 * 1024, Duration::from_millis(10));
        let policy = SeekPolicy { min_gap: 256 * 1024, wait_secs: 0.2 };
        let registry = DownloadRegistry::default();
        let mut old =
            ProgressiveDownload::start_inner(reqwest::Client::new(), server.url.clone(), READ_IDLE_TIMEOUT, policy).unwrap();
        registry.register("t", &old.shared);
        assert_eq!(read_at(&mut old, 0, 16), body[..16]);
        let mut new = registry.attach("t").expect("live download");

        let far_old = 3_500_000u64;
        let far_new = 2_000_000u64;
        old.seek(SeekFrom::Start(far_old)).unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(old.read(&mut buf).unwrap(), 0, "superseded: EOF instead of a restart");
        assert_eq!(read_at(&mut new, far_new, 16), body[far_new as usize..far_new as usize + 16]);
        // Interleave both again: the old reader still never issues a request.
        old.seek(SeekFrom::Start(far_old)).unwrap();
        assert_eq!(old.read(&mut buf).unwrap(), 0);
        assert_eq!(read_at(&mut new, far_new + 1000, 16), body[far_new as usize + 1000..far_new as usize + 1016]);
        let reqs = server.requests();
        assert_eq!(reqs.len(), 2, "initial + the new reader's one seek, got {reqs:?}");
        assert!(reqs[1] <= far_new && reqs[1] >= far_new - 256 * 1024, "{reqs:?}");
        // Bytes already on disk stay readable for the old reader.
        assert_eq!(read_at(&mut old, 0, 16), body[..16]);
    }

    #[test]
    fn a_reattached_reader_reuses_the_running_download() {
        // Output device/mode change: the old decoder (reader) is dropped and a
        // new one starts from byte 0 over the same download: one request total.
        let body = pattern(512 * 1024);
        let server = RangeServer::start(body.clone(), 16 * 1024, Duration::from_millis(5));
        let registry = DownloadRegistry::default();
        let mut first = ProgressiveDownload::start(reqwest::Client::new(), server.url.clone()).unwrap();
        let path = first.path.clone();
        registry.register("t", &first.shared);
        assert_eq!(read_at(&mut first, 0, 32), body[..32]);

        let mut second = registry.attach("t").expect("live download");
        drop(first); // the old output/decoder goes away mid-download
        assert!(path.exists(), "the new reader keeps the temp file and the worker");
        let mut got = Vec::new();
        second.read_to_end(&mut got).unwrap();
        assert_eq!(got, body, "decoded again from the same bytes");
        assert_eq!(server.requests(), vec![0], "no second fetch of the track");
        drop(second);
        assert!(registry.attach("t").is_none(), "nothing left to attach to");
        assert!(!path.exists());
    }

    // --- A real FLAC seek (symphonia's own bisection) over a download ------

    const FLAC_BLOCK: usize = 4096;
    const FLAC_RATE: u32 = 44_100;

    fn flac_crc8(data: &[u8]) -> u8 {
        data.iter().fold(0u8, |mut crc, &b| {
            crc ^= b;
            for _ in 0..8 {
                crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
            }
            crc
        })
    }

    fn flac_crc16(data: &[u8]) -> u16 {
        data.iter().fold(0u16, |mut crc, &b| {
            crc ^= (b as u16) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
            }
            crc
        })
    }

    /// A spec-conformant 16-bit stereo FLAC of `frames` fixed 4096-sample
    /// VERBATIM frames (everything stays byte aligned, so no bit writer is
    /// needed), with a SEEKTABLE point every `seek_every` frames if given
    /// (what real encoders write, by default every ~10 s).
    fn flac_file(frames: usize, seek_every: Option<usize>) -> Vec<u8> {
        let mut audio = Vec::new();
        let mut offsets = Vec::new();
        for f in 0..frames {
            offsets.push(audio.len() as u64);
            let mut fr = vec![0xFF, 0xF8, 0x70, 0x18]; // fixed block, 16-bit size follows, stereo, 16 bit
            let n = f as u32; // UTF-8 coded frame number
            match n {
                0..=0x7F => fr.push(n as u8),
                0x80..=0x7FF => fr.extend([0xC0 | (n >> 6) as u8, 0x80 | (n & 0x3F) as u8]),
                _ => fr.extend([0xE0 | (n >> 12) as u8, 0x80 | ((n >> 6) & 0x3F) as u8, 0x80 | (n & 0x3F) as u8]),
            }
            fr.extend_from_slice(&((FLAC_BLOCK - 1) as u16).to_be_bytes());
            fr.push(flac_crc8(&fr));
            for ch in 0..2usize {
                fr.push(0b0000_0010); // VERBATIM subframe
                for i in 0..FLAC_BLOCK {
                    fr.extend_from_slice(&(((i * 31 + f * 7 + ch) % 20_000) as i16).to_be_bytes());
                }
            }
            let crc = flac_crc16(&fr);
            fr.extend_from_slice(&crc.to_be_bytes());
            audio.extend_from_slice(&fr);
        }
        let mut out = b"fLaC".to_vec();
        let points: Vec<usize> = seek_every.map(|k| (0..frames).step_by(k).collect()).unwrap_or_default();
        out.push(if points.is_empty() { 0x80 } else { 0x00 }); // STREAMINFO (last if no seektable)
        out.extend_from_slice(&[0, 0, 34]);
        out.extend_from_slice(&(FLAC_BLOCK as u16).to_be_bytes());
        out.extend_from_slice(&(FLAC_BLOCK as u16).to_be_bytes());
        out.extend_from_slice(&[0; 6]); // min/max frame size unknown
        let total = (frames * FLAC_BLOCK) as u64;
        let packed = ((FLAC_RATE as u64) << 44) | (1 << 41) | (15 << 36) | total;
        out.extend_from_slice(&packed.to_be_bytes());
        out.extend_from_slice(&[0; 16]); // MD5 unknown
        if !points.is_empty() {
            out.push(0x80 | 3); // SEEKTABLE, last
            out.extend_from_slice(&((points.len() * 18) as u32).to_be_bytes()[1..]);
            for f in points {
                out.extend_from_slice(&((f * FLAC_BLOCK) as u64).to_be_bytes());
                out.extend_from_slice(&offsets[f].to_be_bytes());
                out.extend_from_slice(&(FLAC_BLOCK as u16).to_be_bytes());
            }
        }
        out.extend_from_slice(&audio);
        out
    }

    /// Opens `url` as a progressive download, probes it with symphonia, does
    /// one seek to `frac` of the track while the download is far behind, and
    /// returns (ranges requested, timestamp of the packet read after the seek,
    /// the seek's target timestamp).
    fn flac_far_seek(body: Vec<u8>, frac: f64) -> (Vec<u64>, u64, u64) {
        use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        use symphonia::core::probe::Hint;
        use symphonia::core::units::Time;

        // ~1.6 MB/s and a tight policy: the 90 % target stays far ahead of
        // the head for the whole test, the slow-link / large-file case.
        let server = RangeServer::start(body, 16 * 1024, Duration::from_millis(10));
        let policy = SeekPolicy { min_gap: 256 * 1024, wait_secs: 0.2 };
        let d = ProgressiveDownload::start_inner(reqwest::Client::new(), server.url.clone(), READ_IDLE_TIMEOUT, policy).unwrap();
        let mss = MediaSourceStream::new(Box::new(d), Default::default());
        let mut hint = Hint::new();
        hint.with_extension("flac");
        let mut reader = symphonia::default::get_probe()
            .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
            .unwrap()
            .format;
        let n_frames = reader.default_track().unwrap().codec_params.n_frames.unwrap();
        let target = (n_frames as f64 * frac) as u64;
        let seconds = target as f64 / FLAC_RATE as f64;
        reader
            .seek(SeekMode::Accurate, SeekTo::Time { time: Time::from(seconds), track_id: None })
            .unwrap();
        let packet = reader.next_packet().unwrap();
        (server.requests(), packet.ts(), target)
    }

    #[test]
    fn flac_far_seek_with_a_seektable_costs_one_range_request() {
        // 900 frames (~14.7 MB); seek points every 10 frames like an encoder's
        // default. One far seek: bisection between two seek points, then a
        // linear scan from the lower one; all of it within the one new window.
        let (requests, ts, target) = flac_far_seek(flac_file(900, Some(10)), 0.9);
        assert!(ts <= target && target < ts + FLAC_BLOCK as u64, "landed on the right frame: {ts} for {target}");
        assert_eq!(requests.len(), 2, "initial request + one Range request, got {requests:?}");
    }

    #[test]
    fn flac_far_seek_without_a_seektable_stays_bounded() {
        // No SEEKTABLE: symphonia bisects the whole file. Each probe far from
        // the head may cost a request, but never one per bisection step plus
        // retries: bounded by ~log2(file / gap).
        let (requests, ts, target) = flac_far_seek(flac_file(900, None), 0.9);
        assert!(ts <= target && target < ts + FLAC_BLOCK as u64, "landed on the right frame: {ts} for {target}");
        eprintln!("no seektable: {} requests {requests:?}", requests.len());
        assert!(requests.len() <= 8, "got {requests:?}");
    }
}

#[cfg(test)]
mod temp_file_error_tests {
    use super::*;

    /// A real `tempfile` failure carries the directory in its message; the
    /// scrubbed one keeps the kind and the OS code but no path.
    #[test]
    fn temp_file_errors_keep_the_kind_and_code_but_not_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-subdir-zq7");
        let e = tempfile::Builder::new().prefix("asphonix-stream-").tempfile_in(&missing).unwrap_err();
        assert!(e.to_string().contains("no-such-subdir-zq7"), "precondition: tempfile puts the path in: {e}");
        let clean = temp_file_error(&e);
        let text = clean.to_string();
        assert!(!text.contains("no-such-subdir-zq7") && !text.contains(&*dir.path().to_string_lossy()), "{text}");
        assert_eq!(clean.kind(), e.kind());
        assert!(text.starts_with("stream temp file: NotFound"), "{text}");
        assert!(text.contains("(os error "), "the OS code is kept: {text}");
        // Plain OS errors keep their code too.
        let os = temp_file_error(&io::Error::from_raw_os_error(5));
        assert!(os.to_string().contains("(os error 5)"), "{os}");
    }
}
