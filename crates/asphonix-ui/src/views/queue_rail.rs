//! Queue rail: "Live Transport Deck" (current track, spectrum, position/output row), the "Next in queue"
//! user section and the "Next from <label> · Shuffle" window with the "+ N more" footer; rows support
//! multi-select, drag-reorder, external drops and removal. Docked at >= 1440 px, an overlay drawer below.
//! Sizes: `crate::metrics::rail`.
use super::library::entity_link::{Kind as LinkKind, entity_link};
use super::shell::{OpenMenuKey, PlaySelected, SelDown, SelUp, SelectAll};
use crate::app::{self, PlayerEvent, PlayerModel};
use crate::art::{self, ArtHandle};
use crate::metrics::{layout, rail as rl, space, tracks as tr};
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_LG, BODY_MD, BODY_SM, LABEL_CAPS, MONO_CODE, MONO_DATA};
use crate::widgets::context_menu::ContextMenu;
use crate::widgets::{hard_shadow, icon};
use crate::widgets::tooltip::Tip;
use crate::selection::Selection;
use crate::rail_dnd::{self, Gap, Hit, RowBox};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use gpui::{Bounds, ClickEvent, Context, CursorStyle, DragMoveEvent, Entity, FocusHandle, Focusable, IntoElement, MouseDownEvent, Render, Subscription, Window, canvas, Pixels, Point, div, fill, point, prelude::*, px, size};
use asphonix_api::format as fmt;
use asphonix_api::*;
use std::sync::Arc;

type RowBounds = Rc<RefCell<HashMap<usize, (Bounds<Pixels>, bool)>>>;

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
    /// `(queue length, current track id, shuffle)` the list was last scrolled for.
    scroll_key: Option<(usize, String, bool)>,
    /// list position still to be scrolled to the top: GPUI's `scroll_to_top_of_item` subtracts the handle's bounds from the
    /// previous frame, which are zero until the list has painted once, so it is applied from `render` (see `take_pending_top`)
    top_pending: Option<usize>,
    focus: FocusHandle,
    /// edge auto-scroll while dragging a row (px per frame, + = down)
    drag_dy: f32,
    /// the drag in flight over the rail (internal reorder or an external drop) and where it would land
    drag: Option<RailDrag>,
    /// play-order index of `state.items[0]` (the page is a window around the current row for long queues)
    base: usize,
    /// painted extents of the rows (abs index -> bounds, user-queued), the open gap, the deck / "Next in queue" section /
    /// list: drag and drop hit-tests against what the last frame painted
    rows: RowBounds,
    ph_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    deck_b: Rc<Cell<Bounds<Pixels>>>,
    user_b: Rc<Cell<Bounds<Pixels>>>,
    list_b: Rc<Cell<Bounds<Pixels>>>,
    /// last pointer position of a reorder drag (the gap is re-located every frame while the list auto-scrolls)
    drag_pos: Point<Pixels>,
    menu: Entity<ContextMenu>,
    /// multi-select over absolute queue indices (Ctrl/Shift click, remove acts on the selection)
    sel: Selection,
    _sub: Subscription,
    /// cached view (`crate::part`): the deck shows the position as whole seconds
    pos_gate: crate::app::PosGate,
    _pos_sub: Subscription,
}

/// Per-row layout flags of `queue_row`: playing, Flow source (3-line rows), in the user queue.
#[derive(Clone, Copy)]
struct RowFlags {
    active: bool,
    flow_rows: bool,
    /// in the "Next in queue" run
    user: bool,
}

/// Internal drag of a queue row (reorder). External drops use `crate::dnd::AsphonixDrag`.
#[derive(Clone)]
struct QueueDrag {
    label: String,
}
/// What is being dragged over the rail. `moving` = absolute indices of an internal reorder (empty for an external drag);
/// `gap` = insertion position (before play-order index N); `ext` = the zone an external
/// drop would land in (deck / "Next in queue" = Play next, the main list = Add to queue).
#[derive(Clone, Default)]
struct RailDrag {
    moving: Vec<usize>,
    gap: Option<Gap>,
    ext: Option<EnqueuePosition>,
}
/// Drag ghost of a reorder: accent chip, mono-code, bold, uppercase, max 240 px, hard shadow.
struct QueueGhost {
    label: String,
    th: Theme,
}
impl Render for QueueGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .max_w(px(240.))
            .overflow_hidden()
            .px(px(space::SM))
            .py(px(2.))
            .bg(self.th.accent)
            .shadow(hard_shadow(&self.th, 2.0))
            .child(txt(&self.th, MONO_CODE, self.label.to_uppercase()).weight(700.0).color(self.th.accent_text).ellipsis())
    }
}

/// Grey gap of a drag: a row-high
/// `surface-high` placeholder opens at the insertion point, with the 2 px `accent-fg` insertion line on its edge.
fn gap_placeholder(th: &Theme, h: f32, cell: Rc<Cell<Option<Bounds<Pixels>>>>) -> gpui::AnyElement {
    div()
        .relative()
        .flex_none()
        .w_full()
        .h(px(h))
        .bg(th.high)
        .child(canvas(move |b, _, _| cell.set(Some(b)), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
        .child(div().absolute().top_0().left_0().w_full().h(px(2.)).bg(th.accent_fg))
        .into_any_element()
}

/// Records where an element was painted (zone / row hit-testing of the drag and drop).
fn bounds_probe(cell: Rc<Cell<Bounds<Pixels>>>) -> gpui::AnyElement {
    canvas(move |b, _, _| cell.set(b), |_, _, _, _| {}).absolute().top_0().left_0().size_full().into_any_element()
}

/// Dashed accent-fg frame, 2 px inset, around the zone an external drop would land in.
fn drop_frame(th: &Theme) -> gpui::Div {
    div().absolute().top(px(2.)).left(px(2.)).right(px(2.)).bottom(px(2.)).border_1().border_dashed().border_color(th.accent_fg)
}

impl QueueRail {
    pub fn new(backend: Arc<dyn Backend>, art: ArtHandle, player: &Entity<PlayerModel>, menu: &Entity<ContextMenu>, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(player, |this: &mut Self, _, _: &PlayerEvent, cx| this.refresh(cx));
        let mut s = Self { backend, art, player: player.clone(), state: None, scroll: gpui::ScrollHandle::new(), scroll_key: None, menu: menu.clone(), focus: cx.focus_handle().tab_stop(true), drag_dy: 0.0, drag: None, base: 0, top_pending: None, rows: Default::default(), ph_bounds: Default::default(), deck_b: Default::default(), user_b: Default::default(), list_b: Default::default(), drag_pos: Point::default(), sel: Selection::default(), _sub: sub, pos_gate: Default::default(), _pos_sub: cx.observe(player, |this: &mut Self, p, cx| {
            if this.pos_gate.changed(&p.read(cx).state, 0.0) {
                cx.notify();
            }
        }) };
        s.refresh(cx);
        s
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let q = cx
                .background_executor()
                .spawn(async move {
                    const WINDOW: usize = 100;
                    let q = b.player_get_queue(PageRequest { offset: 0, limit: WINDOW })?;
                    // the page must carry the current row: a long static queue can have it far beyond the first page
                    if q.total > WINDOW && q.current_index >= WINDOW as i64 - 10 {
                        let base = (q.current_index as usize).saturating_sub(10).min(q.total - WINDOW);
                        return Ok((base, b.player_get_queue(PageRequest { offset: base, limit: WINDOW })?));
                    }
                    Ok::<_, ApiError>((0usize, q))
                })
                .await;
            let _ = this.update(cx, |t, cx| {
                match q {
                    Ok((base, q)) => {
                        t.base = base;
                        t.state = Some(q);
                    }
                    Err(_) => t.state = None,
                }
                t.scroll_current_to_top(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// With a rolling source the queue keeps played history
    /// above the current track, so once per queue change (key: length, current track, shuffle)
    /// and only after the fetched page carries the current track, the current row goes to the top of the list
    /// The current row's list position is its distance from the page start (user rows follow it).
    fn scroll_current_to_top(&mut self, cx: &mut Context<Self>) {
        let (cur_id, shuffle) = {
            let m = self.player.read(cx);
            (m.current_id().map(str::to_owned), m.state.shuffle)
        };
        let (Some(q), Some(id)) = (self.state.as_ref(), cur_id) else { return };
        if q.current_index < 0 {
            return;
        }
        let ci = q.current_index as usize;
        let Some(pos) = ci.checked_sub(self.base) else { return };
        if q.items.get(pos).map(|it| it.track.id.as_str()) != Some(id.as_str()) {
            return; // stale index from before the refetch
        }
        let key = (q.total, id, shuffle);
        if self.scroll_key.as_ref() != Some(&key) {
            self.scroll_key = Some(key);
            self.top_pending = Some(pos);
        }
    }

    pub fn release(&mut self) {
        self.state = None;
        self.scroll_key = None;
    }
}

/// Section header: 8/12 padding, `surface-mid`, bottom rule, label caps (+ right-aligned count).
fn header(th: &Theme, label: String, right: Option<String>) -> impl IntoElement {
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
            .children(right.map(|r| txt(th, MONO_CODE, r).color(th.muted)))
}

impl Render for QueueRail {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::rcount::hit("rail");
        crate::live::begin_view(window);
        if self.top_pending.is_some() {
            match take_pending_top(&mut self.top_pending, f32::from(self.scroll.bounds().size.height)) {
                Some(pos) => self.scroll.scroll_to_top_of_item(pos),
                // paint the list once so its bounds exist, then apply (a notify inside render is lost: GPUI clears the
                // view's dirty flag after this render)
                None => window.request_animation_frame(),
            }
        }
        if self.drag_dy != 0.0 {
            if cx.has_active_drag() {
                let off = self.scroll.offset();
                let max = f32::from(self.scroll.max_offset().height);
                let new_y = (f32::from(off.y) - self.drag_dy).clamp(-max, 0.0);
                self.scroll.set_offset(point(off.x, px(new_y)));
                // the rows moved under the pointer: re-locate the gap every frame
                self.locate(f32::from(off.y) - new_y);
                window.request_animation_frame();
            } else {
                self.drag_dy = 0.0;
            }
        }
        if self.drag.is_some() && !cx.has_active_drag() {
            self.drag = None; // dropped elsewhere / cancelled
        }
        // painted extents are re-recorded by this frame
        self.rows.borrow_mut().clear();
        self.ph_bounds.set(None);
        self.user_b.set(Bounds::default());
        let th: Theme = app::theme(cx);
        let ps = self.player.read(cx).state.clone();
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
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(space::SM))
                            .p(px(space::MD))
                            .child(div().size(px(56.)).flex_none().border_1().border_color(th.border).bg(th.high).overflow_hidden().child(cover))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .justify_center()
                                    .min_w_0()
                                    .gap(px(2.))
                                    .child(div().flex().min_w_0().child(txt(&th, BODY_LG, t.title.clone()).color(th.text).ellipsis()))
                                    .child(div().flex().child(entity_link(&th, (LinkKind::Artist, t.artist_id.as_deref()), &t.artist, MONO_DATA, th.muted, "deck", window))),
                            ),
                    )
                    .child(div().h(px(64.)).px(px(space::MD)).child(crate::live::slot(crate::live::Kind::RailSpectrum).size_full()))
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
            None => div().p(px(space::MD)).child(txt(&th, BODY_SM, "Nothing playing.").color(th.muted)).into_any_element(),
        };
        let ext_zone = self.drag.as_ref().and_then(|d| d.ext);
        let deck = div()
            .relative()
            .flex_none()
            .border_b_1()
            .border_color(th.border)
            .when(ext_zone == Some(EnqueuePosition::Next), |d| d.child(drop_frame(&th)))
            .child(bounds_probe(self.deck_b.clone()))
            .child(header(&th, "Live Transport Deck".into(), Some(format!("{total} QUEUED"))))
            .child(deck_body);

        // ---- queue sections: "Next in queue" (user-queued, always ahead) and "Next from <source>" (the rolling window)
        let (ustart, ucount) = self.user_range();
        let is_user = |i: usize| i >= ustart && i < ustart + ucount;
        let label = match q.as_ref().and_then(|q| q.source.as_ref()) {
            Some(s) => format!("Next from {}{}", s.label, match s.mode {
                SourceMode::Shuffle => " · Shuffle".to_string(),
                SourceMode::Flow => fmt::flow_mode_label(s.arc),
                SourceMode::InOrder => String::new(),
            }),
            None if ucount > 0 => "Queue".to_string(),
            None => "Next in buffer".to_string(),
        };
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
                .child(header(&th, "Next in buffer".into(), None))
                .child(div().p(px(space::MD)).child(txt(&th, BODY_SM, "Queue is empty.").color(th.muted)))
        } else {
            let (bar, overflow) = crate::widgets::scrollbar::passive(th, &self.scroll);
            let mut list = div().id("queue-list").track_scroll(&self.scroll).size_full().overflow_y_scroll().flex().flex_col().when(overflow, |d| d.pr(px(tr::SCROLLBAR_W)));
            let mut user_rows = Vec::new();
            // the grey gap: where the dragged row(s) / the external drop would really land
            let base = self.base;
            let drag = self.drag.clone();
            let has_source = src_info.is_some();
            let gap: Option<Gap> = drag.as_ref().and_then(|d| match d.ext {
                Some(z) => Some(rail_dnd::external_gap(z == EnqueuePosition::Next, has_source, ustart, ucount, total)),
                None if d.moving.is_empty() => None,
                None => d.gap,
            });
            let to_user = gap.is_some_and(|g| rail_dnd::in_user_section(g, ustart, ucount));
            let row_h = if flow_rows { 56.0 } else { 44.0 };
            // 120 ms ease: the gap opens, it does not pop
            let open = crate::anim::transition(window, ("qgap", 0), if gap.is_some() { 1.0 } else { 0.0 }, 0.12);
            let ph_h = row_h * open;
            let (mut placed_u, mut placed_m) = (false, false);
            if let Some(q) = &q {
                for (k, it) in q.items.iter().enumerate() {
                    // the main list renders every index (played history above the current
                    // row included); `i` is the absolute queue index.
                    let i = base + k;
                    let user = is_user(i);
                    if user {
                        if to_user && !placed_u && gap.is_some_and(|g| g.at <= i) {
                            placed_u = true;
                            if ph_h > 0.0 {
                                user_rows.push(gap_placeholder(&th, ph_h, self.ph_bounds.clone()));
                            }
                        }
                        user_rows.push(self.queue_row(&th, it, i, RowFlags { active: false, flow_rows, user: true }, window, cx));
                    } else {
                        if !to_user && !placed_m && gap.is_some_and(|g| i >= g.at) {
                            placed_m = true;
                            if ph_h > 0.0 {
                                list = list.child(gap_placeholder(&th, ph_h, self.ph_bounds.clone()));
                            }
                        }
                        list = list.child(self.queue_row(&th, it, i, RowFlags { active: (i as i64) == cur, flow_rows, user: false }, window, cx));
                    }
                }
            }
            if gap.is_some() && ph_h > 0.0 {
                if to_user && !placed_u {
                    user_rows.push(gap_placeholder(&th, ph_h, self.ph_bounds.clone()));
                } else if !to_user && !placed_m {
                    list = list.child(gap_placeholder(&th, ph_h, self.ph_bounds.clone()));
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
                    .relative()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .border_b_1()
                    .border_color(th.border)
                    .when(ext_zone == Some(EnqueuePosition::Next), |d| d.child(drop_frame(&th)))
                    .child(bounds_probe(self.user_b.clone()))
                    .child(header(&th, "Next in queue".into(), None))
                    .children(user_rows)
            });
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .children(user_section)
                .child(header(&th, label, None))
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
                                // anchored to the selected row (left edge, 4 px under it)
                                let pos = match this.row_bounds(c) {
                                    Some(b) => point(b.left(), b.bottom() + px(4.)),
                                    None => point(px(1200.), px(300.)),
                                };
                                this.open_menu(c, pos, window, cx);
                            }
                        }))
                        .when(ext_zone == Some(EnqueuePosition::End), |d| d.child(drop_frame(&th)))
                        .child(bounds_probe(self.list_b.clone()))
                        .child(list)
                        .child(bar)
                        .child(crate::widgets::smooth::drive(window, crate::widgets::smooth::Scrollable::div(std::ptr::addr_of!(self.scroll) as usize, &self.scroll))),
                )
                .children(footer)
        };

        div()
                .w(px(layout::RAIL_W))
                // one decision per pointer move, from the pointer position: deck / "Next in queue" = Play next
                // anywhere else on the rail (header, flow hint, list, footer, empty state) = end
                .on_drag_move::<crate::dnd::AsphonixDrag>(cx.listener(|this, ev: &DragMoveEvent<crate::dnd::AsphonixDrag>, _, cx| this.ext_move(ev.event.position, ev.bounds, cx)))
                .on_drag_move::<QueueDrag>(cx.listener(|this, ev: &DragMoveEvent<QueueDrag>, _, cx| this.reorder_move(ev.event.position, cx)))
                .on_drop(cx.listener(|this, d: &crate::dnd::AsphonixDrag, _, cx| this.drop_ext(d.0.clone(), cx)))
                // reorder: the row(s) land in the gap under the pointer
                .on_drop(cx.listener(|this, _: &QueueDrag, _, cx| this.finish_reorder(cx)))
                .h_full()
                .flex_none()
                .flex()
                .flex_col()
                .min_h_0()
                .bg(if drawer { th.low } else { th.chrome })
                .border_l_1()
                .border_color(th.frame)
                .child(deck)
                .child(queue_section)
    }
}

/// Mini spectrum: bars of `w / n` with a 1 px gap, `max(1, v * h)` tall, on a canvas whose bitmap
/// is the logical size (upsampled by the device scale factor).
pub(crate) fn paint_rail_spectrum(window: &mut gpui::Window, b: Bounds<Pixels>, th: Theme, bands: &[u8]) {
    let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
    let n = bands.len().max(1);
    let bar_w = w / n as f32;
    for (i, v) in bands.iter().enumerate() {
        let bh = (*v as f32 / 255.0 * h).max(1.0);
        window.paint_quad(fill(Bounds::new(point(b.origin.x + px(i as f32 * bar_w), b.origin.y + px(h - bh)), size(px(bar_w - 1.0), px(bh))), th.meter));
    }
}

impl QueueRail {
    /// One queue row: drag handle, title/artist, time, "more", remove. Click selects (Ctrl/Shift
    /// extend), double-click plays; rows are drag sources (reorder) and drop targets (reorder / add).
    fn queue_row(&mut self, th: &Theme, it: &QueueItem, i: usize, flags: RowFlags, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let RowFlags { active, flow_rows, user } = flags;
        let th = *th;
        let t = &it.track;
        let selected = self.sel.contains(i);
        let idx = i;
        // dragging a row of a multi-row selection moves the whole selection
        // read-only: `for_context_menu` collapses the selection onto `i`, which must never happen while painting
        let moving = if selected && self.sel.len() > 1 { self.sel.indices() } else { vec![i] };
        let label = if moving.len() > 1 { format!("{} tracks", moving.len()) } else { t.title.clone() };
        let dim = self.drag.as_ref().is_some_and(|d| d.moving.contains(&i));
        let drag = QueueDrag { label };
        let this_w = cx.entity().downgrade();
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
                .when(dim, |d| d.opacity(0.35)) // rows being dragged are dimmed
                .child({
                    let rows = self.rows.clone();
                    canvas(move |b, _, _| { rows.borrow_mut().insert(i, (b, user)); }, |_, _, _, _| {}).absolute().top_0().left_0().size_full()
                })
                .child(div().flex().flex_none().cursor(CursorStyle::OpenHand).child(icon("drag", 13., th.muted))) // the drag handle
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().flex().min_w_0().child(txt(&th, BODY_MD, t.title.clone()).color(th.text).ellipsis()))
                        .child(div().flex().min_w_0().child(entity_link(&th, (LinkKind::Artist, t.artist_id.as_deref()), &t.artist, MONO_CODE, th.muted, &format!("rail{i}"), window)))
                        .children((flow_rows && it.origin != QueueItemOrigin::User).then(|| div().id(("rail-flow", idx)).tip("Why this track is here", th).flex().min_w_0().h(px(14.)).child(txt(&th, MONO_CODE, fmt::flow_line(it.flow.as_ref())).color(th.accent_fg).ellipsis()))),
                )
                .child(div().flex_none().child(txt(&th, MONO_CODE, fmt::format_duration(Some(t.duration))).color(th.muted)))
                .child(div()
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
                        })))
                .child(div()
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
                        })))
                .when(active, |d| d.child(div().absolute().left_0().top_0().bottom_0().w(px(tr::ACTIVE_BAR_W)).bg(th.accent_fg)))
                // selected: 1 px accent-fg frame inside the row
                .when(selected, |d| d.child(div().absolute().top_0().left_0().size_full().border_1().border_color(th.accent_fg)))
                // a 2 px accent rule on the row edge the dragged row would be inserted at
                // reorder: drag a row onto another one (the insertion edge shows as an accent rule)
                .on_drag(drag, move |d: &QueueDrag, _, _, cx| {
                    let label = d.label.clone();
                    if let Some(t) = this_w.upgrade() {
                        let moving = moving.clone();
                        t.update(cx, |t, cx| {
                            t.drag = Some(RailDrag { moving, gap: None, ext: None });
                            cx.notify();
                        });
                    }
                    cx.new(|_| QueueGhost { label, th })
                })
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
                }))
        .into_any_element()
    }

    /// The contiguous run of user-queued rows after the current one, as absolute `(start, count)`.
    fn user_range(&self) -> (usize, usize) {
        let Some(q) = &self.state else { return (0, 0) };
        let origins: Vec<bool> = q.items.iter().map(|it| it.origin == QueueItemOrigin::User).collect();
        rail_dnd::user_run(self.base, &origins, q.current_index)
    }

    /// The loaded item at play-order index `abs`.
    fn item(&self, abs: usize) -> Option<&QueueItem> {
        self.state.as_ref()?.items.get(abs.checked_sub(self.base)?)
    }

    /// Runs backend calls in order on the background executor, then refreshes (each move / removal finishes
    /// before the next starts and before the reload).
    fn run_then_refresh(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&dyn Backend) + Send + 'static) {
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .spawn(async move {
                    f(&*b);
                })
                .await;
            let _ = this.update(cx, |t, cx| t.refresh(cx));
        })
        .detach();
    }

    /// Pointer move of an internal reorder: remember the pointer, locate the gap, start / stop the edge auto-scroll.
    fn reorder_move(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        if self.drag.as_ref().is_none_or(|d| d.moving.is_empty()) {
            return;
        }
        self.drag_pos = pos;
        self.locate(0.0);
        const EDGE: f32 = 36.0;
        let lb = self.list_b.get();
        let (y, top, bottom) = (f32::from(pos.y), f32::from(lb.top()), f32::from(lb.bottom()));
        let dy = if y < top + EDGE && y > top - EDGE {
            -((top + EDGE - y) / 4.0).ceil()
        } else if y > bottom - EDGE && y < bottom + EDGE {
            ((y - (bottom - EDGE)) / 4.0).ceil()
        } else {
            0.0
        };
        self.drag_dy = dy; // NOT window.request_animation_frame() here (no current view outside render/paint); render keeps requesting frames while drag_dy != 0
        cx.notify();
    }

    /// Locates the gap from what the last frame painted: the row under the pointer (`shift` = how far the list scrolled since),
    /// else the list's / the section's background (end of the queue / front of "Next in queue").
    fn locate(&mut self, shift: f32) {
        let pos = self.drag_pos;
        let (in_list, in_user) = (self.list_b.get().contains(&pos), self.user_b.get().contains(&pos));
        let mut rows: Vec<RowBox> = self
            .rows
            .borrow()
            .iter()
            .filter(|(_, (_, user))| if *user { in_user } else { in_list })
            .map(|(abs, (b, user))| RowBox { abs: *abs, top: f32::from(b.top()), bottom: f32::from(b.bottom()), user: *user })
            .collect();
        rows.sort_by(|a, b| a.top.total_cmp(&b.top));
        let ph = self.ph_bounds.get().filter(|_| in_list || in_user).map(|b| (f32::from(b.top()), f32::from(b.bottom())));
        let new = match rail_dnd::hit(f32::from(pos.y) + shift, &rows, ph) {
            Some(Hit::Keep) => return,
            Some(Hit::Gap(g)) => Some(g),
            None if in_list => self.state.as_ref().filter(|q| self.base + q.items.len() >= q.total).map(|q| Gap { at: q.total, user: false }),
            None if in_user => Some(Gap { at: self.user_range().0, user: true }),
            None => None,
        };
        if let Some(d) = self.drag.as_mut() {
            d.gap = new;
        }
    }

    /// An external drag (tracks/albums/artists) moved over the rail: the zone is decided once, from the pointer.
    fn ext_move(&mut self, pos: Point<Pixels>, rail: Bounds<Pixels>, cx: &mut Context<Self>) {
        let zone = if !rail.contains(&pos) {
            None
        } else if self.deck_b.get().contains(&pos) || self.user_b.get().contains(&pos) {
            Some(EnqueuePosition::Next)
        } else {
            Some(EnqueuePosition::End)
        };
        let d = self.drag.get_or_insert_with(RailDrag::default);
        if d.moving.is_empty() && d.ext != zone {
            d.ext = zone;
            cx.notify();
        }
    }

    /// Drop of an external payload: where the zone shown at the last move says.
    fn drop_ext(&mut self, payload: crate::dnd::DragPayload, cx: &mut Context<Self>) {
        let zone = self.drag.take().and_then(|d| d.ext).unwrap_or(EnqueuePosition::End);
        self.drop_external(payload, zone, cx);
    }

    /// Pointer up of a reorder: moves the dragged row(s) into the gap.
    fn finish_reorder(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.drag.take() else { return };
        let (Some(q), Some(g)) = (&self.state, d.gap) else { return };
        let mut moving = d.moving;
        moving.sort_unstable();
        moving.dedup();
        if moving.is_empty() || rail_dnd::is_noop(&moving, g.at) {
            return;
        }
        let ops = rail_dnd::move_block(q.total, &moving, g.at);
        self.sel.clear();
        self.run_then_refresh(cx, move |b| {
            for (from, to) in ops {
                let _ = b.player_reorder_queue(from, to);
            }
        });
        cx.notify();
    }

    fn remove(&mut self, mut ix: Vec<usize>, cx: &mut Context<Self>) {
        ix.sort_unstable();
        ix.dedup();
        self.sel.clear();
        self.run_then_refresh(cx, move |b| {
            for i in ix.into_iter().rev() {
                let _ = b.player_remove_from_queue(i);
            }
        });
    }

    /// Context menu of a queue row / the selection it belongs to.
    fn open_menu(&mut self, i: usize, pos: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.sel.for_context_menu(i);
        let tracks: Vec<Track> = ix.iter().filter_map(|k| self.item(*k).map(|it| it.track.clone())).collect();
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
            // Play next: take the rows out and re-insert them right after the playing track as user items
            on_play_next: Some(std::rc::Rc::new(move |cx| {
                if let Some(t) = this_next.upgrade() {
                    let (ix, ids) = (ix_next.clone(), ids.clone());
                    t.update(cx, |t, cx| {
                        t.sel.clear();
                        let mut idx = ix;
                        idx.sort_unstable();
                        idx.dedup();
                        t.run_then_refresh(cx, move |b| {
                            for i in idx.iter().rev() {
                                let _ = b.player_remove_from_queue(*i);
                            }
                            let _ = b.player_enqueue(EnqueueRequest { track_ids: ids, position: EnqueuePosition::Next });
                        });
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

    /// External drop (`crate::dnd` payload): resolve to track ids and enqueue them.
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

/// Noun for a Flow source kind.
fn flow_noun(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Playlist => "playlist",
        SourceKind::Artist => "artist",
        SourceKind::Genre => "genre",
        SourceKind::AllTracks => "view",
        _ => "list",
    }
}

/// Hands out the pending scroll-to-top target once the list has painted (its bounds are non-empty). Applied earlier,
/// GPUI computes the offset against zero bounds and the list lands scrolled by the list's own window offset.
fn take_pending_top(pending: &mut Option<usize>, list_height: f32) -> Option<usize> {
    if list_height > 0.0 {
        pending.take()
    } else {
        None
    }
}

impl QueueRail {
    /// Absolute indices of the main list's rows (current track onwards, minus the "Next in queue" section).
    fn main_indices(&self) -> Vec<usize> {
        let Some(q) = &self.state else { return Vec::new() };
        let (us, uc) = self.user_range();
        (0..q.items.len()).map(|k| self.base + k).filter(|i| !(*i >= us && *i < us + uc)).collect()
    }

    /// Up/Down move a single selection (clamped), scrolling it into view.
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

#[cfg(test)]
mod tests {
    use super::take_pending_top;

    #[test]
    fn scroll_to_top_waits_for_the_first_painted_list() {
        let mut p = Some(7);
        assert_eq!(take_pending_top(&mut p, 0.0), None);
        assert_eq!(p, Some(7));
        assert_eq!(take_pending_top(&mut p, 477.0), Some(7));
        assert_eq!(p, None);
        assert_eq!(take_pending_top(&mut p, 477.0), None);
    }
}
