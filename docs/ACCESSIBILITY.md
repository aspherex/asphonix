# Accessibility and font scaling: assessment against WCAG 2.2 AA

Status: investigation (2026-11-22), code-level review of `crates/asphonix-ui` for v0.2.
Nothing here was tested with a screen reader or on real hardware yet; items marked *verify* need a manual check.

WCAG 2.2 is written for web content; for desktop software it is applied through W3C's
"WCAG2ICT" guidance and, in the EU, EN 301 549 chapter 11 (software), which adds "respect the platform's
user preferences" (font size, colors, contrast themes). This document uses both.

## Summary

| Area | Status | WCAG / EN 301 549 |
|---|---|---|
| Screen readers (names, roles, values) | **fails - blocker** | 1.1.1, 1.3.1, 4.1.2; EN 11.5 |
| Text resize to 200 % / app zoom | **fails** (no zoom; Windows "Text size" setting ignored) | 1.4.4; EN 11.7 |
| Platform preferences (text size, contrast themes, reduce animations) | **fails** | EN 11.7 |
| Text contrast | mostly passes; `clip` red fails on raised surfaces | 1.4.3 |
| Non-text contrast (control borders, state fills) | partly fails | 1.4.11 |
| Keyboard operation | largely present (tab stops, shortcuts) - *verify* full coverage | 2.1.1, 2.1.2 |
| Focus visible | present for buttons; weak for text-like controls | 2.4.7 (AA), 2.4.13 (AAA) |
| Target size (min 24x24) | mostly passes; some 12 px icon buttons fail | 2.5.8 |
| Motion | spectrum/meters animate while playing; no "reduce motion" | 2.2.2, 2.3.3 (AAA) |
| Use of color | *verify* (active route / current item may be color-only) | 1.4.1 |
| Display scaling (DPI) | passes (GPUI follows the monitor scale factor, per-monitor DPI) | - |

## Findings

### 1. Screen readers: no accessibility tree (blocker)

GPUI 0.2.2 from crates.io (the latest published version) has no AccessKit/UI Automation integration; screen
readers see one unlabeled window (already listed in `docs/GPUI-GAPS.md`).

**New:** upstream GPUI (zed-industries/zed, `crates/gpui/src/window/a11y.rs`) now has AccessKit support for
Windows, macOS and Linux: elements with an id and a role produce accessibility nodes, synthetic children for
lists, actions routed back to elements. It landed in mid-2026 (commits "a11y: Settings UI", "a11y: Landmarks and
menu improvements") but is **not published to crates.io**.

Options:
1. **Wait** for a crates.io GPUI release with a11y, then add roles/labels. Zero risk now, no a11y in 0.2.
2. **Git dependency** on a pinned upstream GPUI commit. Not a fork, but it breaks the project rule "GPUI 0.2.2
   from crates.io" and means an API migration from 0.2.2 to current upstream (unknown size; Zed's GPUI changes
   often). Must be measured against the perf budget.
3. Interim: nothing meaningful can be done in-app without GPUI support (no way to expose UIA nodes from outside
   GPUI's window procedure safely).

Recommendation: decide 1 vs 2 explicitly (owner decision). Either way, prepare now: give every interactive
element a stable `id` (already common in the code: `.id("pl-add")`) and a label string (which the i18n work
provides), so wiring roles later is mechanical.

### 2. Font scaling / zoom

- All sizes are fixed logical pixels: type scale in `tracking.rs` (smallest: `LABEL_CAPS` 10 px, `MONO_CODE`
  11 px, `BODY_SM`/`MONO_DATA` 12 px), layout in `metrics.rs` (~800 `px(..)` call sites).
- GPUI scales everything with the **display** scale factor (100/125/150 %...), so Windows display scaling works.
- Windows **Settings > Accessibility > Text size** (`UISettings.TextScaleFactor`, up to 225 %) is **ignored**:
  GPUI doesn't read it and the app doesn't either.
- No in-app zoom (no Ctrl+= / Ctrl+- / Ctrl+0).

WCAG 1.4.4 requires text resizable to 200 % without loss of content or function; EN 301 549 11.7 requires
honoring the platform text-size preference.

Proposed implementation:
- Whole-UI zoom (not text-only) avoids clipping in fixed-height rows (`ROW_H` 30/36 px).
- **Not** a crate-local scaled `px()`: tried on paper and rejected. asphonix-ui converts `Pixels` back to `f32` in
  ~105 places (`f32::from(..)`, `natural_width`, viewport math) and often feeds the result into `px()` again,
  which would apply the zoom twice - silently, with no compiler help.
- Instead GPUI's intended mechanism (what Zed uses for its UI font size): sizes in `rems`, zoom via
  `window.set_rem_size(16 px * zoom)`. Converting `metrics.rs` / `tracking.rs` values to rems turns every place
  that needs absolute pixels into a **compile error** to fix deliberately (convert with `window.rem_size()`),
  instead of a silent double scaling. Larger change; scheduled after 0.2, with a 200 % parity run behind login.
- Initial zoom = Windows `TextScaleFactor` (listen for `TextScaleFactorChanged`); user override in Settings
  (100-200 %, steps of 10 %) + Ctrl+= / Ctrl+- / Ctrl+0.
- On change: invalidate cached parts/views (same mechanism as the runtime language switch), re-measure layouts.
- Keep device-pixel-exact code (art buckets via `scale_factor`, borders) on `gpui::px` explicitly.
- Tests: zoom 200 % parity run (boxes.json / text-rects.json) to find clipping; unit test for the scaled `px`.
- Perf: re-run `scripts/perf.ps1` (text atlas grows with larger glyphs; expected small).

### 3. Contrast (computed from `theme.rs`, WCAG relative luminance)

Text (needs 4.5:1, large text 3:1):

| Token on surface | Dark | Light |
|---|---|---|
| `text` on base..highest | 19.8 - 14.3 | 17.7 - 13.7 |
| `muted` on base..highest | 6.4 - 4.6 | 5.9 - 4.6 |
| `accent_fg` on base..highest | 17.1 - 12.3 | 5.9 - 4.6 |
| `accent_text` on `accent` | 17.1 | 16.6 |
| **`clip` (red) on base / low / mid / high / highest** | 5.6 / 5.2 / 4.8 / **4.2 / 4.0** | **4.3 / 4.7 / 3.95 / 3.6 / 3.3** |

- `muted` and `accent_fg` pass on every surface, but only just on `highest` (4.55-4.61): don't add darker/lighter
  surfaces without re-checking.
- **Fix:** `clip` fails as small text on raised surfaces. Use a darker red in light (e.g. `#b0103a`: 5.0-7.0:1 on
  every surface) and a lighter red in dark (e.g. `#ff6b61`: 5.1-7.1:1) - or only use `clip` as text on `base`/`low`.

Non-text (needs 3:1 for UI component boundaries and state indicators):
- `border` vs base: dark **1.38**, light **1.19**. Fine for decorative separators, **fails** where the border
  is the only visible boundary of a control (text inputs, secondary buttons, checkboxes) - *verify* per control.
  Light `frame` (15.9) passes; dark has no separate strong frame color.
- Light theme `accent` `#d4ff00` as a fill on light surfaces: **1.07**. Any state shown only by an accent fill
  (selected chip, slider/progress fill, toggle on) is invisible to many users in light mode - *verify* where
  `th.accent` is used as a fill in light (meters already use `#0e0e10` in light, which passes).
- Focus ring (`focus`): dark 17.1, light 17.7 - passes.

### 4. Keyboard and focus

- Buttons/links use `tab_index(0)` with a visible focus style when keyboard navigation is active
  (`widgets::key_nav()`), Tab cycles in paint order, plenty of shortcuts (J/K scrolling, player keys).
- Text-like controls show focus only as a background change to `th.high` (dark: ~1.3:1 vs base): too weak for
  2.4.7 in practice. Use the `focus` border/outline for all focusable elements.
- *Verify:* every action reachable without a mouse (context menus via keyboard/Shift+F10, drag-and-drop
  reordering has a keyboard alternative such as "Move up/down" menu items, sliders adjustable with arrows,
  dialogs trap and restore focus - the context menu already restores focus).

### 5. Target size (2.5.8: 24x24 CSS px minimum, or enough spacing)

- `ICON_BTN` 24, button heights 40, rows 30-36: pass.
- **Fail:** sidebar "New playlist" / "New smart playlist" (`views/chrome.rs`, `pl-add`, `pl-add-smart`): the hit
  area is the 12 px icon itself, two of them side by side. Give the wrapper a 24x24 box (icon centered).
- *Verify:* card overlay buttons (`CARD_ICON_BTN` 26 passes; star icon 11 px - check its hit box), queue rail
  drag handles, scrollbar thumbs (WCAG exempts targets whose function is available elsewhere).

### 6. Motion and platform preferences

- Spectrum, peak meters and waveform animate continuously while playing (they already stop when not visible).
  Add a "Reduce motion" setting defaulting to Windows *Animation effects* (`SPI_GETCLIENTAREAANIMATION`): static
  meters, no transitions.
- Windows contrast themes (High Contrast): ignored; at minimum detect it (`SystemParametersInfo(SPI_GETHIGHCONTRAST)`)
  and switch to a high-contrast palette derived from system colors.

## Proposed work (as plan phase 7)

| # | Item | Size | Dependency |
|---|---|---|---|
| 1 | Contrast fixes: `clip` colors (**done**, test-guarded), control borders >= 3:1 where they are the boundary, light accent fills (open, need a visual check per control) | S | - |
| 2 | 24x24 hit boxes for small icon buttons; focus indicator for all focusables (**done**: sidebar buttons, `widgets::focus_ring` on every `tab_index` element) | S | - |
| 3 | UI zoom via rems + `set_rem_size`, Windows TextScaleFactor, Ctrl+=/-/0, Settings | M-L | after 0.2; needs a logged-in visual check |
| 4 | Reduce motion (Windows setting + toggle) | S | - |
| 5 | High-contrast detection + palette | M | 1 |
| 6 | Screen reader support | L | owner decision: wait for crates.io GPUI vs git dependency |
| 7 | Tests: contrast unit test over all theme token pairs actually used (fails below thresholds); zoom-200 % and pseudo-locale parity runs; keyboard-only walkthrough checklist | S-M | 1, 3 |
