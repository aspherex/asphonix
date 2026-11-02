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
use std::path::PathBuf;

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

fn cache_path(id: &str, size: u32) -> PathBuf {
    let key: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    crate::data_dir().join("art").join(format!("{key}_{size}.img"))
}

fn read_cached(path: &std::path::Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok().filter(|b| !b.is_empty())
}

fn store(path: &std::path::Path, bytes: &[u8]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn not_found(msg: String) -> ApiError {
    ApiError::new(ErrorCode::NotFound, msg)
}

impl RealBackend {
    fn current_client(&self) -> Option<Client> {
        self.0.client.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Cover bytes at `size`: disk cache first, else `getCoverArt?size=` (the server resizes), stored as received.
    fn cover_bytes(&self, cover_art: &str, size: u32) -> ApiResult<Vec<u8>> {
        let path = cache_path(cover_art, size);
        if let Some(b) = read_cached(&path) {
            return Ok(b);
        }
        let client = self.current_client().ok_or_else(|| ApiError::new(ErrorCode::NotConnected, "not connected"))?;
        let (b, _) = self.0.rt.block_on(client.get_cover_art(cover_art, Some(size))).map_err(err)?;
        store(&path, &b);
        Ok(b)
    }

    /// Mosaic of a playlist's first distinct covers as JPEG bytes, cached under a key that includes a hash of the
    /// source cover ids (so it invalidates when the leading covers change). A partial mosaic (some cover failed) is
    /// served but not cached.
    fn mosaic_bytes(&self, playlist_id: &str, size: u32) -> ApiResult<Vec<u8>> {
        let covers = library::get_playlist_distinct_covers(&self.0.db, playlist_id, MOSAIC_TILES).map_err(err)?;
        if covers.is_empty() {
            return Err(not_found(format!("playlist {playlist_id} has no covered tracks to build a mosaic from")));
        }
        let cache_id = format!("mosaic-{playlist_id}-{:016x}", hash_covers(&covers));
        let path = cache_path(&cache_id, size);
        if let Some(b) = read_cached(&path) {
            return Ok(b);
        }
        let mut tiles: Vec<DynamicImage> = Vec::with_capacity(covers.len());
        for cover in &covers {
            match self.cover_bytes(cover, size).and_then(|b| decode_limited(&b).map_err(|e| ApiError::new(ErrorCode::Decode, e.to_string()))) {
                Ok(img) => tiles.push(img),
                Err(e) => eprintln!("mosaic: cover {cover} unavailable: {}", e.message),
            }
        }
        if tiles.is_empty() {
            return Err(not_found(format!("playlist {playlist_id}: none of its covers could be loaded")));
        }
        let mut bytes = Vec::new();
        compose(&tiles, size)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
            .map_err(|e| ApiError::new(ErrorCode::Unknown, format!("mosaic: encode failed: {e}")))?;
        if tiles.len() == covers.len() {
            store(&path, &bytes);
        }
        Ok(bytes)
    }

    /// `ArtApi::art_get`: `mosaic/{playlistId}` (and a bare id of a mirrored playlist with no cached real cover)
    /// gives the playlist mosaic, anything else a real cover.
    pub(crate) fn art_pixels(&self, cover_art: &str, size: u32) -> ApiResult<ArtPixels> {
        let size = bucket_for(size);
        let bytes = if let Some(pid) = cover_art.strip_prefix(MOSAIC_PREFIX) {
            self.mosaic_bytes(pid, size)?
        } else if !cache_path(cover_art, size).exists() && library::playlist_access(&self.0.db, cover_art).ok().flatten().is_some() {
            self.mosaic_bytes(cover_art, size)?
        } else {
            self.cover_bytes(cover_art, size)?
        };
        let img = decode_limited(&bytes).map_err(|e| ApiError::new(ErrorCode::Decode, format!("cover decode: {e}")))?;
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
