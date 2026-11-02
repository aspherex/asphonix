//! UI-side cover cache: decoded RGBA at an exact bucket size comes from
//! `Backend::art_get` on a worker thread, is converted to a GPUI `RenderImage`
//! (BGRA) and kept in a byte-capped LRU. Nothing is ever held at full resolution;
//! on minimize everything is dropped (`drain_all`) and `window.drop_image`d so the
//! GPU atlas tiles go too.
use gpui::{AnyElement, Context, Hsla, ImageSource, ObjectFit, RenderImage, SharedString, StyledImage, Window, div, hsla, img, prelude::*, px};
use image::{Frame, RgbaImage};
use sonix_api::{ArtPixels, Backend, bucket_for};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

type Key = (SharedString, u32);

pub struct ArtCache {
    cap: usize,
    map: HashMap<Key, (Arc<RenderImage>, u64, usize)>,
    pending: HashSet<Key>,
    failed: HashSet<Key>,
    bytes: usize,
    tick: u64,
}

pub type ArtHandle = Rc<RefCell<ArtCache>>;

pub fn new_handle(cap_bytes: usize) -> ArtHandle {
    Rc::new(RefCell::new(ArtCache::new(cap_bytes)))
}

pub fn render_image(p: ArtPixels) -> Option<Arc<RenderImage>> {
    let mut rgba = p.rgba;
    for px in rgba.chunks_mut(4) {
        px.swap(0, 2); // GPUI wants BGRA
    }
    let buf = RgbaImage::from_raw(p.width, p.height, rgba)?;
    Some(Arc::new(RenderImage::new(vec![Frame::new(buf)])))
}

impl ArtCache {
    pub fn new(cap: usize) -> Self {
        Self { cap, map: HashMap::new(), pending: HashSet::new(), failed: HashSet::new(), bytes: 0, tick: 0 }
    }
    pub fn get(&mut self, id: &SharedString, size: u32) -> Option<Arc<RenderImage>> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(&(id.clone(), size)).map(|e| {
            e.1 = t;
            e.0.clone()
        })
    }
    /// True when the caller should start loading (marks the key pending).
    pub fn want(&mut self, id: &SharedString, size: u32) -> bool {
        let k = (id.clone(), size);
        if self.map.contains_key(&k) || self.pending.contains(&k) || self.failed.contains(&k) {
            return false;
        }
        self.pending.insert(k);
        true
    }
    /// Insert (or record a failure) and return the evicted images the caller must drop from the GPU.
    pub fn insert(&mut self, id: SharedString, size: u32, img: Option<Arc<RenderImage>>) -> Vec<Arc<RenderImage>> {
        let k = (id, size);
        self.pending.remove(&k);
        let mut evicted = Vec::new();
        match img {
            None => {
                self.failed.insert(k);
            }
            Some(i) => {
                let b = (size as usize) * (size as usize) * 4;
                self.tick += 1;
                self.bytes += b;
                self.map.insert(k, (i, self.tick, b));
                while self.bytes > self.cap && self.map.len() > 1 {
                    let oldest = self.map.iter().min_by_key(|(_, v)| v.1).map(|(k, _)| k.clone()).unwrap();
                    if let Some((i, _, b)) = self.map.remove(&oldest) {
                        self.bytes -= b;
                        evicted.push(i);
                    }
                }
            }
        }
        evicted
    }
    pub fn drain_all(&mut self) -> Vec<Arc<RenderImage>> {
        self.bytes = 0;
        self.pending.clear();
        self.failed.clear();
        self.map.drain().map(|(_, v)| v.0).collect()
    }
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.len()
    }
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// A square cover of `logical` px: the cached image when present, otherwise a flat
/// placeholder while a worker loads it (the requesting view is re-rendered on arrival).
pub fn cover<V: 'static>(
    art: &ArtHandle,
    backend: &Arc<dyn Backend>,
    id: Option<&str>,
    logical: f32,
    window: &mut Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let bucket = bucket_for((logical * window.scale_factor()).ceil() as u32);
    cover_bucket(art, backend, id, logical, bucket, window, cx)
}

/// [`cover`] at an explicit size bucket (the sizes the web requests: cards 160, album header 320, thumbs 64).
pub fn cover_bucket<V: 'static>(
    art: &ArtHandle,
    backend: &Arc<dyn Backend>,
    id: Option<&str>,
    logical: f32,
    bucket: u32,
    window: &mut Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let base = div().size(px(logical)).flex_none().overflow_hidden();
    // Parity harness: the web mock cannot resolve `sonix://art` in a plain browser (the <img> is hidden, leaving the
    // slot background), so the native side renders the empty slot too.
    #[cfg(feature = "parity")]
    if std::env::var_os("SONIX_PARITY_NOART").is_some() {
        return base.into_any_element();
    }
    let Some(id) = id else { return base.bg(hsla(0.0, 0.0, 0.5, 0.12)).into_any_element() };
    let sid: SharedString = id.to_string().into();
    let hit = art.borrow_mut().get(&sid, bucket);
    if hit.is_none() && art.borrow_mut().want(&sid, bucket) {
        let (backend, art2, sid2) = (backend.clone(), art.clone(), sid.clone());
        cx.spawn_in(window, async move |this, cx| {
            let key = sid2.to_string();
            let img = cx.background_executor().spawn(async move { backend.art_get(&key, bucket).ok().and_then(render_image) }).await;
            // The cache entry must land even when the requesting view was dropped meanwhile (navigated away): otherwise the
            // key would stay `pending` forever and the next view mounted for it would show the placeholder for good.
            let _ = cx.update(|window, cx| {
                for e in art2.borrow_mut().insert(sid2, bucket, img) {
                    let _ = window.drop_image(e);
                }
                let _ = this.update(cx, |_, cx| cx.notify());
            });
        })
        .detach();
    }
    // `.art { background: var(--color-surface-high) }` until the image arrives (no per-id tint)
    let base = base.bg(crate::app::theme(cx).high);
    match hit {
        Some(i) => base.child(img(ImageSource::Render(i)).size_full().object_fit(ObjectFit::Cover)).into_any_element(),
        None => base.into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(size: u32) -> Arc<RenderImage> {
        render_image(ArtPixels { width: size, height: size, rgba: vec![0; (size * size * 4) as usize] }).unwrap()
    }

    #[test]
    fn lru_is_byte_capped_and_evicts_oldest() {
        let mut c = ArtCache::new(2 * 64 * 64 * 4);
        let a: SharedString = "a".into();
        assert!(c.want(&a, 64));
        assert!(!c.want(&a, 64), "pending");
        assert!(c.insert(a.clone(), 64, Some(img(64))).is_empty());
        c.insert("b".into(), 64, Some(img(64)));
        assert!(c.get(&a, 64).is_some()); // touch a: b is now the oldest
        let ev = c.insert("c".into(), 64, Some(img(64)));
        assert_eq!(ev.len(), 1);
        assert_eq!(c.len(), 2);
        assert!(c.get(&"b".into(), 64).is_none() && c.get(&a, 64).is_some());
        assert!(c.bytes() <= 2 * 64 * 64 * 4);
        assert_eq!(c.drain_all().len(), 2);
        assert!(c.is_empty() && c.bytes() == 0);
    }

    #[test]
    fn failures_are_not_retried_until_drained() {
        let mut c = ArtCache::new(1 << 20);
        let a: SharedString = "x".into();
        assert!(c.want(&a, 64));
        c.insert(a.clone(), 64, None);
        assert!(!c.want(&a, 64));
        c.drain_all();
        assert!(c.want(&a, 64));
    }

    #[test]
    fn rgba_is_swapped_to_bgra() {
        let i = render_image(ArtPixels { width: 1, height: 1, rgba: vec![1, 2, 3, 4] }).unwrap();
        assert_eq!(i.as_bytes(0).unwrap(), &[3, 2, 1, 4]);
    }
}
