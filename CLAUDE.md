# CLAUDE.md (contributor notes)

ASPHONIX is a native GPUI desktop player for Navidrome/Subsonic. See README.md for the overview.

## Rules

- **GPUI 0.2.2 from crates.io, no fork, no vendored patches.** Work around gaps in our own code; if impossible,
  write it to `docs/GPUI-GAPS.md` and leave an extension point. Letter-spacing = `src/tracking.rs`; gradients =
  pre-rendered PNGs (`assets/bg`, `cargo run -p asphonix-ui --example gen_backdrop`); minimize = HWND subclass on
  `WM_SIZE` (`src/winffi.rs`), no polling.
- **Real data only.** Never show numbers the app does not measure or the server does not provide.
- **Look:** tokens in `src/theme.rs`, type scale in `src/tracking.rs`, zero radius, hard shadows only on small
  static elements, both themes. Every margin/padding/gap/size/timing lives in `src/metrics.rs`; single-line text
  goes through `text::txt` (letter-spacing, ellipsis, baseline).
- **Text is translated.** Every user-facing string is a message in `crates/asphonix-i18n/locales/en/asphonix.ftl` with
  its German translation in `locales/de/asphonix.ftl`, used as `tr!(Key)` / `tr!(Key, n = 3)` (keys are generated, a
  typo does not compile). Write normal case; upper-casing is a render-time style. Counts go through Fluent plurals,
  numbers/dates through `asphonix_i18n::fmt`. `crates/asphonix-i18n/tests/usage.rs` fails on English literals passed to
  `txt`/tooltips/menus/buttons and on unused keys. Logs, MCP tool texts and env vars stay English.
  `ASPHONIX_LOCALE=en-XA` shows the pseudo-locale (accented, bracketed, ~40 % longer) to spot hard-coded or clipped text.
- **Perf budget** (docs/PERF.md): <= 40 MB private working set in normal use, refresh-rate animation, no CPU when
  nothing changes beyond GPUI's vsync thread; on minimize stop timers/animations, drop caches/rows,
  `EmptyWorkingSet`. Animation only via `request_animation_frame` while visible + playing + not suspended.
  Re-measure with `scripts/perf.ps1` before/after anything that touches render paths.
- Release profile stays tuned (`lto`, `codegen-units = 1`, `opt-level = 3`, `panic = "abort"`, `strip`). Use
  `--profile fast` for iteration, `--profile relsym` for crash analysis; report numbers from `release` only.
- Tests never contact a real server. `crates/asphonix-real/tests/real_server.rs` is `#[ignore]`d and only runs
  against a server you point it at.
- Only stop processes you started, by exact PID.

## Architecture

- `asphonix-api`: `Backend` = `AuthApi + LibraryApi + BrowseApi + SearchApi + PlaylistsApi + StarsApi + PlayerApi +
  OutputApi + SettingsApi + ArtApi` (+ `subscribe() -> Receiver<Event>`), sync and `Send + Sync`, called from
  GPUI's background executor only. This trait is the only seam between UI and backend.
- `asphonix-core`: Subsonic client, SQLite library mirror + sync, audio engine (symphonia decode, WASAPI exclusive
  output, gapless, ReplayGain, spectrum/waveform), scrobbling, session/continuity, smart playlists, MCP bridge,
  updater.
- `asphonix-real`: the `Backend` implementation on top of `asphonix-core`, plus the cover-art disk cache.
- `asphonix-ui`: `views/shell.rs` (root: routing, event pump, suspend/resume), `chrome.rs` (title bar, top bar,
  sidebar), `tracks.rs`, `dock.rs`, `nowplaying.rs`, `queue_rail.rs`, `views/library/*`, `views/lists/*`;
  `widgets/` (button, tooltip, slider, scrollbar, text_input, context_menu, smooth scrolling); `pager.rs`,
  `selection.rs`, `router.rs`, `columns.rs` are pure and unit-tested. Theme/services are GPUI globals. Views are
  separate entities so the 4 Hz position tick re-renders only the dock/Now Playing; animated elements render in
  one live layer (`live.rs`) so the rest of the frame stays cached (`part.rs`).
- `asphonix-mcp`: stdio MCP server that connects to the running app's bridge.
- `asphonix-mcp-protocol`: discovery file, handshake, line format and tool catalog; `asphonix-core::mcp_bridge` and
  `asphonix-mcp` both depend on it (core uses `asphonix-mcp` only in tests).
- `asphonix-logging`: the only crate that installs a `tracing` subscriber (`init` in both binaries); every other crate just
  emits `tracing` events. Files: `<data dir>/logs/asphonix.YYYY-MM-DD.jsonl` and `asphonix-mcp.*.jsonl`, 7 days kept.
  Log any URL through `asphonix_core::redact::redact_url` (Subsonic auth is in the query string); never log
  passwords, tokens or keyring values; nothing logs periodically while idle; nothing logs between the
  `rt-path:begin`/`rt-path:end` markers in `audio/output/*` (counters + engine thread instead, test-enforced).
- `asphonix-i18n` (no GPUI): Fluent catalogs (`locales/<lang>/asphonix.ftl`, only the active one + English parsed),
  generated `Key` enum, argument-less messages pre-formatted per locale (a lookup on the render path), `fmt`
  (separators/dates from the OS on Windows). `asphonix-ui/src/i18n.rs` holds the `I18n` global; a switch
  (Settings > Language) calls `refresh_windows`, which re-renders cached views too.
- `asphonix-testkit` (dev-dependency only): `FakeBackend`, `MockSubsonic`, fixtures. How to test, replay seeds: docs/TESTING.md.

## GPUI gotchas

- SVGs do not inherit the parent's `text_color` (use `icon()`/`icon_g()`).
- `Window::request_animation_frame` outside render/paint panics; listeners `cx.notify()` instead.
- `gen` is a reserved word in edition 2024.
- More in `docs/GPUI-GAPS.md`.
