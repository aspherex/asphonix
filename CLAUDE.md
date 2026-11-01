# CLAUDE.md (SoniX.native)

Native GPUI UI for SONIX. The Tauri/Svelte app at `C:\Users\<user>\Documents\Code\SoniX` is the
design and behaviour reference (read-only from here: never modify it or its worktrees).

## Rules

- **GPUI 0.2.2 from crates.io, no fork, no vendored patches** (the spike's `gpui-vsync.patch` is
  not used). Work around gaps in our own code; if impossible, write it to `docs/GPUI-GAPS.md` and
  leave an extension point. Letter-spacing = `src/tracking.rs`; gradients = pre-rendered PNGs
  (`assets/bg`); minimize = HWND subclass on `WM_SIZE` (`src/winffi.rs`), no polling.
- **Real data only** in the product. In phase 1 the data is the mock, labelled in the title bar.
- **Look 1:1** with the web app: tokens in `src/theme.rs`, type scale in `src/tracking.rs`
  (`ty!` table from tokens.css), zero radius, hard shadows only on small static elements, both themes.
  Every margin/padding/gap/size/timing lives in `src/metrics.rs` with its CSS source; single-line text goes through
  `text::txt` (letter-spacing, ellipsis, CSS baseline); elements the parity runner compares are tagged `probe::pid("id", el)`
  (identity in normal builds). Change a value in `metrics.rs`, then re-run `node parity/run.mjs` (docs/PARITY.md).
  Buttons reset letter-spacing in the web app (UA stylesheet): text inside `EntityLink`/`sort-btn` uses `.no_tracking()`.
- **Perf budget:** <= 40 MB private WS in normal use, 120 Hz, no CPU when nothing changes beyond
  GPUI's vsync thread, on minimize stop timers/animations, drop caches/rows, `EmptyWorkingSet`.
  Animation only via `request_animation_frame` while visible + playing + not suspended.
  Re-measure with `scripts/perf.ps1` before/after anything that touches render paths.
- Only stop processes you started, by exact PID. Never kill by name/window title. Never touch the
  user's installed SONIX. Never contact a real server or read `.env.local`.
- The `parity` cargo feature (control channel on 127.0.0.1, test clock, probes, PNG capture) must never be enabled in a release
  build; verify with `strings` / size when touching it. Parity runs use their own mock instance (fixed seed, manual clock).
- Release profile stays tuned (`lto`, `codegen-units = 1`, `opt-level = 3`, `panic = "abort"`, `strip`).

## Architecture

- `sonix-api`: `Backend` = `AuthApi + LibraryApi + BrowseApi + SearchApi + PlaylistsApi + StarsApi +
  PlayerApi + OutputApi + SettingsApi + ArtApi` (+ `subscribe() -> Receiver<Event>`), sync and
  `Send + Sync`, called from GPUI's background executor only. Non-phase-1 methods default to
  `ApiError::unimplemented`. Types mirror `ipc.rs`/`types.ts` serde attributes (tested).
  **This trait is the seam: phase 2 plugs a real implementation on top of a shared `sonix-core`
  extracted from SoniX's `src-tauri`; no UI code should change.**
- `sonix-mock`: `library.rs` is a bit-exact port of the mock.ts generators (golden values from
  the JS), `backend.rs` simulates playback (condvar-blocked ticker, spectrum only while active),
  `art.rs` procedural covers at exact bucket sizes.
- `sonix-ui`: `views/shell.rs` (Root: routing, event pump, suspend/resume), `chrome.rs` (title bar,
  top bar, sidebar), `tracks.rs`, `dock.rs`, `nowplaying.rs`, `queue_rail.rs`; `widgets/` (button,
  tooltip, slider, scrollbar, text_input, context_menu); `pager.rs`, `selection.rs`, `router.rs`,
  `columns.rs` are pure and unit-tested. Theme/services are GPUI globals. Views are separate
  entities so the 4 Hz position tick re-renders only the dock/Now Playing.
- GPUI gotchas found: svgs do not inherit the parent's `text_color` (use `icon()`/`icon_g()`);
  percentage widths break `text_ellipsis` in column containers (`typo::ellipsis_line`);
  `gen` is a reserved word in edition 2024; the Bash tool truncates very large heredocs (use files).

## Phase 2 (next)

Extract `sonix-core` from SoniX `src-tauri` (subsonic client, library DB, audio engine, scrobble,
session), implement `Backend` on it, port the remaining routes (`docs/PORTING.md`), close the gaps in
`docs/PARITY.md` (parity harness, box-geometry diff, animation parity), accessibility (needs
upstream accesskit in GPUI), macOS minimize hook.
