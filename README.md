# SoniX.native

A native (GPUI) desktop UI for SONIX, the bit-perfect Navidrome/Subsonic player. It is a
separate project from the Tauri + Svelte app (`SoniX`, which keeps shipping unchanged); in a later
phase it will share the Rust core with it.

**Why:** the WebView build needs ~120-185 MB private working set and ~380 ms to a visible
window. The GPUI spike measured ~30-40 MB, ~300 ms, 120 Hz scrolling/animation and a restore from
minimize in ~15 ms. This repo turns the spike into a structured app that looks like the web app.

**Status (phase 1):** shell, Tracks (54,515 paged rows), dock, Now Playing, minimal queue rail,
widget layer, minimize/restore behaviour, perf harness, all against a deterministic **mock**
backend (the title bar says `MOCK`). Everything else is tracked in `docs/PORTING.md`;
what does not match the web app yet is listed in `docs/PARITY.md`.

## Parity with the web app (measured)

`parity/` drives the web app (mock backend, headless Edge over CDP) and this app (a `parity`-feature build with a local control
channel) through the same scenarios and diffs boxes, pixels, animation frames, behaviour and text rendering. Results and
method: `docs/PARITY.md`.

```
npm ci   # once, in the web reference worktree (D:Codesonix-parity-ref)
cargo build --profile fast --features parity -p sonix-ui   # never a release build
cargo build --release -p sonix-parity                      # image metrics
node parity/run.mjs [--only <substring>]    # scenarios -> parity/out/<scenario>/{web,native,diff}.png, boxes.json, metrics.json
node parity/run.mjs --behavior              # scripted input, state dumps compared per step
node parity/textfidelity.mjs                # text crops, LCD vs grayscale web
node parity/report.mjs                      # regenerate the tables in docs/PARITY.md
```

## Layout

```
crates/sonix-api   the command contract as Rust (types + `Backend` trait + events)
crates/sonix-mock  deterministic mock backend (bit-exact port of mock.ts generators)
crates/sonix-ui    the GPUI app, binary `sonix-native` (`--features parity`: automation hook, never in release)
crates/sonix-parity image diff / text-crop metrics for the parity runner (`parity-diff`)
parity/            Node runner (CDP + control channel), scenarios, behaviours, selector map
assets/            fonts (TTF), icons (split from the SoniX sprite), pre-rendered backdrops
scripts/           perf.ps1, shot.ps1, gen-icons.mjs
docs/              PERF, PORTING, PARITY, GPUI-GAPS, screenshots
```

## Run

```
cargo run -p sonix-ui --release           # or: cargo build --release -> target/release/sonix-native.exe
cargo test                                # 50 tests
cargo clippy --all-targets -- -D warnings
powershell -File scripts/perf.ps1 -Runs 3 # needs a release build; writes perf-out/
powershell -File scripts/shot.ps1 -Name tracks-dark -Play [-Light] [-Route nowplaying] [-Size 1024x640] [-Menu 3]
cargo run -p sonix-ui --example gen_backdrop   # regenerate assets/bg/*.png
```

Env hooks (screenshots/perf only): `SONIX_ROUTE`, `SONIX_LIGHT`, `SONIX_SIZE=WxH`, `SONIX_PLAY`,
`SONIX_MENU=<row>`, `SONIX_SCRIPT`, `SONIX_FPSLOG`, `SONIX_LOG=<file>`.
Theme choice is stored in `%APPDATA%\SoniX.native\ui.json`.

Windows only for now (the minimize hook and working-set trim are `cfg(windows)`; everything else
is cross-platform Rust). GPUI 0.2.2 from crates.io, unmodified.
