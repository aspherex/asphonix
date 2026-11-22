//! Flow: the arc choices, the "Flow ▸" context-menu entry and the header FLOW button's menu with the pool's
//! real coverage hint. Every Flow entry point of every source goes through here.
use crate::widgets::context_menu::{ContextMenu, MenuItem};
use gpui::{App, Bounds, Entity, Pixels, Window, canvas, prelude::*, px};
use asphonix_api::{FlowArc, FlowCoverageRequest, FlowPoolKind};
use std::cell::Cell;
use std::rc::Rc;

/// The Flow arcs: value, label, hint.
pub const ARCS: [(FlowArc, &str, &str); 3] = [
    (FlowArc::Steady, "Steady", "Smooth transitions, even energy"),
    (FlowArc::Build, "Build", "Energy rises over each ~40 tracks"),
    (FlowArc::Peak, "Peak", "Rise, then cool down"),
];

/// A Flow start for one source, called with the chosen arc.
pub type Play = Rc<dyn Fn(FlowArc, &mut App)>;

/// One menu entry per arc: Steady / Build / Peak.
pub fn arc_entries(play: Play) -> Vec<MenuItem> {
    ARCS.iter()
        .map(|&(arc, label, hint)| {
            let play = play.clone();
            MenuItem::action(label, move |_, cx| play(arc, cx)).hint(hint)
        })
        .collect()
}

/// The context menus' "Flow ▸" submenu.
pub fn menu_entry(play: Play, disabled: bool) -> MenuItem {
    MenuItem::submenu("Flow", arc_entries(play)).icon("sliders").disabled(disabled)
}

/// The noun the coverage hint uses for a pool kind.
pub fn noun(kind: FlowPoolKind) -> &'static str {
    match kind {
        FlowPoolKind::Playlist => "playlist",
        FlowPoolKind::Artist => "artist",
        FlowPoolKind::Genre => "genre",
        FlowPoolKind::AllTracks => "view",
        FlowPoolKind::Liked | FlowPoolKind::Smart => "list",
    }
}

/// A coverage request for a source pool.
pub fn pool(kind: FlowPoolKind, id: Option<String>, seed: Option<u32>) -> FlowCoverageRequest {
    FlowCoverageRequest { kind, id, filter: None, seed }
}

/// Looks the coverage up off the UI thread, then opens the hint (disabled entry + rule) and the arcs under the
/// button (left edge, 4 px below).
pub fn open_menu(menu: Entity<ContextMenu>, anchor: Bounds<Pixels>, req: FlowCoverageRequest, noun: &'static str, play: Play, window: &mut Window, cx: &mut App) {
    let b = crate::app::backend(cx);
    let at = gpui::point(anchor.left(), anchor.bottom() + px(4.));
    window
        .spawn(cx, async move |cx| {
            let cov = cx.background_executor().spawn(async move { b.flow_get_coverage(req) }).await;
            let hint = cov.ok().map(|c| asphonix_api::format::coverage_hint(&c, noun)).unwrap_or_default();
            let _ = cx.update(|window, cx| {
                let mut items = Vec::new();
                if !hint.is_empty() {
                    items.push(MenuItem::action(hint, |_, _| {}).disabled(true));
                    items.push(MenuItem::separator());
                }
                items.extend(arc_entries(play));
                menu.update(cx, |m, cx| m.show(items, at, window, cx));
            });
        })
        .detach();
}

/// Records a button's bounds (the FLOW menu anchors to it): add as an absolute child of the (relative) button.
pub fn bounds_probe(cell: Rc<Cell<Bounds<Pixels>>>) -> impl IntoElement {
    canvas(move |b, _, _| cell.set(b), |_, _, _, _| ()).absolute().size_full()
}
