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
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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

fn cache_dir() -> PathBuf {
    crate::data_dir().join("art")
}

pub(crate) fn cache_path(id: &str, size: u32) -> PathBuf {
    let key: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    cache_dir().join(format!("{key}_{size}.img"))
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

/// Atomic write through a unique temp name (concurrent writers of one key never share a temp file).
fn store(path: &Path, bytes: &[u8]) {
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

impl RealBackend {
    fn current_client(&self) -> Option<Client> {
        self.0.client.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Cover at `size`: disk cache first, else `getCoverArt?size=` (the server resizes). Bytes are stored as received,
    /// but only once they decode.
    fn cover_image(&self, cover_art: &str, size: u32) -> ApiResult<DynamicImage> {
        let path = cache_path(cover_art, size);
        if let Some(img) = read_cached(&path) {
            return Ok(img);
        }
        let client = self.current_client().ok_or_else(|| ApiError::new(ErrorCode::NotConnected, "not connected"))?;
        let (b, _) = self.0.rt.block_on(client.get_cover_art(cover_art, Some(size))).map_err(err)?;
        let img = decode_limited(&b).map_err(|e| ApiError::new(ErrorCode::Decode, format!("cover decode: {e}")))?;
        store(&path, &b);
        Ok(img)
    }

    /// Mosaic of a playlist's first distinct covers, cached (JPEG) under a key that includes a hash of the source
    /// cover ids (so it invalidates when the leading covers change). A partial mosaic (some cover failed) is served
    /// but not cached.
    fn mosaic_image(&self, playlist_id: &str, size: u32) -> ApiResult<DynamicImage> {
        let covers = library::get_playlist_distinct_covers(&self.0.db, playlist_id, MOSAIC_TILES).map_err(err)?;
        if covers.is_empty() {
            return Err(not_found(format!("playlist {playlist_id} has no covered tracks to build a mosaic from")));
        }
        let cache_id = format!("mosaic-{playlist_id}-{:016x}", hash_covers(&covers));
        let path = cache_path(&cache_id, size);
        if let Some(img) = read_cached(&path) {
            return Ok(img);
        }
        let mut tiles: Vec<DynamicImage> = Vec::with_capacity(covers.len());
        for cover in &covers {
            match self.cover_image(cover, size) {
                Ok(img) => tiles.push(img),
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
        let img = if let Some(pid) = cover_art.strip_prefix(MOSAIC_PREFIX) {
            self.mosaic_image(pid.split('?').next().unwrap_or(pid), size)?
        } else {
            self.cover_image(cover_art, size)?
        };
        let img = if img.width() > size.max(img.height()) || img.height() > size { img.resize(size, size, imageops::FilterType::Triangle) } else { img };
        let rgba = img.to_rgba8();
        Ok(ArtPixels { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() })
    }
}

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
