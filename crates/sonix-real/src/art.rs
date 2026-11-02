//! Cover art: real covers (`getCoverArt` at the bucket size + disk cache) and playlist mosaics.
//!
//! The mosaic composition (`compose`, `cover_crop`, `gutter_px`, cover-set hash) is vendored from SoniX @ 642db10
//! `src-tauri/src/art/mosaic.rs`; `decode_limited` from `art/resize.rs`. The web app routes `/art/mosaic/{playlistId}`
//! to the mosaic; here `art_get("mosaic/{playlistId}", size)` does the same, and a bare playlist id (a row of the
//! local playlist mirror) is treated the same way. Everything is read-only (`getCoverArt` only).
use crate::{err, RealBackend};
use image::{imageops, DynamicImage, GenericImageView, Rgb, RgbImage};
use sonix_api::*;
use sonix_core::library;
use sonix_core::subsonic::Client;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// Hard gutter color between tiles (`--color-surface-base` / `#0A0A0C`).
const GUTTER: [u8; 3] = [0x0A, 0x0A, 0x0C];
const MAX_DECODE_DIM: u32 = 8192;
const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;
/// A playlist mosaic shows at most this many distinct covers.
const MOSAIC_TILES: usize = 4;
const MOSAIC_PREFIX: &str = "mosaic/";

/// Decodes server-supplied image bytes under dimension/allocation limits.
fn decode_limited(bytes: &[u8]) -> image::ImageResult<DynamicImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_DIM);
    limits.max_image_height = Some(MAX_DECODE_DIM);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    reader.decode()
}

fn gutter_px(size: u32) -> u32 {
    if size <= 64 {
        1
    } else {
        2
    }
}

fn hash_covers(covers: &[String]) -> u64 {
    let mut hasher = DefaultHasher::new();
    covers.hash(&mut hasher);
    hasher.finish()
}

/// Center-crops `img` to the `w:h` aspect ratio, then resizes to exactly `w x h` (`object-fit: cover`).
fn cover_crop(img: &DynamicImage, w: u32, h: u32) -> DynamicImage {
    let (iw, ih) = img.dimensions();
    let w = w.max(1);
    let h = h.max(1);
    if iw == 0 || ih == 0 {
        return img.resize_exact(w, h, imageops::FilterType::Triangle);
    }
    let target_ratio = f64::from(w) / f64::from(h);
    let src_ratio = f64::from(iw) / f64::from(ih);
    let (cw, ch) = if src_ratio > target_ratio {
        (((f64::from(ih) * target_ratio).round() as u32).clamp(1, iw), ih)
    } else {
        (iw, ((f64::from(iw) / target_ratio).round() as u32).clamp(1, ih))
    };
    let x = (iw - cw) / 2;
    let y = (ih - ch) / 2;
    img.crop_imm(x, y, cw, ch).resize_exact(w, h, imageops::FilterType::Triangle)
}

/// Lays out 1-4 distinct covers into a `size x size` canvas with hard `GUTTER` seams between panes:
/// 1 full-bleed, 2 left/right halves, 3 one large pane plus two stacked, 4 a 2x2 grid.
fn compose(tiles: &[DynamicImage], size: u32) -> DynamicImage {
    let g = gutter_px(size);
    let mut canvas = RgbImage::from_pixel(size, size, Rgb(GUTTER));
    match tiles.len() {
        1 => {
            imageops::overlay(&mut canvas, &cover_crop(&tiles[0], size, size).to_rgb8(), 0, 0);
        }
        2 => {
            let left_w = (size - g) / 2;
            let right_w = size - g - left_w;
            imageops::overlay(&mut canvas, &cover_crop(&tiles[0], left_w, size).to_rgb8(), 0, 0);
            imageops::overlay(&mut canvas, &cover_crop(&tiles[1], right_w, size).to_rgb8(), i64::from(left_w + g), 0);
        }
        3 => {
            let left_w = size * 6 / 10;
            let right_w = size - g - left_w;
            let top_h = (size - g) / 2;
            let bottom_h = size - g - top_h;
            imageops::overlay(&mut canvas, &cover_crop(&tiles[0], left_w, size).to_rgb8(), 0, 0);
            imageops::overlay(&mut canvas, &cover_crop(&tiles[1], right_w, top_h).to_rgb8(), i64::from(left_w + g), 0);
            imageops::overlay(&mut canvas, &cover_crop(&tiles[2], right_w, bottom_h).to_rgb8(), i64::from(left_w + g), i64::from(top_h + g));
        }
        _ => {
            let left_w = (size - g) / 2;
            let right_w = size - g - left_w;
            let top_h = (size - g) / 2;
            let bottom_h = size - g - top_h;
            imageops::overlay(&mut canvas, &cover_crop(&tiles[0], left_w, top_h).to_rgb8(), 0, 0);
            imageops::overlay(&mut canvas, &cover_crop(&tiles[1], right_w, top_h).to_rgb8(), i64::from(left_w + g), 0);
            imageops::overlay(&mut canvas, &cover_crop(&tiles[2], left_w, bottom_h).to_rgb8(), 0, i64::from(top_h + g));
            imageops::overlay(&mut canvas, &cover_crop(&tiles[3], right_w, bottom_h).to_rgb8(), i64::from(left_w + g), i64::from(top_h + g));
        }
    }
    DynamicImage::ImageRgb8(canvas)
}

/// Disk cap for cached artwork (`art/mod.rs` `MAX_CACHE_BYTES` / `EVICT_TARGET_BYTES`): past it the least recently
/// read files go first (mtime is bumped on every cache hit), which also removes orphaned mosaics of old cover sets.
const MAX_CACHE_BYTES: u64 = 512 * 1024 * 1024;
const EVICT_TARGET_BYTES: u64 = MAX_CACHE_BYTES * 8 / 10;

/// Approximate bytes in the cache dir; `u64::MAX` = not scanned yet (one scan before the first write).
static CACHE_BYTES: AtomicU64 = AtomicU64::new(u64::MAX);
static EVICTING: AtomicBool = AtomicBool::new(false);
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// The art dir was emptied: rescan before the next eviction decision.
pub(crate) fn reset_cache_bytes() {
    CACHE_BYTES.store(u64::MAX, Ordering::Relaxed);
}

/// `<dir>/<key>_<size>.img`; `dir` = [`RealBackend::art_dir`].
pub(crate) fn cache_path(dir: &Path, id: &str, size: u32) -> PathBuf {
    let key: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    dir.join(format!("{key}_{size}.img"))
}

fn list_files(dir: &Path) -> Vec<(PathBuf, std::time::SystemTime, u64)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            if !md.is_file() || e.file_name().to_string_lossy().contains(".tmp") {
                return None;
            }
            Some((e.path(), md.modified().unwrap_or(std::time::UNIX_EPOCH), md.len()))
        })
        .collect()
}

/// Deletes least-recently-accessed first until at most `target` bytes remain; returns the remaining total.
fn evict_oldest(dir: &Path, target: u64) -> u64 {
    let mut files = list_files(dir);
    let mut total: u64 = files.iter().map(|f| f.2).sum();
    files.sort_by_key(|f| f.1);
    for (path, _, len) in files {
        if total <= target {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
    total
}

fn account_write(dir: &Path, len: u64) {
    account_write_with(dir, len, MAX_CACHE_BYTES, EVICT_TARGET_BYTES, &CACHE_BYTES);
}

fn account_write_with(dir: &Path, len: u64, max: u64, target: u64, counter: &AtomicU64) {
    if counter.load(Ordering::Relaxed) == u64::MAX {
        let total: u64 = list_files(dir).iter().map(|f| f.2).sum();
        let _ = counter.compare_exchange(u64::MAX, total, Ordering::Relaxed, Ordering::Relaxed);
        return; // the scan already includes the file just written
    }
    let total = counter.fetch_add(len, Ordering::Relaxed) + len;
    if total > max && !EVICTING.swap(true, Ordering::AcqRel) {
        counter.store(evict_oldest(dir, target), Ordering::Relaxed);
        EVICTING.store(false, Ordering::Release);
    }
}

/// Decoded cache hit; a file that does not decode (torn write, junk) is deleted so it cannot stick.
fn read_cached(path: &Path) -> Option<DynamicImage> {
    let bytes = std::fs::read(path).ok().filter(|b| !b.is_empty())?;
    match decode_limited(&bytes) {
        Ok(img) => {
            // access recency for eviction
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
                let _ = f.set_modified(std::time::SystemTime::now());
            }
            Some(img)
        }
        Err(_) => {
            let _ = std::fs::remove_file(path);
            None
        }
    }
}

/// Raw bytes of a cache file (recency bumped); no decode.
fn read_bytes_touch(path: &Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok().filter(|b| !b.is_empty())?;
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
    Some(bytes)
}

/// Encoded bytes of a cache hit (SMTC thumbnail), bumping its recency like [`read_cached`]; junk is deleted.
pub(crate) fn read_cached_bytes(path: &Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok().filter(|b| !b.is_empty())?;
    if !decodes(&bytes) {
        let _ = std::fs::remove_file(path);
        return None;
    }
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
    Some(bytes)
}

/// True when `bytes` is an image the cache accepts.
pub(crate) fn decodes(bytes: &[u8]) -> bool {
    decode_limited(bytes).is_ok()
}

/// Atomic write through a unique temp name (concurrent writers of one key never share a temp file).
pub(crate) fn store(path: &Path, bytes: &[u8]) {
    let Some(dir) = path.parent() else { return };
    let _ = std::fs::create_dir_all(dir);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("x");
    let tmp = dir.join(format!("{name}.{}.{}.tmp", std::process::id(), TMP_SEQ.fetch_add(1, Ordering::Relaxed)));
    if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, path).is_ok() {
        account_write(dir, bytes.len() as u64);
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn not_found(msg: String) -> ApiError {
    ApiError::new(ErrorCode::NotFound, msg)
}

/// The only size ever requested from the server (`art/sizes.rs` `MASTER_SIZE`): every smaller bucket is derived from
/// these bytes locally, so one cover costs one network round trip no matter how many sizes the UI asks for.
pub(crate) const MASTER_SIZE: u32 = 640;
/// Concurrent foreground `getCoverArt` fetches (`art/mod.rs` `MAX_CONCURRENT_FETCHES`).
const MAX_CONCURRENT_FETCHES: usize = 4;
/// Background prefetch budget, separate so it never queues a foreground request (`prefetch.rs` `PREFETCH_CONCURRENCY`).
const PREFETCH_CONCURRENCY: usize = 2;
/// Decode + resize + encode at once (bounds the transient working set: a 640 master is 1.6 MB of RGBA).
const MAX_CONCURRENT_DECODES: usize = 2;
const PREFETCH_START_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
/// Poll interval while the prefetcher is paused (`prefetch.rs` `PAUSE_POLL_INTERVAL`).
const PREFETCH_PAUSE_POLL: std::time::Duration = if cfg!(test) { std::time::Duration::from_millis(20) } else { std::time::Duration::from_millis(500) };
/// This many network failures in a row end a prefetch pass (server unreachable): it is retried after the next sync
/// instead of timing out once per remaining id.
const PREFETCH_MAX_NET_ERRORS: usize = 5;
/// JPEG quality of a derived bucket (`art/resize.rs` `DERIVED_JPEG_QUALITY`).
const DERIVED_JPEG_QUALITY: u8 = 85;

/// Counting semaphore for the blocking `art_get` callers.
struct Gate {
    free: Mutex<usize>,
    cv: Condvar,
    /// Highest number of permits held at once (tests assert the limits).
    peak: AtomicUsize,
    cap: usize,
}

struct Permit<'a>(&'a Gate);

impl Gate {
    fn new(n: usize) -> Self {
        Self { free: Mutex::new(n), cv: Condvar::new(), peak: AtomicUsize::new(0), cap: n }
    }
    fn acquire(&self) -> Permit<'_> {
        let mut free = self.free.lock().unwrap_or_else(|p| p.into_inner());
        while *free == 0 {
            free = self.cv.wait(free).unwrap_or_else(|p| p.into_inner());
        }
        *free -= 1;
        self.peak.fetch_max(self.cap - *free, Ordering::Relaxed);
        Permit(self)
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *self.0.free.lock().unwrap_or_else(|p| p.into_inner()) += 1;
        self.0.cv.notify_one();
    }
}

enum Flight<V> {
    Running,
    Done(V),
    /// The leader panicked: waiters retry (one of them becomes the new leader).
    Abandoned,
}

struct Slot<V> {
    state: Mutex<Flight<V>>,
    cv: Condvar,
}

/// Collapses concurrent identical work into one execution (`art/singleflight.rs`): the first caller (leader) runs the
/// closure, everyone arriving while it runs waits for and clones its result. Keyed by cache file path.
struct SingleFlight<V> {
    inflight: Mutex<HashMap<PathBuf, Arc<Slot<V>>>>,
}

impl<V: Clone> SingleFlight<V> {
    fn new() -> Self {
        Self { inflight: Mutex::new(HashMap::new()) }
    }

    fn run(&self, key: &Path, work: impl FnOnce() -> V) -> V {
        let mut work = Some(work);
        loop {
            let (slot, leader) = {
                let mut map = self.inflight.lock().unwrap_or_else(|p| p.into_inner());
                match map.get(key) {
                    Some(s) => (s.clone(), false),
                    None => {
                        let s = Arc::new(Slot { state: Mutex::new(Flight::Running), cv: Condvar::new() });
                        map.insert(key.to_path_buf(), s.clone());
                        (s, true)
                    }
                }
            };
            if leader {
                // On every exit (also a panic in `work`): drop the entry first, so retrying waiters find an empty map and
                // one of them leads, then mark an unfinished slot abandoned and wake everyone.
                struct Finish<'a, V> {
                    flight: &'a SingleFlight<V>,
                    key: &'a Path,
                    slot: Arc<Slot<V>>,
                }
                impl<V> Drop for Finish<'_, V> {
                    fn drop(&mut self) {
                        let mut map = self.flight.inflight.lock().unwrap_or_else(|p| p.into_inner());
                        if map.get(self.key).is_some_and(|s| Arc::ptr_eq(s, &self.slot)) {
                            map.remove(self.key);
                        }
                        drop(map);
                        let mut st = self.slot.state.lock().unwrap_or_else(|p| p.into_inner());
                        if matches!(*st, Flight::Running) {
                            *st = Flight::Abandoned;
                        }
                        drop(st);
                        self.slot.cv.notify_all();
                    }
                }
                let _finish = Finish { flight: self, key, slot: slot.clone() };
                let v = (work.take().expect("leader runs once"))();
                *slot.state.lock().unwrap_or_else(|p| p.into_inner()) = Flight::Done(v.clone());
                return v;
            }
            let mut g = slot.state.lock().unwrap_or_else(|p| p.into_inner());
            loop {
                match &*g {
                    Flight::Done(v) => return v.clone(),
                    Flight::Abandoned => break,
                    Flight::Running => g = slot.cv.wait(g).unwrap_or_else(|p| p.into_inner()),
                }
            }
        }
    }
}

type ImageFlight = SingleFlight<ApiResult<Arc<DynamicImage>>>;
type BytesFlight = SingleFlight<ApiResult<Arc<Vec<u8>>>>;

/// All cover-fetch state of one backend (per instance, so nothing leaks between backends or tests).
pub(crate) struct ArtState {
    /// Foreground `getCoverArt` fetches (the UI's `art_get`).
    fetch_gate: Gate,
    /// Prefetch fetches; separate so a prefetch never makes a foreground request wait.
    bg_gate: Gate,
    decode_gate: Gate,
    image_flight: ImageFlight,
    master_flight: BytesFlight,
    /// `art_get` calls currently past the disk cache; the prefetcher pauses while nonzero.
    foreground: AtomicUsize,
    /// The window is minimized: the prefetcher pauses (no network, CPU or heap).
    pub(crate) suspended: AtomicBool,
    /// Epoch of the prefetch pass that is running (a pass of another epoch may start; the old one stops itself).
    prefetch_run: Mutex<Option<u64>>,
    /// Ids whose master the decoder cannot read (server-resized buckets are requested instead).
    undecodable: Mutex<HashSet<String>>,
    /// Ids the server has no cover for (not asked again by later prefetch passes this session).
    absent: Mutex<HashSet<String>>,
    /// Prefetch fetch attempts (tests).
    bg_attempts: AtomicUsize,
}

impl Default for ArtState {
    fn default() -> Self {
        Self {
            fetch_gate: Gate::new(MAX_CONCURRENT_FETCHES),
            bg_gate: Gate::new(PREFETCH_CONCURRENCY),
            decode_gate: Gate::new(MAX_CONCURRENT_DECODES),
            image_flight: SingleFlight::new(),
            master_flight: SingleFlight::new(),
            foreground: AtomicUsize::new(0),
            suspended: AtomicBool::new(false),
            prefetch_run: Mutex::new(None),
            undecodable: Mutex::new(HashSet::new()),
            absent: Mutex::new(HashSet::new()),
            bg_attempts: AtomicUsize::new(0),
        }
    }
}

/// Downscale `img` to fit `target x target` keeping the aspect ratio; never upscales (`art/resize.rs`).
fn downscale(img: &DynamicImage, target: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w <= target && h <= target {
        return img.clone();
    }
    let filter = if target <= 64 { imageops::FilterType::Triangle } else { imageops::FilterType::CatmullRom };
    let scale = f64::from(target) / f64::from(w.max(h));
    let nw = ((f64::from(w) * scale).round() as u32).max(1);
    let nh = ((f64::from(h) * scale).round() as u32).max(1);
    img.resize_exact(nw, nh, filter)
}

fn encode_jpeg(img: &DynamicImage) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, DERIVED_JPEG_QUALITY);
    img.to_rgb8().write_with_encoder(enc).ok()?;
    Some(out)
}

/// Content sniff (`art/mod.rs` `sniff_image_type`): jpeg, png, webp, gif, avif. Only real image payloads are cached.
fn looks_like_image(b: &[u8]) -> bool {
    b.starts_with(&[0xFF, 0xD8, 0xFF])
        || b.starts_with(&[0x89, b'P', b'N', b'G'])
        || b.starts_with(b"GIF8")
        || (b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP")
        || (b.len() > 12 && &b[4..8] == b"ftyp" && (&b[8..12] == b"avif" || &b[8..12] == b"avis"))
}

/// Counts `art_get` calls that are past the disk cache (what the prefetcher yields to).
struct ForegroundGuard<'a>(&'a AtomicUsize);

impl<'a> ForegroundGuard<'a> {
    fn new(c: &'a AtomicUsize) -> Self {
        c.fetch_add(1, Ordering::AcqRel);
        Self(c)
    }
}

impl Drop for ForegroundGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl RealBackend {
    fn current_client(&self) -> Option<Client> {
        self.0.client.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// `getCoverArt?size=` of one id through `gate` (blocking); the body must look like an image.
    fn fetch_from_server(&self, client: &Client, cover_art: &str, size: u32, gate: &Gate) -> ApiResult<Vec<u8>> {
        let _permit = gate.acquire();
        let (b, _) = self.0.rt.block_on(client.get_cover_art(cover_art, Some(size))).map_err(err)?;
        if !looks_like_image(&b) {
            return Err(not_found(format!("cover art {cover_art}: server returned a non-image payload")));
        }
        Ok(b)
    }

    /// Bytes of the cached master (`<id>_640.img`), fetching it once (single-flight, gated) on a miss.
    fn master_bytes(&self, client: Option<&Client>, cover_art: &str, gate: &Gate) -> ApiResult<Arc<Vec<u8>>> {
        let path = cache_path(&self.art_dir(), cover_art, MASTER_SIZE);
        let cached = |path: &Path| match read_bytes_touch(path) {
            Some(b) if looks_like_image(&b) => Some(Arc::new(b)),
            Some(_) => {
                let _ = std::fs::remove_file(path); // torn / junk file
                None
            }
            None => None,
        };
        if let Some(b) = cached(&path) {
            return Ok(b);
        }
        self.0.art.master_flight.run(&path, || {
            // a flight that finished just before this one started has filled the disk cache
            if let Some(b) = cached(&path) {
                return Ok(b);
            }
            let client = client.ok_or_else(|| ApiError::new(ErrorCode::NotConnected, "not connected"))?;
            let b = self.fetch_from_server(client, cover_art, MASTER_SIZE, gate)?;
            store(&path, &b);
            Ok(Arc::new(b))
        })
    }

    /// Decodes `bytes` (under the decode gate) and fits it to `size`; `None` when the decoder cannot read it.
    fn derive(&self, bytes: &[u8], size: u32, path: &Path) -> Option<Arc<DynamicImage>> {
        let _d = self.0.art.decode_gate.acquire();
        let master = decode_limited(bytes).ok()?;
        let (w, h) = master.dimensions();
        if w <= size && h <= size {
            store(path, bytes);
            return Some(Arc::new(master));
        }
        let small = downscale(&master, size);
        if let Some(enc) = encode_jpeg(&small) {
            store(path, &enc);
        }
        Some(Arc::new(small))
    }

    /// Cover at `size`: bucket file on disk, else derived from the (once fetched) 640 master, else - when the master
    /// is not decodable (avif, junk) - fetched at exactly `size` like the reference's odd-format fallback. An id whose
    /// master cannot be decoded is remembered, so it is not decoded (or re-requested) again.
    fn cover_image(&self, cover_art: &str, size: u32) -> ApiResult<Arc<DynamicImage>> {
        let path = cache_path(&self.art_dir(), cover_art, size);
        let st = &self.0.art;
        if size >= MASTER_SIZE && st.undecodable.lock().unwrap_or_else(|p| p.into_inner()).contains(cover_art) {
            return Err(ApiError::new(ErrorCode::Decode, format!("cover {cover_art}: undecodable")));
        }
        if let Some(img) = read_cached(&path) {
            return Ok(Arc::new(img));
        }
        let _fg = ForegroundGuard::new(&st.foreground);
        let client = self.current_client();
        let known_bad = || st.undecodable.lock().unwrap_or_else(|p| p.into_inner()).contains(cover_art);
        if size >= MASTER_SIZE {
            let bytes = self.master_bytes(client.as_ref(), cover_art, &st.fetch_gate)?;
            return match self.derive(&bytes, size, &path) {
                Some(img) => Ok(img),
                None => {
                    st.undecodable.lock().unwrap_or_else(|p| p.into_inner()).insert(cover_art.to_string());
                    Err(ApiError::new(ErrorCode::Decode, format!("cover {cover_art}: undecodable")))
                }
            };
        }
        st.image_flight.run(&path, || {
            if let Some(img) = read_cached(&path) {
                return Ok(Arc::new(img));
            }
            if !known_bad() {
                let bytes = self.master_bytes(client.as_ref(), cover_art, &st.fetch_gate)?;
                if let Some(img) = self.derive(&bytes, size, &path) {
                    return Ok(img);
                }
                st.undecodable.lock().unwrap_or_else(|p| p.into_inner()).insert(cover_art.to_string());
            }
            let client = client.as_ref().ok_or_else(|| ApiError::new(ErrorCode::NotConnected, "not connected"))?;
            let b = self.fetch_from_server(client, cover_art, size, &st.fetch_gate)?;
            let _d = st.decode_gate.acquire();
            let img = decode_limited(&b).map_err(|e| ApiError::new(ErrorCode::Decode, format!("cover decode: {e}")))?;
            store(&path, &b);
            Ok(Arc::new(img))
        })
    }

    /// Mosaic of a playlist's first distinct covers, cached (JPEG) under a key that includes a hash of the source
    /// cover ids (so it invalidates when the leading covers change). A partial mosaic (some cover failed) is served
    /// but not cached. Tiles load one after another like the reference (`mosaic.rs`), which keeps the working set small.
    fn mosaic_image(&self, playlist_id: &str, size: u32) -> ApiResult<DynamicImage> {
        let covers = library::get_playlist_distinct_covers(&self.0.db, playlist_id, MOSAIC_TILES).map_err(err)?;
        if covers.is_empty() {
            return Err(not_found(format!("playlist {playlist_id} has no covered tracks to build a mosaic from")));
        }
        let cache_id = format!("mosaic-{playlist_id}-{:016x}", hash_covers(&covers));
        let path = cache_path(&self.art_dir(), &cache_id, size);
        if let Some(img) = read_cached(&path) {
            return Ok(img);
        }
        let mut tiles: Vec<DynamicImage> = Vec::with_capacity(covers.len());
        for cover in &covers {
            match self.cover_image(cover, size) {
                Ok(img) => tiles.push(Arc::try_unwrap(img).unwrap_or_else(|a| (*a).clone())),
                Err(e) => log::debug!("mosaic: cover {cover} unavailable: {}", e.message),
            }
        }
        if tiles.is_empty() {
            return Err(not_found(format!("playlist {playlist_id}: none of its covers could be loaded")));
        }
        let composed = compose(&tiles, size);
        if tiles.len() == covers.len() {
            let mut bytes = Vec::new();
            composed
                .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
                .map_err(|e| ApiError::new(ErrorCode::Unknown, format!("mosaic: encode failed: {e}")))?;
            store(&path, &bytes);
        }
        Ok(composed)
    }

    /// `ArtApi::art_get`: `mosaic/{playlistId}[?v=..]` (the web's mosaic route; `?v=` is only a cache-buster) gives the
    /// playlist mosaic, anything else a real cover.
    pub(crate) fn art_pixels(&self, cover_art: &str, size: u32) -> ApiResult<ArtPixels> {
        let size = bucket_for(size);
        let img: Arc<DynamicImage> = if let Some(pid) = cover_art.strip_prefix(MOSAIC_PREFIX) {
            Arc::new(self.mosaic_image(pid.split('?').next().unwrap_or(pid), size)?)
        } else {
            self.cover_image(cover_art, size)?
        };
        let rgba = if img.width() > size || img.height() > size { img.resize(size, size, imageops::FilterType::Triangle).to_rgba8() } else { img.to_rgba8() };
        Ok(ArtPixels { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() })
    }

    /// Reference `Prefetcher`: after a sync, fetch the missing 640 masters of every album then artist cover in the
    /// background (2 at a time), so grids open from disk. Idempotent: ids that already hold a master are skipped.
    pub(crate) fn schedule_art_prefetch(&self, epoch: u64) {
        let me = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(PREFETCH_START_DELAY);
            let Ok(ids) = me.0.db.list_art_ids() else { return };
            me.art_prefetch_pass(&ids, epoch);
        });
    }

    /// Reference `wait_until_clear`: blocks (polling) while a foreground request runs, a track is buffering, or the
    /// window is minimized; `false` as soon as the session (epoch) has moved on.
    fn prefetch_wait_until_clear(&self, epoch: u64) -> bool {
        loop {
            if self.0.epoch.load(Ordering::SeqCst) != epoch {
                return false;
            }
            let st = &self.0.art;
            let loading = self.0._state_rx.borrow().status == sonix_core::ipc::PlaybackStatus::Loading;
            if st.foreground.load(Ordering::Acquire) == 0 && !st.suspended.load(Ordering::SeqCst) && !loading {
                return true;
            }
            std::thread::sleep(PREFETCH_PAUSE_POLL);
        }
    }

    /// One prefetch pass over `ids`; returns how many masters it fetched. The client is captured once (a pass never
    /// follows a re-login), a pass of an old epoch stops at its next step and does not block the new one, and
    /// [`PREFETCH_MAX_NET_ERRORS`] network failures in a row end the pass.
    pub(crate) fn art_prefetch_pass(&self, ids: &[String], epoch: u64) -> usize {
        let st = &self.0.art;
        {
            let mut run = st.prefetch_run.lock().unwrap_or_else(|p| p.into_inner());
            if *run == Some(epoch) {
                return 0;
            }
            *run = Some(epoch);
        }
        struct Release<'a>(&'a Mutex<Option<u64>>, u64);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                let mut run = self.0.lock().unwrap_or_else(|p| p.into_inner());
                if *run == Some(self.1) {
                    *run = None;
                }
            }
        }
        let _release = Release(&st.prefetch_run, epoch);
        let Some(client) = self.current_client() else { return 0 };
        let dir = self.art_dir();
        let missing: Vec<&String> = {
            let absent = st.absent.lock().unwrap_or_else(|p| p.into_inner());
            ids.iter().filter(|id| !absent.contains(*id) && !cache_path(&dir, id, MASTER_SIZE).exists()).collect()
        };
        let next = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        let net_errors = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            for _ in 0..PREFETCH_CONCURRENCY {
                s.spawn(|| {
                    while !stop.load(Ordering::Relaxed) {
                        let Some(id) = missing.get(next.fetch_add(1, Ordering::Relaxed)) else { break };
                        if !self.prefetch_wait_until_clear(epoch) {
                            break;
                        }
                        st.bg_attempts.fetch_add(1, Ordering::Relaxed);
                        match self.master_bytes(Some(&client), id, &st.bg_gate) {
                            Ok(_) => {
                                done.fetch_add(1, Ordering::Relaxed);
                                net_errors.store(0, Ordering::Relaxed);
                            }
                            Err(e) if e.code == ErrorCode::Network => {
                                if net_errors.fetch_add(1, Ordering::Relaxed) + 1 >= PREFETCH_MAX_NET_ERRORS {
                                    log::info!("art prefetch: server unreachable, pass ended");
                                    stop.store(true, Ordering::Relaxed);
                                }
                            }
                            Err(e) => {
                                net_errors.store(0, Ordering::Relaxed);
                                if e.code == ErrorCode::NotFound {
                                    st.absent.lock().unwrap_or_else(|p| p.into_inner()).insert((*id).clone());
                                }
                            }
                        }
                    }
                });
            }
        });
        done.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod perf_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, Rgb(rgb)))
    }

    #[test]
    fn compose_one_tile_is_full_bleed_no_gutter() {
        let out = compose(&[solid(64, 64, [255, 0, 0])], 160).to_rgb8();
        assert_eq!(out.dimensions(), (160, 160));
        assert_eq!(*out.get_pixel(80, 80), Rgb([255, 0, 0]));
    }

    #[test]
    fn compose_four_tiles_has_a_gutter_seam_at_center() {
        let tiles = [solid(64, 64, [255, 0, 0]), solid(64, 64, [0, 255, 0]), solid(64, 64, [0, 0, 255]), solid(64, 64, [255, 255, 0])];
        let out = compose(&tiles, 320).to_rgb8();
        assert_eq!(*out.get_pixel(60, 60), Rgb([255, 0, 0]));
        assert_eq!(*out.get_pixel(260, 60), Rgb([0, 255, 0]));
        assert_eq!(*out.get_pixel(60, 260), Rgb([0, 0, 255]));
        assert_eq!(*out.get_pixel(260, 260), Rgb([255, 255, 0]));
        assert_eq!(*out.get_pixel(160, 160), Rgb(GUTTER));
    }

    #[test]
    fn cover_crop_fills_target_without_distortion_bands() {
        let out = cover_crop(&solid(200, 100, [10, 20, 30]), 50, 50).to_rgb8();
        assert_eq!(out.dimensions(), (50, 50));
        assert_eq!(*out.get_pixel(0, 0), Rgb([10, 20, 30]));
        assert_eq!(*out.get_pixel(49, 49), Rgb([10, 20, 30]));
    }

    #[test]
    fn gutter_is_thinner_at_the_smallest_size() {
        assert_eq!(gutter_px(64), 1);
        assert_eq!(gutter_px(160), 2);
        assert_eq!(gutter_px(640), 2);
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn eviction_drops_least_recently_used_first_and_ignores_temp_files() {
        let dir = PathBuf::from("<build-dir>/tmp").join(format!("art-evict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = std::time::SystemTime::now();
        for (name, age) in [("old.img", 300u64), ("mid.img", 200), ("new.img", 100)] {
            let p = dir.join(name);
            std::fs::write(&p, [0u8; 100]).unwrap();
            std::fs::OpenOptions::new().write(true).open(&p).unwrap().set_modified(now - std::time::Duration::from_secs(age)).unwrap();
        }
        std::fs::write(dir.join("x.img.1.tmp"), [0u8; 500]).unwrap();
        assert_eq!(evict_oldest(&dir, 200), 200);
        assert!(!dir.join("old.img").exists() && dir.join("mid.img").exists() && dir.join("new.img").exists());
        assert!(dir.join("x.img.1.tmp").exists());
        // accounting: first write scans, later writes add up and trigger eviction past the cap
        let counter = AtomicU64::new(u64::MAX);
        account_write_with(&dir, 100, 250, 100, &counter);
        assert_eq!(counter.load(Ordering::Relaxed), 200);
        account_write_with(&dir, 100, 250, 100, &counter);
        assert_eq!(counter.load(Ordering::Relaxed), 100, "over the cap: evicted down to the target");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
