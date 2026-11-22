# Performance

SONIX treats memory, CPU and latency as features. These are the budgets every change is measured against:

| Metric | Budget |
|---|---|
| process start -> window visible | ~300 ms |
| private working set, normal use (library browsing, Now Playing animating) | <= 40 MB |
| minimized | a few MB (caches and rows dropped, `EmptyWorkingSet`), ~0 % CPU |
| restore -> first rendered frame | ~15 ms |
| frame pacing | display refresh rate (e.g. 8.3 ms at 120 Hz) while scrolling and on Now Playing |
| CPU when nothing changes | GPUI's vsync thread only (see [GPUI-GAPS](GPUI-GAPS.md)) |

Rules that keep it there:

- Animation runs only through `request_animation_frame` while the window is visible, something is playing and the
  app is not suspended. Timers and animations stop on minimize.
- Views are separate entities, so the 4 Hz position tick re-renders only the dock / Now Playing; animated elements
  live in one "live layer" so the rest of the frame stays cached.
- Lists are paged from the local SQLite mirror and virtualized; covers are decoded at the display bucket size.

## Measuring

`scripts/perf.ps1` starts a release build with `SONIX_SCRIPT=1`. The app then drives its own timeline (idle ->
scroll the whole Tracks list -> play all -> Now Playing 40 s -> back) and logs `phase ...` / `frames ...` lines to
`SONIX_LOG`; the script samples the process tree around those phases, then minimizes and restores the window.
It needs a signed-in profile with a library, and an otherwise idle desktop (an occluded or background window skews
the minimize/restore and frame-pacing numbers).

```
cargo build --release
powershell -File scripts/perf.ps1 -Runs 3      # writes perf-out/run-N.json + summary.json (git-ignored)
```

Other diagnostics (all off unless the variable is set):

| Variable | Effect |
|---|---|
| `SONIX_LOG=<file>` | timestamped startup/phase/backend log lines |
| `SONIX_FPSLOG=1` | frame-interval statistics |
| `SONIX_RCOUNT=1` | per-view render counters (needs `SONIX_LOG`) |
| `SONIX_PAUSE_MS=<ms>` | toggle play/pause once after `<ms>` (CPU of a real pause without input) |
| `SONIX_NOSCROLL=1` | skip the scroll phase of the scripted run |
| `SONIX_NO_ACTIVATE=1` | show the window without activating it (test runs while the PC is in use) |

`scripts/mem/` holds lower-level tools: `snap.ps1` (virtual-memory region dumps at three points of the scripted run),
`vmm.ps1` and `threads.ps1` (per-process region / thread breakdowns), and the `memprof` cargo feature (counting
allocator with big-allocation backtraces; never in a release build).
