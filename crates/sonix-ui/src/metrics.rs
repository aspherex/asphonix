//! Every margin, padding, gap, border, row height, column width, component size and breakpoint
//! used by the ported screens, defined once, each with the SONIX CSS that it comes from
//! (`file:selector`, files relative to the SONIX repo's `src/`). Views must use these constants
//! instead of literals; the parity runner (docs/PARITY.md) measures the result against the web app.
//! All values are CSS px (logical px in GPUI).
// This is a reference table first: it also lists values of components whose native views are only partly
// ported (spectrum/meter constants for the Now Playing panel, the 24 px spacing step), so some entries are
// intentionally unreferenced until those views land.
#![allow(dead_code)]

/// `styles/tokens.css :root` spacing scale (`--space-xs/sm/md/lg/xl`).
pub mod space {
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 24.0;
}

/// Breakpoints (CSS px of viewport width).
pub mod bp {
    /// `styles/tokens.css @media (max-width: 1279px)`: narrower sidebar, ALB/ART counts hidden, dock transport basis 420.
    pub const COMPACT_BELOW: f32 = 1280.0;
    /// `components/QueueRail.svelte @media (min-width: 1440px)`: rail docked; below it is an overlay drawer.
    pub const RAIL_DOCKED_FROM: f32 = 1440.0;
    /// `components/PlayerDock.svelte @media (max-width: 1679px)`: short output readout below this width.
    pub const FULL_READOUT_FROM: f32 = 1680.0;
}

/// `styles/tokens.css` layout constants.
pub mod layout {
    /// `--titlebar-height` (`TitleBar.svelte:.titlebar`).
    pub const TITLEBAR_H: f32 = 44.0;
    /// `--sidebar-width`.
    pub const SIDEBAR_W: f32 = 260.0;
    /// `--sidebar-width` inside `@media (max-width: 1279px)`.
    pub const SIDEBAR_W_COMPACT: f32 = 216.0;
    /// `--dock-height`.
    pub const DOCK_H: f32 = 72.0;
    /// `--inspector-width` (`QueueRail.svelte:.rail`).
    pub const RAIL_W: f32 = 340.0;
    /// Frame / divider rules are 1 CSS px everywhere (`border-*: 1px solid`).
    pub const RULE: f32 = 1.0;
}

/// `components/TitleBar.svelte`
pub mod titlebar {
    /// `.titlebar { padding: 0 var(--space-sm) 0 var(--space-md) }`
    pub const PAD_LEFT: f32 = super::space::MD;
    pub const PAD_RIGHT: f32 = super::space::SM;
    /// `.brand { gap: var(--space-sm) }`
    pub const BRAND_GAP: f32 = super::space::SM;
    /// Light theme `.slash { margin-left: 2px; padding: 0 2px }`
    pub const SLASH_MARGIN_LIGHT: f32 = 2.0;
    pub const SLASH_PAD_LIGHT: f32 = 2.0;
    /// `.version { padding: 0 var(--space-xs) }`
    pub const VERSION_PAD_X: f32 = super::space::XS;
    /// `.controls { gap: 2px }`, `.ctrl { width: 32px; height: 28px }`
    pub const CTRL_GAP: f32 = 2.0;
    pub const CTRL_W: f32 = 32.0;
    pub const CTRL_H: f32 = 28.0;
    /// Icon sizes: `<Icon name="minimize" size={12}/> maximize 11, close 12`.
    pub const ICON_MIN: f32 = 12.0;
    pub const ICON_MAX: f32 = 11.0;
    pub const ICON_CLOSE: f32 = 12.0;
}

/// `components/TopBar.svelte`
pub mod topbar {
    /// `.topbar { height: 44px; gap: var(--space-md); padding: 0 var(--space-md) }`
    pub const H: f32 = 44.0;
    pub const GAP: f32 = super::space::MD;
    pub const PAD_X: f32 = super::space::MD;
    /// `.pill { height: 26px; padding: 0 var(--space-sm); gap: var(--space-xs); border: 1px }`
    pub const PILL_H: f32 = 26.0;
    pub const PILL_PAD_X: f32 = super::space::SM;
    pub const PILL_GAP: f32 = super::space::XS;
    /// `.dot { width: 6px; height: 6px }`
    pub const DOT: f32 = 6.0;
    /// `.search { max-width: 420px; height: 28px; gap: var(--space-sm); padding: 0 var(--space-sm) }`
    pub const SEARCH_MAX_W: f32 = 420.0;
    pub const SEARCH_H: f32 = 28.0;
    pub const SEARCH_GAP: f32 = super::space::SM;
    pub const SEARCH_PAD_X: f32 = super::space::SM;
    /// `.kbd { padding: 1px var(--space-xs) }`
    pub const KBD_PAD_Y: f32 = 1.0;
    pub const KBD_PAD_X: f32 = super::space::XS;
    /// `.icon-link { width: 28px; height: 28px }`
    pub const ICON_LINK: f32 = 28.0;
    /// `.collection { gap: var(--space-md) }`
    pub const COLLECTION_GAP: f32 = super::space::MD;
    /// `<Icon size={16}>` nav/theme, `size={14}` search glyph.
    pub const ICON_NAV: f32 = 16.0;
    pub const ICON_SEARCH: f32 = 14.0;
}

/// `components/Sidebar.svelte`, `components/ProfileSwitcher.svelte`
pub mod sidebar {
    /// `.row { height: 30px; gap: var(--space-sm); padding: 0 var(--space-md) }`
    pub const ROW_H: f32 = 30.0;
    pub const ROW_GAP: f32 = super::space::SM;
    pub const ROW_PAD_X: f32 = super::space::MD;
    /// `<Icon size={14}>` on every row.
    pub const ICON: f32 = 14.0;
    /// `.section { padding: var(--space-xs) 0 var(--space-sm) }`
    pub const SECTION_PAD_TOP: f32 = super::space::XS;
    pub const SECTION_PAD_BOTTOM: f32 = super::space::SM;
    /// `.section-title { padding: var(--space-md) var(--space-md) var(--space-xs) }`
    pub const TITLE_PAD_TOP: f32 = super::space::MD;
    pub const TITLE_PAD_X: f32 = super::space::MD;
    pub const TITLE_PAD_BOTTOM: f32 = super::space::XS;
    /// `.add-group { gap: var(--space-sm) }`, `<Icon size={12}>`
    pub const ADD_GAP: f32 = super::space::SM;
    pub const ADD_ICON: f32 = 12.0;
    /// `.pl-sep { height: 1px; margin: var(--space-xs) var(--space-md) }`
    pub const SEP_MARGIN_Y: f32 = super::space::XS;
    pub const SEP_MARGIN_X: f32 = super::space::MD;
    /// `.footer { padding: var(--space-xs) 0 }`
    pub const FOOTER_PAD_Y: f32 = super::space::XS;
    /// `.profile { padding: var(--space-sm) var(--space-md); gap: var(--space-sm) }`
    pub const PROFILE_PAD_Y: f32 = super::space::SM;
    pub const PROFILE_PAD_X: f32 = super::space::MD;
    pub const PROFILE_GAP: f32 = super::space::SM;
    /// `.avatar { width: 28px; height: 28px }`, `.avatar.online::after { 7px, right/bottom -3px }`
    pub const AVATAR: f32 = 28.0;
    pub const AVATAR_PIP: f32 = 7.0;
    pub const AVATAR_PIP_OFFSET: f32 = -3.0;
}

/// `components/PlayerDock.svelte`, `components/Fader.svelte`
pub mod dock {
    /// `.dock { height: var(--dock-height); gap: var(--space-lg); padding: 0 var(--space-lg) }`
    pub const GAP: f32 = super::space::LG;
    pub const PAD_X: f32 = super::space::LG;
    /// `.now-playing { gap: var(--space-md); min-width: 180px }`, `.np-link { gap: var(--space-md) }`
    pub const NP_GAP: f32 = super::space::MD;
    pub const NP_MIN_W: f32 = 180.0;
    /// `.art-slot { 44x44; border: 1px }`
    pub const ART: f32 = 44.0;
    /// `.transport { flex: 0 1 520px; min-width: 300px; max-width: 620px; gap: 6px }`
    pub const TRANSPORT_BASIS: f32 = 520.0;
    /// `@media (max-width: 1279px) .transport { flex-basis: 420px }`
    pub const TRANSPORT_BASIS_COMPACT: f32 = 420.0;
    pub const TRANSPORT_MIN_W: f32 = 300.0;
    pub const TRANSPORT_MAX_W: f32 = 620.0;
    pub const TRANSPORT_GAP: f32 = 6.0;
    /// `.controls { gap: var(--space-md) }`
    pub const CONTROLS_GAP: f32 = super::space::MD;
    /// `.t-btn { 28x28 }`, `.t-btn-play { 36x36; box-shadow: var(--shadow-panel-sm) (2px) }`
    pub const T_BTN: f32 = 28.0;
    pub const PLAY_BTN: f32 = 36.0;
    pub const PLAY_SHADOW: f32 = 2.0;
    /// `<Icon size>`: shuffle/repeat 15, prev/next 16, play/pause 18, like 18, volume 15, queue 16.
    pub const ICON_SMALL: f32 = 15.0;
    pub const ICON_SKIP: f32 = 16.0;
    pub const ICON_PLAY: f32 = 18.0;
    pub const ICON_LIKE: f32 = 18.0;
    pub const ICON_QUEUE: f32 = 16.0;
    /// `.scrub { gap: var(--space-md) }`, `.time { width: 40px; text-align: center }`
    pub const SCRUB_GAP: f32 = super::space::MD;
    pub const TIME_W: f32 = 40.0;
    /// `.aux { min-width: 260px; gap: var(--space-md) }`
    pub const AUX_MIN_W: f32 = 260.0;
    pub const AUX_GAP: f32 = super::space::MD;
    /// `.volume { gap: var(--space-sm) }`, `.volume .fader { width: 112px }`, `.db { min-width: 52px }`
    pub const VOLUME_GAP: f32 = super::space::SM;
    pub const VOLUME_FADER_W: f32 = 112.0;
    pub const DB_MIN_W: f32 = 52.0;
    /// `.output.warn { max-width: 220px }`
    pub const WARN_MAX_W: f32 = 220.0;
    /// `.qcount { top: -2px; right: -6px; padding: 0 2px }`
    pub const QCOUNT_TOP: f32 = -2.0;
    pub const QCOUNT_RIGHT: f32 = -6.0;
    pub const QCOUNT_PAD_X: f32 = 2.0;
}

/// `components/Fader.svelte`
pub mod fader {
    /// `.fader { padding: 10px 0 }` around a 4px `.rail` = 24px hit area.
    pub const HIT_H: f32 = 24.0;
    pub const RAIL_H: f32 = 4.0;
    pub const RAIL_TOP: f32 = 10.0;
    /// `.cap { width: 10px; height: 18px; border: 1px solid #000 }`
    pub const CAP_W: f32 = 10.0;
    pub const CAP_H: f32 = 18.0;
}

/// `components/Button.svelte`, `components/Input.svelte`
pub mod controls {
    /// `.btn { gap: var(--space-sm); border: 1px }`, `.btn-primary/-secondary { padding: var(--space-sm) var(--space-lg) }`
    pub const BTN_GAP: f32 = super::space::SM;
    pub const BTN_PAD_Y: f32 = super::space::SM;
    pub const BTN_PAD_X: f32 = super::space::LG;
    /// `.btn-primary { box-shadow: var(--shadow-panel-sm) }`
    pub const BTN_SHADOW: f32 = 2.0;
    /// Icon inside page buttons: `<Icon size={14}/>`
    pub const BTN_ICON: f32 = 14.0;
    /// `.field { height: 36px; padding: 0 var(--space-md); border: 1px }`
    pub const FIELD_H: f32 = 36.0;
    pub const FIELD_PAD_X: f32 = super::space::MD;
    /// Intrinsic width of Chromium's default `<input>` (size=20) in the body font at 13px:
    /// measured on the web app (field border box 191 px with Inter, see parity boxes `page.filter`).
    pub const FIELD_W_DARK: f32 = 191.0;
    pub const FIELD_W_LIGHT: f32 = 191.0;
}

/// `routes/Tracks.svelte`, `components/TrackRow.svelte`, `components/VirtualList.svelte`, `styles/base.css`
pub mod tracks {
    /// `.page { padding: var(--space-lg); gap: var(--space-md) }`
    pub const PAGE_PAD: f32 = super::space::LG;
    pub const PAGE_GAP: f32 = super::space::MD;
    /// `.header { gap: var(--space-md) }`, `.header-right { gap: var(--space-sm) }`
    pub const HEADER_GAP: f32 = super::space::MD;
    pub const HEADER_RIGHT_GAP: f32 = super::space::SM;
    /// `ROW_HEIGHT = 36`, `PAGE_SIZE = 100`
    pub const ROW_H: f32 = 36.0;
    /// `.row { gap: var(--space-sm); padding: 0 var(--space-sm) 0 var(--space-xs); border-bottom: 1px }`
    pub const ROW_GAP: f32 = super::space::SM;
    pub const ROW_PAD_LEFT: f32 = super::space::XS;
    pub const ROW_PAD_RIGHT: f32 = super::space::SM;
    /// `.row.head { padding-top/bottom: var(--space-xs) }` (height auto: the 24px `.icon-btn` spacers decide)
    pub const HEAD_PAD_Y: f32 = super::space::XS;
    /// `.idx-play { width: 36px }`, `.year { width: 56px }`, `.added { width: 88px }`,
    /// `.format { width: 96px }`, `.time { width: 48px }`, `.icon-btn { 24x24 }`
    pub const IDX_W: f32 = 36.0;
    pub const YEAR_W: f32 = 56.0;
    pub const ADDED_W: f32 = 88.0;
    pub const FORMAT_W: f32 = 96.0;
    pub const TIME_W: f32 = 48.0;
    pub const ICON_BTN: f32 = 24.0;
    /// `.title-wrap { flex: 3 1 0; gap: var(--space-sm) }`, `.artist/.album-col { flex: 2 1 0 }`
    pub const TITLE_GROW: f32 = 3.0;
    pub const ARTIST_GROW: f32 = 2.0;
    pub const ALBUM_GROW: f32 = 2.0;
    /// `.row.active { box-shadow: inset 2px 0 0 }`
    pub const ACTIVE_BAR_W: f32 = 2.0;
    /// `.now-playing { gap: 2px; height: 12px }`, `.now-playing i { width: 2px; height: 12px }`
    pub const BARS_GAP: f32 = 2.0;
    pub const BARS_H: f32 = 12.0;
    pub const BAR_W: f32 = 2.0;
    /// `@container track-table (max-width: 760px) .added {display:none}` / `(max-width: 600px) .year`
    pub const HIDE_ADDED_BELOW: f32 = 760.0;
    pub const HIDE_YEAR_BELOW: f32 = 600.0;
    /// `styles/base.css ::-webkit-scrollbar { width: 10px }`; thumb border 2px
    pub const SCROLLBAR_W: f32 = 10.0;
    pub const SCROLLBAR_THUMB_BORDER: f32 = 2.0;
    /// `<Icon size={14}>` star / more / row play glyph is 12.
    pub const ROW_ICON: f32 = 14.0;
    pub const ROW_PLAY_ICON: f32 = 12.0;
}

/// Animation timings (`src` file:selector in the comment).
pub mod anim {
    /// `TrackRow.svelte:.now-playing i { animation: bar 0.9s ease-in-out infinite }`
    pub const BARS_PERIOD: f32 = 0.9;
    /// `.now-playing i:nth-child(1/2) { animation-delay: -0.6s / -0.3s }` (third: none)
    pub const BARS_DELAY: [f32; 3] = [-0.6, -0.3, 0.0];
    /// `@keyframes bar { 0%,100% scaleY(.25); 50% scaleY(1) }`; paused: `scaleY(.33)`
    pub const BARS_MIN: f32 = 0.25;
    pub const BARS_MAX: f32 = 1.0;
    pub const BARS_PAUSED: f32 = 0.33;
    /// `Button.svelte:.btn { transition: border-color 0.1s, background-color 0.1s }` (CSS default easing `ease`)
    pub const BUTTON_TRANSITION: f32 = 0.1;
    /// `TrackRow.svelte:.idx { transition: opacity 0.1s }`
    pub const IDX_FADE: f32 = 0.1;
}

/// Spectrum / meter constants copied from `lib/spectrum.ts` and `lib/np/FftPanel.svelte`.
pub mod spectrum {
    /// `spectrum.ts: SPECTRUM_BANDS`, `FFT_SIZE`
    pub const BANDS: usize = 28;
    pub const FFT_SIZE: usize = 2048;
    /// `FftPanel.svelte` consts
    pub const FLOOR_DB: f32 = -60.0;
    pub const SEGMENTS: usize = 20;
    pub const CLIP_FROM_DB: f32 = -3.0;
    pub const PEAK_HOLD_MS: f32 = 1200.0;
    pub const METER_FALL_DB_PER_S: f32 = 30.0;
    pub const CAP_FALL_PER_S: f32 = 0.45;
    pub const MIN_DRAW_MS: f32 = 20.0;
    /// `frame()`: `dt = min(0.1, ...)`; no new frame for > 120 ms: bands fall `255 * 2.2 * dt`.
    pub const MAX_DT: f32 = 0.1;
    pub const STALE_AFTER_MS: f32 = 120.0;
    pub const STALE_FALL_PER_S: f32 = 2.2;
}

/// `components/QueueRail.svelte`
pub mod rail {
    /// `.deck-header { padding: var(--space-sm) var(--space-md) }`
    pub const HEADER_PAD_Y: f32 = super::space::SM;
    pub const HEADER_PAD_X: f32 = super::space::MD;
}

/// List routes (`routes/PlaylistDetail.svelte`, `Liked.svelte`, `SmartPlaylistDetail.svelte`, `History.svelte`,
/// `Playlists.svelte`), `components/Badge.svelte`, `SegmentedToggle.svelte`.
pub mod lists {
    /// `.page / .playlist-page { padding: var(--space-lg); gap: var(--space-lg) }`
    pub const PAGE_PAD: f32 = super::space::LG;
    pub const PAGE_GAP: f32 = super::space::LG;
    /// History `.page { gap: var(--space-md) }`, `.header-right { gap: var(--space-md) }`
    pub const HISTORY_GAP: f32 = super::space::MD;
    /// `.header { gap: var(--space-lg) }`, `.art { 120x120 }`
    pub const HEADER_GAP: f32 = super::space::LG;
    pub const ART: f32 = 120.0;
    /// `.info { gap: var(--space-xs) }`
    pub const INFO_GAP: f32 = super::space::XS;
    /// `.actions { gap: var(--space-sm); margin-top: var(--space-sm) }`
    pub const ACTIONS_GAP: f32 = super::space::SM;
    pub const ACTIONS_MT: f32 = super::space::SM;
    /// `.play-btn / .btn-secondary / .btn-icon { height / width: 40px }`
    pub const BTN_H: f32 = 40.0;
    /// `.play-btn { padding: 0 var(--space-lg) }`, `.btn-secondary { padding: 0 var(--space-md) }`
    pub const PLAY_PAD_X: f32 = super::space::LG;
    pub const SEC_PAD_X: f32 = super::space::MD;
    /// `<Icon size={16}>` in PLAY, `size={15}` in the others; `Icon size={56}` Liked art, `52` smart art, `32` empty playlist art
    pub const ICON_PLAY: f32 = 16.0;
    pub const ICON_SEC: f32 = 15.0;
    pub const ICON_ART_LIKED: f32 = 56.0;
    pub const ICON_ART_SMART: f32 = 52.0;
    pub const ICON_ART_EMPTY: f32 = 32.0;
    /// `.status { padding: var(--space-xl) }`
    pub const STATUS_PAD: f32 = super::space::XL;
    /// `.day { padding: 0 var(--space-sm) var(--space-xs) var(--space-sm) }`
    pub const DAY_PAD_X: f32 = super::space::SM;
    pub const DAY_PAD_BOTTOM: f32 = super::space::XS;
    /// `.empty { padding: var(--space-md) }`
    pub const EMPTY_PAD: f32 = super::space::MD;
    /// Playlists grid: `repeat(auto-fill, minmax(240px, 1fr)); gap: var(--space-md)`
    pub const GRID_MIN: f32 = 240.0;
    pub const GRID_GAP: f32 = super::space::MD;
    /// `.card { gap: var(--space-sm); padding: var(--space-sm) }`, `.card-art { 44x44 }`
    pub const CARD_GAP: f32 = super::space::SM;
    pub const CARD_PAD: f32 = super::space::SM;
    pub const CARD_ART: f32 = 44.0;
    /// `.play-overlay { 20x20; inset: auto 2px 2px auto }`, `<Icon size={10}>`
    pub const OVERLAY: f32 = 20.0;
    pub const OVERLAY_INSET: f32 = 2.0;
    pub const OVERLAY_ICON: f32 = 10.0;
    /// `.card-actions { gap: 2px }`, `.icon-btn { 26x26 }`, `<Icon size={14}>`
    pub const CARD_ACTIONS_GAP: f32 = 2.0;
    pub const CARD_ICON_BTN: f32 = 26.0;
    pub const CARD_ICON: f32 = 14.0;
    /// `.new-btn { gap: var(--space-xs); padding: var(--space-sm) var(--space-md) }`, `<Icon size={13}>`
    pub const NEW_GAP: f32 = super::space::XS;
    pub const NEW_PAD_Y: f32 = super::space::SM;
    pub const NEW_PAD_X: f32 = super::space::MD;
    pub const NEW_ICON: f32 = 13.0;
    /// `Badge.svelte:.badge { padding: 0 var(--space-xs); line-height: 16px }`
    pub const BADGE_PAD_X: f32 = super::space::XS;
    pub const BADGE_LINE: f32 = 16.0;
    /// `SegmentedToggle.svelte:.segment { padding: var(--space-xs) var(--space-md) }`
    pub const SEG_PAD_Y: f32 = super::space::XS;
    pub const SEG_PAD_X: f32 = super::space::MD;
}
