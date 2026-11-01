//! Button variants (`Button.svelte`: primary / secondary / danger / icon) and the
//! flat "ghost" icon button used by the top bar, dock and rows.
use super::{hard_shadow, icon, icon_g};
use crate::theme::{DISPLAY, Theme};
use gpui::{CursorStyle, ElementId, FontWeight, SharedString, Stateful, Div, div, prelude::*, px};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Primary,
    Secondary,
    Danger,
}

/// A text button, optionally with a leading icon. Attach `.on_click(...)` on the result.
pub fn button(id: impl Into<ElementId>, th: Theme, variant: Variant, label: &str, icon_name: Option<&str>, disabled: bool) -> Stateful<Div> {
    let (bg, fg, border, display) = match variant {
        Variant::Primary => (th.accent, th.accent_text, gpui::transparent_black(), true),
        Variant::Secondary => (th.mid, th.text, th.border, false),
        Variant::Danger => (th.clip, th.text, gpui::transparent_black(), true),
    };
    let text: SharedString = if display { label.to_uppercase().into() } else { label.to_string().into() };
    let mut b = div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .px(px(16.))
        .py(px(8.))
        .flex_none()
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_color(fg)
        .text_size(px(13.))
        .line_height(px(18.))
        .font_family(if display { DISPLAY } else { th.body_font })
        .font_weight(if display { FontWeight::BOLD } else { FontWeight::MEDIUM })
        .when(variant == Variant::Primary, |d| d.shadow(hard_shadow(&th, 2.)))
        .children(icon_name.map(|n| icon(n, 14., fg)))
        .child(text);
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
    let col = if disabled { th.border } else if on { th.accent_fg } else { th.muted };
    let mut d = div().id(id).flex().items_center().justify_center().size(px(box_size)).flex_none().group("hv").child(if disabled { icon(name, icon_size, col) } else { icon_g(name, icon_size, col, th.accent_fg) });
    if !disabled {
        d = d.cursor(CursorStyle::PointingHand).hover(move |s| s.text_color(th.accent_fg));
    }
    d
}
