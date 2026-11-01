# GPUI 0.2.2 gaps and how this repo works around them (no fork)

| Gap | Workaround here | Residual |
|---|---|---|
| No letter-spacing in `TextStyle` | `tracking.rs`: lay out the line once with the text system, paint each glyph with the public `Window::paint_glyph` at `x + i*tracking` (CSS semantics: spacing after every glyph); width = shaped + n*tracking (unit-tested) | Used for caps labels, headlines, title block. Body/mono-data rows use native text (0.01-0.02em is < 1 px) so ellipsis keeps working; tracked labels cannot truncate; per-glyph painting skips emoji |
| No text-transform | caps applied in the same helper (`spec.caps`) and in buttons | none |
| No radial gradients (`--bg-canvas`) | `examples/gen_backdrop.rs` renders dark "oil slick" and light "pearl" to 480x270 dithered PNGs, drawn once behind the body/dock, dropped on minimize | stretched 4x (soft washes, no visible loss expected; not pixel-diffed) |
| No minimize callback; swallowed `WM_SIZE` | `winffi::size_hook`: `SetWindowSubclass` on the HWND obtained via `HasWindowHandle for Window` | Windows only; macOS hook TBD |
| Permanent vsync thread (`DwmFlush` loop) | none possible without a patch | ~0-3 % of one core idle; ~0 when minimized in 2 of 3 runs, not guaranteed. A `set_vsync_suspended` hook would need upstream |
| Quits when the last window closes; cannot release D3D device | n/a | ~22 MB WS / ~150 MB commit / ~54 MB VRAM floor per process |
| No scrollbar for `uniform_list` | `widgets/scrollbar.rs` reading `UniformListScrollHandle` (public) and setting `base_handle.set_offset` while dragging | no track click paging |
| Svg ignores the parent's `text_color` | explicit colour per icon; hover tints through `group_hover("hv")` (`icon_g`) | each icon call carries its colours |
| `text_ellipsis` in a column flex container collapsed to "…" | `typo::ellipsis_line` wraps the text node in a one-item row so width comes from flex growth | **Tracks artist/album cells still cut hard without "…"** (unresolved, root cause unknown: maybe the "…" glyph is missing from the bundled JetBrains Mono subset) |
| Text input | own widget (`text_input.rs`, derived from GPUI's example): caret, selection, word navigation, double/triple click, clipboard | no undo/redo, no context menu; IME composition wired (`EntityInputHandler`) but **not exercised with a real IME**; no bidi |
| Accessibility | none | GPUI 0.2.2 has no accesskit/UIA: screen readers see an unlabeled window. Blocker for a real release |
| Text weight/softness vs Chromium | **not yet measured**. Fonts are identical TTFs (converted from the WOFF2), so differences should come from rasterisation (DirectWrite grayscale, GPUI shader blend vs Chromium gamma/contrast). The spike observed "slightly lighter/softer horizontally". Public API knobs found so far: none (no gamma/contrast setting in `Window`/`TextSystem` 0.2.2). The text-fidelity harness requested by the owner is not built, see PARITY.md | open |
| Rust edition 2024, `impl Trait` capture rules (`+ use<>`) | followed | friction only |
