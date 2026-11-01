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

/// Library routes (`routes/Home|Albums|Artists|AlbumDetail|ArtistDetail|Genres|GenreDetail.svelte`,
/// `components/AlbumCard|ArtistCard|VirtualGrid|SegmentedToggle|Checkbox|EntityLink.svelte`).
pub mod library {
    use super::space::*;
    /// `.page { padding: var(--space-lg); gap: var(--space-md) }` (Albums, Artists, Genres, GenreDetail)
    pub const PAGE_PAD: f32 = LG;
    pub const PAGE_GAP: f32 = MD;
    /// `<VirtualGrid minCellWidth={140} textHeight={40} gap={16}>`
    pub const GRID_MIN_CELL: f32 = 140.0;
    pub const GRID_TEXT_H: f32 = 40.0;
    pub const GRID_GAP: f32 = 16.0;
    /// `.controls { gap: var(--space-lg) }`
    pub const CONTROLS_GAP: f32 = LG;
    /// `AlbumCard.svelte:.card { gap: var(--space-xs) }`, `.art { border: 1px }`
    pub const CARD_GAP: f32 = XS;
    /// `.star { top: 4px; right: 4px }`, `<Icon name="star-filled" size={11}>`
    pub const CARD_STAR_INSET: f32 = 4.0;
    pub const CARD_STAR_ICON: f32 = 11.0;
    /// `.play-overlay { inset: auto 4px 4px auto; 30x30; translateY(4px) }`, `<Icon play size={18}>`
    pub const CARD_PLAY: f32 = 30.0;
    pub const CARD_PLAY_ICON: f32 = 18.0;
    /// `.menu-overlay { inset: auto auto 4px 4px; 26x26 }`, `<Icon more size={14}>`
    pub const CARD_MENU: f32 = 26.0;
    pub const CARD_MENU_ICON: f32 = 14.0;
    pub const CARD_INSET: f32 = 4.0;
    pub const CARD_OVERLAY_SHIFT: f32 = 4.0;
    /// `transition: opacity 0.1s, transform 0.1s`
    pub const CARD_TRANSITION: f32 = 0.1;
    /// `SegmentedToggle.svelte`: `.segmented { border: 1px }`, `.segment { padding: var(--space-xs) var(--space-md) }`
    pub const SEG_PAD_Y: f32 = XS;
    pub const SEG_PAD_X: f32 = MD;
    /// `Checkbox.svelte`: `.checkbox { gap: var(--space-sm) }`, `.box { 14x14; border: 1px }`, `.mark { 8x8 }`
    pub const CHECK_BOX: f32 = 14.0;
    pub const CHECK_MARK: f32 = 8.0;
    pub const CHECK_GAP: f32 = SM;
    /// `Genres.svelte:.grid { minmax(220px, 1fr); gap: var(--space-md) }`, `.card { padding: var(--space-md); gap: xs }`, `.bar { height: 3px }`
    pub const GENRE_MIN_CELL: f32 = 220.0;
    pub const GENRE_BAR_H: f32 = 3.0;
    /// `Home.svelte:.home { gap: var(--space-xl); padding: var(--space-lg) }`
    pub const HOME_PAD: f32 = LG;
    pub const HOME_GAP: f32 = XL;
    /// `.card-grid { minmax(150px, 1fr); gap: var(--space-md) }`, `.tile-grid { minmax(220px, 1fr) }`
    pub const CARD_GRID_MIN: f32 = 150.0;
    pub const TILE_GRID_MIN: f32 = 220.0;
    /// `.pl-card { padding: var(--space-sm); gap: var(--space-sm) }`, `.pl-art { 56x56 }`
    pub const PL_ART: f32 = 56.0;
    /// `.pl-play-overlay { inset: auto 2px 2px auto; 22x22 }`, `.pl-menu-overlay { top/right 4px; 22x22 }`
    pub const PL_PLAY: f32 = 22.0;
    pub const PL_PLAY_INSET: f32 = 2.0;
    pub const PL_MENU: f32 = 22.0;
    pub const PL_MENU_INSET: f32 = 4.0;
    /// `.list-block { height: 460px }`, `<VirtualList itemHeight={34}>`
    pub const HOME_LIST_H: f32 = 460.0;
    pub const HOME_ROW_H: f32 = 34.0;
    /// `AlbumDetail.svelte`: `.art { 200x200 }`, `.play-btn/.btn-secondary/.btn-icon { height: 40px }`, `.tracks-panel { height: 640px }`, `.meta-panel { width: 280px } / 220px < 1280`
    pub const ALBUM_ART: f32 = 200.0;
    pub const BTN_H: f32 = 40.0;
    pub const TRACKS_PANEL_H: f32 = 640.0;
    pub const META_PANEL_W: f32 = 280.0;
    pub const META_PANEL_W_COMPACT: f32 = 220.0;
    /// `ArtistDetail.svelte`: `.art { 160x160 }`, `.top-row { height: 34px }`, `.extras { 1.4fr 1fr; gap lg }`, `@media (max-width: 1100px)` one column
    pub const ARTIST_ART: f32 = 160.0;
    pub const TOP_ROW_H: f32 = 34.0;
    pub const EXTRAS_STACK_BELOW: f32 = 1100.0;
}
