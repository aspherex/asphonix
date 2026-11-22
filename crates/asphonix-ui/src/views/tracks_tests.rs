//! Tracks paging under GPUI's deterministic executor (harness: `crate::tests`): the view is the window's root, so
//! every frame asks the pager for the rows `uniform_list` shows, exactly like the app.
use super::*;
use crate::tests::{install, soon, ticks};
use gpui::{TestAppContext, VisualTestContext};
use rand::rngs::StdRng;
use asphonix_testkit::{FakeBackend, fixtures};
use std::collections::HashMap;

fn mount<'a>(cx: &'a mut TestAppContext, fake: &Arc<FakeBackend>) -> (Entity<TracksView>, &'a mut VisualTestContext) {
    install(cx, fake);
    let player = cx.new(|_| PlayerModel::default());
    let menu = cx.new(|cx| ContextMenu::new(Theme::dark(), cx));
    let backend: Arc<dyn Backend> = fake.clone();
    let (view, cx) = cx.add_window_view(|window, cx| TracksView::new(backend, crate::art::new_handle(1 << 20), &player, &menu, window, cx));
    cx.run_until_parked();
    (view, cx)
}

/// Every resident row as `(index, track id)`.
pub(crate) fn resident(t: &TracksView) -> Vec<(usize, String)> {
    (0..t.pager.total).filter_map(|i| t.pager.get(i).map(|tr| (i, tr.id.clone()))).collect()
}

/// `browse_get_tracks` offsets requested, with how often each was asked for.
fn offsets(fake: &FakeBackend) -> HashMap<usize, usize> {
    let mut m = HashMap::new();
    for c in fake.calls_to("browse_get_tracks") {
        let off = c.args.split("offset: ").nth(1).and_then(|s| s.split(',').next()).and_then(|s| s.trim().parse().ok()).unwrap();
        *m.entry(off).or_insert(0) += 1;
    }
    m
}

fn scroll(view: &Entity<TracksView>, cx: &mut VisualTestContext, ix: usize) {
    view.update(cx, |t, cx| t.script_scroll(ix, cx));
    cx.run_until_parked();
}

#[gpui::test]
fn the_first_page_brings_the_total_and_the_visible_rows(cx: &mut TestAppContext) {
    let fake = FakeBackend::new().with_tracks(fixtures::tracks(1000)).arc();
    let (view, cx) = mount(cx, &fake);
    view.read_with(cx, |t, _| {
        assert!(t.loaded);
        assert_eq!(t.pager.total, 1000);
        assert_eq!(t.pager.get(0).map(|tr| tr.id.as_str()), Some("t0000"));
        assert!(!t.vis.is_empty() && t.vis.end <= PAGE, "the first screen fits in page 0: {:?}", t.vis);
    });
    assert_eq!(offsets(&fake), HashMap::from([(0, 1)]), "page 0 once (reload), never again from the frames");
}

/// Scrolling deep, then deeper, with random answer delays: each page is fetched once, every row sits at its index.
#[gpui::test(iterations = 30)]
fn scrolling_fetches_each_page_once_and_rows_never_shift(cx: &mut TestAppContext, mut rng: StdRng) {
    let fake = FakeBackend::new().with_tracks(fixtures::tracks(1000)).arc();
    let (view, cx) = mount(cx, &fake);
    view.update(cx, |t, cx| t.script_scroll(450, cx));
    ticks(cx, &mut rng);
    scroll(&view, cx, 820);
    scroll(&view, cx, 455);
    let rows = view.read_with(cx, |t, _| resident(t));
    assert!(rows.iter().any(|(i, _)| (450..500).contains(i)) && rows.iter().any(|(i, _)| (820..870).contains(i)), "both scroll targets loaded");
    for (i, id) in &rows {
        assert_eq!(id, &format!("t{i:04}"), "row {i}");
    }
    assert!(offsets(&fake).values().all(|n| *n == 1), "a page is never requested twice: {:?}", offsets(&fake));
}

/// A sort change while page answers are in flight: answers of the old order are dropped (epoch), never mixed in.
#[gpui::test(iterations = 30)]
fn a_sort_change_mid_flight_never_mixes_rows_of_two_orders(cx: &mut TestAppContext) {
    let fake = FakeBackend::new().with_tracks(fixtures::tracks(1000)).arc();
    let (view, cx) = mount(cx, &fake);
    view.update(cx, |t, cx| t.script_scroll(450, cx));
    let v2 = view.clone();
    // same field again = toggle to descending
    soon(cx, move |cx| v2.update(cx, |t, cx| t.on_sort(TrackSortField::Title, cx)));
    cx.run_until_parked();
    scroll(&view, cx, 300);
    let rows = view.read_with(cx, |t, _| {
        assert_eq!(t.dir, SortDir::Desc);
        resident(t)
    });
    assert!(!rows.is_empty());
    for (i, id) in &rows {
        assert_eq!(id, &format!("t{:04}", 999 - i), "row {i} of the descending order");
    }
}

#[gpui::test]
fn release_drops_the_rows_and_the_next_frame_brings_them_back(cx: &mut TestAppContext) {
    let fake = FakeBackend::new().with_tracks(fixtures::tracks(300)).arc();
    let (view, cx) = mount(cx, &fake);
    view.update(cx, |t, _| t.release());
    view.read_with(cx, |t, _| assert!(resident(t).is_empty() && t.pager.total == 300, "rows dropped, total kept"));
    view.update(cx, |t, cx| t.restore(cx));
    cx.run_until_parked();
    view.read_with(cx, |t, _| assert_eq!(t.pager.get(0).map(|tr| tr.id.as_str()), Some("t0000")));
    assert_eq!(offsets(&fake)[&0], 2, "page 0 again after restore");
}
