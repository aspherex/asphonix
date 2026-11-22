//! Small shared pieces of the library routes: `SegmentedToggle`, `Checkbox`, the art slot, width measuring and the
//! CSS `repeat(auto-fill, minmax(min, 1fr))` column maths.
use crate::art::{self, ArtHandle};
use crate::metrics::library as m;
use crate::probe::pid;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_SM, LABEL_CAPS};
use crate::widgets::icon;
use gpui::{AnyElement, App, Context, CursorStyle, Div, IntoElement, Window, canvas, div, prelude::*, px};
use sonix_api::Backend;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

/// `SegmentedToggle.svelte`: inline-flex, 1 px frame, `label-caps` segments (4 x 12 padding), the active one accent-filled.
/// `ids` are the parity ids of the segments (`<prefix>.$i`).
pub fn segmented<T: Copy + PartialEq + 'static>(
    th: &Theme,
    prefix: &'static str,
    options: &[(T, &'static str)],
    value: T,
    on_change: impl Fn(T, &mut Window, &mut App) + Clone + 'static,
) -> impl IntoElement {
    let th = *th;
    let n = options.len();
    let mut row = div().flex().flex_row().flex_none().border_1().border_color(th.border);
    for (i, (v, label)) in options.iter().enumerate() {
        let (v, active) = (*v, *v == value);
        let cb = on_change.clone();
        row = row.child(pid(
            format!("{prefix}.{i}"),
            div()
                .id((prefix, i))
                .flex()
                .items_center()
                .px(px(m::SEG_PAD_X))
                .py(px(m::SEG_PAD_Y))
                .cursor(CursorStyle::PointingHand)
                .bg(if active { th.accent } else { th.low })
                .when(i + 1 < n, |d| d.border_r_1().border_color(th.border))
                .child(txt(&th, LABEL_CAPS, *label).color(if active { th.accent_text } else { th.muted }))
                .on_click(move |_, window, cx| {
                    cb(v, window, cx);
                    window.refresh();
                }),
        ));
    }
    row
}

/// `Checkbox.svelte`: 14 px box + `body-sm` label, gap 8.
pub fn checkbox(th: &Theme, id: &'static str, checked: bool, label: &'static str, on_toggle: impl Fn(bool, &mut Window, &mut App) + 'static) -> impl IntoElement {
    let th = *th;
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .flex_none()
        .gap(px(m::CHECK_GAP))
        .cursor(CursorStyle::PointingHand)
        .child(
            div()
                .size(px(m::CHECK_BOX))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .border_1()
                .border_color(if checked { th.accent_fg } else { th.border })
                .bg(if checked { th.accent } else { th.low })
                .when(checked, |d| d.child(div().size(px(m::CHECK_MARK)).bg(th.accent_text))),
        )
        .child(txt(&th, BODY_SM, label).color(th.text))
        .on_click(move |_, window, cx| {
            on_toggle(!checked, window, cx);
            window.refresh();
        })
}

/// Records the width of the box it is placed in (absolute, full size) and asks for another frame when it changed,
/// which is how the grids learn their `clientWidth` (a `ResizeObserver` in the Svelte app).
pub fn measure(cell: &Rc<Cell<f32>>) -> AnyElement {
    measure_inner(None, cell)
}

thread_local! {
    /// Width of the shell's stage box (set every frame by the shell, which is never remounted).
    static STAGE_W: Cell<f32> = const { Cell::new(0.0) };
    /// Per measured box: how much narrower than the stage it was last time (padding, scrollbar gutter).
    static INSETS: std::cell::RefCell<std::collections::HashMap<&'static str, f32>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Called by the shell with its stage box width.
pub fn set_stage_width(w: f32) {
    STAGE_W.with(|s| s.set(w));
}

/// Start width for a view that is created on navigation. The Svelte app lays out a new route at its real width in
/// its first frame; a native view that starts at 0 and learns its width from `measure` one frame later draws once
/// at the wrong width and then re-flows (grid columns, tiles). Seed from the stage width minus this box's inset.
pub fn initial_width(key: &'static str) -> f32 {
    let stage = STAGE_W.with(|s| s.get());
    if stage <= 0.0 {
        return 0.0;
    }
    let inset = INSETS.with(|m| m.borrow().get(key).copied().unwrap_or(0.0));
    (stage - inset).max(0.0)
}

/// `measure` that also records the box's inset from the stage under `key` for [`initial_width`].
pub fn measure_keyed(key: &'static str, cell: &Rc<Cell<f32>>) -> AnyElement {
    measure_inner(Some(key), cell)
}

fn measure_inner(key: Option<&'static str>, cell: &Rc<Cell<f32>>) -> AnyElement {
    let cell = cell.clone();
    canvas(
        move |b, window, _| {
            let w = f32::from(b.size.width);
            if let Some(k) = key {
                let stage = STAGE_W.with(|s| s.get());
                if stage > 0.0 {
                    INSETS.with(|m| m.borrow_mut().insert(k, (stage - w).max(0.0)));
                }
            }
            if (cell.get() - w).abs() > 0.4 {
                cell.set(w);
                // prepaint: `refresh()` is a no-op while drawing; re-render the measuring view next frame (also when cached)
                window.request_animation_frame();
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
    .into_any_element()
}

/// `grid-template-columns: repeat(auto-fill, minmax(min, 1fr)); gap`: `(columns, cell width)` for a container of `width`.
pub fn auto_fill(width: f32, min: f32, gap: f32) -> (usize, f32) {
    let cols = (((width + gap) / (min + gap)).floor() as usize).max(1);
    (cols, ((width - gap * (cols as f32 - 1.0)) / cols as f32).max(1.0))
}

/// The art slot of a card/header: `surface-high` square with a 1 px idle frame (or none), the cover (or the empty-state
/// icon when the item has no cover id) clipped inside. `outer` is the box size in CSS px.
#[allow(clippy::too_many_arguments)]
pub fn art_slot<V: 'static>(
    th: &Theme,
    art: &ArtHandle,
    backend: &Arc<dyn Backend>,
    cover: Option<&str>,
    outer: f32,
    framed: bool,
    bucket: u32,
    fallback_icon: &str,
    fallback_size: f32,
    window: &mut Window,
    cx: &mut Context<V>,
) -> Div {
    let inner = if framed { outer - 2.0 } else { outer };
    let slot = div().relative().size(px(outer)).flex_none().overflow_hidden().flex().items_center().justify_center().bg(th.high).text_color(th.muted);
    let slot = if framed { slot.border_1().border_color(th.border) } else { slot };
    match cover {
        Some(id) => slot.child(art::cover_bucket(art, backend, Some(id), inner, bucket, window, cx)),
        None => slot.child(icon(fallback_icon, fallback_size, th.muted)),
    }
}

/// `main.stage { overflow-y: auto }` for a route that is a plain page: a scroll container with the 10 px scrollbar
/// gutter that only exists while the content overflows. The gutter decision comes from the previous frame, so a
/// canvas asks for one more frame when it changed.
pub struct PageScroll {
    pub handle: gpui::ScrollHandle,
    used: Rc<Cell<bool>>,
    /// When this page was created, and whether the same page (scroll id) overflowed last time.
    born: std::time::Instant,
    seed: Cell<Option<bool>>,
}

thread_local! {
    /// Last gutter state per page scroll id. A page created on navigation starts with it: the browser decides the
    /// gutter in the same layout pass that adds the content, while here the decision comes from the previous frame,
    /// so a page whose content arrives after its first frame would draw once without the 10 px gutter and then
    /// re-flow (grid columns tip over, right-aligned header controls shift).
    static GUTTERS: std::cell::RefCell<std::collections::HashMap<&'static str, bool>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// How long a remembered gutter is kept while the page's content may still be loading.
const GUTTER_SEED_GRACE: std::time::Duration = std::time::Duration::from_millis(600);

impl Default for PageScroll {
    fn default() -> Self {
        Self::new()
    }
}

impl PageScroll {
    pub fn new() -> Self {
        Self { handle: gpui::ScrollHandle::new(), used: Rc::new(Cell::new(false)), born: std::time::Instant::now(), seed: Cell::new(None) }
    }

    pub fn wrap(&self, window: &mut Window, th: &Theme, id: &'static str, content: impl IntoElement) -> AnyElement {
        let (bar, measured) = crate::widgets::scrollbar::passive(*th, &self.handle);
        let seed = match self.seed.get() {
            Some(s) => s,
            None => {
                let s = GUTTERS.with(|g| g.borrow().get(id).copied().unwrap_or(false));
                self.seed.set(Some(s));
                s
            }
        };
        let settled = self.born.elapsed() >= GUTTER_SEED_GRACE;
        // during the grace period a remembered gutter is only ever added to, never dropped (content may still be
        // loading); the watch below keeps frames coming while seed and measurement disagree, so it settles after it
        let overflow = if settled { measured } else { measured || seed };
        if measured || settled {
            GUTTERS.with(|g| g.borrow_mut().insert(id, measured));
        }
        self.used.set(overflow);
        let (handle, used) = (self.handle.clone(), self.used.clone());
        let watch = canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                let now = f32::from(handle.max_offset().height) > 0.5;
                if now != used.get() {
                    window.refresh();
                }
            },
        )
        .absolute()
        .size_full();
        div()
            .relative()
            .size_full()
            .child(
                div()
                    .id(id)
                    .track_scroll(&self.handle)
                    .size_full()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .when(overflow, |d| d.pr(px(crate::widgets::scrollbar::WIDTH)))
                    .child(content),
            )
            .child(watch)
            .child(bar)
            .child(crate::widgets::smooth::drive(window, crate::widgets::smooth::Scrollable::div(std::ptr::addr_of!(self.handle) as usize, &self.handle)))
            .into_any_element()
    }
}

/// `.genre-link` / `.artist-link` style text button: `base` colour, `hover` colour and an underline while hovered.
pub fn text_link(th: &Theme, id: &'static str, text: String, spec: crate::tracking::TypeSpec, (base, hover): (gpui::Hsla, gpui::Hsla), recolour: bool, on_click: impl Fn(&mut Window, &mut App) + 'static) -> impl IntoElement {
    let th = *th;
    let mut d = div().id(id).group("lnk").relative().flex().flex_none().cursor(CursorStyle::PointingHand).text_color(base);
    if recolour {
        d = d.hover(move |s| s.text_color(hover));
    }
    d.child(if spec.caps { txt(&th, spec, text) } else { txt(&th, spec, text).no_tracking() }) // a `<button class="genre-link">` resets the inherited letter-spacing; `.type-label-caps` on the button itself keeps it
        .child(div().absolute().left_0().right_0().bottom(px(spec.line * 0.12)).h(px(0.67)).group_hover("lnk", move |s| s.bg(if recolour { hover } else { base })))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            on_click(window, cx);
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_fill_matches_the_css_grid() {
        // Home card grid at 1440 x 900: stage 840 - 32 padding - 10 scrollbar = 798, minmax(150px, 1fr), gap 12
        let (cols, w) = auto_fill(798.0, 150.0, 12.0);
        assert_eq!(cols, 5);
        assert!((w - 150.0).abs() < 1e-4);
        assert_eq!(auto_fill(100.0, 150.0, 12.0).0, 1, "at least one column");
        // exactly one more column fits when width + gap reaches n * (min + gap)
        assert_eq!(auto_fill(5.0 * 150.0 + 4.0 * 12.0, 150.0, 12.0).0, 5);
        assert_eq!(auto_fill(5.0 * 150.0 + 4.0 * 12.0 - 0.1, 150.0, 12.0).0, 4);
    }
}
