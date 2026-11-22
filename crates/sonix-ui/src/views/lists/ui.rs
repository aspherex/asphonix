//! Shared building blocks of the list routes: the page header buttons (play, secondary, 40 px icon),
//! the text field, the panel frame and status lines.
use crate::metrics::{controls as ctl, lists as lm};
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_MD, HEADLINE_SM, MONO_CODE, MONO_DATA};
use crate::widgets::hard_shadow;
use crate::widgets::icon;
use crate::widgets::text_input::TextInput;
use gpui::{AnyElement, App, CursorStyle, Div, ElementId, Entity, IntoElement, SharedString, Stateful, Window, div, prelude::*, px};

/// Play button (`HEADLINE_SM`): accent fill, hard shadow, 40 px high, "PLAY" in the display face.
/// When this page's source is the loaded one (`pst.active()`) the button is an outline with a pause icon.
pub fn play_btn(id: impl Into<ElementId>, th: &Theme, label: &str, disabled: bool, pst: crate::app::PlayState) -> Stateful<Div> {
    let th = *th;
    let act = pst.active();
    let fg = if act { th.accent_fg } else { th.accent_text };
    let mut b = div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(lm::ACTIONS_GAP))
        .h(px(lm::BTN_H))
        .px(px(lm::PLAY_PAD_X))
        .flex_none()
        .bg(if act { th.mid } else { th.accent })
        .when(act, |d| d.border_1().border_color(th.accent))
        .text_color(fg)
        .shadow(hard_shadow(&th, ctl::BTN_SHADOW))
        .child(icon(pst.icon(), lm::ICON_PLAY, fg))
        .child(txt(&th, HEADLINE_SM, label.to_string()).color(fg));
    b = if disabled { b.opacity(0.5).cursor(CursorStyle::OperationNotAllowed) } else { b.tab_index(0).cursor(CursorStyle::PointingHand).hover(move |s| s.bg(if act { th.high } else { th.accent_hover })) };
    b
}

/// Secondary button: mid fill, 1 px frame (focus colour on hover), icon + body-font label.
pub fn sec_btn(id: impl Into<ElementId>, th: &Theme, icon_name: &str, label: &str, disabled: bool) -> Stateful<Div> {
    let th = *th;
    let mut b = div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(lm::ACTIONS_GAP))
        .h(px(lm::BTN_H))
        .px(px(lm::SEC_PAD_X))
        .flex_none()
        .bg(th.mid)
        .border_1()
        .border_color(th.border)
        .text_color(th.text)
        .child(icon(icon_name, lm::ICON_SEC, th.text))
        .child(txt(&th, BODY_MD, label.to_string()).color(th.text));
    b = if disabled { b.opacity(0.5).cursor(CursorStyle::OperationNotAllowed) } else { b.tab_index(0).cursor(CursorStyle::PointingHand).hover(move |s| s.border_color(th.focus)) };
    b
}

/// Icon button: 40x40, same frame as [`sec_btn`].
pub fn icon_btn40(id: impl Into<ElementId>, th: &Theme, icon_name: &str, disabled: bool) -> Stateful<Div> {
    let th = *th;
    let mut b = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(lm::BTN_H))
        .flex_none()
        .bg(th.mid)
        .border_1()
        .border_color(th.border)
        .text_color(th.text)
        .child(icon(icon_name, lm::ICON_SEC, th.text));
    b = if disabled { b.opacity(0.5).cursor(CursorStyle::OperationNotAllowed) } else { b.tab_index(0).cursor(CursorStyle::PointingHand).hover(move |s| s.border_color(th.focus)) };
    b
}

/// Status line (`LOADING PLAYLIST…`, `PLAYLIST UNAVAILABLE.`).
pub fn status_line(th: &Theme, text: &str, error: bool) -> Div {
    div().p(px(lm::STATUS_PAD)).child(txt(th, MONO_CODE, text.to_string()).color(if error { th.clip } else { th.muted }))
}

/// Text field: 36 px, `//` prefix, focus ring + hard shadow, placeholder when empty.
pub fn field(th: &Theme, input: &Entity<TextInput>, prefix: Option<&str>, placeholder: &str, window: &Window, cx: &App) -> Div {
    let th = *th;
    let focused = input.read(cx).focus_handle.is_focused(window);
    let empty = input.read(cx).content.is_empty();
    let ph: SharedString = placeholder.to_string().into();
    div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(crate::metrics::space::XS))
        .h(px(ctl::FIELD_H))
        .px(px(ctl::FIELD_PAD_X))
        .bg(th.low)
        .border_1()
        .border_color(if focused { th.focus } else { th.border })
        .when(focused, |d| d.shadow(hard_shadow(&th, ctl::BTN_SHADOW)))
        .children(prefix.map(|p| div().flex_none().child(txt(&th, MONO_DATA, p.to_string()).color(th.muted))))
        .child(div().flex_1().min_w_0().h_full().flex().items_center().child(input.clone()))
        .when(empty, |d| {
            d.child(
                div()
                    .absolute()
                    .left(px(ctl::FIELD_PAD_X))
                    .right(px(ctl::FIELD_PAD_X))
                    .top_0()
                    .bottom_0()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .child(div().flex_none().ml(px(prefix.map(prefix_w).unwrap_or(0.0))).child(txt(&th, MONO_DATA, ph).color(th.muted))),
            )
        })
}

/// Width of the `//` prefix span + gap (mono 12 px glyphs are 7.2 px wide with 0.02em tracking).
fn prefix_w(p: &str) -> f32 {
    p.chars().count() as f32 * (7.2 + 0.24) + crate::metrics::space::XS
}

/// Panel: frame + header (title in `MONO_DATA` caps, optional meta) + body slot.
pub fn panel(th: &Theme, title: &str, body: impl IntoElement) -> Div {
    let th = *th;
    div()
        .flex()
        .flex_col()
        .min_h_0()
        .bg(th.low)
        .border_1()
        .border_color(th.border)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(crate::metrics::space::SM))
                .px(px(crate::metrics::space::MD))
                .py(px(crate::metrics::space::SM))
                .bg(th.mid)
                .border_b_1()
                .border_color(th.border)
                .child(txt(&th, crate::tracking::TypeSpec { weight: 700.0, caps: true, ..MONO_DATA }, title.to_string()).color(th.text)),
        )
        .child(div().min_h_0().child(body))
}


/// A page that scrolls inside the stage (with the shared 10 px scrollbar):
/// `build(overflow)` gets whether the scrollbar currently takes its gutter (known from the previous layout).
pub fn scroll_page(th: Theme, id: &'static str, h: &gpui::ScrollHandle, key: usize, window: &mut Window, build: impl FnOnce(bool, &mut Window) -> AnyElement) -> AnyElement {
    let (bar, overflow) = crate::widgets::scrollbar::passive(th, h);
    let content = build(overflow, window);
    div()
        .relative()
        .size_full()
        .child(div().id(id).track_scroll(h).size_full().overflow_y_scroll().when(overflow, |d| d.pr(px(crate::metrics::tracks::SCROLLBAR_W))).child(content))
        .child(bar)
        // wheel notches animate smoothly (`widgets::smooth`); `key` = the address of the view's handle field
        .child(crate::widgets::smooth::drive(window, crate::widgets::smooth::Scrollable::div(key, h)))
        .into_any_element()
}

/// Content width of the stage page (viewport - sidebar - docked rail - 2 x page padding - scrollbar gutter).
pub fn page_content_width(window: &Window, overflow: bool) -> f32 {
    let vw = f32::from(window.viewport_size().width);
    let rail = if super::super::shell::Shell::rail_docked(vw, &crate::router::Route::Tracks) { crate::metrics::layout::RAIL_W } else { 0.0 };
    vw - super::super::shell::Shell::sidebar_width(vw) - rail - 2.0 * crate::metrics::lists::PAGE_PAD - if overflow { crate::metrics::tracks::SCROLLBAR_W } else { 0.0 }
}

/// Greedy word wrap of text (spaces collapse) into `width` px with the font of `spec` (block-box text).
pub fn wrap_text(th: &Theme, window: &Window, text: &str, spec: crate::tracking::TypeSpec, width: f32) -> Vec<String> {
    let fam = spec.family.name(th.body_font);
    let w = |t: &str| crate::text::natural_width(window, t, fam, spec.size, spec.weight, spec.tracking_px());
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split(' ') {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if !cur.is_empty() && w(&cand) > width {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        } else {
            cur = cand;
        }
    }
    lines.push(cur);
    lines
}

/// Select-box arrow: a 1.6 px chevron in the text colour, centred in a 20 px slot.
pub fn select_arrow(color: gpui::Hsla) -> impl IntoElement {
    gpui::canvas(
        |_, _, _| (),
        move |b, _, window, _| {
            let c = b.center();
            let mut p = gpui::PathBuilder::stroke(px(1.6));
            p.move_to(gpui::point(c.x - px(4.5), c.y - px(2.2)));
            p.line_to(gpui::point(c.x, c.y + px(2.3)));
            p.line_to(gpui::point(c.x + px(4.5), c.y - px(2.2)));
            if let Ok(path) = p.build() {
                window.paint_path(path, color);
            }
        },
    )
    .w(px(20.))
    .h(px(16.))
    .flex_none()
}
