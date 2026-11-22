//! Horizontal slider / fader: a 24 px hit area around a 4 px rail,
//! accent fill, optional buffered bar, 10x18 cap. Drag is tracked with window-level
//! mouse listeners registered while dragging, so it keeps working outside the element.
use crate::metrics::fader as fd;
use crate::theme::Theme;
use gpui::{
    App, Bounds, CursorStyle, DispatchPhase, ElementId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Stateful, Div,
    Window, canvas, div, prelude::*, px, relative,
};
use std::cell::Cell;
use std::rc::Rc;

#[derive(Default)]
pub struct SliderState {
    pub bounds: Cell<Bounds<Pixels>>,
    pub dragging: Cell<bool>,
}
pub type SliderHandle = Rc<SliderState>;

pub fn new_handle() -> SliderHandle {
    Rc::new(SliderState::default())
}

/// `(frac, committed)`: `committed` is true on the final mouse-up.
pub type OnChange = Rc<dyn Fn(f32, bool, &mut Window, &mut App)>;

/// Fraction (0..1) of `x` inside `b` (pure; the unit test pins the clamping).
pub fn frac_at(x: f32, left: f32, width: f32) -> f32 {
    if width <= 0.0 { 0.0 } else { ((x - left) / width).clamp(0.0, 1.0) }
}

/// The interactive part only (no visuals): a full-width, relative box that tracks the
/// mouse (press + window-level drag) and reports `(frac, committed)`. Callers add the
/// height and the visuals: [`slider`] draws the fader rail, Now Playing a waveform.
pub fn track(id: impl Into<ElementId>, st: &SliderHandle, on_change: OnChange) -> Stateful<Div> {
    let (st_down, st_cv) = (st.clone(), st.clone());
    let cb_down = on_change.clone();
    let cb_cv = on_change;
    div()
        .id(id)
        .relative()
        .w_full()
        .cursor(CursorStyle::PointingHand)
        .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, window, cx| {
            let b = st_down.bounds.get();
            st_down.dragging.set(true);
            cb_down(frac_at(f32::from(ev.position.x), f32::from(b.left()), f32::from(b.size.width)), false, window, cx);
            window.refresh();
        })
        .child(
            canvas(
                {
                    let st = st_cv.clone();
                    move |b, _, _| st.bounds.set(b)
                },
                move |_, _, window, _| {
                    if !st_cv.dragging.get() {
                        return;
                    }
                    let (st1, cb1) = (st_cv.clone(), cb_cv.clone());
                    window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                        if phase == DispatchPhase::Bubble && st1.dragging.get() {
                            let b = st1.bounds.get();
                            cb1(frac_at(f32::from(ev.position.x), f32::from(b.left()), f32::from(b.size.width)), false, window, cx);
                            window.refresh();
                        }
                    });
                    let (st2, cb2) = (st_cv.clone(), cb_cv.clone());
                    window.on_mouse_event(move |ev: &MouseUpEvent, phase, window, cx| {
                        if phase == DispatchPhase::Bubble && st2.dragging.get() {
                            st2.dragging.set(false);
                            let b = st2.bounds.get();
                            cb2(frac_at(f32::from(ev.position.x), f32::from(b.left()), f32::from(b.size.width)), true, window, cx);
                            window.refresh();
                        }
                    });
                },
            )
            .absolute()
            .size_full(),
        )
}

/// The fader: 24 px hit area, 4 px rail, accent fill, optional buffered bar, 10x18 cap
/// (accent while dragging).
pub fn slider(id: impl Into<ElementId>, th: Theme, st: &SliderHandle, frac: f32, buffered: Option<f32>, on_change: OnChange) -> Stateful<Div> {
    let frac = frac.clamp(0.0, 1.0);
    let dragging = st.dragging.get();
    track(id, st, on_change)
        .h(px(fd::HIT_H))
        .child(
            div()
                .absolute()
                .top(px(fd::RAIL_TOP))
                .left_0()
                .w_full()
                .h(px(fd::RAIL_H))
                .bg(th.highest)
                .children(buffered.map(|b| div().absolute().top_0().left_0().h_full().w(relative(b.clamp(0., 1.))).bg(th.muted)))
                .child(div().absolute().top_0().left_0().h_full().w(relative(frac)).bg(th.accent)),
        )
        .child(
            div()
                .absolute()
                .top(px((fd::HIT_H - fd::CAP_H) / 2.0))
                .left(relative(frac))
                .ml(px(-fd::CAP_W / 2.0))
                .w(px(fd::CAP_W))
                .h(px(fd::CAP_H))
                .border_1()
                .border_color(gpui::black())
                .bg(if dragging { th.accent } else { th.text }),
        )
}

#[cfg(test)]
mod tests {
    use super::frac_at;

    #[test]
    fn fraction_is_clamped_to_the_rail() {
        assert_eq!(frac_at(100.0, 100.0, 200.0), 0.0);
        assert_eq!(frac_at(200.0, 100.0, 200.0), 0.5);
        assert_eq!(frac_at(300.0, 100.0, 200.0), 1.0);
        assert_eq!(frac_at(-50.0, 100.0, 200.0), 0.0);
        assert_eq!(frac_at(900.0, 100.0, 200.0), 1.0);
        assert_eq!(frac_at(5.0, 0.0, 0.0), 0.0);
    }
}
