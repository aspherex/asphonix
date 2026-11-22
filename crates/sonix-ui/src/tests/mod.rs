//! `#[gpui::test]` UI tests: GPUI's deterministic test executor + [`sonix_testkit::FakeBackend`]. View tests live next
//! to their view (`views/*_tests.rs`, included with `#[cfg(test)] #[path]` so they reach private state); this module
//! holds the shared harness. Replaying a randomized failure: docs/TESTING.md (`SEED`, `ITERATIONS`).
mod smoke;

use crate::app::Services;
use crate::theme::Theme;
use gpui::{BackgroundExecutor, TestAppContext};
use sonix_testkit::FakeBackend;
use std::cell::Cell;
use std::sync::Arc;

thread_local! {
    /// A backend call made synchronously on the UI thread is on the stack (an entity update may hold the app).
    static SYNC_ON_UI: Cell<bool> = const { Cell::new(false) };
}

/// Number of steps of one `simulate_random_delay` (0..10, drawn from the test's seeded RNG).
fn random_steps(ex: &BackgroundExecutor) -> usize {
    let mut delay = std::pin::pin!(ex.simulate_random_delay());
    let mut cx = std::task::Context::from_waker(futures::task::noop_waker_ref());
    let mut n = 0;
    while std::future::Future::poll(delay.as_mut(), &mut cx).is_pending() {
        n += 1;
    }
    n
}

/// Backend latency under the test scheduler: before the fake answers and again after it computed the answer, a
/// seeded random number of other tasks run. On the background executor (where the UI makes its calls) that includes
/// foreground tasks: a newer request can be issued and answered while an older one is in flight, and an answer read
/// from older state can land last, so each seed of `#[gpui::test(iterations = N)]` sees another interleaving. A call
/// made synchronously on the UI thread may sit inside an entity update, so there only background tasks run.
fn latency(ex: &BackgroundExecutor) {
    if ex.is_main_thread() || SYNC_ON_UI.get() {
        let prev = SYNC_ON_UI.replace(true);
        ex.block(ex.simulate_random_delay());
        SYNC_ON_UI.set(prev);
    } else {
        for _ in 0..random_steps(ex) {
            if !ex.tick() {
                break;
            }
        }
    }
}

/// Theme + services globals (what `main` sets) around `fake`, whose answers get [`latency`].
pub(crate) fn install(cx: &mut TestAppContext, fake: &Arc<FakeBackend>) {
    let ex = cx.executor();
    fake.set_hook(move |_| latency(&ex));
    let backend: Arc<dyn sonix_api::Backend> = fake.clone();
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Services { backend, art: crate::art::new_handle(1 << 20) });
    });
}

/// Run between 0 and 31 single scheduler steps (seeded): lets the tasks started so far get a random distance ahead
/// before the test does its next step.
pub(crate) fn ticks(cx: &TestAppContext, rng: &mut rand::rngs::StdRng) {
    use rand::Rng;
    for _ in 0..rng.random_range(0..32) {
        if !cx.executor().tick() {
            break;
        }
    }
}

/// Run `f` from a foreground task, the way an engine event arrives: it can run while backend calls are in flight
/// (inside their [`latency`]), unlike a step of the test body, which always runs between scheduler steps.
pub(crate) fn soon(cx: &TestAppContext, f: impl FnOnce(&mut gpui::App) + 'static) {
    cx.spawn(|cx| async move {
        let _ = cx.update(f);
    })
    .detach();
}
