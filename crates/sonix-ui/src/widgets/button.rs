//! Button variants (`Button.svelte`: primary / secondary / danger / icon) and the
//! flat "ghost" icon button used by the top bar, dock and rows.
use super::{hard_shadow, icon, icon_g};
use crate::anim;
use crate::metrics::{anim as m_anim, controls as ctl};
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{Family, TypeSpec};
use gpui::{CursorStyle, Div, ElementId, Stateful, Window, div, prelude::*, px};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Primary,
    Secondary,
    Danger,
}

/// A text button, optionally with a leading icon (`Button.svelte`). Attach `.on_click(...)` on the result.
/// Primary/danger: Space Grotesk 13/18 700 caps; secondary: body font 13/18 500.
pub fn button(id: impl Into<ElementId>, th: Theme, variant: Variant, label: &str, icon_name: Option<&str>, disabled: bool, window: &mut Window) -> Stateful<Div> {
    let (mut bg, fg, mut border, display) = match variant {
        Variant::Primary => (th.accent, th.accent_text, gpui::transparent_black(), true),
        Variant::Secondary => (th.mid, th.text, th.border, false),
        Variant::Danger => (th.clip, th.text, gpui::transparent_black(), true),
    };
    // `.btn { transition: border-color 0.1s, background-color 0.1s }` (hover colours below), eased with CSS `ease`
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (label, variant as u8).hash(&mut h);
        h.finish() as usize
    };
    if !disabled {
        let t = anim::transition(window, ("btn-hover", key), if anim::hovered(key) { 1.0 } else { 0.0 }, m_anim::BUTTON_TRANSITION);
        match variant {
            Variant::Primary => bg = anim::mix(th.accent, th.accent_hover, t),
            Variant::Secondary => border = anim::mix(th.border, th.focus, t),
            Variant::Danger => {}
        }
    }
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
        // `<button>` is a tab stop (document order = paint order); keyboard focus shows the hover border
        b = b.tab_index(0).focus(move |s| if super::key_nav() { s.border_color(th.focus) } else { s }).cursor(CursorStyle::PointingHand).on_hover(move |h, window, _| {
            anim::set_hovered(key, *h);
            window.refresh();
        });
        if variant == Variant::Danger {
            b = b.hover(|s| s.opacity(0.9));
        }
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
// flat parameter list on purpose: every call site is a one-line icon button
#[allow(clippy::too_many_arguments)]
pub fn ghost_icon_with(id: impl Into<ElementId>, th: Theme, name: &str, icon_size: f32, box_size: f32, on: bool, disabled: bool, disabled_col: gpui::Hsla) -> Stateful<Div> {
    let col = if disabled { disabled_col } else if on { th.accent_fg } else { th.muted };
    let mut d = div().id(id).flex().items_center().justify_center().size(px(box_size)).flex_none().group("hv").child(if disabled { icon(name, icon_size, col) } else { icon_g(name, icon_size, col, th.accent_fg) });
    if disabled {
        d = d.opacity(0.5).cursor(CursorStyle::OperationNotAllowed);
    } else {
        d = d.tab_index(0).focus(move |s| if super::key_nav() { s.bg(th.high) } else { s }).cursor(CursorStyle::PointingHand).hover(move |s| s.text_color(th.accent_fg));
    }
    d
}
