//! Flow: the arc choices, the "Flow ▸" context-menu entry and the header FLOW button's menu with the pool's
//! real coverage hint. Every Flow entry point of every source goes through here.
use crate::widgets::context_menu::{ContextMenu, MenuItem};
use gpui::{App, Bounds, Entity, Pixels, Window, canvas, prelude::*, px};
use sonix_api::{FlowArc, FlowCoverage, FlowCoverageRequest, FlowPoolKind};
use sonix_i18n::{Key, tr};
use std::cell::Cell;
use std::rc::Rc;

/// The Flow arcs: value, label, hint.
pub const ARCS: [(FlowArc, Key, Key); 3] = [
    (FlowArc::Steady, Key::FlowSteady, Key::FlowSteadyHint),
    (FlowArc::Build, Key::FlowBuild, Key::FlowBuildHint),
    (FlowArc::Peak, Key::FlowPeak, Key::FlowPeakHint),
];

/// A Flow start for one source, called with the chosen arc.
pub type Play = Rc<dyn Fn(FlowArc, &mut App)>;

/// One menu entry per arc: Steady / Build / Peak.
pub fn arc_entries(play: Play) -> Vec<MenuItem> {
    ARCS.iter()
        .map(|&(arc, label, hint)| {
            let play = play.clone();
            MenuItem::action(tr(label), move |_, cx| play(arc, cx)).hint(tr(hint))
        })
        .collect()
}

/// The context menus' "Flow ▸" submenu.
pub fn menu_entry(play: Play, disabled: bool) -> MenuItem {
    MenuItem::submenu(tr!(FlowMenu), arc_entries(play)).icon("sliders").disabled(disabled)
}

/// The pool kind as the coverage hint's selector (`playlist`, `artist`, `genre`, `view`, `list`): an id, not text;
/// each language words "this playlist" / "diese Wiedergabeliste" itself.
pub fn noun(kind: FlowPoolKind) -> &'static str {
    match kind {
        FlowPoolKind::Playlist => "playlist",
        FlowPoolKind::Artist => "artist",
        FlowPoolKind::Genre => "genre",
        FlowPoolKind::AllTracks => "view",
        FlowPoolKind::Liked | FlowPoolKind::Smart => "list",
    }
}

/// `"70"`, `"<1"`, `"0"`: the share for the hint's `{ $pct }%`.
fn pct(n: u32, total: u32) -> String {
    if n == 0 || total == 0 {
        return "0".into();
    }
    let p = ((100.0 * n as f64) / total as f64).round() as u32;
    if p == 0 { "<1".into() } else { p.to_string() }
}

/// How much of the pool Flow actually has data for ("" when the pool is empty); the translated form of
/// `sonix_api::format::coverage_hint`.
pub fn coverage_hint(c: &FlowCoverage, noun: &str) -> String {
    if c.total == 0 {
        return String::new();
    }
    if c.bpm == 0 {
        if c.loudness > 0 {
            return tr!(FlowHintNoBpmLoudness, kind = noun, pct = pct(c.loudness, c.total));
        }
        return tr!(FlowHintNoBpm, kind = noun);
    }
    let base = tr!(FlowHintBpm, kind = noun, pct = pct(c.bpm, c.total));
    if c.key > 0 { format!("{base} · {}", tr!(FlowHintKey, pct = pct(c.key, c.total))) } else { base }
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
            let hint = cov.ok().map(|c| coverage_hint(&c, noun)).unwrap_or_default();
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coverage_hint_matches_the_api_wording_in_english() {
        let c = FlowCoverage { total: 200, bpm: 140, key: 56, loudness: 0 };
        let en = sonix_i18n::Catalog::get(sonix_i18n::Lang::En);
        assert_eq!(en.tr(Key::FlowMenu), "Flow");
        // the UI's hint is the translated form of sonix_api::format::coverage_hint (same English text)
        if sonix_i18n::lang() == sonix_i18n::Lang::En {
            assert_eq!(coverage_hint(&c, "view"), sonix_api::format::coverage_hint(&c, "view"));
            let c2 = FlowCoverage { total: 10, bpm: 0, key: 0, loudness: 5 };
            assert_eq!(coverage_hint(&c2, "playlist"), sonix_api::format::coverage_hint(&c2, "playlist"));
            assert_eq!(coverage_hint(&FlowCoverage { total: 0, bpm: 0, key: 0, loudness: 0 }, "list"), "");
        }
    }
}
