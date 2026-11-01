# Performance (2026-11-01)

Machine: Windows 11, RTX 4090 (+iGPU), one 7680x2160 display at 150 % and ~119 Hz, 16 cores.
Release build (`lto`, `cgu 1`, `opt 3`, `panic=abort`, `strip`), window 1440x900 logical, mock
backend, `scripts/perf.ps1 -Runs 3` (private working set summed over the process tree = the one
sonix-native.exe; CPU as % of one core). Raw per-run JSON is produced in `perf-out/` (git-ignored).

| Metric | native (3 runs, median / range) | spike | WebView app (CLAUDE.md) | target |
|---|---|---|---|---|
| process start -> window visible | **326 ms** [269-436] | 299 ms | ~380 ms | n/a |
| private WS idle, Tracks | **32.5 MB** [30.9-32.6] | 30.0 | ~120 (Home) | <= 40 |
| after full 54k scroll | **38.7 MB** [37.7-40.7] | 36.7 | up to 190 transient | <= 40 |
| Now Playing animating 60 s | **40.2 MB** [40.2-40.6] | 36.4 | 170-185 | <= 40 (borderline, +3.8 vs spike) |
| minimized 5 s | **0.6-0.7 MB** (after `EmptyWorkingSet`) | 0.4 | 8.4 (bg mode) | below 8.4 |
| minimized 35 s | **1.9 MB** (2 of 3 runs, see caveat) | 1.3 | 8.4 | below 8.4 |
| CPU minimized | 0 % (2 of 3 runs) | 0-2.7 (stock gpui) | ~0.3 | ~0 |
| restore -> first rendered frame | **10 and 17 ms** (2 valid runs) | 15 ms | ~1 s | ~15 ms |
| private WS after restore | 21-22 MB | 24 | ~115 | |
| CPU idle, window visible | 0 / 2.2 / 3.4 % (noisy) | 0.3-1.5 | n/a | vsync thread only |
| CPU scripted 54k scroll | 30.8 % (1 run logged) | 37.5 | n/a | |
| CPU Now Playing at vsync | 14.6-16.4 % | 13.0 | n/a | |
| exe size | 7.16 MB | 8.17 | 12 MB + WebView2 | |

Frame pacing (interval between consecutive root renders, display ~119 Hz = 8.4 ms):

| Scenario | frames | p50 | p95 | p99 | max | > 16.7 ms |
|---|---|---|---|---|---|---|
| scroll 54k rows end to end (100 rows / 16 ms), 3 runs | ~1,980 | 8.3 | 8.7-8.9 | 9.0-10.5 | 24.7-25.4 | 10-11 |
| Now Playing, spectrum + peak meters + waveform every vsync (2 valid runs) | 5,998-6,000 | 8.3 | 8.7-8.8 | 8.9-9.4 | 14.2-21.5 | 0-1 |

Caveats, stated plainly:
* **Run 2 is an outlier** and is excluded from the minimize/restore/Now-Playing rows: the Now
  Playing animation did not run (167 frames at ~308 ms, i.e. only the 4 Hz state tick) and the
  window never reported a minimize (22 MB at 35 s, 2.3 % CPU, negative restore latency). Most likely
  the window was occluded/not the foreground window while I was using the machine; not
  reproduced, not understood. The harness should be re-run on an otherwise idle desktop.
* Idle CPU is dominated by GPUI's permanent DwmFlush/vsync thread (cannot be stopped without a
  patch, see GPUI-GAPS); the 0-3.4 % spread is measurement noise on a 4 s window.
* `EmptyWorkingSet` lowers the Task-Manager metric only: commit (~150 MB) and VRAM (~54 MB) are
  unchanged (spike finding, not re-measured here).
* Now Playing is 0.2 MB over the 40 MB target. Candidates (not tried): smaller cover bucket
  (640 px bucket for a ~600 px cover = 1.6 MB BGRA), no `RenderImage` for the backdrop.
* Backdrop on/off private-WS delta was **not measured**; the two PNGs are 480x270 (0.5 MB each decoded)
  and are dropped on minimize.
* Not measured: first launch on a cold disk/shader cache, an iGPU-only machine, text-fidelity numbers.

## Re-run after the parity work (2026-11-01, release build WITHOUT the `parity` feature)

`scripts/perf.ps1 -Runs 3` after the metrics table, `Txt` text primitive, animated bars/transitions, queue rail and menu
rework (exe 7.19 MB, was 7.16; the release binary contains none of the hook's strings, checked with `grep -a`).
**Not a pristine idle desktop**: the machine was used for polling/builds-adjacent commands during the run, which is visible
in run 2's Now Playing frame pacing (234 frames over 16.7 ms), so treat that row as noisy.

| Metric | run 1 | run 2 | run 3 | target |
|---|---|---|---|---|
| time to visible (ms) | 436 | 273 | 296 | n/a |
| private WS idle, Tracks (MB) | 32.5 | 32.1 | 31.1 | <= 40 |
| Now Playing animating 60 s (MB) | 39.4 | 43.0 | 43.8 | <= 40 (**2 of 3 runs over**; was 40.2-40.6 before, partly noise: run 1 is under) |
| minimized 5 s / 35 s (MB) | 0.6 / 1.9 | 0.6 / 1.6 | 0.6 / 1.9 | < 8.4 |
| CPU minimized | 1.17 % | 0 % | 2.34 % | ~0 (vsync thread, see GPUI-GAPS) |
| restore -> first rendered frame (ms) | 15 | 11 | 14 | ~15 |
| frame pacing scroll p50 / p95 | 8.3 / 8.8 | 8.3 / 8.7 | 8.3 / 8.8 | 120 Hz = 8.3 |
| frame pacing Now Playing p50 / p95 | 8.3 / 8.8 | 8.3 / 16.7 (noisy run) | 8.3 / 8.8 | 120 Hz |

Median: idle 32.1 MB, minimized 1.9 MB, restore 14 ms, Now Playing 43.0 MB. New cost to watch: the now-playing bars and hover
transitions request frames while active (one repaint per vsync only while an active row is visible and playing), and the
Now Playing view clones the 28 band values per frame; neither shows up in idle/minimized, Now Playing needs an idle-desktop
re-measure before it is judged against the 40 MB target.
