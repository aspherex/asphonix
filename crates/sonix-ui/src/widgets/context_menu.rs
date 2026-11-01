//! Context menu (`ContextMenu.svelte`): right-click / "..." / Menu key opens it at a
//! point; it flips to stay inside the window (GPUI `anchored` with `SwitchAnchor`),
//! supports one submenu level, full keyboard navigation (Up/Down/Home/End/Enter/Left/
//! Right/Esc) and restores the previous focus when it closes.
use super::{hard_shadow, icon};
use crate::theme::Theme;
use gpui::{
    AnchoredPositionMode, AnyElement, App, Context, FocusHandle, Focusable, IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels,
    Point, Render, SharedString, Window, anchored, deferred, div, point, prelude::*, px,
};
use std::rc::Rc;

pub type Action = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub enum Kind {
    Action(Action),
    Submenu(Vec<MenuItem>),
    Separator,
}

#[derive(Clone)]
pub struct MenuItem {
    pub label: SharedString,
    pub icon: Option<&'static str>,
    pub hint: Option<SharedString>,
    pub danger: bool,
    pub disabled: bool,
    pub kind: Kind,
}

impl MenuItem {
    pub fn action(label: impl Into<SharedString>, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        Self { label: label.into(), icon: None, hint: None, danger: false, disabled: false, kind: Kind::Action(Rc::new(f)) }
    }
    pub fn submenu(label: impl Into<SharedString>, items: Vec<MenuItem>) -> Self {
        Self { label: label.into(), icon: None, hint: None, danger: false, disabled: false, kind: Kind::Submenu(items) }
    }
    pub fn separator() -> Self {
        Self { label: "".into(), icon: None, hint: None, danger: false, disabled: false, kind: Kind::Separator }
    }
    pub fn icon(mut self, i: &'static str) -> Self {
        self.icon = Some(i);
        self
    }
    pub fn hint(mut self, h: impl Into<SharedString>) -> Self {
        self.hint = Some(h.into());
        self
    }
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
    pub fn disabled(mut self, d: bool) -> Self {
        self.disabled = d;
        self
    }
    fn selectable(&self) -> bool {
        !self.disabled && !matches!(self.kind, Kind::Separator)
    }
}

/// Next selectable index from `from` in `dir` (+1/-1), wrapping; `None` if there is none.
pub fn step(items: &[MenuItem], from: Option<usize>, dir: i32) -> Option<usize> {
    let n = items.len() as i32;
    if n == 0 {
        return None;
    }
    let mut i = match from {
        Some(f) => f as i32,
        None if dir > 0 => -1,
        None => n,
    };
    for _ in 0..n {
        i = (i + dir).rem_euclid(n);
        if items[i as usize].selectable() {
            return Some(i as usize);
        }
    }
    None
}

struct Open {
    items: Vec<MenuItem>,
    pos: Point<Pixels>,
    active: Option<usize>,
    sub_open: bool,
    sub_active: Option<usize>,
    prev_focus: Option<FocusHandle>,
}

pub struct ContextMenu {
    th: Theme,
    open: Option<Open>,
    focus: FocusHandle,
}

const WIDTH: f32 = 220.0;

impl ContextMenu {
    pub fn new(th: Theme, cx: &mut Context<Self>) -> Self {
        Self { th, open: None, focus: cx.focus_handle() }
    }
    pub fn set_theme(&mut self, th: Theme) {
        self.th = th;
    }
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn show(&mut self, items: Vec<MenuItem>, pos: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let prev_focus = window.focused(cx);
        let active = step(&items, None, 1);
        self.open = Some(Open { items, pos, active, sub_open: false, sub_active: None, prev_focus });
        window.focus(&self.focus);
        cx.notify();
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(o) = self.open.take() {
            if let Some(f) = o.prev_focus {
                window.focus(&f);
            }
            cx.notify();
        }
    }

    fn activate(&mut self, sub: Option<usize>, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = self.open.as_mut() else { return };
        let item = match sub {
            Some(p) => match &o.items[p].kind {
                Kind::Submenu(v) => v.get(ix).cloned(),
                _ => None,
            },
            None => o.items.get(ix).cloned(),
        };
        let Some(item) = item else { return };
        if item.disabled {
            return;
        }
        match item.kind {
            Kind::Action(f) => {
                self.close(window, cx);
                f(window, cx);
            }
            Kind::Submenu(v) => {
                o.active = Some(ix);
                o.sub_open = true;
                o.sub_active = step(&v, None, 1);
                cx.notify();
            }
            Kind::Separator => {}
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = self.open.as_mut() else { return };
        let key = ev.keystroke.key.as_str();
        match key {
            "escape" => {
                if o.sub_open {
                    o.sub_open = false;
                    cx.notify();
                } else {
                    self.close(window, cx);
                }
            }
            "down" | "up" => {
                let dir = if key == "down" { 1 } else { -1 };
                if o.sub_open {
                    if let Some(a) = o.active {
                        if let Kind::Submenu(v) = &o.items[a].kind {
                            o.sub_active = step(v, o.sub_active, dir);
                        }
                    }
                } else {
                    o.active = step(&o.items, o.active, dir);
                }
                cx.notify();
            }
            "home" | "end" => {
                let (from, dir) = if key == "home" { (None, 1) } else { (None, -1) };
                if !o.sub_open {
                    o.active = step(&o.items, from, dir);
                    cx.notify();
                }
            }
            "right" => {
                if let Some(a) = o.active {
                    self.activate(None, a, window, cx);
                }
            }
            "left" => {
                if o.sub_open {
                    o.sub_open = false;
                    cx.notify();
                }
            }
            "enter" | "space" => {
                let (a, sub_open, sa) = (o.active, o.sub_open, o.sub_active);
                match (a, sub_open, sa) {
                    (Some(a), true, Some(s)) => self.activate(Some(a), s, window, cx),
                    (Some(a), false, _) => self.activate(None, a, window, cx),
                    _ => {}
                }
            }
            _ => {}
        }
        cx.stop_propagation();
    }

    fn panel(&self, items: &[MenuItem], active: Option<usize>, sub: Option<usize>, parent: Option<usize>, cx: &mut Context<Self>) -> gpui::Div {
        let th = self.th;
        let mut col = div()
            .occlude()
            .flex()
            .flex_col()
            .w(px(WIDTH))
            .py(px(4.))
            .bg(th.mid)
            .border_1()
            .border_color(th.border)
            .shadow(hard_shadow(&th, 4.))
            .text_size(px(12.))
            .line_height(px(16.))
            .font_family(th.body_font);
        for (i, it) in items.iter().enumerate() {
            if matches!(it.kind, Kind::Separator) {
                col = col.child(div().h(px(1.)).my(px(4.)).bg(th.border));
                continue;
            }
            let is_active = active == Some(i);
            let colr = if it.disabled {
                th.muted
            } else if it.danger {
                th.clip
            } else if is_active {
                th.accent_fg
            } else {
                th.text
            };
            let is_sub = matches!(it.kind, Kind::Submenu(_));
            let submenu = match (&it.kind, is_active && self.open.as_ref().map(|o| o.sub_open).unwrap_or(false)) {
                (Kind::Submenu(v), true) => {
                    let p = self.panel(v, sub, None, Some(i), cx);
                    Some(deferred(anchored().position_mode(AnchoredPositionMode::Local).position(point(px(WIDTH - 4.), px(-5.))).child(p)).with_priority(2))
                }
                _ => None,
            };
            let row = div()
                .id(("mi", i))
                .relative()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .px(px(12.))
                .py(px(4.))
                .text_color(colr)
                .when(is_active && !it.disabled, |d| d.bg(th.high))
                .when(!it.disabled, |d| d.cursor(gpui::CursorStyle::PointingHand))
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if !*hovered {
                        return;
                    }
                    if let Some(o) = this.open.as_mut() {
                        if parent.is_some() {
                            if o.sub_active != Some(i) {
                                o.sub_active = Some(i);
                                cx.notify();
                            }
                        } else if o.active != Some(i) {
                            o.active = Some(i);
                            o.sub_open = false;
                            if matches!(o.items[i].kind, Kind::Submenu(_)) && !o.items[i].disabled {
                                o.sub_open = true;
                                if let Kind::Submenu(v) = &o.items[i].kind {
                                    o.sub_active = step(v, None, 1);
                                }
                            }
                            cx.notify();
                        }
                    }
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    // clicks inside the submenu panel carry their own handler (index space of the sub items)
                    this.activate(parent, i, window, cx);
                }))
                .children(it.icon.map(|n| icon(n, 14., th.muted).text_color(if it.danger { th.clip } else if is_active { th.accent_fg } else { th.muted })))
                .child(div().flex_1().min_w_0().child(it.label.clone()))
                .children(is_sub.then(|| icon("chevron-right", 12., colr)))
                .children(submenu);
            col = col.child(row);
            if let Some(h) = &it.hint {
                col = col.child(div().px(px(12.)).pb(px(4.)).mt(px(-4.)).text_size(px(11.)).line_height(px(14.)).text_color(th.muted).child(h.clone()));
            }
        }
        col
    }
}

impl Focusable for ContextMenu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ContextMenu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(o) = self.open.as_ref() else { return div().into_any_element() };
        let (items, pos, active, sub) = (o.items.clone(), o.pos, o.active, o.sub_active);
        let panel: AnyElement = self
            .panel(&items, active, sub, None, cx)
            .id("context-menu")
            .track_focus(&self.focus)
            .key_context("ContextMenu")
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| this.on_key(ev, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(|this, _, window, cx| this.close(window, cx)))
            .into_any_element();
        deferred(anchored().position(pos).child(panel)).with_priority(1).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<MenuItem> {
        vec![
            MenuItem::action("a", |_, _| {}),
            MenuItem::separator(),
            MenuItem::action("b", |_, _| {}).disabled(true),
            MenuItem::action("c", |_, _| {}),
            MenuItem::submenu("d", vec![MenuItem::action("d1", |_, _| {})]),
        ]
    }

    #[test]
    fn keyboard_stepping_skips_separators_and_disabled_and_wraps() {
        let it = items();
        assert_eq!(step(&it, None, 1), Some(0));
        assert_eq!(step(&it, Some(0), 1), Some(3), "separator and disabled skipped");
        assert_eq!(step(&it, Some(3), 1), Some(4));
        assert_eq!(step(&it, Some(4), 1), Some(0), "wraps");
        assert_eq!(step(&it, Some(0), -1), Some(4), "wraps backwards");
        assert_eq!(step(&it, None, -1), Some(4));
        assert_eq!(step(&[], None, 1), None);
        assert_eq!(step(&[MenuItem::separator()], None, 1), None);
    }
}
