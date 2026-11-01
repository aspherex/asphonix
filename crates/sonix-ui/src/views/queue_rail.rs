//! Queue rail (`QueueRail.svelte`, phase-1 subset): "Live Transport Deck" (current track, spectrum,
//! position/output row), the "Next from <label> · Shuffle" window with the real "+ N more" footer.
//! Docked at >= 1440 px, an overlay drawer below. Not yet ported: drag-reorder, multi-select, remove,
//! the "Next in queue" user section, flow rows (docs/PORTING.md). Sizes: `crate::metrics::rail`.
use super::library::entity_link::{Kind as LinkKind, entity_link};
use super::shell::{OpenMenuKey, PlaySelected, SelDown, SelUp, SelectAll};
use crate::app::{self, PlayerEvent, PlayerModel};
use crate::art::{self, ArtHandle};
use crate::metrics::{layout, rail as rl, space, tracks as tr};
use crate::probe::pid;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_LG, BODY_MD, BODY_SM, LABEL_CAPS, MONO_CODE, MONO_DATA};
use crate::widgets::context_menu::ContextMenu;
use crate::widgets::icon;
use crate::selection::Selection;
use gpui::{Bounds, ClickEvent, Context, CursorStyle, DragMoveEvent, Entity, FocusHandle, Focusable, IntoElement, MouseDownEvent, Render, Subscription, Window, canvas, div, fill, point, prelude::*, px, size};
use sonix_api::format as fmt;
use sonix_api::*;
use std::sync::Arc;

pub enum RailEvent {
    Navigate(crate::router::Route),
    Played,
}
impl gpui::EventEmitter<RailEvent> for QueueRail {}

pub struct QueueRail {
    backend: Arc<dyn Backend>,
    art: ArtHandle,
    player: Entity<PlayerModel>,
    state: Option<QueueState>,
    scroll: gpui::ScrollHandle,
    focus: FocusHandle,
    /// edge auto-scroll while dragging a row (px per frame, + = down), `startAutoScroll`
    drag_dy: f32,
    menu: Entity<ContextMenu>,
    /// multi-select over absolute queue indices (`QueueRail.svelte`: Ctrl/Shift click, remove acts on the selection)
    sel: Selection,
    _sub: Subscription,
}

/// Per-row layout flags of `queue_row`: distance from the playing row, playing, Flow source (3-line rows).
#[derive(Clone, Copy)]
struct RowFlags {
    n: usize,
    active: bool,
    flow_rows: bool,
}

/// Internal drag of a queue row (reorder). External drops use `crate::dnd::SonixDrag`.
#[derive(Clone)]
struct QueueDrag {
    index: usize,
    label: String,
}
struct QueueGhost {
    label: String,
    th: Theme,
}
impl Render for QueueGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().px(px(8.)).py(px(4.)).bg(self.th.accent).child(txt(&self.th, MONO_DATA, self.label.clone()).weight(700.0).color(self.th.accent_text))
    }
}

impl QueueRail {
    pub fn new(backend: Arc<dyn Backend>, art: ArtHandle, player: &Entity<PlayerModel>, menu: &Entity<ContextMenu>, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(player, |this: &mut Self, _, _: &PlayerEvent, cx| this.refresh(cx));
        let mut s = Self { backend, art, player: player.clone(), state: None, scroll: gpui::ScrollHandle::new(), menu: menu.clone(), focus: cx.focus_handle(), drag_dy: 0.0, sel: Selection::default(), _sub: sub };
        s.refresh(cx);
        s
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let q = cx.background_executor().spawn(async move { b.player_get_queue(PageRequest { offset: 0, limit: 100 }) }).await;
            let _ = this.update(cx, |t, cx| {
                t.state = q.ok();
                cx.notify();
            });
        })
        .detach();
    }

    pub fn release(&mut self) {
        self.state = None;
    }
}

/// `deck-header`: 8/12 padding, `surface-mid`, bottom rule, label caps (+ right-aligned count).
fn header(th: &Theme, id: &'static str, label: String, right: Option<String>) -> impl IntoElement {
    pid(
        id,
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .justify_between()
            .px(px(rl::HEADER_PAD_X))
            .py(px(rl::HEADER_PAD_Y))
            .bg(th.mid)
            .border_b_1()
            .border_color(th.border)
            .child(txt(th, LABEL_CAPS, label).color(th.text))
            .children(right.map(|r| txt(th, MONO_CODE, r).color(th.muted))),
    )
}

impl Render for QueueRail {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.drag_dy != 0.0 {
            if cx.has_active_drag() {
                let off = self.scroll.offset();
                let max = f32::from(self.scroll.max_offset().height);
                self.scroll.set_offset(point(off.x, px((f32::from(off.y) - self.drag_dy).clamp(-max, 0.0))));
                window.request_animation_frame();
            } else {
                self.drag_dy = 0.0;
            }
        }
        let th: Theme = app::theme(cx);
        let (ps, frame) = {
            let m = self.player.read(cx);
            (m.state.clone(), m.spectrum.clone())
        };
        let backend = self.backend.clone();
        let vw = f32::from(window.viewport_size().width);
        let drawer = vw < crate::metrics::bp::RAIL_DOCKED_FROM;
        let q = self.state.clone();
        let cur = q.as_ref().map(|q| q.current_index).unwrap_or(-1);
        let total = q.as_ref().map(|q| q.total).unwrap_or(0);
        let readout = if ps.track.is_some() {
            format!("{}{}", fmt::output_readout(&ps, "NO OUTPUT"), if ps.bit_perfect { " · BIT-PERFECT" } else { "" })
        } else {
            String::new()
        };

        // ---- deck
        let deck_body = match &ps.track {
            Some(t) => {
                let cover = art::cover(&self.art, &backend, t.cover_art.as_deref(), 56.0, window, cx);
                let bands: Option<Vec<u8>> = if ps.status == PlaybackStatus::Stopped { None } else { frame.map(|f| f.bands) };
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(space::SM))
                            .p(px(space::MD))
                            .child(pid("rail.deck.art", div().size(px(56.)).flex_none().border_1().border_color(th.border).bg(th.high).overflow_hidden().child(cover)))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .justify_center()
                                    .min_w_0()
                                    .gap(px(2.))
                                    .child(pid("rail.deck.title", div().flex().min_w_0().child(txt(&th, BODY_LG, t.title.clone()).color(th.text).ellipsis())))
                                    .child(pid("rail.deck.artist", div().flex().child(entity_link(&th, (LinkKind::Artist, t.artist_id.as_deref()), &t.artist, MONO_DATA, th.muted, "deck", window)))),
                            ),
                    )
                    .child(pid("rail.deck.spectrum", div().h(px(64.)).px(px(space::MD)).child(rail_spectrum(th, bands))))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap(px(space::SM))
                            .pt(px(space::XS))
                            .px(px(space::MD))
                            .pb(px(space::MD))
                            .child(txt(&th, MONO_CODE, fmt::format_duration(Some(ps.position_sec))).color(th.muted))
                            .child(div().flex().min_w_0().child(txt(&th, MONO_CODE, readout).color(th.muted).ellipsis()))
                            .child(txt(&th, MONO_CODE, fmt::format_duration(Some(ps.duration_sec))).color(th.muted)),
                    )
                    .into_any_element()
            }
            None => pid("rail.deck.empty", div().p(px(space::MD)).child(txt(&th, BODY_SM, "Nothing playing.").color(th.muted))).into_any_element(),
        };
        let deck = div().flex_none().border_b_1().border_color(th.border).child(header(&th, "rail.deckheader", "Live Transport Deck".into(), Some(format!("{total} QUEUED")))).child(deck_body);

        // ---- queue sections: "Next in queue" (user-queued, always ahead) and "Next from <source>" (the rolling window)
        let label = match q.as_ref().and_then(|q| q.source.as_ref()) {
            Some(s) => format!("Next from {}{}", s.label, match s.mode {
                SourceMode::Shuffle => " · Shuffle".to_string(),
                SourceMode::Flow => fmt::flow_mode_label(s.arc),
                SourceMode::InOrder => String::new(),
            }),
            None if q.as_ref().map(|q| q.items.iter().enumerate().any(|(i, it)| (i as i64) > cur && it.origin == QueueItemOrigin::User)).unwrap_or(false) => "Queue".to_string(),
            None => "Next in buffer".to_string(),
        };
        let is_user = |i: usize, it: &QueueItem| (i as i64) > cur && it.origin == QueueItemOrigin::User;
        let src_info = q.as_ref().and_then(|q| q.source.clone());
        let flow_rows = src_info.as_ref().map(|s| s.mode == SourceMode::Flow).unwrap_or(false);
        let flow_hint = match &src_info {
            Some(s) if s.mode == SourceMode::Flow => s.flow_coverage.as_ref().map(|c| fmt::coverage_hint(c, flow_noun(s.kind))).unwrap_or_default(),
            _ => String::new(),
        };
        let queue_section = if total == 0 {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(header(&th, "rail.queue.header", "Next in buffer".into(), None))
                .child(pid("rail.queue.empty", div().p(px(space::MD)).child(txt(&th, BODY_SM, "Queue is empty.").color(th.muted))))
        } else {
            let (bar, overflow) = crate::widgets::scrollbar::passive(th, &self.scroll);
            let mut list = div().id("queue-list").track_scroll(&self.scroll).size_full().overflow_y_scroll().flex().flex_col().when(overflow, |d| d.pr(px(tr::SCROLLBAR_W)));
            let mut user_rows = Vec::new();
            if let Some(q) = &q {
                for (i, it) in q.items.iter().enumerate() {
                    if (i as i64) < cur {
                        continue;
                    }
                    let n = i - cur.max(0) as usize;
                    if is_user(i, it) {
                        user_rows.push(self.queue_row(&th, it, i, RowFlags { n, active: false, flow_rows }, window, cx));
                    } else {
                        list = list.child(self.queue_row(&th, it, i, RowFlags { n, active: (i as i64) == cur, flow_rows }, window, cx));
                    }
                }
            }
            let footer = q.as_ref().and_then(|q| q.source.as_ref()).filter(|s| s.remaining > 0).map(|s| {
                div()
                    .flex_none()
                    .py(px(space::XS))
                    .px(px(space::MD))
                    .border_t_1()
                    .border_color(th.border)
                    .child(txt(&th, BODY_SM, format!("+ {} more from {}", fmt::fmt_int(s.remaining as u64), s.label)).color(th.muted))
            });
            let user_section = (!user_rows.is_empty()).then(|| {
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .border_b_1()
                    .border_color(th.border)
                    // external drops on "Next in queue" = Play next (`data-drop-next`)
                    .on_drop(cx.listener(|this, d: &crate::dnd::SonixDrag, _, cx| this.drop_external(d.0.clone(), EnqueuePosition::Next, cx)))
                    .child(header(&th, "rail.user.header", "Next in queue".into(), None))
                    .children(user_rows)
            });
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .children(user_section)
                .child(header(&th, "rail.queue.header", label, None))
                .children((!flow_hint.is_empty()).then(|| div().flex_none().px(px(space::MD)).py(px(space::XS)).border_b_1().border_color(th.border).child(crate::typo::ts(&th, MONO_CODE).text_color(th.muted).child(flow_hint.clone()))))
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .track_focus(&self.focus)
                        .key_context("TrackList QueueList")
                        .on_action(cx.listener(|this, _: &SelDown, _, cx| this.key_move(1, cx)))
                        .on_action(cx.listener(|this, _: &SelUp, _, cx| this.key_move(-1, cx)))
                        .on_action(cx.listener(|this, _: &SelectAll, _, cx| this.select_all_main(cx)))
                        .on_action(cx.listener(|this, _: &PlaySelected, _, cx| {
                            if let Some(c) = this.sel.cursor() {
                                app::call(cx, move |b| {
                                    let _ = b.player_jump_to(c);
                                });
                                cx.emit(RailEvent::Played);
                            }
                        }))
                        .on_action(cx.listener(|this, _: &OpenMenuKey, window, cx| {
                            if let Some(c) = this.sel.cursor() {
                                // anchored to the selected row (`openContextMenuFor(row)`: left edge, 4 px under it)
                                let pos = match this.row_bounds(c) {
                                    Some(b) => point(b.left(), b.bottom() + px(4.)),
                                    None => point(px(1200.), px(300.)),
                                };
                                this.open_menu(c, pos, window, cx);
                            }
                        }))
                        // `startAutoScroll`: the list scrolls while a dragged row sits within 36 px of its top/bottom edge
                        .on_drag_move::<QueueDrag>(cx.listener(|this, ev: &DragMoveEvent<QueueDrag>, _window, cx| {
                            const EDGE: f32 = 36.0;
                            let (y, top, bottom) = (f32::from(ev.event.position.y), f32::from(ev.bounds.top()), f32::from(ev.bounds.bottom()));
                            let dy = if y < top + EDGE && y > top - EDGE {
                                -((top + EDGE - y) / 4.0).ceil()
                            } else if y > bottom - EDGE && y < bottom + EDGE {
                                ((y - (bottom - EDGE)) / 4.0).ceil()
                            } else {
                                0.0
                            };
                            if dy != this.drag_dy {
                                this.drag_dy = dy;
                                // NOT window.request_animation_frame(): outside render/paint GPUI has no current view
                                // and unwraps None (panic = abort: the crash at fault offset 0x65e1e5). notify()
                                // re-renders, and render keeps requesting frames while drag_dy != 0.
                                cx.notify();
                            }
                        }))
                        // everywhere else on the rail = Add to queue
                        .on_drop(cx.listener(|this, d: &crate::dnd::SonixDrag, _, cx| this.drop_external(d.0.clone(), EnqueuePosition::End, cx)))
                        .child(list)
                        .child(bar)
                        .child(crate::widgets::smooth::drive(window, crate::widgets::smooth::Scrollable::div(std::ptr::addr_of!(self.scroll) as usize, &self.scroll))),
                )
                .children(footer)
        };

        pid(
            "rail",
            div()
                .w(px(layout::RAIL_W))
                .h_full()
                .flex_none()
                .flex()
                .flex_col()
                .min_h_0()
                .bg(if drawer { th.low } else { th.chrome })
                .border_l_1()
                .border_color(th.frame)
                .child(deck)
                .child(queue_section),
        )
    }
}

/// `QueueRail.svelte: draw()`: bars of `barW = w / n` with a 1 px gap, `max(1, v * h)` tall, on a canvas whose bitmap
/// is the CSS size (so Chromium upsamples it by the device scale factor).
fn rail_spectrum(th: Theme, bands: Option<Vec<u8>>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |b, _, window, _| {
            let Some(bands) = &bands else { return };
            let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
            let n = bands.len().max(1);
            let bar_w = w / n as f32;
            for (i, v) in bands.iter().enumerate() {
                let bh = (*v as f32 / 255.0 * h).max(1.0);
                window.paint_quad(fill(Bounds::new(point(b.origin.x + px(i as f32 * bar_w), b.origin.y + px(h - bh)), size(px(bar_w - 1.0), px(bh))), th.meter));
            }
        },
    )
    .size_full()
}

impl QueueRail {
    /// One queue row (`QueueRail.svelte .row`): drag handle, title/artist, time, "more", remove. Click selects (Ctrl/Shift
    /// extend), double-click plays; rows are drag sources (reorder) and drop targets (reorder / add).
    fn queue_row(&self, th: &Theme, it: &QueueItem, i: usize, flags: RowFlags, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let RowFlags { n, active, flow_rows } = flags;
        let th = *th;
        let t = &it.track;
        let selected = self.sel.contains(i);
        let idx = i;
        let drag = QueueDrag { index: i, label: t.title.clone() };
        pid(
            format!("rail.row.{n}"),
            div()
                .id(("qrow", i))
                .relative()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(space::XS))
                .h(px(if flow_rows && it.origin != QueueItemOrigin::User { 56.0 } else { 44.0 }))
                .px(px(space::SM))
                .border_b_1()
                .border_color(th.mid)
                .cursor(CursorStyle::PointingHand)
                .group("qrow")
                .when(active, |d| d.bg(th.mid))
                .child(icon("drag", 13., th.muted))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().flex().min_w_0().child(txt(&th, BODY_MD, t.title.clone()).color(th.text).ellipsis()))
                        .child(div().flex().min_w_0().child(entity_link(&th, (LinkKind::Artist, t.artist_id.as_deref()), &t.artist, MONO_CODE, th.muted, &format!("rail{i}"), window)))
                        .children((flow_rows && it.origin != QueueItemOrigin::User).then(|| div().flex().min_w_0().h(px(14.)).child(txt(&th, MONO_CODE, fmt::flow_line(it.flow.as_ref())).color(th.accent_fg).ellipsis()))),
                )
                .child(div().flex_none().child(txt(&th, MONO_CODE, fmt::format_duration(Some(t.duration))).color(th.muted)))
                .child(pid(format!("rail.row.{n}.more"),
                    div()
                        .id(("qmore", i))
                        .group("hv")
                        .size(px(20.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(crate::widgets::icon_g("more", 11., th.muted, th.text))
                        .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            let p = ev.position();
                            this.open_menu(idx, gpui::point(p.x, p.y + px(8.)), window, cx);
                        })),
                ))
                .child(pid(format!("rail.row.{n}.remove"),
                    div()
                        .id(("qremove", i))
                        .group("hv")
                        .size(px(20.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(crate::widgets::icon_g("close", 11., th.muted, th.clip))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            let ix = this.sel.for_context_menu(idx);
                            this.remove(ix, cx);
                        })),
                ))
                .when(active, |d| d.child(div().absolute().left_0().top_0().bottom_0().w(px(tr::ACTIVE_BAR_W)).bg(th.accent_fg)))
                // `.qrow.selected { outline: 1px solid accent-fg; outline-offset: -1px }`
                .when(selected, |d| d.child(div().absolute().top_0().left_0().size_full().border_1().border_color(th.accent_fg)))
                // `.gap-before::before`: a 2 px accent rule on the row edge the dragged row would be inserted at
                .child(div().absolute().top_0().left_0().w_full().h(px(2.)).bg(gpui::transparent_black()).group_drag_over::<QueueDrag>("qrow", move |s| s.bg(th.accent_fg)))
                // reorder: drag a row onto another one (the insertion edge shows as an accent rule)
                .on_drag(drag, move |d: &QueueDrag, _, _, cx| {
                    let label = d.label.clone();
                    cx.new(|_| QueueGhost { label, th })
                })
                .on_drop(cx.listener(move |this, d: &QueueDrag, _, cx| {
                    let (from, to) = (d.index, idx);
                    app::call(cx, move |b| {
                        let _ = b.player_reorder_queue(from, to);
                    });
                    this.refresh(cx);
                }))
                // external drag (tracks/albums/artists): onto a row = "Add to queue"
                .on_drop(cx.listener(|this, d: &crate::dnd::SonixDrag, _, cx| this.drop_external(d.0.clone(), EnqueuePosition::End, cx)))
                .on_mouse_down(
                    gpui::MouseButton::Right,
                    cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.open_menu(idx, ev.position, window, cx);
                    }),
                )
                .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                    window.focus(&this.focus);
                    if ev.click_count() >= 2 {
                        app::call(cx, move |b| {
                            let _ = b.player_jump_to(idx);
                        });
                        cx.emit(RailEvent::Played);
                    } else {
                        let m = ev.modifiers();
                        this.sel.click(idx, m.control || m.platform, m.shift);
                        cx.notify();
                    }
                })),
        )
        .into_any_element()
    }

    fn remove(&mut self, mut ix: Vec<usize>, cx: &mut Context<Self>) {
        ix.sort_unstable();
        ix.dedup();
        self.sel.clear();
        app::call(cx, move |b| {
            for i in ix.into_iter().rev() {
                let _ = b.player_remove_from_queue(i);
            }
        });
        self.refresh(cx);
    }

    /// Context menu of a queue row / the selection it belongs to (`QueueRail.svelte buildQueueRowMenu` -> `trackMenuItems`).
    fn open_menu(&mut self, i: usize, pos: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.sel.for_context_menu(i);
        let Some(q) = &self.state else { return };
        let tracks: Vec<Track> = ix.iter().filter_map(|k| q.items.get(*k).map(|it| it.track.clone())).collect();
        if tracks.is_empty() {
            return;
        }
        let ids: Vec<String> = tracks.iter().map(|t| t.id.clone()).collect();
        let first = ix.first().copied().unwrap_or(i);
        let (this_rm, this_nav, this_next) = (cx.entity().downgrade(), cx.entity().downgrade(), cx.entity().downgrade());
        let (ix_rm, ix_next) = (ix.clone(), ix.clone());
        let items = crate::menus::track_menu_items(crate::menus::TrackMenu {
            tracks,
            at: pos,
            on_play_now: std::rc::Rc::new(move |cx| {
                app::call(cx, move |b| {
                    let _ = b.player_jump_to(first);
                })
            }),
            // `moveToPlayNext`: take the rows out and re-insert them right after the playing track as user items
            on_play_next: Some(std::rc::Rc::new(move |cx| {
                if let Some(t) = this_next.upgrade() {
                    let (ix, ids) = (ix_next.clone(), ids.clone());
                    t.update(cx, |t, cx| {
                        t.sel.clear();
                        let mut idx = ix;
                        idx.sort_unstable();
                        idx.dedup();
                        app::call(cx, move |b| {
                            for i in idx.iter().rev() {
                                let _ = b.player_remove_from_queue(*i);
                            }
                            let _ = b.player_enqueue(EnqueueRequest { track_ids: ids, position: EnqueuePosition::Next });
                        });
                        t.refresh(cx);
                    });
                }
            })),
            on_remove_from_playlist: None,
            on_remove_from_queue: Some(std::rc::Rc::new(move |cx| {
                if let Some(t) = this_rm.upgrade() {
                    let ix = ix_rm.clone();
                    t.update(cx, |t, cx| t.remove(ix, cx));
                }
            })),
            on_navigate: std::rc::Rc::new(move |r, cx| {
                if let Some(t) = this_nav.upgrade() {
                    t.update(cx, |_, cx| cx.emit(RailEvent::Navigate(r)));
                }
            }),
        });
        self.menu.update(cx, |m, cx| m.show(items, pos, window, cx));
    }

    /// External drop (`dnd.ts` payload): resolve to track ids and enqueue them.
    fn drop_external(&mut self, payload: crate::dnd::DragPayload, position: EnqueuePosition, cx: &mut Context<Self>) {
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let ok = cx
                .background_executor()
                .spawn(async move {
                    let ids = crate::dnd::resolve_ids(&payload, &*b);
                    if ids.is_empty() {
                        return false;
                    }
                    b.player_enqueue(EnqueueRequest { track_ids: ids, position }).is_ok()
                })
                .await;
            let _ = this.update(cx, |t, cx| {
                if ok {
                    t.refresh(cx);
                }
            });
        })
        .detach();
    }
}

/// `flow.ts kindNoun`
fn flow_noun(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Playlist => "playlist",
        SourceKind::Artist => "artist",
        SourceKind::Genre => "genre",
        SourceKind::AllTracks => "view",
        _ => "list",
    }
}

impl QueueRail {
    /// Absolute indices of the main list's rows (current track onwards, minus the "Next in queue" section).
    fn main_indices(&self) -> Vec<usize> {
        let Some(q) = &self.state else { return Vec::new() };
        let cur = q.current_index;
        q.items.iter().enumerate().filter(|(i, it)| (*i as i64) >= cur && !((*i as i64) > cur && it.origin == QueueItemOrigin::User)).map(|(i, _)| i).collect()
    }

    /// `trackListKeydown`: Up/Down move a single selection (clamped), scrolling it into view.
    fn key_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let idx = self.main_indices();
        if idx.is_empty() {
            return;
        }
        let pos = self.sel.cursor().and_then(|c| idx.iter().position(|i| *i == c));
        let from = match pos {
            Some(p) => p as isize,
            None => {
                if delta > 0 {
                    -1
                } else {
                    idx.len() as isize
                }
            }
        };
        let to = (from + delta).clamp(0, idx.len() as isize - 1) as usize;
        self.sel.click(idx[to], false, false);
        self.scroll.scroll_to_item(to);
        cx.notify();
    }

    /// Ctrl/Cmd+A: every row of the main list.
    fn select_all_main(&mut self, cx: &mut Context<Self>) {
        let idx = self.main_indices();
        self.sel.clear();
        for i in idx {
            self.sel.click(i, true, false);
        }
        cx.notify();
    }

    fn row_bounds(&self, abs: usize) -> Option<Bounds<gpui::Pixels>> {
        let pos = self.main_indices().iter().position(|i| *i == abs)?;
        self.scroll.bounds_for_item(pos)
    }
}

impl Focusable for QueueRail {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}
