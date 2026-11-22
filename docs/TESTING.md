# Testing SONIX

`cargo test --workspace --locked` runs everything below. No test contacts a real server (`real_server.rs` and
`real_wave.rs` are `#[ignore]`d). New tests do not sleep for correctness. Some older sonix-real tests still poll real
engine and sync threads with a bounded wait.

## Kinds of tests and where they live

| What | Where | Technique |
|---|---|---|
| Unit tests | next to the code (`#[cfg(test)] mod tests`) | plain `#[test]` |
| UI logic: routing, stale answers, paging, queue rail, dock / Now Playing, suspend/resume | `crates/sonix-ui/src/views/*_tests.rs` (`#[path]` modules of their view), harness in `crates/sonix-ui/src/tests/` | `#[gpui::test]` + `TestAppContext` + `sonix_testkit::FakeBackend` |
| Queue / shuffle / Flow / A-B loop invariants | `crates/sonix-core/tests/*_props.rs`, `crates/sonix-core/src/audio/queue_prop_tests.rs` | `proptest` |
| MCP tool catalog and JSON-RPC replies | `crates/sonix-mcp/tests/snapshots.rs` | `insta` snapshots |
| Timeouts (MCP bridge) | `crates/sonix-core/tests/mcp_bridge_paused_time.rs` | `#[tokio::test(start_paused = true)]` |
| Backend write/read paths over HTTP | `crates/sonix-real/src/*_tests.rs` | `sonix_testkit::MockSubsonic` (127.0.0.1) |

## The test kit (`crates/sonix-testkit`, dev-dependency only)

- `fixtures`: `track`, `tracks(n)`, `album`, `artist`, `playlist`, `player_state`, `session_connected`, `page` (an
  offset/limit slice with the true total). Change single fields with struct update syntax.
- `FakeBackend`: every `Backend` method, answered from a public `FakeState`. It records calls (`calls`,
  `calls_to`, `count`), can fail a method (`fail(method, err)` / `heal()`), sends events (`emit`), and runs a
  hook before and after each answer (`set_hook`). Player commands change the fake state but emit nothing; the
  test decides which events arrive.
- `MockSubsonic`: a local Subsonic server. It logs each request with the raw target and the parsed query pairs. It
  answers in this order: injected failure (`fail(Some("*" | endpoint))`, `fail_with`), then the per-test handler
  (`with_handler`, `None` falls through), then built-in stateful playlists, then the empty OK envelope. It also
  has latency (`set_latency`, `set_latency_for`). The server stops when the mock is dropped.

## Deterministic UI tests (`#[gpui::test]`)

GPUI's test dispatcher runs every task on the test thread and, seeded, picks the next runnable task at random.
`crate::tests::install` makes the `FakeBackend` take part in that schedule: before an answer and again after it
has been computed, a seeded random number (0-9) of other tasks run.

- On the background executor, which is where the UI calls the backend, those tasks include foreground ones. So a
  newer request can be sent and answered while an older one is still in flight, and the older answer can land last.
- A call made synchronously on the UI thread may be inside an entity update, so there only background tasks run.

`soon(cx, f)` runs a step from a foreground task, the way an engine event arrives. Use it for "the state changes
while a request is in flight": a step in the test body always runs between scheduler steps and cannot overlap a
call.

Time: `cx.executor().advance_clock(d)` fires `BackgroundExecutor::timer`s. `gpui::Timer` is smol's wall-clock timer
and is not controlled by the test clock (see docs/GPUI-GAPS.md). Events are fed to `Shell::on_event` directly: the
production event pump forwards them from an OS thread, which the dispatcher does not schedule.

### Replaying a randomized failure

A failing `#[gpui::test(iterations = N)]` prints `failing seed: <n>`. Replay exactly that run:

```sh
SEED=<n> cargo test -p sonix-ui <test_name>                # only seed n
SEED=<n> ITERATIONS=50 cargo test -p sonix-ui <test_name>   # seeds n .. n+49
ITERATIONS=500 cargo test -p sonix-ui gpui_tests             # more seeds for every gpui test
```

`SEED` and `ITERATIONS` apply to every `#[gpui::test]` in the run, so filter by test name. (In PowerShell, use
`$env:SEED=17; cargo test ...`.) Fix the bug or the test; never retry until it is green.

### Known bugs pinned by ignored tests

- `views::queue_rail::gpui_tests::racing_refreshes_end_on_the_latest_queue`: `QueueRail::refresh` has no ordering
  guard, so an older answer can overwrite a newer one (`SEED=17`).
- `views::shell::gpui_tests::answers_in_flight_at_minimize_do_not_refill_released_rows`: answers that are in flight
  at minimize refill the released rail and track rows (every seed).

Run them with `cargo test -p sonix-ui -- --ignored`. Remove the `#[ignore]` together with the fix.

## Property tests (`proptest`)

A failure prints the minimal (shrunk) input. Proptest writes its seed to a `*.proptest-regressions` file next to
the test source, for example `crates/sonix-core/tests/queue_props.proptest-regressions`. Commit that file: every
later run replays it first.

```sh
PROPTEST_CASES=4096 cargo test -p sonix-core --test queue_props    # more cases (default 256)
PROPTEST_RNG_SEED=<n> cargo test -p sonix-core --test queue_props   # a fixed RNG seed
```

## Snapshots (`insta`)

A changed MCP catalog or reply fails `crates/sonix-mcp/tests/snapshots.rs`. Look at the diff in the test output,
then accept it with `cargo insta review` (needs `cargo install cargo-insta`) or with
`INSTA_UPDATE=always cargo test -p sonix-mcp --test snapshots`. Commit the `.snap` files. Maps are snapshotted with
sorted keys, because a workspace build enables `serde_json/preserve_order`.

## Virtual time in tokio code

`#[tokio::test(start_paused = true)]` starts the test with the clock paused. Whenever every task is idle, the clock
jumps to the next timer, so a 60 s idle timeout passes in no real time. A running `spawn_blocking` task stops
those jumps, so advance the clock by hand with `tokio::time::advance` there. Only `tokio::time` is virtual;
`std::time::Instant` and real sockets are not.

## CI

- `.github/workflows/build.yml` is the gate: clippy and `cargo test --workspace --locked` on Windows. On macOS and
  Linux they still run, but failures are reported without blocking.
- `.github/workflows/tests-extended.yml` is informational and runs nightly, on pushes to master and on manual
  dispatch:
  - `cargo llvm-cov` coverage, uploaded as an artifact and shown in the run summary.
  - The randomized tests with `ITERATIONS=300` and `PROPTEST_CASES=4096`.
