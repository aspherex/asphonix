//! Small shared components of the list routes: badges and the segmented toggle.
use crate::metrics::lists as m;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{LABEL_CAPS, MONO_CODE, TypeSpec};
use gpui::{App, CursorStyle, Div, Hsla, Stateful, Window, div, prelude::*, px};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Default,
    Accent,
}

/// A badge: 4 px side padding, 16 px line box, upper case, weight 700, mono-code.
pub fn badge(th: &Theme, tone: Tone, label: &str) -> Div {
    let (bg, fg): (Hsla, Hsla) = match tone {
        Tone::Default => (th.high, th.muted),
        Tone::Accent => (th.highest, th.accent_fg),
    };
    let spec = TypeSpec { line: m::BADGE_LINE, weight: 700.0, caps: true, ..MONO_CODE };
    div().flex().flex_row().items_center().flex_none().px(px(m::BADGE_PAD_X)).bg(bg).text_color(fg).child(txt(th, spec, label.to_string()).color(fg))
}

/// A segmented toggle: a 1 px frame, one label-caps button per option, the active one on accent.
/// `on_select(value)` is called with the option's value.
pub fn segmented(id: &'static str, th: &Theme, options: &[(&str, &str)], value: &str, on_select: impl Fn(&str, &mut Window, &mut App) + Clone + 'static) -> Stateful<Div> {
    let th = *th;
    let n = options.len();
        let mut row = div().id(id).flex().flex_row().flex_none().border_1().border_color(th.border);
    for (i, (v, label)) in options.iter().enumerate() {
        let active = *v == value;
        let (v, label) = (v.to_string(), label.to_string());
        let on = on_select.clone();
        let (bg, fg) = if active { (th.accent, th.accent_text) } else { (th.low, th.muted) };
        // segment labels are buttons: no letter-spacing
        let seg = div()
            .id((id, i))
            .tab_index(0).focus(crate::widgets::focus_ring(th))
            .flex()
            .flex_row()
            .items_center()
            .px(px(m::SEG_PAD_X))
            .py(px(m::SEG_PAD_Y))
            .bg(bg)
            .text_color(fg)
            .cursor(CursorStyle::PointingHand)
            .when(i + 1 < n, |d| d.border_r_1().border_color(th.border))
            .child(txt(&th, LABEL_CAPS, label.clone()).color(fg))
            .on_click(move |_, w, cx| on(&v, w, cx));
        row = row.child(seg);
    }
    row
}
