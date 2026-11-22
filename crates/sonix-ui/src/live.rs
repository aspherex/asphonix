//! The live layer (CPU work): everything that animates while music plays (the now-playing bars of the current row,
//! the rail spectrum, the Now Playing FFT bars + stereo meters) is painted by a small view, [`LiveLayer`] (one over the
//! stage, one over the window for the rail), instead of inside the (cached) page that lays it out. A spectrum or bar frame then re-renders only this
//! view and the shell root; the track table, the rail list, the sidebar and the dock are reused as recorded.
//!
//! The page keeps the layout: where an animated element used to be it places a [`slot`] of the same size, which
//! records its bounds and clip (content mask) every time the page lays out. The layer paints each recorded slot
//! there, clipped exactly as the original element was. A page's slots are dropped when it renders again
//! ([`begin_view`]: rows scrolled away disappear) and all of them when the stage changes ([`clear_all`]).
//!
//! Frame pacing: the bars and the FFT panel are time-based (shared clock), so the layer draws them at a capped
//! rate instead of on every vsync: the FFT panel every `MIN_DRAW_MS` like `FftPanel.svelte`, the bars at
//! [`BARS_FPS`]. The rail spectrum draws on each engine frame (`QueueRail.svelte` draws per `spectrum-frame`).
//! Nothing is scheduled while nothing moves (paused, stopped, no slot on screen).
use crate::app::PlayerModel;
use crate::views::nowplaying::NowPlayingView;
use gpui::{Bounds, ContentMask, Context, Entity, EntityId, IntoElement, Pixels, Render, Styled, Task, WeakEntity, Window, canvas};
use sonix_api::PlaybackStatus;
use std::cell::RefCell;
use std::time::{Duration, Instant};

/// What a slot shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// `TrackRow.svelte .now-playing`: three bars, animated while playing, static `scaleY(.33)` when paused.
    Bars { playing: bool },
    /// `QueueRail.svelte` deck spectrum: the latest engine frame as plain bars.
    RailSpectrum,
    /// `FftPanel.svelte`: spectrum bars with falling peak caps.
    NpSpectrum,
    /// `FftPanel.svelte`: L/R segment meters with peak hold.
    NpMeters,
}

/// Redraw rate of the now-playing bars. The keyframes move a bar 0.25..1 of 12 px in 0.45 s: 13.5 device px at 150 %,
/// about 1 device px per frame at this rate (2 at the ease-in-out peak); every frame costs a whole-window GPUI draw.
/// (The web animates them on the compositor at the display rate.)
pub const BARS_FPS: f64 = 30.0;

#[derive(Clone)]
struct Slot {
    owner: EntityId,
    kind: Kind,
    bounds: Bounds<Pixels>,
    mask: ContentMask<Pixels>,
}

thread_local! {
    static SLOTS: RefCell<Vec<Slot>> = const { RefCell::new(Vec::new()) };
}

/// Call at the top of `render` of every view that places slots: its slots of the last layout are dropped and the
/// ones its new layout records replace them (a cached view that is reused keeps its slots).
pub fn begin_view(window: &Window) {
    let v = window.current_view();
    SLOTS.with(|s| s.borrow_mut().retain(|x| x.owner != v));
}

/// The stage, the rail or the window state changed: every slot is re-recorded by the views that are shown.
pub fn clear_all() {
    SLOTS.with(|s| s.borrow_mut().clear());
}

/// The slots one layer paints: the top layer the rail spectrum (the rail is never covered by the stage), the stage
/// layer everything inside the stage (so the compact rail drawer, laid over the stage, covers them as before).
fn snapshot(top: bool) -> Vec<Slot> {
    SLOTS.with(|s| s.borrow().iter().filter(|x| (x.kind == Kind::RailSpectrum) == top).cloned().collect())
}

/// The placeholder for an animated element: size it like the element it replaces.
pub fn slot(kind: Kind) -> gpui::Canvas<()> {
    canvas(
        move |b, window, _| {
            let s = Slot { owner: window.current_view(), kind, bounds: b, mask: window.content_mask() };
            SLOTS.with(|v| {
                let mut v = v.borrow_mut();
                // a page lays out twice in one frame (re-render after a reuse): keep one entry per place
                v.retain(|x| !(x.owner == s.owner && x.bounds == s.bounds));
                v.push(s);
            });
        },
        |_, _, _, _| {},
    )
}

/// What the FFT panel draws this frame (`NowPlayingView::fft_step`).
pub struct FftDraw {
    pub bands: Vec<f32>,
    pub caps: Vec<f32>,
    pub level: [f32; 2],
    pub hold: [f32; 2],
    /// still playing or falling: draw again after `MIN_DRAW_MS`
    pub animate: bool,
}

pub struct LiveLayer {
    /// window-wide layer for the rail spectrum (`true`) or the stage layer (bars, Now Playing FFT)
    top: bool,
    player: Entity<PlayerModel>,
    np: WeakEntity<NowPlayingView>,
    /// the pending redraw and when it is due
    timer: Option<(Instant, Task<()>)>,
}

impl LiveLayer {
    pub fn new(top: bool, player: &Entity<PlayerModel>, np: WeakEntity<NowPlayingView>) -> Self {
        Self { top, player: player.clone(), np, timer: None }
    }

    /// Drop the pending redraw (minimize).
    pub fn stop(&mut self) {
        self.timer = None;
    }

    /// New data for a slot (an engine spectrum frame for the rail): draw now, or with the redraw already pending (at
    /// most one bar / FFT frame away), so the bars and the spectrum share frames instead of adding theirs up.
    pub fn poke(&mut self, cx: &mut Context<Self>) {
        if self.timer.is_none() {
            cx.notify();
        }
    }

    /// Redraw after `d` unless an earlier redraw is already pending.
    fn schedule(&mut self, d: Duration, cx: &mut Context<Self>) {
        let due = Instant::now() + d;
        if self.timer.as_ref().is_some_and(|(at, _)| *at <= due) {
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            delay(d).await;
            let _ = this.update(cx, |l, cx| {
                l.timer = None;
                cx.notify();
            });
        });
        self.timer = Some((due, task));
    }
}

impl Render for LiveLayer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::rcount::hit(if self.top { "live" } else { "live_stage" });
        let top = self.top;
        let slots = snapshot(top);
        let th = crate::app::theme(cx);
        let now = crate::clock::now();
        let frozen = crate::clock::frozen();
        let has = |f: &dyn Fn(Kind) -> bool| slots.iter().any(|s| f(s.kind));
        let seen = kinds(&slots);
        let mut next: Option<f64> = None;
        if !frozen && has(&|k| k == Kind::Bars { playing: true }) {
            next = Some(1.0 / BARS_FPS);
        }
        let fft = if has(&|k| matches!(k, Kind::NpSpectrum | Kind::NpMeters)) { self.np.update(cx, |np, cx| np.fft_step(cx)).ok() } else { None };
        if let Some(f) = &fft
            && f.animate
            && !frozen
        {
            let d = crate::metrics::spectrum::MIN_DRAW_MS as f64 / 1000.0;
            next = Some(next.map_or(d, |n: f64| n.min(d)));
        }
        let rail: Option<Vec<u8>> = if has(&|k| k == Kind::RailSpectrum) {
            let m = self.player.read(cx);
            if m.state.status == PlaybackStatus::Stopped { None } else { m.spectrum.as_ref().map(|f| f.bands.clone()) }
        } else {
            None
        };
        match next {
            // the timer wakes a little early: the redraw lands on the next vsync after it
            Some(s) => self.schedule(Duration::from_secs_f64((s - 0.002).max(0.004)), cx),
            None => self.timer = None,
        }
        canvas(
            |_, _, _| (),
            move |_, _, window, cx| {
                // the pages lay out (and record their slots) after this view rendered: paint what this frame recorded, and
                // render once more when a kind of slot appeared that the data above was not prepared for
                let slots = snapshot(top);
                if kinds(&slots) != seen {
                    window.request_animation_frame();
                }
                for s in &slots {
                    window.with_content_mask(Some(s.mask.clone()), |window| match s.kind {
                        Kind::Bars { playing } => {
                            crate::anim::paint_bars(window, s.bounds.origin, th.accent, crate::anim::bar_heights(playing, now as f32))
                        }
                        Kind::RailSpectrum => {
                            if let Some(b) = &rail {
                                crate::views::queue_rail::paint_rail_spectrum(window, s.bounds, th, b);
                            }
                        }
                        Kind::NpSpectrum => {
                            if let Some(f) = &fft {
                                crate::views::nowplaying::paint_spectrum(window, s.bounds, th, &f.bands, &f.caps);
                            }
                        }
                        Kind::NpMeters => {
                            if let Some(f) = &fft {
                                crate::views::nowplaying::paint_meters(window, cx, s.bounds, th, f.level, f.hold);
                            }
                        }
                    });
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element()
    }
}


/// Which kinds of slot are present (bit set), to notice a slot the render did not prepare data for.
fn kinds(slots: &[Slot]) -> u8 {
    slots.iter().fold(0, |m, s| {
        m | match s.kind {
            Kind::Bars { playing: false } => 1,
            Kind::Bars { playing: true } => 2,
            Kind::RailSpectrum => 4,
            Kind::NpSpectrum => 8,
            Kind::NpMeters => 16,
        }
    })
}

/// A precise one-shot delay. GPUI's `Timer` on Windows is a WinRT thread-pool timer (~15.6 ms granularity: a 20 ms
/// wait became 31 ms), so the layer's frame pacing sleeps on one small thread instead (`std::thread::sleep` uses a
/// high-resolution waitable timer) and wakes the UI task through a oneshot channel.
fn delay(d: Duration) -> impl std::future::Future<Output = ()> {
    use std::sync::OnceLock;
    use std::sync::mpsc::{Sender, channel};
    type Req = (Instant, futures::channel::oneshot::Sender<()>);
    static TICKER: OnceLock<Option<Sender<Req>>> = OnceLock::new();
    let tx = TICKER.get_or_init(|| {
        let (tx, rx) = channel::<Req>();
        std::thread::Builder::new()
            .name("live-ticker".into())
            .spawn(move || {
                while let Ok((due, done)) = rx.recv() {
                    let now = Instant::now();
                    if due > now {
                        std::thread::sleep(due - now);
                    }
                    let _ = done.send(());
                }
            })
            .ok()
            .map(|_| tx)
    });
    let (done, wait) = futures::channel::oneshot::channel();
    let sent = tx.as_ref().is_some_and(|tx| tx.send((Instant::now() + d, done)).is_ok());
    async move {
        if sent {
            let _ = wait.await;
        }
    }
}
