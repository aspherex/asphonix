//! Shell under GPUI's deterministic executor: routing, stale answers, player events reaching the dock / Now Playing,
//! the Now Playing waveform poll on the test clock, suspend/resume with requests in flight. Harness: `crate::tests`.
//! Events are fed to `Shell::on_event` directly: the production pump forwards them from a real OS thread, which the
//! test scheduler does not control.
use super::*;
use crate::tests::{install, soon, ticks};
use gpui::{TestAppContext, VisualTestContext};
use rand::rngs::StdRng;
use asphonix_api::{PlaybackStatus, PlayerApi, QueueSourceInfo, SourceKind, SourceMode, Track, Waveform};
use asphonix_testkit::{FakeBackend, fixtures};
use std::cell::Cell;
use std::rc::Rc;

fn mount<'a>(cx: &'a mut TestAppContext, fake: &Arc<FakeBackend>) -> (Entity<Shell>, &'a mut VisualTestContext) {
    install(cx, fake);
    let (shell, cx) = cx.add_window_view(Shell::new);
    cx.run_until_parked();
    (shell, cx)
}

fn send(shell: &Entity<Shell>, cx: &mut VisualTestContext, ev: Event) {
    shell.update(cx, |sh, cx| sh.on_event(ev, cx));
}

fn state_of(t: &Track, status: PlaybackStatus, pos: f64) -> Event {
    Event::PlayerState(Box::new(fixtures::player_state(Some(t.clone()), status, pos)))
}

/// Counts `notify()`s of `e` (what makes a cached view repaint).
fn notifications<T: 'static>(e: &Entity<T>, cx: &mut VisualTestContext) -> (Rc<Cell<usize>>, gpui::Subscription) {
    let n = Rc::new(Cell::new(0));
    let n2 = n.clone();
    let sub = cx.update(|_, cx| cx.observe(e, move |_, _| n2.set(n2.get() + 1)));
    (n, sub)
}

fn waveform_polls(fake: &FakeBackend, id: &str) -> usize {
    fake.calls_to("player_get_waveform").iter().filter(|c| c.args.contains(&format!("\"{id}\""))).count()
}

fn source(id: &str) -> QueueSourceInfo {
    QueueSourceInfo { kind: SourceKind::Playlist, label: id.to_uppercase(), id: Some(id.into()), mode: SourceMode::InOrder, remaining: 0, arc: None, flow_coverage: None }
}

#[gpui::test]
fn startup_loads_counts_and_stays_off_the_login_screen(cx: &mut TestAppContext) {
    let fake = FakeBackend::new().with_tracks(fixtures::tracks(250)).with_albums(fixtures::albums(12)).arc();
    let (shell, cx) = mount(cx, &fake);
    shell.read_with(cx, |sh, _| {
        assert_eq!(*sh.router.current(), Route::Home, "a connected session keeps the start page");
        assert_eq!((sh.counts.tracks, sh.counts.albums), (Some(250), Some(12)));
        assert_eq!(sh.lib_stage_route, Some(Route::Home));
    });
    assert_eq!(fake.subscriber_count(), 1, "one event subscription per shell");
}

#[gpui::test]
fn a_disconnected_session_opens_the_login_screen(cx: &mut TestAppContext) {
    let fake = FakeBackend::new().arc();
    fake.state(|s| s.session.state = asphonix_api::ConnectionState::Disconnected);
    let (shell, cx) = mount(cx, &fake);
    shell.read_with(cx, |sh, _| assert_eq!(*sh.router.current(), Route::Login));
}

#[gpui::test]
fn routing_navigates_back_and_forward_and_remounts_library_pages(cx: &mut TestAppContext) {
    let fake = FakeBackend::new().with_albums(fixtures::albums(3)).arc();
    let (shell, cx) = mount(cx, &fake);
    let route = |shell: &Entity<Shell>, cx: &mut VisualTestContext| shell.read_with(cx, |sh, _| (sh.router.current().clone(), sh.lib_stage_route.clone()));
    shell.update(cx, |sh, cx| {
        sh.navigate(Route::Albums, cx);
        sh.navigate(Route::Album("al0001".into()), cx);
    });
    cx.run_until_parked();
    assert_eq!(route(&shell, cx), (Route::Album("al0001".into()), Some(Route::Album("al0001".into()))));
    assert_eq!(fake.count("browse_get_album"), 1);
    // navigating to the route already shown is a no-op: no remount, no re-query
    shell.update(cx, |sh, cx| sh.navigate(Route::Album("al0001".into()), cx));
    cx.run_until_parked();
    assert_eq!(fake.count("browse_get_album"), 1);
    shell.update(cx, |sh, cx| sh.go_back(cx));
    cx.run_until_parked();
    assert_eq!(route(&shell, cx), (Route::Albums, Some(Route::Albums)));
    shell.update(cx, |sh, cx| sh.go_forward(cx));
    cx.run_until_parked();
    assert_eq!(route(&shell, cx), (Route::Album("al0001".into()), Some(Route::Album("al0001".into()))));
    assert_eq!(fake.count("browse_get_album"), 2, "a library page is remounted (and re-queried) on every navigation");
    // Tracks is not a library page: no stage entity
    shell.update(cx, |sh, cx| sh.navigate(Route::Tracks, cx));
    cx.run_until_parked();
    assert_eq!(route(&shell, cx), (Route::Tracks, None));
}

#[gpui::test]
fn closing_now_playing_brings_the_parked_page_back(cx: &mut TestAppContext) {
    let fake = FakeBackend::new().with_albums(fixtures::albums(30)).arc();
    let (shell, cx) = mount(cx, &fake);
    shell.update(cx, |sh, cx| sh.navigate(Route::Albums, cx));
    cx.run_until_parked();
    let before = shell.read_with(cx, |sh, _| sh.lib_stage.as_ref().map(|v| v.entity_id()));
    shell.update(cx, |sh, cx| sh.toggle_now_playing(cx));
    cx.run_until_parked();
    shell.read_with(cx, |sh, _| assert!(sh.lib_stage.is_none() && sh.lib_parked.is_some()));
    shell.update(cx, |sh, cx| sh.toggle_now_playing(cx));
    cx.run_until_parked();
    let after = shell.read_with(cx, |sh, _| sh.lib_stage.as_ref().map(|v| v.entity_id()));
    assert!(before.is_some() && before == after, "same view entity (scroll and data kept), not a remount");
}

/// Album A is opened, then album B before A's answer arrives (random delays, random interleaving): B's page shows B.
#[gpui::test(iterations = 50)]
fn a_stale_answer_after_a_route_change_never_fills_the_new_page(cx: &mut TestAppContext) {
    let fake = FakeBackend::new().with_albums(fixtures::albums(2)).arc();
    let (shell, cx) = mount(cx, &fake);
    shell.update(cx, |sh, cx| sh.navigate(Route::Album("al0000".into()), cx));
    let sh2 = shell.clone();
    soon(cx, move |cx| sh2.update(cx, |sh, cx| sh.navigate(Route::Album("al0001".into()), cx)));
    cx.run_until_parked();
    let ids = shell.read_with(cx, |sh, cx| {
        let page = sh.lib_stage.clone().expect("album page").downcast::<crate::views::library::album_detail::AlbumDetailView>().unwrap_or_else(|_| panic!("album detail"));
        let (routed, shown) = page.read(cx).test_ids();
        (routed.to_string(), shown.map(str::to_string))
    });
    assert_eq!(ids, ("al0001".to_string(), Some("al0001".to_string())));
    assert_eq!(fake.count("browse_get_album"), 2, "both albums were asked for");
}

/// Two queue refreshes race (the backend's source changes between them): the newest answer wins in every order.
#[gpui::test(iterations = 50)]
fn queue_source_refreshes_keep_the_newest_answer(cx: &mut TestAppContext) {
    let fake = FakeBackend::new().with_queue(fixtures::tracks(3), 0).arc();
    fake.state(|s| s.queue_source = Some(source("p1")));
    let (shell, cx) = mount(cx, &fake);
    send(&shell, cx, Event::QueueChanged);
    let (f2, sh2) = (fake.clone(), shell.clone());
    soon(cx, move |cx| {
        f2.state(|s| s.queue_source = Some(source("p2")));
        sh2.update(cx, |sh, cx| sh.on_event(Event::QueueChanged, cx));
    });
    cx.run_until_parked();
    let id = shell.read_with(cx, |sh, cx| sh.player.read(cx).source.as_ref().and_then(|s| s.id.clone()));
    assert_eq!(id.as_deref(), Some("p2"));
}

#[gpui::test]
fn player_state_events_repaint_dock_and_now_playing_only_when_the_shown_state_changes(cx: &mut TestAppContext) {
    let fake = FakeBackend::new().arc();
    let (shell, cx) = mount(cx, &fake);
    let t = Track { duration: 3600.0, ..fixtures::track("t1") };
    let (dock, np) = shell.read_with(cx, |sh, _| (sh.dock.clone(), sh.np.clone()));
    let (dock_n, _s1) = notifications(&dock, cx);
    let (np_n, _s2) = notifications(&np, cx);

    let queue_reads = fake.count("player_get_queue");
    send(&shell, cx, state_of(&t, PlaybackStatus::Playing, 10.05));
    cx.run_until_parked();
    assert_eq!(shell.read_with(cx, |sh, cx| sh.player.read(cx).current_id().map(str::to_string)), Some("t1".into()));
    assert!(dock_n.get() > 0 && np_n.get() > 0, "a new track repaints both");
    assert!(fake.count("player_get_queue") > queue_reads, "a track change refreshes the queue source");

    // the 4 Hz position tick inside the same second (and the same seek-bar pixel): the cached dock does not repaint
    dock_n.set(0);
    np_n.set(0);
    send(&shell, cx, state_of(&t, PlaybackStatus::Playing, 10.10));
    cx.run_until_parked();
    assert_eq!(dock_n.get(), 0, "same second, same pixel");
    assert!(np_n.get() > 0, "Now Playing observes every state");

    send(&shell, cx, state_of(&t, PlaybackStatus::Playing, 11.0));
    cx.run_until_parked();
    assert!(dock_n.get() > 0, "the shown second changed");
    dock_n.set(0);
    send(&shell, cx, state_of(&t, PlaybackStatus::Paused, 11.0));
    cx.run_until_parked();
    assert!(dock_n.get() > 0, "status changed");
}

#[gpui::test]
fn now_playing_polls_the_waveform_on_the_test_clock_and_drops_the_old_track(cx: &mut TestAppContext) {
    let fake = FakeBackend::new().with_queue(fixtures::tracks(3), 0).arc();
    let (shell, cx) = mount(cx, &fake);
    assert_eq!(waveform_polls(&fake, "t0000"), 0, "no waveform while Now Playing is closed");
    shell.update(cx, |sh, cx| sh.navigate(Route::NowPlaying, cx));
    cx.run_until_parked();
    assert_eq!(waveform_polls(&fake, "t0000"), 1);
    // not done yet: asked again every 400 ms of (virtual) time
    cx.executor().advance_clock(Duration::from_millis(399));
    assert_eq!(waveform_polls(&fake, "t0000"), 1);
    cx.executor().advance_clock(Duration::from_millis(1));
    assert_eq!(waveform_polls(&fake, "t0000"), 2);
    let done = Waveform { track_id: "t0000".into(), buckets: vec![9; 240], filled: 240, done: true };
    fake.state(|s| s.waveforms.insert("t0000".into(), done));
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.executor().advance_clock(Duration::from_secs(5));
    assert_eq!(waveform_polls(&fake, "t0000"), 3, "a done waveform ends the poll");

    // next track: its own poll; the old track is never asked again
    let t1 = fixtures::tracks(3)[1].clone();
    send(&shell, cx, state_of(&t1, PlaybackStatus::Playing, 0.0));
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(800));
    assert_eq!((waveform_polls(&fake, "t0000"), waveform_polls(&fake, "t0001")), (3, 3));

    // failures: retried at 1.5 s, at most 3 tries
    fake.fail("player_get_waveform", asphonix_api::ApiError::new(asphonix_api::ErrorCode::Network, "down"));
    let t2 = fixtures::tracks(3)[2].clone();
    send(&shell, cx, state_of(&t2, PlaybackStatus::Playing, 0.0));
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(1499));
    assert_eq!(waveform_polls(&fake, "t0002"), 1);
    cx.executor().advance_clock(Duration::from_secs(30));
    assert_eq!(waveform_polls(&fake, "t0002"), 3);
}

/// Minimize while backend answers are in flight, events keep arriving while minimized, restore: nothing repaints while
/// suspended, the state is still kept current, and after restore the views show what the backend has now.
#[gpui::test(iterations = 20)]
fn suspend_and_resume_with_requests_in_flight(cx: &mut TestAppContext, mut rng: StdRng) {
    let tracks = fixtures::tracks(3);
    let fake = FakeBackend::new().with_queue(tracks.clone(), 0).arc();
    let (shell, cx) = mount(cx, &fake);
    let (dock, np) = shell.read_with(cx, |sh, _| (sh.dock.clone(), sh.np.clone()));

    send(&shell, cx, Event::QueueChanged);
    ticks(cx, &mut rng);
    shell.update_in(cx, |sh, window, cx| sh.on_minimize_change(true, window, cx));
    let (dock_n, _s1) = notifications(&dock, cx);
    let (np_n, _s2) = notifications(&np, cx);
    fake.state(|s| {
        s.current = Some(1);
        s.player = fixtures::player_state(Some(tracks[1].clone()), PlaybackStatus::Playing, 3.0);
    });
    send(&shell, cx, Event::PlayerState(Box::new(fake.state(|s| s.player.clone()))));
    send(&shell, cx, Event::QueueChanged);
    send(&shell, cx, Event::LibraryChanged);
    cx.run_until_parked();
    assert_eq!((dock_n.get(), np_n.get()), (0, 0), "nothing repaints while minimized");
    shell.read_with(cx, |sh, cx| {
        assert!(sh.suspended);
        assert_eq!(sh.player.read(cx).current_id(), Some("t0001"), "state is kept current while minimized");
    });
    assert!(fake.state(|s| s.suspended && !s.spectrum_active), "backend told: suspended, no spectrum");

    let before = fake.count("player_get_queue");
    shell.update_in(cx, |sh, window, cx| sh.on_minimize_change(false, window, cx));
    cx.run_until_parked();
    assert!(dock_n.get() > 0 && np_n.get() > 0, "restore repaints");
    assert!(!fake.state(|s| s.suspended));
    assert!(fake.count("player_get_queue") > before, "restore re-reads the queue");
    let rail = shell.read_with(cx, |sh, cx| crate::views::queue_rail::gpui_tests::rail_rows(sh.queue.read(cx)));
    assert_eq!(rail.map(|(_, cur)| cur), Some(1), "the rail shows the queue as it is now");
}

/// Minimize lands while a queue refresh and a page fetch are in flight: the rows released by `suspend` stay released
/// (an answer arriving after minimize must not bring them back while minimized).
///
/// Found that answers in flight at minimize (and renders while released) refilled the rail and up to a page of rows;
/// fixed by `QueueRail::release` invalidating `refresh_seq`, `Pager::release` bumping the epoch and `TracksView`
/// requesting no pages until `restore`.
#[gpui::test(iterations = 50)]
fn answers_in_flight_at_minimize_do_not_refill_released_rows(cx: &mut TestAppContext) {
    let fake = FakeBackend::new().with_queue(fixtures::tracks(3), 0).with_tracks(fixtures::tracks(500)).arc();
    let (shell, cx) = mount(cx, &fake);
    shell.update(cx, |sh, cx| sh.navigate(Route::Tracks, cx));
    cx.run_until_parked();
    send(&shell, cx, Event::QueueChanged);
    shell.update(cx, |sh, cx| sh.tracks.update(cx, |t, cx| t.refresh_rows(cx)));
    let wh = cx.update(|window, _| window.window_handle());
    let sh2 = shell.clone();
    soon(cx, move |cx| {
        let _ = wh.update(cx, |_, window, cx| sh2.update(cx, |sh, cx| sh.on_minimize_change(true, window, cx)));
    });
    cx.run_until_parked();
    let (rail, rows) = shell.read_with(cx, |sh, cx| {
        assert!(sh.suspended);
        (crate::views::queue_rail::gpui_tests::rail_rows(sh.queue.read(cx)), crate::views::tracks::gpui_tests::resident(sh.tracks.read(cx)).len())
    });
    assert_eq!((rail, rows), (None, 0), "released while minimized");
}

/// A queue change (`Event::QueueChanged`) reaches the docked rail while the window is live; minimized, only the
/// queue source is re-read (the rail is released and re-reads on restore).
#[gpui::test(iterations = 20)]
fn queue_changed_refreshes_the_rail_and_only_the_source_while_minimized(cx: &mut TestAppContext) {
    let fake = FakeBackend::new().with_queue(fixtures::tracks(3), 1).arc();
    let (shell, cx) = mount(cx, &fake);
    let backend_rows = |fake: &FakeBackend| fake.state(|s| (s.queue.iter().map(|it| it.track.id.clone()).collect::<Vec<_>>(), s.current.map(|c| c as i64).unwrap_or(-1)));
    let rail = |shell: &Entity<Shell>, cx: &mut VisualTestContext| shell.read_with(cx, |sh, cx| crate::views::queue_rail::gpui_tests::rail_rows(sh.queue.read(cx)));

    fake.player_enqueue(asphonix_api::EnqueueRequest { track_ids: vec!["x".into()], position: asphonix_api::EnqueuePosition::Next }).unwrap();
    let reads = fake.count("player_get_queue");
    send(&shell, cx, Event::QueueChanged);
    cx.run_until_parked();
    assert_eq!(rail(&shell, cx), Some(backend_rows(&fake)), "the rail shows the new row");
    // rail + queue source (+ one more rail refresh: the one-album static queue grew, which repaints play buttons)
    assert!(fake.count("player_get_queue") - reads >= 2);

    shell.update_in(cx, |sh, window, cx| sh.on_minimize_change(true, window, cx));
    cx.run_until_parked();
    let reads = fake.count("player_get_queue");
    send(&shell, cx, Event::QueueChanged);
    cx.run_until_parked();
    assert_eq!(fake.count("player_get_queue") - reads, 1, "minimized: the queue source only");
    assert_eq!(rail(&shell, cx), None, "the released rail stays empty");
}
