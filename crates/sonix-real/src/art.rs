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
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

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
const PREFETCH_START_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
const PREFETCH_PAUSE_POLL: std::time::Duration = std::time::Duration::from_millis(500);
/// JPEG quality of a derived bucket (`art/resize.rs` `DERIVED_JPEG_QUALITY`).
const DERIVED_JPEG_QUALITY: u8 = 85;

/// Counting semaphore for the blocking `art_get` callers.
struct Gate {
    free: Mutex<usize>,
    cv: Condvar,
}

struct Permit<'a>(&'a Gate);

impl Gate {
    const fn new(n: usize) -> Self {
        Self { free: Mutex::new(n), cv: Condvar::new() }
    }
    fn acquire(&self) -> Permit<'_> {
        let mut free = self.free.lock().unwrap_or_else(|p| p.into_inner());
        while *free == 0 {
            free = self.cv.wait(free).unwrap_or_else(|p| p.into_inner());
        }
        *free -= 1;
        Permit(self)
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *self.0.free.lock().unwrap_or_else(|p| p.into_inner()) += 1;
        self.0.cv.notify_one();
    }
}

static FETCH_GATE: Gate = Gate::new(MAX_CONCURRENT_FETCHES);
static BACKGROUND_GATE: Gate = Gate::new(PREFETCH_CONCURRENCY);
/// `art_get` calls currently missing the disk cache; the prefetcher pauses while nonzero (`foreground_inflight`).
static FOREGROUND_INFLIGHT: AtomicUsize = AtomicUsize::new(0);
static PREFETCHING: AtomicBool = AtomicBool::new(false);

/// Collapses concurrent identical work into one execution (`art/singleflight.rs`): the first caller (leader) runs the
/// closure, everyone arriving while it runs waits for and clones its result. Keyed by cache file path.
struct SingleFlight<V> {
    inflight: Mutex<HashMap<PathBuf, Arc<Slot<V>>>>,
}

struct Slot<V> {
    value: Mutex<Option<V>>,
    cv: Condvar,
}

impl<V: Clone + FromErr> SingleFlight<V> {
    fn new() -> Self {
        Self { inflight: Mutex::new(HashMap::new()) }
    }

    fn run(&self, key: &Path, work: impl FnOnce() -> V) -> V {
        let (slot, leader) = {
            let mut map = self.inflight.lock().unwrap_or_else(|p| p.into_inner());
            match map.get(key) {
                Some(s) => (s.clone(), false),
                None => {
                    let s = Arc::new(Slot { value: Mutex::new(None), cv: Condvar::new() });
                    map.insert(key.to_path_buf(), s.clone());
                    (s, true)
                }
            }
        };
        if leader {
            // Removes the entry and wakes the waiters even if `work` panics.
            struct Done<'a, V>(&'a SingleFlight<V>, &'a Path, &'a Slot<V>);
            impl<V> Drop for Done<'_, V> {
                fn drop(&mut self) {
                    self.0.inflight.lock().unwrap_or_else(|p| p.into_inner()).remove(self.1);
                    self.2.cv.notify_all();
                }
            }
            let _done = Done(self, key, &slot);
            let v = work();
            *slot.value.lock().unwrap_or_else(|p| p.into_inner()) = Some(v.clone());
            v
        } else {
            let mut g = slot.value.lock().unwrap_or_else(|p| p.into_inner());
            loop {
                if let Some(v) = g.as_ref() {
                    return v.clone();
                }
                if Arc::strong_count(&slot) == 1 {
                    // the leader panicked without a value: do not hang
                    return V::from_err(ApiError::new(ErrorCode::Unknown, "art fetch aborted"));
                }
                g = slot.cv.wait_timeout(g, std::time::Duration::from_millis(200)).unwrap_or_else(|p| p.into_inner()).0;
            }
        }
    }
}

trait FromErr {
    fn from_err(e: ApiError) -> Self;
}

impl<T> FromErr for ApiResult<T> {
    fn from_err(e: ApiError) -> Self {
        Err(e)
    }
}

type ImageFlight = SingleFlight<ApiResult<Arc<DynamicImage>>>;
type BytesFlight = SingleFlight<ApiResult<Arc<Vec<u8>>>>;

fn image_flight() -> &'static ImageFlight {
    static F: OnceLock<ImageFlight> = OnceLock::new();
    F.get_or_init(SingleFlight::new)
}

fn master_flight() -> &'static BytesFlight {
    static F: OnceLock<BytesFlight> = OnceLock::new();
    F.get_or_init(SingleFlight::new)
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

fn looks_like_image(b: &[u8]) -> bool {
    b.starts_with(&[0xFF, 0xD8]) || b.starts_with(&[0x89, b'P', b'N', b'G'])
}

/// Counts `art_get` calls that are past the disk cache (what the prefetcher yields to).
struct ForegroundGuard;

impl ForegroundGuard {
    fn new() -> Self {
        FOREGROUND_INFLIGHT.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for ForegroundGuard {
    fn drop(&mut self) {
        FOREGROUND_INFLIGHT.fetch_sub(1, Ordering::AcqRel);
    }
}

impl RealBackend {
    fn current_client(&self) -> Option<Client> {
        self.0.client.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// `getCoverArt?size=` of one id through the concurrency gate (blocking); the body must look like an image.
    fn fetch_from_server(&self, cover_art: &str, size: u32, gate: &'static Gate) -> ApiResult<Vec<u8>> {
        let client = self.current_client().ok_or_else(|| ApiError::new(ErrorCode::NotConnected, "not connected"))?;
        let _permit = gate.acquire();
        let (b, _) = self.0.rt.block_on(client.get_cover_art(cover_art, Some(size))).map_err(err)?;
        if !looks_like_image(&b) {
            return Err(not_found(format!("cover art {cover_art}: server returned a non-image payload")));
        }
        Ok(b)
    }

    /// Bytes of the cached master (`<id>_640.img`), fetching it once (single-flight, gated) on a miss.
    fn master_bytes(&self, cover_art: &str, gate: &'static Gate) -> ApiResult<Arc<Vec<u8>>> {
        let path = cache_path(&self.art_dir(), cover_art, MASTER_SIZE);
        if let Some(b) = read_bytes_touch(&path) {
            return Ok(Arc::new(b));
        }
        master_flight().run(&path, || {
            // a flight that finished just before this one started has filled the disk cache
            if let Some(b) = read_bytes_touch(&path) {
                return Ok(Arc::new(b));
            }
            let b = self.fetch_from_server(cover_art, MASTER_SIZE, gate)?;
            store(&path, &b);
            Ok(Arc::new(b))
        })
    }

    /// Decoded master plus its bytes; a master that does not decode is deleted and reported as `Decode`.
    fn master_image(&self, cover_art: &str) -> ApiResult<(Arc<DynamicImage>, Arc<Vec<u8>>)> {
        let bytes = self.master_bytes(cover_art, &FETCH_GATE)?;
        match decode_limited(&bytes) {
            Ok(img) => Ok((Arc::new(img), bytes)),
            Err(e) => {
                let _ = std::fs::remove_file(cache_path(&self.art_dir(), cover_art, MASTER_SIZE));
                Err(ApiError::new(ErrorCode::Decode, format!("cover decode: {e}")))
            }
        }
    }

    /// Cover at `size`: bucket file on disk, else derived from the (once fetched) 640 master, else - when the master
    /// is not a decodable jpeg/png - fetched at exactly `size` like the reference's odd-format fallback.
    fn cover_image(&self, cover_art: &str, size: u32) -> ApiResult<Arc<DynamicImage>> {
        let path = cache_path(&self.art_dir(), cover_art, size);
        if let Some(img) = read_cached(&path) {
            return Ok(Arc::new(img));
        }
        let _fg = ForegroundGuard::new();
        if size >= MASTER_SIZE {
            return self.master_image(cover_art).map(|(img, _)| img);
        }
        image_flight().run(&path, || {
            if let Some(img) = read_cached(&path) {
                return Ok(Arc::new(img));
            }
            match self.master_image(cover_art) {
                Ok((master, bytes)) => {
                    let (w, h) = master.dimensions();
                    if w <= size && h <= size {
                        store(&path, &bytes);
                        return Ok(master);
                    }
                    let small = downscale(&master, size);
                    if let Some(enc) = encode_jpeg(&small) {
                        store(&path, &enc);
                    }
                    Ok(Arc::new(small))
                }
                Err(e) if e.code == ErrorCode::Decode => {
                    let b = self.fetch_from_server(cover_art, size, &FETCH_GATE)?;
                    let img = decode_limited(&b).map_err(|e| ApiError::new(ErrorCode::Decode, format!("cover decode: {e}")))?;
                    store(&path, &b);
                    Ok(Arc::new(img))
                }
                Err(e) => Err(e),
            }
        })
    }

    /// Mosaic of a playlist's first distinct covers, cached (JPEG) under a key that includes a hash of the source
    /// cover ids (so it invalidates when the leading covers change). A partial mosaic (some cover failed) is served
    /// but not cached. The tiles load in parallel (each through the shared gate / single-flight).
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
        let loaded: Vec<ApiResult<Arc<DynamicImage>>> = std::thread::scope(|s| {
            let hs: Vec<_> = covers.iter().map(|c| s.spawn(move || self.cover_image(c, size))).collect();
            hs.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(ApiError::new(ErrorCode::Unknown, "mosaic tile panicked")))).collect()
        });
        let mut tiles: Vec<DynamicImage> = Vec::with_capacity(covers.len());
        for (cover, r) in covers.iter().zip(loaded) {
            match r {
                Ok(img) => tiles.push((*img).clone()),
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

    /// Reference `Prefetcher`: after a sync, fetch the missing 640 masters of every album/artist cover in the
    /// background (2 at a time, paused while a foreground request runs), so grids open from disk. Idempotent: ids that
    /// already hold a master are skipped, and only one pass runs at a time.
    pub(crate) fn schedule_art_prefetch(&self, epoch: u64) {
        let me = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(PREFETCH_START_DELAY);
            let Ok(ids) = me.0.db.list_art_ids() else { return };
            me.art_prefetch_pass(&ids, epoch);
        });
    }

    /// One prefetch pass over `ids`; returns how many masters it fetched.
    pub(crate) fn art_prefetch_pass(&self, ids: &[String], epoch: u64) -> usize {
        if PREFETCHING.swap(true, Ordering::AcqRel) {
            return 0;
        }
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                PREFETCHING.store(false, Ordering::Release);
            }
        }
        let _reset = Reset;
        let dir = self.art_dir();
        let missing: Vec<&String> = ids.iter().filter(|id| !cache_path(&dir, id, MASTER_SIZE).exists()).collect();
        let next = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..PREFETCH_CONCURRENCY {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(id) = missing.get(i) else { break };
                        // foreground requests always win
                        while FOREGROUND_INFLIGHT.load(Ordering::Acquire) > 0 && self.0.epoch.load(Ordering::SeqCst) == epoch {
                            std::thread::sleep(PREFETCH_PAUSE_POLL);
                        }
                        if self.0.epoch.load(Ordering::SeqCst) != epoch {
                            break;
                        }
                        if self.master_bytes(id, &BACKGROUND_GATE).is_ok() {
                            done.fetch_add(1, Ordering::Relaxed);
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
