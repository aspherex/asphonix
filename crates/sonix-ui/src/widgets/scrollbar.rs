//! Styled, draggable scrollbar for `uniform_list` (GPUI has none): 10 px track on
//! `surface-low`, thumb `surface-high` with a 2 px track-coloured border (base.css
//! `::-webkit-scrollbar`), `text-muted` on hover. Drag uses window-level mouse listeners.
use crate::theme::Theme;
use gpui::{
    AnyElement, App, Bounds, DispatchPhase, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, UniformListScrollHandle, Window,
    canvas, div, point, prelude::*, px,
};
use std::cell::Cell;
use std::rc::Rc;

pub const WIDTH: f32 = 10.0;
const MIN_THUMB: f32 = 24.0;

#[derive(Default)]
pub struct ScrollbarState {
    drag: Cell<Option<(f32, f32)>>, // (mouse y at grab, scroll offset at grab)
    track: Cell<Bounds<Pixels>>,
}
pub type ScrollbarHandle = Rc<ScrollbarState>;

pub fn new_handle() -> ScrollbarHandle {
    Rc::new(ScrollbarState::default())
}

/// Thumb geometry: `(thumb_len, thumb_top)` for a viewport/content/scroll triple (all px).
pub fn thumb(viewport: f32, content: f32, scroll: f32) -> Option<(f32, f32)> {
    if viewport <= 0.0 || content <= viewport + 0.5 {
        return None;
    }
    let len = (viewport * viewport / content).clamp(MIN_THUMB.min(viewport), viewport);
    let max_scroll = content - viewport;
    let top = (scroll.clamp(0.0, max_scroll) / max_scroll) * (viewport - len);
    Some((len, top))
}

/// Inverse of [`thumb`]: scroll offset for a given thumb top.
pub fn scroll_for_thumb_top(viewport: f32, content: f32, thumb_len: f32, top: f32) -> f32 {
    let travel = (viewport - thumb_len).max(1.0);
    (top / travel).clamp(0.0, 1.0) * (content - viewport).max(0.0)
}

fn metrics(h: &UniformListScrollHandle) -> Option<(f32, f32, f32)> {
    let s = h.0.borrow();
    let item = s.last_item_size?;
    let scroll = -f32::from(s.base_handle.offset().y);
    Some((f32::from(item.item.height), f32::from(item.contents.height), scroll))
}

pub fn scrollbar(id: &'static str, th: Theme, list: &UniformListScrollHandle, st: &ScrollbarHandle) -> AnyElement {
    let Some((viewport, content, scroll)) = metrics(list) else { return div().into_any_element() };
    let Some((len, top)) = thumb(viewport, content, scroll) else { return div().into_any_element() };
    let dragging = st.drag.get().is_some();
    let (st_down, st_cv, st_cv2) = (st.clone(), st.clone(), st.clone());
    let (h_down, h_cv) = (list.clone(), list.clone());
    div()
        .id(id)
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .w(px(WIDTH))
        .bg(th.low)
        .child(canvas(move |b, _, _| st_cv2.track.set(b), {
            move |_, _, window, _| {
                if st_cv.drag.get().is_none() {
                    return;
                }
                let (st1, h1) = (st_cv.clone(), h_cv.clone());
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, _cx: &mut App| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    if let (Some((y0, s0)), Some((viewport, content, _))) = (st1.drag.get(), metrics(&h1)) {
                        let len = thumb(viewport, content, 0.0).map(|t| t.0).unwrap_or(viewport);
                        let travel = (viewport - len).max(1.0);
                        let dy = f32::from(ev.position.y) - y0;
                        let new_scroll = (s0 + dy * (content - viewport) / travel).clamp(0.0, (content - viewport).max(0.0));
                        h1.0.borrow().base_handle.set_offset(point(px(0.), px(-new_scroll)));
                        window.refresh();
                    }
                });
                let st2 = st_cv.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, window, _cx: &mut App| {
                    if phase == DispatchPhase::Bubble && st2.drag.get().is_some() {
                        st2.drag.set(None);
                        window.refresh();
                    }
                });
            }
        }).absolute().size_full())
        .child(
            div()
                .id("scrollbar-thumb")
                .absolute()
                .left_0()
                .top(px(top))
                .w_full()
                .h(px(len))
                .bg(th.low)
                .child(div().size_full().m(px(0.)).border_2().border_color(th.low).bg(if dragging { th.muted } else { th.high }).hover(move |s| s.bg(th.muted)))
                .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, window: &mut Window, _cx| {
                    if let Some((_, _, scroll)) = metrics(&h_down) {
                        st_down.drag.set(Some((f32::from(ev.position.y), scroll)));
                        window.refresh();
                    }
                }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_thumb_when_everything_fits() {
        assert_eq!(thumb(500.0, 400.0, 0.0), None);
        assert_eq!(thumb(500.0, 500.0, 0.0), None);
        assert_eq!(thumb(0.0, 500.0, 0.0), None);
    }

    #[test]
    fn thumb_is_proportional_with_a_minimum() {
        let (len, top) = thumb(500.0, 1000.0, 0.0).unwrap();
        assert_eq!((len, top), (250.0, 0.0));
        let (len, top) = thumb(500.0, 1000.0, 500.0).unwrap();
        assert_eq!((len, top), (250.0, 250.0), "at the end the thumb touches the bottom");
        // 54,515 rows x 36 px: the proportional thumb would be ~0.1 px, the minimum applies
        let (len, _) = thumb(600.0, 54_515.0 * 36.0, 0.0).unwrap();
        assert_eq!(len, MIN_THUMB);
    }

    #[test]
    fn drag_math_inverts_the_geometry() {
        let (vp, content) = (600.0, 54_515.0 * 36.0);
        let (len, _) = thumb(vp, content, 0.0).unwrap();
        for scroll in [0.0, 12_345.0, content - vp] {
            let (_, top) = thumb(vp, content, scroll).unwrap();
            let back = scroll_for_thumb_top(vp, content, len, top);
            assert!((back - scroll).abs() < 0.5, "{scroll} vs {back}");
        }
        assert_eq!(scroll_for_thumb_top(vp, content, len, -50.0), 0.0);
        assert!((scroll_for_thumb_top(vp, content, len, 9999.0) - (content - vp)).abs() < 0.5);
    }
}

/// Passive scrollbar for plain scroll containers (`overflow-y: auto` + `track_scroll(handle)`): the same 10 px
/// track/thumb look as [`scrollbar`], drawn only when the content overflows (as Chromium does). Returns the
/// element and whether it overflows (callers reserve [`WIDTH`] on the right of their content in that case).
pub fn passive(th: Theme, h: &gpui::ScrollHandle) -> (AnyElement, bool) {
    let max = f32::from(h.max_offset().height);
    let view = f32::from(h.bounds().size.height);
    if max <= 0.5 || view <= 0.0 {
        return (div().into_any_element(), false);
    }
    let content = view + max;
    let scroll = -f32::from(h.offset().y);
    let (len, top) = thumb(view, content, scroll).unwrap_or((view, 0.0));
    let el = div()
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .w(px(WIDTH))
        .bg(th.low)
        .child(div().absolute().left_0().top(px(top)).w_full().h(px(len)).bg(th.low).child(div().size_full().border_2().border_color(th.low).bg(th.high)))
        .into_any_element();
    (el, true)
}
