//! Render accounting for CPU work (diagnostics only, active when `$SONIX_RCOUNT` is set; the lines go to
//! `$SONIX_LOG`): how often each view's `render()` runs and what a whole window frame costs on the UI thread (root
//! `render()` start to the end of the paint pass), logged once a second as
//! `renders/s root=.. tracks=.. ... frame_ms avg=.. max=..`.
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

static ON: AtomicBool = AtomicBool::new(false);
struct Acc {
    hits: BTreeMap<&'static str, u32>,
    frames: u32,
    frame_ns: u64,
    frame_max_ns: u64,
    /// phase sums: render + layout (root render -> first prepaint), prepaint (-> first paint), paint (-> last paint)
    phase_ns: [u64; 3],
    start: Option<Instant>,
    prepaint_at: Option<Instant>,
    paint_at: Option<Instant>,
}
static ACC: Mutex<Acc> = Mutex::new(Acc { hits: BTreeMap::new(), frames: 0, frame_ns: 0, frame_max_ns: 0, phase_ns: [0; 3], start: None, prepaint_at: None, paint_at: None });

/// Call once at startup: turns the counters on when `$SONIX_RCOUNT` (and `$SONIX_LOG`, where the lines go) is set and
/// starts the 1 Hz logger.
pub fn init() {
    if std::env::var_os("SONIX_RCOUNT").is_none() || std::env::var_os("SONIX_LOG").is_none() {
        return;
    }
    ON.store(true, Relaxed);
    let _ = std::thread::Builder::new().name("rcount".into()).spawn(|| {
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let (hits, frames, ns, max, ph) = {
                let Ok(mut a) = ACC.lock() else { return };
                let r = (std::mem::take(&mut a.hits), a.frames, a.frame_ns, a.frame_max_ns, a.phase_ns);
                (a.frames, a.frame_ns, a.frame_max_ns, a.phase_ns) = (0, 0, 0, [0; 3]);
                r
            };
            if frames == 0 && hits.is_empty() {
                continue;
            }
            let mut s = String::from("renders/s");
            for (k, v) in &hits {
                s.push_str(&format!(" {k}={v}"));
            }
            let avg = if frames > 0 { ns as f64 / frames as f64 / 1e6 } else { 0.0 };
            s.push_str(&format!(" frames={frames} frame_ms avg={avg:.3} max={:.3}", max as f64 / 1e6));
            if frames > 0 {
                let f = |n: u64| n as f64 / frames as f64 / 1e6;
                s.push_str(&format!(" render+layout={:.3} prepaint={:.3} paint={:.3}", f(ph[0]), f(ph[1]), f(ph[2])));
            }
            crate::log(&s);
        }
    });
}

/// One `render()` of the view `name`.
#[inline]
pub fn hit(name: &'static str) {
    if ON.load(Relaxed)
        && let Ok(mut a) = ACC.lock()
    {
        *a.hits.entry(name).or_default() += 1;
    }
}

/// The root view's `render()` starts a window frame.
#[inline]
pub fn frame_start() {
    if ON.load(Relaxed)
        && let Ok(mut a) = ACC.lock()
    {
        a.start = Some(Instant::now());
    }
}

/// The last element of the root paints: the frame's render + layout + prepaint + paint are done.
#[inline]
pub fn frame_end() {
    if ON.load(Relaxed)
        && let Ok(mut a) = ACC.lock()
        && let Some(t) = a.start.take()
    {
        let ns = t.elapsed().as_nanos() as u64;
        if let (Some(p1), Some(p2)) = (a.prepaint_at.take(), a.paint_at.take()) {
            let now = Instant::now();
            a.phase_ns[0] += p1.duration_since(t).as_nanos() as u64;
            a.phase_ns[1] += p2.duration_since(p1).as_nanos() as u64;
            a.phase_ns[2] += now.duration_since(p2).as_nanos() as u64;
        }
        a.frames += 1;
        a.frame_ns += ns;
        a.frame_max_ns = a.frame_max_ns.max(ns);
    }
}

/// An empty element painted last in the root: closes the frame measurement.
pub fn frame_end_probe() -> gpui::AnyElement {
    use gpui::{IntoElement, Styled};
    gpui::canvas(|_, _, _| (), |_, _, _, _| frame_end()).absolute().size_0().into_any_element()
}

/// Painted first in the root: its prepaint marks the end of render + layout, its paint the end of prepaint.
pub fn frame_phase_probe() -> gpui::AnyElement {
    use gpui::{IntoElement, Styled};
    gpui::canvas(|_, _, _| mark(1), |_, _, _, _| mark(2)).absolute().size_0().into_any_element()
}

fn mark(phase: u8) {
    if ON.load(Relaxed)
        && let Ok(mut a) = ACC.lock()
    {
        let now = Instant::now();
        if phase == 1 {
            a.prepaint_at = Some(now);
        } else {
            a.paint_at = Some(now);
        }
    }
}
