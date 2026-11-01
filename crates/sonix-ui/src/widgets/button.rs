//! Button variants (`Button.svelte`: primary / secondary / danger / icon) and the
//! flat "ghost" icon button used by the top bar, dock and rows.
use super::{hard_shadow, icon, icon_g};
use crate::metrics::controls as ctl;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{Family, TypeSpec};
use gpui::{CursorStyle, Div, ElementId, Stateful, div, prelude::*, px};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Primary,
    Secondary,
    Danger,
}

/// A text button, optionally with a leading icon (`Button.svelte`). Attach `.on_click(...)` on the result.
/// Primary/danger: Space Grotesk 13/18 700 caps; secondary: body font 13/18 500.
pub fn button(id: impl Into<ElementId>, th: Theme, variant: Variant, label: &str, icon_name: Option<&str>, disabled: bool) -> Stateful<Div> {
    let (bg, fg, border, display) = match variant {
        Variant::Primary => (th.accent, th.accent_text, gpui::transparent_black(), true),
        Variant::Secondary => (th.mid, th.text, th.border, false),
        Variant::Danger => (th.clip, th.text, gpui::transparent_black(), true),
    };
    let spec = TypeSpec { family: if display { Family::Display } else { Family::Body }, size: 13.0, line: 18.0, tracking_em: 0.0, weight: if display { 700.0 } else { 500.0 }, caps: display };
    let mut b = div()
        .id(id)
        .group("btn")
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(ctl::BTN_GAP))
        .px(px(ctl::BTN_PAD_X))
        .py(px(ctl::BTN_PAD_Y))
        .flex_none()
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_color(fg)
        .when(variant == Variant::Primary, |d| d.shadow(hard_shadow(&th, ctl::BTN_SHADOW)))
        .children(icon_name.map(|n| icon(n, ctl::BTN_ICON, fg)))
        .child(txt(&th, spec, label.to_string()));
    if disabled {
        b = b.opacity(0.5).cursor(CursorStyle::OperationNotAllowed);
    } else {
        b = b.cursor(CursorStyle::PointingHand).hover(move |s| match variant {
            Variant::Primary => s.bg(th.accent_hover),
            Variant::Secondary => s.border_color(th.focus),
            Variant::Danger => s.opacity(0.9),
        });
        b = b.active(move |s| s.opacity(0.85));
    }
    b
}

/// Flat icon button (`.icon-link` / dock `.t-btn`): muted, accent on hover, accent when `on`.
/// The svg inherits the parent text colour, so the hover tint is just a style on the box.
pub fn ghost_icon(id: impl Into<ElementId>, th: Theme, name: &str, icon_size: f32, box_size: f32, on: bool, disabled: bool) -> Stateful<Div> {
    ghost_icon_with(id, th, name, icon_size, box_size, on, disabled, th.border)
}

/// `ghost_icon` with the colour a disabled button gets (`.icon-link:disabled`: border-idle; `.t-btn:disabled`:
/// muted). Both also carry `base.css button:disabled { opacity: 0.5 }`.
pub fn ghost_icon_with(id: impl Into<ElementId>, th: Theme, name: &str, icon_size: f32, box_size: f32, on: bool, disabled: bool, disabled_col: gpui::Hsla) -> Stateful<Div> {
    let col = if disabled { disabled_col } else if on { th.accent_fg } else { th.muted };
    let mut d = div().id(id).flex().items_center().justify_center().size(px(box_size)).flex_none().group("hv").child(if disabled { icon(name, icon_size, col) } else { icon_g(name, icon_size, col, th.accent_fg) });
    if disabled {
        d = d.opacity(0.5).cursor(CursorStyle::OperationNotAllowed);
    } else {
        d = d.cursor(CursorStyle::PointingHand).hover(move |s| s.text_color(th.accent_fg));
    }
    d
}
