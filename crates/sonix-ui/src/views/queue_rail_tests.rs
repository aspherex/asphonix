//! Queue rail under GPUI's deterministic executor (harness: `crate::tests`).
use super::*;
use crate::tests::{install, soon, ticks};
use gpui::TestAppContext;
use rand::rngs::StdRng;
use sonix_testkit::{FakeBackend, fixtures};

/// `(row track ids, current index)` of what the rail shows (`None` = nothing loaded / released).
pub(crate) fn rail_rows(rail: &QueueRail) -> Option<(Vec<String>, i64)> {
    rail.state.as_ref().map(|q| (q.items.iter().map(|it| it.track.id.clone()).collect(), q.current_index))
}

fn backend_rows(fake: &FakeBackend) -> (Vec<String>, i64) {
    fake.state(|s| (s.queue.iter().map(|it| it.track.id.clone()).collect(), s.current.map(|c| c as i64).unwrap_or(-1)))
}

fn rail(cx: &mut TestAppContext, fake: &Arc<FakeBackend>) -> (Entity<QueueRail>, Entity<PlayerModel>) {
    install(cx, fake);
    let player = cx.new(|_| PlayerModel::default());
    let menu = cx.new(|cx| ContextMenu::new(Theme::dark(), cx));
    let backend: Arc<dyn Backend> = fake.clone();
    let rail = cx.new(|cx| QueueRail::new(backend, crate::art::new_handle(1 << 20), &player, &menu, cx));
    cx.run_until_parked();
    (rail, player)
}

#[gpui::test]
fn the_rail_shows_the_queue_and_follows_player_events(cx: &mut TestAppContext) {
    let fake = FakeBackend::new().with_queue(fixtures::tracks(5), 0).arc();
    let (rail, player) = rail(cx, &fake);
    assert_eq!(rail.read_with(cx, |r, _| rail_rows(r)), Some(backend_rows(&fake)));
    // the engine moved on: a track change (PlayerEvent) refreshes the rail
    fake.player_next().unwrap();
    fake.player_next().unwrap();
    let st = fake.state(|s| s.player.clone());
    player.update(cx, |p, cx| p.apply_state(st, true, cx));
    cx.run_until_parked();
    assert_eq!(rail.read_with(cx, |r, _| rail_rows(r)).map(|r| r.1), Some(2));
    // minimize drops the rows, the next refresh brings them back
    rail.update(cx, |r, _| r.release());
    assert_eq!(rail.read_with(cx, |r, _| rail_rows(r)), None);
    rail.update(cx, |r, cx| r.refresh(cx));
    cx.run_until_parked();
    assert_eq!(rail.read_with(cx, |r, _| rail_rows(r)), Some(backend_rows(&fake)));
}

/// Removing a selection: the backend calls run in order (highest index first, so the lower indices stay valid),
/// then the rail re-reads; under any interleaving the rail ends up showing exactly the backend's queue.
#[gpui::test(iterations = 30)]
fn a_multi_row_removal_runs_in_order_then_refreshes(cx: &mut TestAppContext, mut rng: StdRng) {
    let fake = FakeBackend::new().with_queue(fixtures::tracks(6), 3).arc();
    let (rail, _player) = rail(cx, &fake);
    ticks(cx, &mut rng);
    rail.update(cx, |r, cx| r.remove(vec![4, 0, 2, 4], cx));
    cx.run_until_parked();
    let (ids, cur) = backend_rows(&fake);
    assert_eq!(ids, ["t0001", "t0003", "t0005"], "rows 0, 2 and 4 of the list as shown are gone");
    assert_eq!(cur, 1, "the playing row followed its track");
    let removed: Vec<String> = fake.calls_to("player_remove_from_queue").into_iter().map(|c| c.args).collect();
    assert_eq!(removed, ["4", "2", "0"], "deduplicated, highest first");
    assert_eq!(rail.read_with(cx, |r, _| rail_rows(r)), Some((ids, cur)));
}

/// Two refreshes race while the queue changes between them (a track change and a QueueChanged in a row).
///
/// Found the missing ordering guard in `QueueRail::refresh` (the answer read before the change could land last and
/// the rail showed the old queue); fixed with `refresh_seq`. Replay a failure with `SEED=<n> cargo test -p sonix-ui racing_refreshes`.
#[gpui::test(iterations = 100)]
fn racing_refreshes_end_on_the_latest_queue(cx: &mut TestAppContext) {
    let fake = FakeBackend::new().with_queue(fixtures::tracks(4), 0).arc();
    let (rail, _player) = rail(cx, &fake);
    rail.update(cx, |r, cx| r.refresh(cx));
    let (f2, r2) = (fake.clone(), rail.clone());
    soon(cx, move |cx| {
        f2.player_enqueue(EnqueueRequest { track_ids: vec!["x".into()], position: EnqueuePosition::End }).unwrap();
        r2.update(cx, |r, cx| r.refresh(cx));
    });
    cx.run_until_parked();
    assert_eq!(rail.read_with(cx, |r, _| rail_rows(r)), Some(backend_rows(&fake)));
}
