//! The history page: locally recorded plays, grouped by local day (the backend interleaves day-header
//! rows into one flat paged list), single selection, no column header.
use super::menus::{self, TrackMenu};
use super::table::{Arrows, Fetch, RowItem, Table, TableCfg, TableEvent};
use super::ListsEvent;
use crate::app::{self, PlayerModel};
use crate::metrics::lists as lm;
use crate::text::txt;
use crate::tracking::{BODY_SM, HEADLINE_MD, MONO_CODE};
use crate::widgets::button::{Variant, button};
use crate::widgets::context_menu::{ContextMenu, MenuItem};
use gpui::{Context, Entity, EventEmitter, IntoElement, Render, Subscription, Window, div, prelude::*, px};
use sonix_api::format as fmt;
use sonix_api::*;
use std::rc::Rc;
use std::sync::Arc;

impl EventEmitter<ListsEvent> for HistoryView {}

pub struct HistoryView {
    backend: Arc<dyn Backend>,
    menu: Entity<ContextMenu>,
    table: Entity<Table>,
    clearing: bool,
    _subs: Vec<Subscription>,
}

impl HistoryView {
    pub fn new(backend: Arc<dyn Backend>, player: &Entity<PlayerModel>, menu: &Entity<ContextMenu>, cx: &mut Context<Self>) -> Self {
        let cfg = TableCfg {
            head: false,
            album_column: true,
            draggable: false,
            add_btn: false,
            remove_btn: false,
            multi: false,
            arrows: Arrows::Select,
            action_count: 1,
            empty: "Nothing here yet. Tracks you listen to (past the scrobble threshold) appear here.",
        };
        let table = cx.new(|cx| Table::new(backend.clone(), player, cfg, cx));
        let subs = vec![cx.subscribe(&table, Self::on_table)];
        let mut this = Self { backend, menu: menu.clone(), table, clearing: false, _subs: subs };
        this.reload(cx);
        this
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let fetch: Fetch = Arc::new(|b, offset, limit| {
            let p = b.history_get(PageRequest { offset, limit })?;
            let items = p
                .items
                .into_iter()
                .map(|it| match it {
                    HistoryItem::Day { day, count } => RowItem::Day { day, count },
                    HistoryItem::Track { track, played_at } => RowItem::Track { t: track, label: Some(fmt::format_clock(played_at)) },
                })
                .collect();
            Ok(Paged { items, total: p.total, offset: p.offset })
        });
        self.table.update(cx, |t, cx| t.set_paged(fetch, cx));
        cx.notify();
    }

    /// A like changed: re-read the resident pages in place (the scroll position stays).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.table.update(cx, |t, cx| t.refresh_visible(cx));
    }

    pub fn release(&mut self, cx: &mut Context<Self>) {
        self.table.update(cx, |t, _| t.release());
    }


    fn play(&self, id: String, cx: &mut Context<Self>) {
        app::call(cx, move |b| {
            let _ = b.player_play_queue(PlayQueueRequest { track_ids: vec![id], start_index: 0 });
        });
    }

    fn on_table(&mut self, _: Entity<Table>, ev: &TableEvent, cx: &mut Context<Self>) {
        match ev {
            TableEvent::Play(ix) => {
                if let Some(id) = self.table.read(cx).track_at(*ix).map(|t| t.id.clone()) {
                    self.play(id, cx);
                }
            }
            TableEvent::Enqueue(ix) => {
                if let Some(id) = self.table.read(cx).track_at(*ix).map(|t| t.id.clone()) {
                    app::call(cx, move |b| {
                        let _ = b.player_enqueue(EnqueueRequest { track_ids: vec![id], position: EnqueuePosition::End });
                    });
                }
            }
            TableEvent::Star(ix) => {
                let Some(t) = self.table.read(cx).track_at(*ix).cloned() else { return };
                let (id, starred) = (t.id, t.starred.is_some());
                let b = self.backend.clone();
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .spawn(async move {
                            let req = StarRequest { id, kind: StarTargetType::Track };
                            let _ = if starred { b.star_remove(req) } else { b.star_add(req) };
                        })
                        .await;
                    let _ = this.update(cx, |t, cx| t.refresh(cx));
                })
                .detach();
            }
            TableEvent::Menu { ix, pos } => {
                let Some(t) = self.table.update(cx, |t, _| {
                    t.menu_indices(*ix);
                    t.track_at(*ix).cloned()
                }) else {
                    return;
                };
                let this = cx.entity().downgrade();
                let t2 = this.clone();
                let id = t.id.clone();
                let play_now: Rc<dyn Fn(&mut gpui::App)> = Rc::new(move |cx| {
                    if let Some(h) = t2.upgrade() {
                        let id = id.clone();
                        h.update(cx, |h, cx| h.play(id, cx));
                    }
                });
                let items: Vec<MenuItem> = menus::track_menu_items(this, TrackMenu { tracks: vec![t], at: *pos, play_now, remove_from_playlist: None });
                let (menu, pos) = (self.menu.clone(), *pos);
                cx.defer(move |cx| {
                    let Some(w) = cx.active_window() else { return };
                    let _ = w.update(cx, |_, window, cx| menu.update(cx, |m, cx| m.show(items, pos, window, cx)));
                });
            }
            TableEvent::SelectionChanged => cx.notify(),
            _ => {}
        }
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        crate::overlays::confirm(cx, "Clear history", "Remove all recorded plays from this device? This cannot be undone.", "Clear", true, move |cx| {
            let b = app::backend(cx);
            cx.spawn(async move |cx| {
                cx.background_executor().spawn(async move { let _ = b.history_clear(); }).await;
                if let Some(t) = this.upgrade() {
                    let _ = t.update(cx, |h, cx| {
                        h.clearing = false;
                        h.reload(cx);
                    });
                }
            })
            .detach();
        });
        self.clearing = true;
        cx.notify();
    }
}

impl Render for HistoryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        let total = self.table.read(cx).total();
        let _ = BODY_SM;
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(lm::HISTORY_GAP))
            .flex_none()
            .child(txt(&th, HEADLINE_MD, "History").color(th.text))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(lm::HISTORY_GAP))
                    .child(txt(&th, MONO_CODE, "RECORDED LOCALLY · LAST 10,000 PLAYS").color(th.muted))
                    .child(button("clear-history", th, Variant::Secondary, "Clear history", Some("trash"), total == 0 || self.clearing, window).on_click(cx.listener(|this, _, _, cx| this.clear(cx)))),
            );
        div().size_full().flex().flex_col().gap(px(lm::HISTORY_GAP)).p(px(lm::PAGE_PAD)).child(header).child(self.table.clone())
    }
}
