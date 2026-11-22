//! UI-side cover cache: decoded RGBA at an exact bucket size comes from
//! `Backend::art_get` on a worker thread, is converted to a GPUI `RenderImage`
//! (BGRA) and kept in a byte-capped LRU. Nothing is ever held at full resolution;
//! on minimize everything is dropped (`drain_all`) and `window.drop_image`d so the
//! GPU atlas tiles go too.
use gpui::{AnyElement, Context, EntityId, ImageSource, ObjectFit, RenderImage, SharedString, StyledImage, Window, div, hsla, img, prelude::*, px};
use image::{Frame, RgbaImage};
use sonix_api::{ArtPixels, Backend, bucket_for};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};
use std::sync::Arc;

type Key = (SharedString, u32);

/// Cover fetches running at once: a grid asks for dozens of covers in one
/// frame; starting them all would open as many connections / threads and starve the visible ones.
const MAX_WORKERS: usize = 4;
/// Requests waiting beyond this are dropped oldest-batch first (they re-request when their view renders again).
const MAX_QUEUED: usize = 96;
/// Requests within this gap of each other belong to one render pass ("batch"); the newest batch is served first.
const BATCH_GAP: Duration = Duration::from_millis(4);
/// Arrivals re-render the requesting views at most this often.
const FLUSH_EVERY: Duration = Duration::from_millis(40);

struct Queued {
    key: Key,
    batch: u64,
    seq: u64,
}

pub struct ArtCache {
    cap: usize,
    map: HashMap<Key, (Arc<RenderImage>, u64, usize)>,
    pending: HashSet<Key>,
    failed: HashSet<Key>,
    bytes: usize,
    tick: u64,
    queue: Vec<Queued>,
    workers: usize,
    batch: u64,
    seq: u64,
    last_want: Option<Instant>,
    waiters: HashSet<EntityId>,
    last_flush: Option<Instant>,
    flush_scheduled: bool,
}

pub type ArtHandle = Rc<RefCell<ArtCache>>;

/// Cover loads run on their own threads, not on GPUI's shared background pool: a `art_get` blocks (network, disk, decode) and
/// must never occupy a pool thread a page query (`browse_get_*`, scroll restore) is waiting for. One thread per loader slot.
fn run_on_art_thread<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> futures::channel::oneshot::Receiver<T> {
    type Job = Box<dyn FnOnce() + Send>;
    static POOL: std::sync::OnceLock<std::sync::mpsc::Sender<Job>> = std::sync::OnceLock::new();
    let tx = POOL.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let rx = Arc::new(std::sync::Mutex::new(rx));
        for i in 0..MAX_WORKERS {
            let rx = rx.clone();
            let _ = std::thread::Builder::new().name(format!("art-loader-{i}")).spawn(move || {
                loop {
                    let job = rx.lock().unwrap_or_else(|p| p.into_inner()).recv();
                    match job {
                        // a panicking decode must not take the thread with it (unwinding builds only; release aborts):
                        // its oneshot is dropped, the awaiting loader sees `Canceled` -> no image, the thread stays
                        Ok(job) => {
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                        }
                        Err(_) => break,
                    }
                }
            });
        }
        tx
    });
    let (done, rx) = futures::channel::oneshot::channel();
    let _ = tx.send(Box::new(move || {
        let _ = done.send(job());
    }));
    rx
}

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
        Self {
            cap,
            map: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            bytes: 0,
            tick: 0,
            queue: Vec::new(),
            workers: 0,
            batch: 0,
            seq: 0,
            last_want: None,
            waiters: HashSet::new(),
            last_flush: None,
            flush_scheduled: false,
        }
    }
    pub fn get(&mut self, id: &SharedString, size: u32) -> Option<Arc<RenderImage>> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(&(id.clone(), size)).map(|e| {
            e.1 = t;
            e.0.clone()
        })
    }
    /// True when the key was newly queued for loading (it is then `pending` until [`insert`](Self::insert)). A key
    /// that is still waiting in the queue is re-stamped with the current render batch, so what is on screen now is
    /// served before what was requested for an earlier (scrolled-away) frame.
    pub fn want(&mut self, id: &SharedString, size: u32) -> bool {
        self.want_at(id, size, Instant::now())
    }
    /// [`want`](Self::want) with an explicit clock (render batches are separated by [`BATCH_GAP`]).
    pub fn want_at(&mut self, id: &SharedString, size: u32, now: Instant) -> bool {
        let k = (id.clone(), size);
        if self.map.contains_key(&k) || self.failed.contains(&k) {
            return false;
        }
        if self.last_want.is_none_or(|l| now.duration_since(l) > BATCH_GAP) {
            self.batch += 1;
        }
        self.last_want = Some(now);
        if self.pending.contains(&k) {
            let b = self.batch;
            self.seq += 1;
            let s = self.seq;
            if let Some(q) = self.queue.iter_mut().find(|q| q.key == k) {
                q.batch = b;
                q.seq = s;
            }
            return false;
        }
        self.pending.insert(k.clone());
        self.seq += 1;
        self.queue.push(Queued { key: k, batch: self.batch, seq: self.seq });
        if self.queue.len() > MAX_QUEUED {
            // drop the stalest: oldest render batch first, and within a batch the last requested (furthest down the page)
            self.queue.sort_by_key(|q| std::cmp::Reverse((q.batch, std::cmp::Reverse(q.seq))));
            for q in self.queue.split_off(MAX_QUEUED) {
                self.pending.remove(&q.key);
            }
        }
        true
    }
    /// Next key to load: newest render batch first, in request order within it.
    pub fn pop(&mut self) -> Option<Key> {
        let i = self.queue.iter().enumerate().max_by_key(|(_, q)| (q.batch, std::cmp::Reverse(q.seq))).map(|(i, _)| i)?;
        Some(self.queue.swap_remove(i).key)
    }
    /// Claims a loader slot when there is queued work and fewer than [`MAX_WORKERS`] loaders run.
    pub fn take_worker(&mut self) -> bool {
        if self.workers < MAX_WORKERS && !self.queue.is_empty() {
            self.workers += 1;
            return true;
        }
        false
    }
    pub fn worker_done(&mut self) {
        self.workers = self.workers.saturating_sub(1);
    }
    /// Registers a view to be re-rendered when covers arrive.
    pub fn watch(&mut self, view: EntityId) {
        self.waiters.insert(view);
    }
    /// Arrival bookkeeping: `Ok(views)` = notify these now, `Err(delay)` = a flush must be scheduled after `delay`
    /// (None = one is already scheduled). Coalesces bursts of arrivals into at most one re-render per `FLUSH_EVERY`.
    pub fn flush_plan(&mut self) -> Result<Vec<EntityId>, Option<Duration>> {
        self.flush_plan_at(Instant::now())
    }
    pub fn flush_plan_at(&mut self, now: Instant) -> Result<Vec<EntityId>, Option<Duration>> {
        let since = self.last_flush.map(|t| now.saturating_duration_since(t));
        match since {
            Some(s) if s < FLUSH_EVERY => {
                if self.flush_scheduled {
                    Err(None)
                } else {
                    self.flush_scheduled = true;
                    Err(Some(FLUSH_EVERY - s))
                }
            }
            _ => Ok(self.take_flush_at(now)),
        }
    }
    pub fn take_flush(&mut self) -> Vec<EntityId> {
        self.take_flush_at(Instant::now())
    }
    fn take_flush_at(&mut self, now: Instant) -> Vec<EntityId> {
        self.flush_scheduled = false;
        self.last_flush = Some(now);
        self.waiters.drain().collect()
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
        self.queue.clear();
        self.waiters.clear();
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

/// Cache key of a playlist mosaic: always a mosaic when the playlist has tracks (the real
/// `cover_art` is ignored); `v` only busts the cache when the playlist changes, the backend strips it.
pub fn mosaic_key(p: &sonix_api::Playlist) -> String {
    format!("mosaic/{}?v={}-{}", p.id, p.song_count, p.updated_at.as_deref().unwrap_or(""))
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

/// [`cover`] at an explicit size bucket (cards 160, album header 320, thumbs 64).
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
    let Some(id) = id else { return base.bg(hsla(0.0, 0.0, 0.5, 0.12)).into_any_element() };
    let sid: SharedString = id.to_string().into();
    let hit = art.borrow_mut().get(&sid, bucket);
    if hit.is_none() {
        let mut a = art.borrow_mut();
        a.watch(cx.entity_id());
        let queued = a.want(&sid, bucket);
        let spawn_worker = queued && a.take_worker();
        drop(a);
        if spawn_worker {
            let (backend, art2) = (backend.clone(), art.clone());
            // One loader of at most `MAX_WORKERS`: pulls the newest-batch request until the queue is empty. Arrivals land
            // in the cache even when the requesting view was dropped meanwhile (navigated away), so no key stays
            // `pending` forever, and the views that asked are re-rendered in coalesced bursts.
            cx.spawn_in(window, async move |_this, cx| {
                loop {
                    let Some((sid2, bucket)) = art2.borrow_mut().pop() else {
                        art2.borrow_mut().worker_done();
                        break;
                    };
                    let (b, key) = (backend.clone(), sid2.to_string());
                    let img = run_on_art_thread(move || b.art_get(&key, bucket).ok().and_then(render_image)).await.ok().flatten();
                    let _ = cx.update(|window, cx| {
                        let plan = {
                            let mut a = art2.borrow_mut();
                            for e in a.insert(sid2, bucket, img) {
                                let _ = window.drop_image(e);
                            }
                            a.flush_plan()
                        };
                        match plan {
                            Ok(views) => views.into_iter().for_each(|v| cx.notify(v)),
                            Err(None) => {}
                            Err(Some(delay)) => {
                                let art3 = art2.clone();
                                cx.spawn(async move |cx| {
                                    cx.background_executor().timer(delay).await;
                                    let _ = cx.update(|cx| {
                                        let views = art3.borrow_mut().take_flush();
                                        views.into_iter().for_each(|v| cx.notify(v));
                                    });
                                })
                                .detach();
                            }
                        }
                    });
                }
            })
            .detach();
        }
    }
    // the high surface shows until the image arrives (no per-id tint)
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


    fn s(n: impl ToString) -> SharedString {
        SharedString::from(n.to_string())
    }

    #[test]
    fn newest_render_batch_is_served_first_and_rerequests_bump() {
        let mut c = ArtCache::new(1 << 20);
        let t0 = Instant::now();
        let next_frame = BATCH_GAP + Duration::from_millis(1);
        // old frame: a, b; then the user scrolled and a new frame asks for c, d (and a again)
        assert!(c.want_at(&s("a"), 160, t0) && c.want_at(&s("b"), 160, t0));
        assert!(c.want_at(&s("c"), 160, t0 + next_frame) && c.want_at(&s("d"), 160, t0 + next_frame));
        assert!(!c.want_at(&s("a"), 160, t0 + next_frame), "already queued: no second entry, but it is re-stamped");
        let order: Vec<_> = std::iter::from_fn(|| c.pop()).map(|k| k.0.to_string()).collect();
        assert_eq!(order, ["c", "d", "a", "b"]);
    }

    #[test]
    fn requests_within_the_gap_share_a_batch_in_request_order() {
        let mut c = ArtCache::new(1 << 20);
        let t0 = Instant::now();
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            c.want_at(&s(id), 64, t0 + Duration::from_millis(i as u64)); // each within BATCH_GAP of the previous
        }
        let order: Vec<_> = std::iter::from_fn(|| c.pop()).map(|k| k.0.to_string()).collect();
        assert_eq!(order, ["a", "b", "c"]);
    }

    #[test]
    fn art_threads_survive_panicking_jobs() {
        let panics: Vec<_> = (0..MAX_WORKERS + 1).map(|_| run_on_art_thread(|| -> u32 { panic!("bad image") })).collect();
        for rx in panics {
            assert!(futures::executor::block_on(rx).is_err(), "a panicked job resolves as Canceled, not a hang");
        }
        let ok: Vec<_> = (0..MAX_WORKERS * 2).map(|i| run_on_art_thread(move || i)).collect();
        let got: Vec<_> = ok.into_iter().map(|rx| futures::executor::block_on(rx).unwrap()).collect();
        assert_eq!(got, (0..MAX_WORKERS * 2).collect::<Vec<_>>(), "the pool still runs jobs after more panics than threads");
    }

    #[test]
    fn loader_slots_are_capped_and_queue_is_bounded() {
        let mut c = ArtCache::new(1 << 20);
        for i in 0..200 {
            c.want(&s(format!("x{i}")), 64);
        }
        assert!(c.queue.len() <= MAX_QUEUED);
        assert_eq!(c.pending.len(), c.queue.len(), "dropped requests are no longer pending");
        let mut n = 0;
        while c.take_worker() {
            n += 1;
        }
        assert_eq!(n, MAX_WORKERS);
        c.worker_done();
        assert!(c.take_worker());
    }

    #[test]
    fn dropped_backlog_is_oldest_batch_first_and_can_be_requested_again() {
        let mut c = ArtCache::new(1 << 20);
        let t0 = Instant::now();
        let frame = |n: u64| t0 + (BATCH_GAP + Duration::from_millis(1)) * n as u32;
        for i in 0..MAX_QUEUED {
            c.want_at(&s(format!("old{i}")), 64, frame(0));
        }
        // a newer frame adds 10 more: the 10 dropped are the last of the OLD batch, the new frame survives
        for i in 0..10 {
            assert!(c.want_at(&s(format!("new{i}")), 64, frame(1)));
        }
        assert_eq!(c.queue.len(), MAX_QUEUED);
        assert!((0..10).all(|i| c.queue.iter().any(|q| q.key.0 == format!("new{i}"))));
        let dropped = format!("old{}", MAX_QUEUED - 1);
        assert!(!c.queue.iter().any(|q| q.key.0 == dropped.as_str()));
        // the dropped key is no longer pending: rendering it again queues it again
        assert!(c.want_at(&s(&dropped), 64, frame(2)));
        assert_eq!(c.pop().unwrap().0.to_string(), dropped, "and it is now the newest demand");
    }

    #[test]
    fn arrivals_coalesce_into_one_flush() {
        let mut c = ArtCache::new(1 << 20);
        let t0 = Instant::now();
        assert!(c.flush_plan_at(t0).is_ok(), "first arrival notifies at once");
        assert!(matches!(c.flush_plan_at(t0 + Duration::from_millis(5)), Err(Some(d)) if d == FLUSH_EVERY - Duration::from_millis(5)), "next one schedules a flush");
        assert!(matches!(c.flush_plan_at(t0 + Duration::from_millis(9)), Err(None)), "and the rest ride on it");
        assert!(c.take_flush_at(t0 + FLUSH_EVERY).is_empty());
        assert!(matches!(c.flush_plan_at(t0 + FLUSH_EVERY + Duration::from_millis(1)), Err(Some(_))), "the next burst starts a new window");
        assert!(c.flush_plan_at(t0 + FLUSH_EVERY * 4).is_ok(), "an arrival after a quiet spell is immediate");
    }

    #[test]
    fn rgba_is_swapped_to_bgra() {
        let i = render_image(ArtPixels { width: 1, height: 1, rgba: vec![1, 2, 3, 4] }).unwrap();
        assert_eq!(i.as_bytes(0).unwrap(), &[3, 2, 1, 4]);
    }
}
