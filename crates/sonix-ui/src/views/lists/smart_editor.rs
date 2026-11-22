//! The smart-playlist editor: the modal rule builder for local smart playlists (preset chips, name, match mode,
//! rule rows, sort + limit, live preview of the real match count and five sample tracks).
use super::smart_rules::{self as sr, Kind};
use super::ui::{field, panel, wrap_text};
use super::ListsEvent;
use crate::app;
use crate::metrics::{controls as ctl, space};
use crate::router::Route;
use crate::text::txt;
use crate::theme::{MONO, Theme};
use crate::tracking::{BODY_SM, LABEL_CAPS, MONO_CODE, MONO_DATA};
use crate::widgets::button::{Variant, button};
use crate::widgets::context_menu::{ContextMenu, MenuItem};
use crate::widgets::text_input::{InputEvent, TextInput};
use crate::widgets::ui::segmented;
use crate::widgets::{hard_shadow, icon, icon_g};
use crate::widgets::tooltip::Tip;
use gpui::{AnyElement, ClickEvent, Context, CursorStyle, Entity, EventEmitter, IntoElement, MouseButton, Render, SharedString, Subscription, Timer, WeakEntity, Window, div, prelude::*, px, rgba};
use sonix_api::*;
use std::sync::Arc;
use std::time::Duration;

impl EventEmitter<ListsEvent> for SmartEditor {}

struct Row {
    field: SmartRuleField,
    op: SmartRuleOp,
    input: Entity<TextInput>,
}

struct Open {
    editing: Option<SmartSummary>,
    rows: Vec<Row>,
    mode: SmartMatchMode,
    sort: SmartSort,
    prev_focus: Option<gpui::FocusHandle>,
}

pub struct SmartEditor {
    backend: Arc<dyn Backend>,
    menu: Entity<ContextMenu>,
    open: Option<Open>,
    name: Entity<TextInput>,
    limit: Entity<TextInput>,
    presets: Vec<SmartPreset>,
    saving: bool,
    save_error: String,
    preview_total: Option<usize>,
    preview_tracks: Vec<Track>,
    preview_error: String,
    preview_seq: u64,
    seed: u32,
    /// scroll handle of the body, which is capped at the window height minus 96 px
    body_scroll: gpui::ScrollHandle,
    _subs: Vec<Subscription>,
}

fn no_value(op: SmartRuleOp) -> bool {
    matches!(op, SmartRuleOp::Never | SmartRuleOp::Unrated | SmartRuleOp::Yes | SmartRuleOp::No)
}

impl SmartEditor {
    pub fn new(backend: Arc<dyn Backend>, menu: &Entity<ContextMenu>, cx: &mut Context<Self>) -> Self {
        let th = app::theme(cx);
        let mk = |cx: &mut Context<Self>| cx.new(|cx| {
            let mut i = TextInput::new(cx, "", &th, MONO, MONO_DATA.size, MONO_DATA.line, MONO_DATA.weight);
            i.context_menu = false; // modal: the shell menu paints below the editor
            i
        });
        let (name, limit) = (mk(cx), mk(cx));
        let subs = vec![
            cx.subscribe(&name, |t: &mut Self, _, ev: &InputEvent, cx| match ev {
                InputEvent::Changed => t.changed(cx),
                InputEvent::Submit => t.save(cx),
            }),
            cx.subscribe(&limit, |t: &mut Self, _, ev: &InputEvent, cx| match ev {
                InputEvent::Changed => t.changed(cx),
                InputEvent::Submit => t.save(cx),
            }),
        ];
        Self {
            backend,
            menu: menu.clone(),
            open: None,
            name,
            limit,
            presets: Vec::new(),
            saving: false,
            save_error: String::new(),
            preview_total: None,
            preview_tracks: Vec::new(),
            preview_error: String::new(),
            preview_seq: 0,
            seed: 500_000,
            body_scroll: gpui::ScrollHandle::new(),
            _subs: subs,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }


    /// Open for a new playlist (`None`) or to edit one.
    pub fn show(&mut self, editing: Option<SmartSummary>, window: &mut Window, cx: &mut Context<Self>) {
        let th = app::theme(cx);
        let prev_focus = window.focused(cx);
        let (name, mode, rules, sort, limit) = match &editing {
            Some(p) => (p.input.name.clone(), p.input.match_mode, p.input.rules.clone(), p.input.sort, p.input.limit),
            None => ("".into(), SmartMatchMode::All, vec![SmartRule { field: SmartRuleField::Rating, op: SmartRuleOp::Gte, value: Some(SmartValue::Num(4.0)) }], SmartSort::Title, None),
        };
        self.name.update(cx, |i, cx| i.set_content(&name, cx));
        let lim = limit.map(|l| l.to_string()).unwrap_or_default();
        self.limit.update(cx, |i, cx| i.set_content(&lim, cx));
        let rows = rules.iter().map(|r| self.make_row(&th, r.field, r.op, value_text(&r.value), cx)).collect();
        self.open = Some(Open { editing, rows, mode, sort, prev_focus });
        self.body_scroll.set_offset(gpui::point(px(0.), px(0.)));
        self.save_error.clear();
        self.saving = false;
        let f = self.name.read(cx).focus_handle.clone();
        window.focus(&f);
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let p = cx.background_executor().spawn(async move { b.smart_presets() }).await;
            let _ = this.update(cx, |t, cx| {
                t.presets = p.unwrap_or_default();
                cx.notify();
            });
        })
        .detach();
        self.changed(cx);
    }

    fn make_row(&mut self, th: &Theme, field: SmartRuleField, op: SmartRuleOp, value: String, cx: &mut Context<Self>) -> Row {
        let th = *th;
        let input = cx.new(|cx| {
            let mut i = TextInput::new(cx, "", &th, MONO, MONO_DATA.size, MONO_DATA.line, MONO_DATA.weight);
            i.context_menu = false;
            i
        });
        input.update(cx, |i, cx| i.set_content(&value, cx));
        self._subs.push(cx.subscribe(&input, |t: &mut Self, _, ev: &InputEvent, cx| match ev {
            InputEvent::Changed => t.changed(cx),
            InputEvent::Submit => t.save(cx),
        }));
        Row { field, op, input }
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(o) = self.open.take() {
            if let Some(f) = o.prev_focus {
                window.focus(&f);
            }
            cx.notify();
        }
    }

    /// The payload, or the first reason there is none.
    fn draft(&self, cx: &gpui::App) -> Result<SmartInput, String> {
        let o = self.open.as_ref().ok_or_else(String::new)?;
        let name = self.name.read(cx).content.trim().to_string();
        if name.is_empty() {
            return Err("Give it a name.".into());
        }
        let mut rules = Vec::new();
        for r in &o.rows {
            let meta = sr::meta(r.field);
            if no_value(r.op) {
                rules.push(SmartRule { field: r.field, op: r.op, value: None });
                continue;
            }
            let t = r.input.read(cx).content.trim().to_string();
            if meta.kind == Kind::Text {
                if t.is_empty() {
                    return Err(format!("{}: enter a value", meta.label));
                }
                rules.push(SmartRule { field: r.field, op: r.op, value: Some(SmartValue::Text(t)) });
            } else {
                match t.parse::<f64>() {
                    Ok(n) if n.is_finite() => rules.push(SmartRule { field: r.field, op: r.op, value: Some(SmartValue::Num(n)) }),
                    _ => return Err(format!("{}: enter a number", meta.label)),
                }
            }
        }
        let ls = self.limit.read(cx).content.trim().to_string();
        let mut limit = None;
        if !ls.is_empty() {
            match ls.parse::<u32>() {
                Ok(l) if (1..=10_000).contains(&l) => limit = Some(l),
                _ => return Err("Limit must be a whole number from 1 to 10,000.".into()),
            }
        }
        Ok(SmartInput { name, match_mode: o.mode, rules, sort: o.sort, limit })
    }

    /// Anything changed: re-render and (debounced) refresh the live preview.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.preview_seq += 1;
        let seq = self.preview_seq;
        match self.draft(cx) {
            Err(_) => {
                self.preview_total = None;
                self.preview_tracks.clear();
                self.preview_error.clear();
            }
            Ok(input) => {
                let (b, seed) = (self.backend.clone(), self.seed);
                cx.spawn(async move |this, cx| {
                    Timer::after(Duration::from_millis(250)).await;
                    if !this.update(cx, |t, _| t.preview_seq == seq).unwrap_or(false) {
                        return;
                    }
                    let r = cx.background_executor().spawn(async move { b.smart_preview(input, seed, PageRequest { offset: 0, limit: 5 }) }).await;
                    let _ = this.update(cx, |t, cx| {
                        if t.preview_seq != seq {
                            return;
                        }
                        match r {
                            Ok(p) => {
                                t.preview_total = Some(p.total);
                                t.preview_tracks = p.items;
                                t.preview_error.clear();
                            }
                            Err(e) => {
                                t.preview_total = None;
                                t.preview_tracks.clear();
                                t.preview_error = if e.message.is_empty() { "Preview failed.".into() } else { e.message };
                            }
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Ok(input) = self.draft(cx) else { return };
        let Some(o) = &self.open else { return };
        if self.saving {
            return;
        }
        self.saving = true;
        self.save_error.clear();
        let (b, edit_id) = (self.backend.clone(), o.editing.as_ref().map(|p| p.id.clone()));
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    match &edit_id {
                        Some(id) => b.smart_update(id, input),
                        None => b.smart_create(input),
                    }
                })
                .await;
            let _ = this.update(cx, |t, cx| {
                t.saving = false;
                match r {
                    Ok(saved) => {
                        let creating = t.open.as_ref().map(|o| o.editing.is_none()).unwrap_or(false);
                        t.open = None;
                        cx.emit(ListsEvent::SmartChanged);
                        if creating {
                            cx.emit(ListsEvent::Navigate(Route::Smart(saved.id)));
                        }
                    }
                    Err(e) => t.save_error = if e.message.is_empty() { "Could not save.".into() } else { e.message },
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn pick_menu(&self, items: Vec<MenuItem>, pos: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let menu = self.menu.clone();
        cx.defer(move |cx| {
            let Some(w) = cx.active_window() else { return };
            let _ = w.update(cx, |_, window, cx| menu.update(cx, |m, cx| m.show(items, pos, window, cx)));
        });
    }

    /// Dropdown box look: 32 px, mono-data text and a 20 px arrow slot.
    fn select(&self, th: &Theme, id: impl Into<gpui::ElementId>, label: String, w: Option<f32>) -> gpui::Stateful<gpui::Div> {
        let th = *th;
        let d = div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .h(px(32.))
            .px(px(space::SM))
            .min_w_0()
            .bg(th.low)
            .border_1()
            .border_color(th.border)
            .text_color(th.text)
            .cursor(CursorStyle::PointingHand)
            .child(div().flex_1().min_w_0().flex().child(txt(&th, MONO_DATA, label).color(th.text).ellipsis()))
            .child(super::ui::select_arrow(th.text));
        match w {
            Some(w) => d.w(px(w)).flex_none(),
            None => d.flex_none(),
        }
    }

    fn rule_row(&mut self, th: Theme, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let o = self.open.as_ref().unwrap();
        let r = &o.rows[ix];
        let meta = sr::meta(r.field);
        let (field, op) = (r.field, r.op);
        let input = r.input.clone();
        let this: WeakEntity<Self> = cx.entity().downgrade();

        let t1 = this.clone();
        let field_sel = self.select(&th, ("ed-field", ix), meta.label.to_string(), Some(168.0)).on_click(cx.listener(move |t, ev: &ClickEvent, _, cx| {
            let items: Vec<MenuItem> = sr::FIELD_ORDER
                .iter()
                .map(|f| {
                    let (f, t1) = (*f, t1.clone());
                    MenuItem::action(sr::meta(f).label, move |_, cx| {
                        if let Some(t) = t1.upgrade() {
                            t.update(cx, |t, cx| t.set_field(ix, f, cx));
                        }
                    })
                })
                .collect();
            let p = ev.position();
            t.pick_menu(items, gpui::point(p.x, p.y + px(8.)), cx);
        }));
        let t2 = this.clone();
        let ops = meta.ops;
        let op_sel = self.select(&th, ("ed-op", ix), sr::op_label(field, op).to_string(), Some(176.0)).on_click(cx.listener(move |t, ev: &ClickEvent, _, cx| {
            let items: Vec<MenuItem> = ops
                .iter()
                .map(|o| {
                    let (o, t2) = (*o, t2.clone());
                    MenuItem::action(sr::op_label(field, o), move |_, cx| {
                        if let Some(t) = t2.upgrade() {
                            t.update(cx, |t, cx| t.set_op(ix, o, cx));
                        }
                    })
                })
                .collect();
            let p = ev.position();
            t.pick_menu(items, gpui::point(p.x, p.y + px(8.)), cx);
        }));
        let mut row = div().flex().flex_row().items_center().gap(px(space::SM)).child(field_sel).child(op_sel);
        if !no_value(op) {
            let empty = input.read(cx).content.is_empty();
            let ph = meta.placeholder.to_string();
            row = row
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h(px(32.))
                        .px(px(space::SM))
                        .flex()
                        .items_center()
                        .bg(th.low)
                        .border_1()
                        .border_color(th.border)
                        .child(div().flex_1().min_w_0().child(input))
                        .when(empty && !ph.is_empty(), |d| d.child(div().absolute().left(px(space::SM)).child(txt(&th, MONO_DATA, ph).color(th.muted)))),
                )
                .child(div().w(px(56.)).flex_none().flex().child(txt(&th, MONO_CODE, meta.unit.to_string()).color(th.muted)));
        } else {
            row = row.child(div().flex_1());
        }
        row.child(
            div()
                .id(("ed-rm", ix))
                .group("hv")
                .size(px(28.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .cursor(CursorStyle::PointingHand)
                .text_color(th.muted)
                .hover(move |s| s.text_color(th.clip))
                .child(icon_g("close", 13., th.muted, th.clip))
                .on_click(cx.listener(move |t, _, _, cx| {
                    if let Some(o) = t.open.as_mut() {
                        o.rows.remove(ix);
                    }
                    t.changed(cx);
                })),
        )
        .into_any_element()
    }

    fn set_field(&mut self, ix: usize, f: SmartRuleField, cx: &mut Context<Self>) {
        let meta = sr::meta(f);
        if let Some(r) = self.open.as_mut().and_then(|o| o.rows.get_mut(ix)) {
            r.field = f;
            r.op = meta.ops[0];
            let v = meta.def.map(|d| format!("{}", d as i64)).unwrap_or_default();
            r.input.update(cx, |i, cx| i.set_content(&v, cx));
        }
        self.changed(cx);
    }

    fn set_op(&mut self, ix: usize, op: SmartRuleOp, cx: &mut Context<Self>) {
        if let Some(r) = self.open.as_mut().and_then(|o| o.rows.get_mut(ix)) {
            r.op = op;
            let meta = sr::meta(r.field);
            if no_value(op) {
                r.input.update(cx, |i, cx| i.set_content("", cx));
            } else if r.input.read(cx).content.is_empty() && meta.kind == Kind::Num {
                let v = meta.def.map(|d| format!("{}", d as i64)).unwrap_or_default();
                r.input.update(cx, |i, cx| i.set_content(&v, cx));
            }
        }
        self.changed(cx);
    }

    fn apply_preset(&mut self, p: &SmartPreset, cx: &mut Context<Self>) {
        let th = app::theme(cx);
        self.name.update(cx, |i, cx| i.set_content(&p.input.name, cx));
        let lim = p.input.limit.map(|l| l.to_string()).unwrap_or_default();
        self.limit.update(cx, |i, cx| i.set_content(&lim, cx));
        let rows: Vec<Row> = p.input.rules.iter().map(|r| self.make_row(&th, r.field, r.op, value_text(&r.value), cx)).collect();
        if let Some(o) = self.open.as_mut() {
            o.rows = rows;
            o.mode = p.input.match_mode;
            o.sort = p.input.sort;
        }
        self.changed(cx);
    }
}

fn value_text(v: &Option<SmartValue>) -> String {
    match v {
        Some(SmartValue::Num(n)) if n.fract() == 0.0 => format!("{}", *n as i64),
        Some(SmartValue::Num(n)) => format!("{n}"),
        Some(SmartValue::Text(t)) => t.clone(),
        None => String::new(),
    }
}

impl Render for SmartEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        let Some(o) = &self.open else { return div().into_any_element() };
        let editing = o.editing.is_some();
        let (mode, sort, n_rows) = (o.mode, o.sort, o.rows.len());
        let vw = f32::from(window.viewport_size().width);
        let vh = f32::from(window.viewport_size().height);
        let box_w = (vw - 32.0).min(680.0);
        let inner_w = box_w - 2.0 - 2.0 * space::MD;
        let this: WeakEntity<Self> = cx.entity().downgrade();
        let draft = self.draft(cx);

        let mut body = div().flex().flex_col().gap(px(space::MD)).p(px(space::MD));
        let note = wrap_text(&th, window, "LOCAL ONLY — evaluated on this machine from your synced library; never sent to the server.", MONO_CODE, inner_w);
        body = body.child(div().flex().flex_col().children(note.into_iter().map(|l| txt(&th, MONO_CODE, l).color(th.accent_fg))));
        if !editing && !self.presets.is_empty() {
            let mut chips = div().flex().flex_row().flex_wrap().gap(px(space::XS)).mt(px(space::XS));
            for (i, p) in self.presets.iter().enumerate() {
                let pc = p.clone();
                chips = chips.child(
                    div()
                        .id(("ed-chip", i))
                        .tip(p.description.clone(), th)
                        .px(px(space::SM))
                        .py(px(4.))
                        .bg(th.high)
                        .border_1()
                        .border_color(th.border)
                        .text_color(th.text)
                        .cursor(CursorStyle::PointingHand)
                        .hover(move |s| s.border_color(th.focus).text_color(th.accent_fg))
                        .child(txt(&th, LABEL_CAPS, p.input.name.clone()))
                        .on_click(cx.listener(move |t, _, _, cx| t.apply_preset(&pc, cx))),
                );
            }
            body = body.child(div().flex().flex_col().child(div().flex().items_center().h(px(18.)).child(txt(&th, LABEL_CAPS, "Start from a preset").color(th.muted))).child(chips));
        }
        body = body.child(field(&th, &self.name, Some("//"), "Playlist name", window, cx));
        let t_m = this.clone();
        body = body.child(div().flex().flex_row().items_center().gap(px(space::MD)).child(txt(&th, LABEL_CAPS, "Match").color(th.muted)).child(segmented(
                "ed-match",
                &th,
                &[("all", "All rules"), ("any", "Any rule")],
                if mode == SmartMatchMode::All { "all" } else { "any" },
                move |v, _, cx| {
                    if let Some(t) = t_m.upgrade() {
                        t.update(cx, |t, cx| {
                            if let Some(o) = t.open.as_mut() {
                                o.mode = if v == "all" { SmartMatchMode::All } else { SmartMatchMode::Any };
                            }
                            t.changed(cx);
                        });
                    }
                },
            )));
        let mut rules = div().flex().flex_col().gap(px(space::SM));
        for i in 0..n_rows {
            let r = self.rule_row(th, i, cx);
            rules = rules.child(if i == 0 { r.into_any_element() } else { r });
        }
        let can_add = n_rows < 20;
        rules = rules.child(
            div()
                .flex()
                .flex_row()
                .child(
                    div()
                        .id("ed-add")
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(space::XS))
                        .px(px(space::SM))
                        .py(px(4.))
                        .text_color(th.muted)
                        .border_1()
                        .border_dashed()
                        .border_color(th.border)
                        .map(|d| if can_add { d.cursor(CursorStyle::PointingHand).hover(move |s| s.text_color(th.accent_fg).border_color(th.focus)) } else { d.opacity(0.5).cursor(CursorStyle::OperationNotAllowed) })
                        .child(icon("add", 12., th.muted))
                        .child(txt(&th, LABEL_CAPS, "Add rule"))
                        .when(can_add, |d| {
                            d.on_click(cx.listener(|t, _, _, cx| {
                                let th = app::theme(cx);
                                let meta = sr::meta(SmartRuleField::Rating);
                                let v = meta.def.map(|d| format!("{}", d as i64)).unwrap_or_default();
                                let row = t.make_row(&th, SmartRuleField::Rating, meta.ops[0], v, cx);
                                if let Some(o) = t.open.as_mut() {
                                    o.rows.push(row);
                                }
                                t.changed(cx);
                            }))
                        }),
                ),
        );
        if n_rows == 0 {
            rules = rules.child(txt(&th, BODY_SM, "No rules: every track in your library matches.").color(th.muted));
        }
        body = body.child(rules);
        // sort + limit
        let t_s = this.clone();
        let sort_sel = self.select(&th, "ed-sort", sr::sort_label(sort).to_string(), None).on_click(cx.listener(move |t, ev: &ClickEvent, _, cx| {
            let items: Vec<MenuItem> = sr::SORT_ORDER
                .iter()
                .map(|s| {
                    let (s, t_s) = (*s, t_s.clone());
                    MenuItem::action(sr::sort_label(s), move |_, cx| {
                        if let Some(t) = t_s.upgrade() {
                            t.update(cx, |t, cx| {
                                if let Some(o) = t.open.as_mut() {
                                    o.sort = s;
                                }
                                t.changed(cx);
                            });
                        }
                    })
                })
                .collect();
            let p = ev.position();
            t.pick_menu(items, gpui::point(p.x, p.y + px(8.)), cx);
        }));
        let lim_empty = self.limit.read(cx).content.is_empty();
        let limit_box = div()
            .relative()
            .w(px(190.))
            .h(px(32.))
            .px(px(space::SM))
            .flex()
            .items_center()
            .bg(th.low)
            .border_1()
            .border_color(th.border)
            .child(div().flex_1().min_w_0().child(self.limit.clone()))
            .when(lim_empty, |d| d.child(div().absolute().left(px(space::SM)).child(txt(&th, MONO_DATA, "no limit").color(th.muted))));
        body = body.child(div()
                .flex()
                .flex_row()
                .gap(px(space::LG))
                .child(div().flex().flex_col().gap(px(space::XS)).child(txt(&th, LABEL_CAPS, "Sort").color(th.muted)).child(sort_sel))
                .child(div().flex().flex_col().gap(px(space::XS)).child(txt(&th, LABEL_CAPS, "Limit (optional)").color(th.muted)).child(limit_box)));
        // preview
        let mut prev = div().flex().flex_col().min_h(px(40.)).px(px(space::MD)).py(px(space::SM)).bg(th.low).border_1().border_color(th.border);
        match (&draft, &self.preview_error, self.preview_total) {
            (Err(e), _, _) => prev = prev.child(txt(&th, MONO_CODE, e.clone()).color(th.muted)),
            (_, e, _) if !e.is_empty() => prev = prev.child(txt(&th, MONO_CODE, e.clone()).color(th.clip)),
            (_, _, None) => prev = prev.child(txt(&th, MONO_CODE, "EVALUATING…").color(th.muted)),
            (_, _, Some(n)) => {
                prev = prev.child(
                    div()
                        .flex()
                        .flex_row()
                        .child(txt(&th, crate::tracking::TypeSpec { weight: 700.0, ..MONO_DATA }, sonix_api::format::fmt_int(n as u64)).color(th.accent_fg))
                        .child(txt(&th, MONO_DATA, " MATCHING TRACKS").color(th.text)),
                );
                if !self.preview_tracks.is_empty() {
                    prev = prev.child(div().flex().flex_col().mt(px(space::XS)).children(self.preview_tracks.iter().map(|t| div().flex().min_w_0().child(txt(&th, BODY_SM, format!("{} — {}", t.artist, t.title)).color(th.muted).ellipsis()))));
                }
            }
        }
        body = body.child(prev);
        if !self.save_error.is_empty() {
            body = body.child(txt(&th, MONO_CODE, self.save_error.clone()).color(th.clip));
        }
        let cancel = button("ed-cancel", th, Variant::Secondary, "Cancel", None, false, window).on_click(cx.listener(|t, _, window, cx| t.close(window, cx)));
        let ok = button("ed-ok", th, Variant::Primary, if editing { "Save" } else { "Create" }, None, draft.is_err() || self.saving, window).on_click(cx.listener(|t, _, _, cx| t.save(cx)));
        body = body.child(div().flex().flex_row().justify_end().gap(px(space::SM)).child(cancel).child(ok));

        let title: SharedString = if editing { "Edit smart playlist" } else { "New smart playlist" }.into();
        let _ = ctl::FIELD_H;
        let (bar, overflow) = crate::widgets::scrollbar::passive(th, &self.body_scroll);
        let body = div()
            .relative()
            .min_h_0()
            .child(body.id("ed-body").track_scroll(&self.body_scroll).overflow_y_scroll().max_h(px(vh - 96.0)).when(overflow, |d| d.pr(px(space::MD + crate::metrics::tracks::SCROLLBAR_W))))
            .child(bar)
            .child(crate::widgets::smooth::drive(window, crate::widgets::smooth::Scrollable::div(std::ptr::addr_of!(self.body_scroll) as usize, &self.body_scroll)));
        div()
            .id("editor-overlay")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x0a0a0cb3))
            .on_mouse_down(MouseButton::Left, cx.listener(|t, _, window, cx| t.close(window, cx)))
            .child(
                div()
                    .id("editor-box")
                    .w(px(box_w))
                    .max_h(px(vh - 32.0))
                    .flex()
                    .flex_col()
                    .bg(th.mid)
                    .shadow(hard_shadow(&th, 4.0))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(panel(&th, &title, body)),
            )
            .into_any_element()
    }
}
