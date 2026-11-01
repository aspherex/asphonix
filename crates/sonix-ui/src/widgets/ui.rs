//! Small shared components of the list routes: `Badge.svelte`, `SegmentedToggle.svelte`.
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

/// `Badge.svelte`: inline-flex, `padding: 0 4px`, 16 px line box, upper case, weight 700, mono-code.
pub fn badge(th: &Theme, tone: Tone, label: &str) -> Div {
    let (bg, fg): (Hsla, Hsla) = match tone {
        Tone::Default => (th.high, th.muted),
        Tone::Accent => (th.highest, th.accent_fg),
    };
    let spec = TypeSpec { line: m::BADGE_LINE, weight: 700.0, caps: true, ..MONO_CODE };
    div().flex().flex_row().items_center().flex_none().px(px(m::BADGE_PAD_X)).bg(bg).text_color(fg).child(txt(th, spec, label.to_string()).color(fg))
}

/// `SegmentedToggle.svelte`: inline-flex with a 1 px frame, one label-caps button per option, the active one on accent.
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
        // `.segment` is a <button>: letter-spacing is reset by the UA stylesheet
        let seg = div()
            .id((id, i))
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
