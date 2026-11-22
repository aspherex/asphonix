//! Chromium-style smooth wheel scrolling for every scroll container (GPUI applies a wheel notch as one jump).
//!
//! * One notch = 100 px (3 GPUI lines x 100/3); precise deltas (touchpads: `ScrollDelta::Pixels`) are applied at once,
//!   unanimated, exactly like Chromium.
//! * A notch starts (or retargets) an animation of the container's offset on the frame clock (`crate::clock`): the
//!   curve is `cc/animation/scroll_offset_animation_curve.cc` of Chromium: `DurationBehavior::kInverseDelta`
//!   (duration = clamp(14 - delta / 60, 6, 12) frames / 60 = 100-200 ms, 200 ms up to 120 px), cubic-bezier
//!   ease-in-out (0.42, 0, 0.58, 1). A notch that arrives mid-animation moves the target by another notch and
//!   `UpdateTarget` retargets from the current position, keeping the current velocity (the bezier's first control
//!   point y becomes `x1 * slope`, `slope = velocity * new_duration / new_delta`). Not ported: the velocity-based
//!   upper bound of the retargeted duration.
//! * Plumbing without a GPUI patch: each container renders [`drive`] (ticks its animation, returns an invisible overlay
//!   that registers a hitbox), and [`host`] (last child of the root) installs one capture-phase wheel listener that
//!   picks the innermost hovered container that can still scroll in the wheel's direction, consumes the event and
//!   animates that container.
use crate::anim::cubic_bezier;
use gpui::{AnyElement, Bounds, DispatchPhase, Hitbox, HitboxBehavior, IntoElement, Pixels, ScrollDelta, ScrollHandle, ScrollWheelEvent, UniformListScrollHandle, Window, canvas, point, prelude::*, px};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Distance of one wheel notch (Chromium on Windows: 100 px).
pub const NOTCH_PX: f32 = 100.0;
/// GPUI reports a notch as this many lines with the default Windows setting.
const LINES_PER_NOTCH: f32 = 3.0;

const X1: f64 = 0.42;
const Y1: f64 = 0.0;
const X2: f64 = 0.58;
const Y2: f64 = 1.0;

/// `kInverseDelta` segment duration in seconds for a scroll of `delta` px.
pub fn inverse_delta_duration(delta: f64) -> f64 {
    (14.0 - delta.abs() / 60.0).clamp(6.0, 12.0) / 60.0
}

/// One running scroll animation (`ScrollOffsetAnimationCurve`), times in seconds on the frame clock.
#[derive(Clone, Debug)]
pub struct Curve {
    t0: f64,
    initial: f64,
    target: f64,
    /// seconds since `t0` where the current segment started / ends
    last_retarget: f64,
    total: f64,
    slope: Option<f64>,
}

impl Curve {
    pub fn new(now: f64, current: f64, target: f64) -> Self {
        Self { t0: now, initial: current, target, last_retarget: 0.0, total: inverse_delta_duration(target - current), slope: None }
    }

    pub fn target(&self) -> f64 {
        self.target
    }

    fn ease(&self, u: f64) -> f64 {
        let y1 = match self.slope {
            Some(s) => X1 * s.clamp(-1000.0, 1000.0),
            None => Y1,
        };
        cubic_bezier(X1 as f32, y1 as f32, X2 as f32, Y2 as f32, u as f32) as f64
    }

    fn value_rel(&self, t: f64) -> f64 {
        if t >= self.total {
            return self.target;
        }
        let u = ((t - self.last_retarget) / (self.total - self.last_retarget)).clamp(0.0, 1.0);
        self.initial + (self.target - self.initial) * self.ease(u)
    }

    /// (offset at `now`, finished)
    pub fn sample(&self, now: f64) -> (f64, bool) {
        let t = (now - self.t0).max(0.0);
        (self.value_rel(t), t >= self.total)
    }

    fn velocity_rel(&self, t: f64) -> f64 {
        let e = 0.001;
        (self.value_rel(t + e) - self.value_rel((t - e).max(0.0))) / (t + e - (t - e).max(0.0))
    }

    /// `UpdateTarget`: a new target while the animation runs.
    pub fn retarget(&mut self, now: f64, new_target: f64) {
        let t = (now - self.t0).max(self.last_retarget);
        let current = self.value_rel(t);
        let new_delta = new_target - current;
        if new_delta.abs() < 0.01 {
            self.initial = current;
            self.target = new_target;
            self.last_retarget = t;
            self.total = t;
            return;
        }
        let new_duration = inverse_delta_duration(new_delta);
        let velocity = self.velocity_rel(t);
        self.slope = Some(velocity * (new_duration / new_delta));
        self.initial = current;
        self.target = new_target;
        self.last_retarget = t;
        self.total = t + new_duration;
    }
}

/// Accessors of one scroll container; `scroll` is the positive distance scrolled from the top.
#[derive(Clone)]
pub struct Scrollable {
    key: usize,
    get: Rc<dyn Fn() -> f32>,
    set: Rc<dyn Fn(f32)>,
    max: Rc<dyn Fn() -> f32>,
}

impl Scrollable {
    /// A plain `overflow_y_scroll` div; `key` must be stable for the view (e.g. the address of its handle field).
    pub fn div(key: usize, h: &ScrollHandle) -> Self {
        let (g, s, m) = (h.clone(), h.clone(), h.clone());
        Self {
            key,
            get: Rc::new(move || -f32::from(g.offset().y)),
            set: Rc::new(move |v| {
                let o = s.offset();
                s.set_offset(point(o.x, px(-v)));
            }),
            max: Rc::new(move || f32::from(m.max_offset().height)),
        }
    }

    pub fn list(h: &UniformListScrollHandle) -> Self {
        let key = Rc::as_ptr(&h.0) as usize;
        let (g, s, m) = (h.clone(), h.clone(), h.clone());
        Self {
            key,
            get: Rc::new(move || -f32::from(g.0.borrow().base_handle.offset().y)),
            set: Rc::new(move |v| {
                let base = s.0.borrow().base_handle.clone();
                let o = base.offset();
                base.set_offset(point(o.x, px(-v)));
            }),
            max: Rc::new(move || {
                m.0.borrow().last_item_size.map(|i| (f32::from(i.contents.height) - f32::from(i.item.height)).max(0.0)).unwrap_or(0.0)
            }),
        }
    }
}

#[derive(Clone)]
struct Target {
    key: usize,
    hitbox: Hitbox,
    area: f32,
}

thread_local! {
    static ANIMS: RefCell<HashMap<usize, Curve>> = RefCell::new(HashMap::new());
    static SETTERS: RefCell<HashMap<usize, Scrollable>> = RefCell::new(HashMap::new());
    /// the registered wheel targets by container. Kept across frames: a cached view (`crate::part`) that is reused
    /// does not run its prepaint again, but its hitbox (same id) is replayed; a container that left the screen keeps
    /// an inert entry (its hitbox is never hit again) until the stage changes (`forget_targets`).
    static TARGETS: RefCell<HashMap<usize, Target>> = RefCell::new(HashMap::new());
    /// frame-clock time of the last wheel event each container consumed
    static LAST_WHEEL: RefCell<HashMap<usize, f64>> = RefCell::new(HashMap::new());
    /// (key, area) of the containers registered in this frame (cached views that are reused do not register again)
    static THIS_FRAME: RefCell<Vec<(usize, f32)>> = const { RefCell::new(Vec::new()) };
    /// a J/K scroll waiting for the next full frame, which lists exactly the containers on screen
    static PENDING_KEY_SCROLL: std::cell::Cell<Option<f32>> = const { std::cell::Cell::new(None) };
}

/// When the container `key` last took a wheel event. The host consumes wheel events in the capture phase, so a
/// container's own `on_scroll_wheel` never sees them (the lyrics panel pauses its auto-scroll on this instead).
pub fn last_wheel(key: usize) -> Option<f64> {
    LAST_WHEEL.with(|l| l.borrow().get(&key).copied())
}

/// Drop every running animation (minimize).
pub fn clear() {
    ANIMS.with(|a| a.borrow_mut().clear());
}

/// The stage changed: forget the wheel targets (the views on screen register theirs again in the next, full frame).
pub fn forget_targets() {
    TARGETS.with(|t| t.borrow_mut().clear());
}

/// Called from a view's `render`: advances the container's animation to the frame clock and returns the overlay that
/// registers it as a wheel target (add it as an absolute, full-size child of the container's relative wrapper).
pub fn drive(window: &mut Window, sc: Scrollable) -> AnyElement {
    let key = sc.key;
    let now = crate::clock::now();
    let step = ANIMS.with(|a| {
        let mut a = a.borrow_mut();
        let (v, done) = a.get(&key).map(|c| c.sample(now))?;
        if done {
            a.remove(&key);
        }
        Some((v, done))
    });
    if let Some((v, done)) = step {
        (sc.set)(v as f32);
        if !done && !crate::clock::frozen() {
            window.request_animation_frame();
        }
    }
    canvas(
        move |b: Bounds<Pixels>, window, _| {
            let hitbox = window.insert_hitbox(b, HitboxBehavior::Normal);
            TARGETS.with(|t| t.borrow_mut().insert(key, Target { key, hitbox, area: f32::from(b.size.width) * f32::from(b.size.height) }));
            THIS_FRAME.with(|t| t.borrow_mut().push((key, f32::from(b.size.width) * f32::from(b.size.height))));
            SETTERS.with(|s| s.borrow_mut().insert(key, sc));
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

/// `shortcuts.ts: scrollStage(delta)` (J/K outside Now Playing): scrolls the largest container on screen that can
/// scroll, instantly (`behavior: "auto"`). The caller refreshes the window: cached pages (`crate::part`) do not
/// register their containers on a reused frame, so the scroll is applied by [`host`] in the next, full frame, from
/// exactly the containers it lays out (no stale one from a closed dialog or a hidden panel).
pub fn scroll_largest(delta: f32) {
    PENDING_KEY_SCROLL.with(|p| p.set(Some(delta)));
}

fn apply_scroll_largest(mut cands: Vec<(usize, f32)>, delta: f32) -> bool {
    cands.sort_by(|a, b| b.1.total_cmp(&a.1));
    // the web only considers a viewport taller than half the stage (`clientHeight > target.clientHeight / 2`): never fall
    // through to the narrow sidebar / queue rail when the page itself cannot scroll
    let biggest = cands.first().map(|c| c.1).unwrap_or(0.0);
    cands.retain(|c| c.1 >= biggest * 0.5);
    for (key, _) in cands {
        let Some(sc) = SETTERS.with(|s| s.borrow().get(&key).cloned()) else { continue };
        let max = (sc.max)();
        if max <= 0.5 {
            continue;
        }
        ANIMS.with(|a| a.borrow_mut().remove(&key));
        (sc.set)(((sc.get)() + delta).clamp(0.0, max));
        return true;
    }
    false
}

/// The single wheel listener; put it last in the root so every container has registered by its prepaint.
pub fn host() -> AnyElement {
    canvas(
        |_, window, _| {
            let fresh = THIS_FRAME.with(|t| std::mem::take(&mut *t.borrow_mut()));
            if let Some(dy) = PENDING_KEY_SCROLL.with(|p| p.take())
                && apply_scroll_largest(fresh, dy)
            {
                // the offset changed after the containers laid out: draw it with one more full frame
                window.on_next_frame(|window, _| window.refresh());
            }
            TARGETS.with(|t| t.borrow().values().cloned().collect::<Vec<Target>>())
        },
        |_, mut targets: Vec<Target>, window, _| {
            targets.sort_by(|a, b| a.area.total_cmp(&b.area)); // innermost first
            window.on_mouse_event(move |ev: &ScrollWheelEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                let (dy, precise) = match ev.delta {
                    ScrollDelta::Lines(p) => (-p.y * (NOTCH_PX / LINES_PER_NOTCH), false),
                    ScrollDelta::Pixels(p) => (-f32::from(p.y), true),
                };
                if dy == 0.0 {
                    return;
                }
                for t in targets.iter().filter(|t| t.hitbox.should_handle_scroll(window)) {
                    let Some(sc) = SETTERS.with(|s| s.borrow().get(&t.key).cloned()) else { continue };
                    let (cur, max) = ((sc.get)() as f64, (sc.max)() as f64);
                    let pending = ANIMS.with(|a| a.borrow().get(&t.key).map(|c| c.target())).unwrap_or(cur);
                    // chain to the next container once this one has nothing left in the wheel's direction
                    if (dy > 0.0 && pending >= max - 0.5) || (dy < 0.0 && pending <= 0.5) {
                        continue;
                    }
                    let target = (pending + dy as f64).clamp(0.0, max);
                    LAST_WHEEL.with(|l| l.borrow_mut().insert(t.key, crate::clock::now()));
                    if precise {
                        ANIMS.with(|a| a.borrow_mut().remove(&t.key));
                        (sc.set)(target as f32);
                    } else {
                        let now = crate::clock::now();
                        ANIMS.with(|a| {
                            let mut a = a.borrow_mut();
                            match a.get_mut(&t.key) {
                                Some(c) => c.retarget(now, target),
                                None => {
                                    a.insert(t.key, Curve::new(now, cur, target));
                                }
                            }
                        });
                    }
                    window.refresh();
                    cx.stop_propagation();
                    return;
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_follow_the_inverse_delta_ramp() {
        let f = |d: f64| (inverse_delta_duration(d) * 60.0).round();
        assert_eq!(f(50.0), 12.0);
        assert_eq!(f(100.0), 12.0);
        assert_eq!(f(120.0), 12.0);
        assert_eq!(f(300.0), 9.0);
        assert_eq!(f(480.0), 6.0);
        assert_eq!(f(5000.0), 6.0);
        assert!((inverse_delta_duration(100.0) - 0.2).abs() < 1e-9);
    }

    #[test]
    fn one_notch_eases_in_and_out_over_200_ms() {
        let c = Curve::new(10.0, 0.0, 100.0);
        assert_eq!(c.sample(10.0), (0.0, false));
        let (mid, done) = c.sample(10.1);
        assert!((mid - 50.0).abs() < 0.5 && !done);
        assert!(c.sample(10.02).0 < 5.0, "slow start");
        assert_eq!(c.sample(10.201), (100.0, true));
        assert_eq!(c.sample(11.0), (100.0, true));
    }

    #[test]
    fn a_second_notch_retargets_without_a_jump_and_keeps_moving() {
        let mut c = Curve::new(0.0, 0.0, 100.0);
        let before = c.sample(0.1).0;
        c.retarget(0.1, 200.0);
        let after = c.sample(0.1).0;
        assert!((before - after).abs() < 1e-6, "position continuous at the retarget");
        assert!(c.sample(0.15).0 > after, "still moving");
        assert_eq!(c.sample(0.1 + 0.25).0, 200.0);
        assert_eq!(c.target(), 200.0);
    }
}
