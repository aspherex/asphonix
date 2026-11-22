//! Every margin, padding, gap, border, row height, column width, component size, breakpoint and
//! timing lives here, defined once. Views use these constants instead of literals.
//! All values are logical px (seconds for timings).
// Some entries (spectrum/meter constants, the 24 px spacing step) are kept for completeness even where
// no view references them yet.
#![allow(dead_code)]

/// Spacing scale.
pub mod space {
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 24.0;
}

/// Breakpoints (viewport width).
pub mod bp {
    /// Below this: narrower sidebar, ALB/ART counts hidden, dock transport basis 420.
    pub const COMPACT_BELOW: f32 = 1280.0;
    /// From this width the queue rail is docked; below it is an overlay drawer.
    pub const RAIL_DOCKED_FROM: f32 = 1440.0;
    /// Below this width the dock shows the short output readout.
    pub const FULL_READOUT_FROM: f32 = 1680.0;
}

/// App-wide layout constants.
pub mod layout {
    pub const TITLEBAR_H: f32 = 44.0;
    pub const SIDEBAR_W: f32 = 260.0;
    /// Sidebar width below `bp::COMPACT_BELOW`.
    pub const SIDEBAR_W_COMPACT: f32 = 216.0;
    pub const DOCK_H: f32 = 72.0;
    /// Queue rail width.
    pub const RAIL_W: f32 = 340.0;
    /// Frame / divider rules are 1 px everywhere.
    pub const RULE: f32 = 1.0;
}

/// Title bar.
pub mod titlebar {
    pub const PAD_LEFT: f32 = super::space::MD;
    pub const PAD_RIGHT: f32 = super::space::SM;
    pub const BRAND_GAP: f32 = super::space::SM;
    /// Light theme: margin and padding around the logo slash.
    pub const SLASH_MARGIN_LIGHT: f32 = 2.0;
    pub const SLASH_PAD_LIGHT: f32 = 2.0;
    pub const VERSION_PAD_X: f32 = super::space::XS;
    /// Window controls: gap, button size.
    pub const CTRL_GAP: f32 = 2.0;
    pub const CTRL_W: f32 = 32.0;
    pub const CTRL_H: f32 = 28.0;
    /// Icon sizes of minimize / maximize / close.
    pub const ICON_MIN: f32 = 12.0;
    pub const ICON_MAX: f32 = 11.0;
    pub const ICON_CLOSE: f32 = 12.0;
}

/// Top bar.
pub mod topbar {
    pub const H: f32 = 44.0;
    pub const GAP: f32 = super::space::MD;
    pub const PAD_X: f32 = super::space::MD;
    /// Status pill.
    pub const PILL_H: f32 = 26.0;
    pub const PILL_PAD_X: f32 = super::space::SM;
    pub const PILL_GAP: f32 = super::space::XS;
    pub const DOT: f32 = 6.0;
    /// Search field.
    pub const SEARCH_MAX_W: f32 = 420.0;
    pub const SEARCH_H: f32 = 28.0;
    pub const SEARCH_GAP: f32 = super::space::SM;
    pub const SEARCH_PAD_X: f32 = super::space::SM;
    /// Keyboard-shortcut hint inside the search field.
    pub const KBD_PAD_Y: f32 = 1.0;
    pub const KBD_PAD_X: f32 = super::space::XS;
    pub const ICON_LINK: f32 = 28.0;
    pub const COLLECTION_GAP: f32 = super::space::MD;
    /// Nav/theme icons; search glyph.
    pub const ICON_NAV: f32 = 16.0;
    pub const ICON_SEARCH: f32 = 14.0;
}

/// Sidebar and profile switcher.
pub mod sidebar {
    pub const ROW_H: f32 = 30.0;
    pub const ROW_GAP: f32 = super::space::SM;
    pub const ROW_PAD_X: f32 = super::space::MD;
    /// Icon on every row.
    pub const ICON: f32 = 14.0;
    pub const SECTION_PAD_TOP: f32 = super::space::XS;
    pub const SECTION_PAD_BOTTOM: f32 = super::space::SM;
    pub const TITLE_PAD_TOP: f32 = super::space::MD;
    pub const TITLE_PAD_X: f32 = super::space::MD;
    pub const TITLE_PAD_BOTTOM: f32 = super::space::XS;
    /// "New playlist" buttons next to the section title.
    pub const ADD_GAP: f32 = super::space::SM;
    pub const ADD_ICON: f32 = 12.0;
    /// 1 px separator between smart and server playlists.
    pub const SEP_MARGIN_Y: f32 = super::space::XS;
    pub const SEP_MARGIN_X: f32 = super::space::MD;
    pub const FOOTER_PAD_Y: f32 = super::space::XS;
    pub const PROFILE_PAD_Y: f32 = super::space::SM;
    pub const PROFILE_PAD_X: f32 = super::space::MD;
    pub const PROFILE_GAP: f32 = super::space::SM;
    /// Avatar and its online pip (offset outside the bottom-right corner).
    pub const AVATAR: f32 = 28.0;
    pub const AVATAR_PIP: f32 = 7.0;
    pub const AVATAR_PIP_OFFSET: f32 = -3.0;
}

/// Player dock.
pub mod dock {
    pub const GAP: f32 = super::space::LG;
    pub const PAD_X: f32 = super::space::LG;
    /// Now-playing block.
    pub const NP_GAP: f32 = super::space::MD;
    pub const NP_MIN_W: f32 = 180.0;
    pub const ART: f32 = 44.0;
    /// Transport column: flex basis, min/max width, gap.
    pub const TRANSPORT_BASIS: f32 = 520.0;
    /// Transport basis below `bp::COMPACT_BELOW`.
    pub const TRANSPORT_BASIS_COMPACT: f32 = 420.0;
    pub const TRANSPORT_MIN_W: f32 = 300.0;
    pub const TRANSPORT_MAX_W: f32 = 620.0;
    pub const TRANSPORT_GAP: f32 = 6.0;
    pub const CONTROLS_GAP: f32 = super::space::MD;
    /// Transport buttons; the play button is larger and has a 2 px hard shadow.
    pub const T_BTN: f32 = 28.0;
    pub const PLAY_BTN: f32 = 36.0;
    pub const PLAY_SHADOW: f32 = 2.0;
    /// Icon sizes: shuffle/repeat/volume 15, prev/next 16, play/pause 18, like 18, queue 16.
    pub const ICON_SMALL: f32 = 15.0;
    pub const ICON_SKIP: f32 = 16.0;
    pub const ICON_PLAY: f32 = 18.0;
    pub const ICON_LIKE: f32 = 18.0;
    pub const ICON_QUEUE: f32 = 16.0;
    /// Scrub bar and its time labels.
    pub const SCRUB_GAP: f32 = super::space::MD;
    pub const TIME_W: f32 = 40.0;
    /// Right-hand auxiliary column.
    pub const AUX_MIN_W: f32 = 260.0;
    pub const AUX_GAP: f32 = super::space::MD;
    /// Volume fader and dB readout.
    pub const VOLUME_GAP: f32 = super::space::SM;
    pub const VOLUME_FADER_W: f32 = 112.0;
    pub const DB_MIN_W: f32 = 52.0;
    /// Output warning text.
    pub const WARN_MAX_W: f32 = 220.0;
    /// Queue count badge on the queue button.
    pub const QCOUNT_TOP: f32 = -2.0;
    pub const QCOUNT_RIGHT: f32 = -6.0;
    pub const QCOUNT_PAD_X: f32 = 2.0;
}

/// Fader (volume / scrub slider).
pub mod fader {
    /// 10 px above and below a 4 px rail = 24 px hit area.
    pub const HIT_H: f32 = 24.0;
    pub const RAIL_H: f32 = 4.0;
    pub const RAIL_TOP: f32 = 10.0;
    /// Fader cap (1 px black border).
    pub const CAP_W: f32 = 10.0;
    pub const CAP_H: f32 = 18.0;
}

/// Buttons and text fields.
pub mod controls {
    pub const BTN_GAP: f32 = super::space::SM;
    pub const BTN_PAD_Y: f32 = super::space::SM;
    pub const BTN_PAD_X: f32 = super::space::LG;
    /// Hard shadow of the primary button.
    pub const BTN_SHADOW: f32 = 2.0;
    /// Icon inside page buttons.
    pub const BTN_ICON: f32 = 14.0;
    pub const FIELD_H: f32 = 36.0;
    pub const FIELD_PAD_X: f32 = super::space::MD;
    /// Default width of a text field without an explicit width (border box).
    pub const FIELD_W_DARK: f32 = 191.0;
    pub const FIELD_W_LIGHT: f32 = 191.0;
}

/// Tracks page, track rows, track list.
pub mod tracks {
    pub const PAGE_PAD: f32 = super::space::LG;
    pub const PAGE_GAP: f32 = super::space::MD;
    pub const HEADER_GAP: f32 = super::space::MD;
    pub const HEADER_RIGHT_GAP: f32 = super::space::SM;
    pub const ROW_H: f32 = 36.0;
    pub const ROW_GAP: f32 = super::space::SM;
    pub const ROW_PAD_LEFT: f32 = super::space::XS;
    pub const ROW_PAD_RIGHT: f32 = super::space::SM;
    /// Header row vertical padding (its height comes from the 24 px icon-button spacers).
    pub const HEAD_PAD_Y: f32 = super::space::XS;
    /// Column widths: index/play, year, added, format, time; row icon buttons.
    pub const IDX_W: f32 = 36.0;
    pub const YEAR_W: f32 = 56.0;
    pub const ADDED_W: f32 = 88.0;
    pub const FORMAT_W: f32 = 96.0;
    pub const TIME_W: f32 = 48.0;
    pub const ICON_BTN: f32 = 24.0;
    /// Flex-grow of the title / artist / album columns.
    pub const TITLE_GROW: f32 = 3.0;
    pub const ARTIST_GROW: f32 = 2.0;
    pub const ALBUM_GROW: f32 = 2.0;
    /// Accent bar on the left edge of the playing row.
    pub const ACTIVE_BAR_W: f32 = 2.0;
    /// Now-playing bars.
    pub const BARS_GAP: f32 = 2.0;
    pub const BARS_H: f32 = 12.0;
    pub const BAR_W: f32 = 2.0;
    /// Table widths below which the Added / Year columns are hidden.
    pub const HIDE_ADDED_BELOW: f32 = 760.0;
    pub const HIDE_YEAR_BELOW: f32 = 600.0;
    /// Scrollbar width; the thumb is inset by a 2 px border.
    pub const SCROLLBAR_W: f32 = 10.0;
    pub const SCROLLBAR_THUMB_BORDER: f32 = 2.0;
    /// Star / more icons; the row play glyph is 12.
    pub const ROW_ICON: f32 = 14.0;
    pub const ROW_PLAY_ICON: f32 = 12.0;
}

/// Animation timings (seconds).
pub mod anim {
    /// Now-playing bars: one ease-in-out cycle.
    pub const BARS_PERIOD: f32 = 0.9;
    /// Phase offsets of the three bars.
    pub const BARS_DELAY: [f32; 3] = [-0.6, -0.3, 0.0];
    /// Bar height range (fraction of full height); a paused bar rests at 0.33.
    pub const BARS_MIN: f32 = 0.25;
    pub const BARS_MAX: f32 = 1.0;
    pub const BARS_PAUSED: f32 = 0.33;
    /// Button border/background colour transition (ease).
    pub const BUTTON_TRANSITION: f32 = 0.1;
    /// Row index fade on hover.
    pub const IDX_FADE: f32 = 0.1;
}

/// Spectrum / meter constants.
pub mod spectrum {
    pub const BANDS: usize = 28;
    pub const FFT_SIZE: usize = 2048;
    pub const FLOOR_DB: f32 = -60.0;
    pub const SEGMENTS: usize = 20;
    pub const CLIP_FROM_DB: f32 = -3.0;
    pub const PEAK_HOLD_MS: f32 = 1200.0;
    pub const METER_FALL_DB_PER_S: f32 = 30.0;
    pub const CAP_FALL_PER_S: f32 = 0.45;
    pub const MIN_DRAW_MS: f32 = 20.0;
    /// Frame delta is capped at 0.1 s; with no new frame for > 120 ms the bands fall `255 * 2.2 * dt`.
    pub const MAX_DT: f32 = 0.1;
    pub const STALE_AFTER_MS: f32 = 120.0;
    pub const STALE_FALL_PER_S: f32 = 2.2;
}

/// Queue rail.
pub mod rail {
    /// Section header padding.
    pub const HEADER_PAD_Y: f32 = super::space::SM;
    pub const HEADER_PAD_X: f32 = super::space::MD;
}

/// Library routes (Home, Albums, Artists, album/artist detail, Genres, genre detail) and their cards,
/// grid, segmented toggle, checkbox and entity links.
pub mod library {
    use super::space::*;
    /// Page padding / gap (Albums, Artists, Genres, genre detail).
    pub const PAGE_PAD: f32 = LG;
    pub const PAGE_GAP: f32 = MD;
    /// Card grid: minimum cell width, text block height under the art, gap.
    pub const GRID_MIN_CELL: f32 = 140.0;
    pub const GRID_TEXT_H: f32 = 40.0;
    pub const GRID_GAP: f32 = 16.0;
    pub const CONTROLS_GAP: f32 = LG;
    /// Gap between a card's art and its text.
    pub const CARD_GAP: f32 = XS;
    /// Starred marker in the art's top-right corner.
    pub const CARD_STAR_INSET: f32 = 4.0;
    pub const CARD_STAR_ICON: f32 = 11.0;
    /// Hover play button (bottom-right, slides up 4 px).
    pub const CARD_PLAY: f32 = 30.0;
    pub const CARD_PLAY_ICON: f32 = 18.0;
    /// Hover menu button (bottom-left).
    pub const CARD_MENU: f32 = 26.0;
    pub const CARD_MENU_ICON: f32 = 14.0;
    pub const CARD_INSET: f32 = 4.0;
    pub const CARD_OVERLAY_SHIFT: f32 = 4.0;
    /// Fade/slide of the hover overlays.
    pub const CARD_TRANSITION: f32 = 0.1;
    /// Segmented toggle segment padding.
    pub const SEG_PAD_Y: f32 = XS;
    pub const SEG_PAD_X: f32 = MD;
    /// Checkbox: box, check mark, gap to the label.
    pub const CHECK_BOX: f32 = 14.0;
    pub const CHECK_MARK: f32 = 8.0;
    pub const CHECK_GAP: f32 = SM;
    /// Genres grid minimum cell width; the share bar height.
    pub const GENRE_MIN_CELL: f32 = 220.0;
    pub const GENRE_BAR_H: f32 = 3.0;
    /// Home page.
    pub const HOME_PAD: f32 = LG;
    pub const HOME_GAP: f32 = XL;
    /// Home card / tile grids: minimum cell widths.
    pub const CARD_GRID_MIN: f32 = 150.0;
    pub const TILE_GRID_MIN: f32 = 220.0;
    /// Home playlist tile art.
    pub const PL_ART: f32 = 56.0;
    /// Playlist tile play (bottom-right) and menu (top-right) overlays.
    pub const PL_PLAY: f32 = 22.0;
    pub const PL_PLAY_INSET: f32 = 2.0;
    pub const PL_MENU: f32 = 22.0;
    pub const PL_MENU_INSET: f32 = 4.0;
    /// Home "Recently Ingested" list block and its rows.
    pub const HOME_LIST_H: f32 = 460.0;
    pub const HOME_ROW_H: f32 = 34.0;
    /// Album page: art, button height, tracks panel height, meta panel width (220 below `bp::COMPACT_BELOW`).
    pub const ALBUM_ART: f32 = 200.0;
    pub const BTN_H: f32 = 40.0;
    pub const TRACKS_PANEL_H: f32 = 640.0;
    pub const META_PANEL_W: f32 = 280.0;
    pub const META_PANEL_W_COMPACT: f32 = 220.0;
    /// Artist page: art, top-track row height; below 1100 px the two extras columns stack.
    pub const ARTIST_ART: f32 = 160.0;
    pub const TOP_ROW_H: f32 = 34.0;
    pub const EXTRAS_STACK_BELOW: f32 = 1100.0;
}

/// List routes (playlist, Liked, smart playlist, History, Playlists) and badges.
pub mod lists {
    pub const PAGE_PAD: f32 = super::space::LG;
    pub const PAGE_GAP: f32 = super::space::LG;
    /// History page gap and header-right gap.
    pub const HISTORY_GAP: f32 = super::space::MD;
    /// Header gap; header art.
    pub const HEADER_GAP: f32 = super::space::LG;
    pub const ART: f32 = 120.0;
    pub const INFO_GAP: f32 = super::space::XS;
    /// Action buttons row.
    pub const ACTIONS_GAP: f32 = super::space::SM;
    pub const ACTIONS_MT: f32 = super::space::SM;
    /// Height of the play / secondary buttons (and width of icon buttons).
    pub const BTN_H: f32 = 40.0;
    pub const PLAY_PAD_X: f32 = super::space::LG;
    pub const SEC_PAD_X: f32 = super::space::MD;
    /// Icons: play 16, other buttons 15; header art icon: Liked 56, smart 52, empty playlist 32.
    pub const ICON_PLAY: f32 = 16.0;
    pub const ICON_SEC: f32 = 15.0;
    pub const ICON_ART_LIKED: f32 = 56.0;
    pub const ICON_ART_SMART: f32 = 52.0;
    pub const ICON_ART_EMPTY: f32 = 32.0;
    /// Loading / error status block.
    pub const STATUS_PAD: f32 = super::space::XL;
    /// History day heading.
    pub const DAY_PAD_X: f32 = super::space::SM;
    pub const DAY_PAD_BOTTOM: f32 = super::space::XS;
    pub const EMPTY_PAD: f32 = super::space::MD;
    /// Playlists grid: minimum cell width, gap.
    pub const GRID_MIN: f32 = 240.0;
    pub const GRID_GAP: f32 = super::space::MD;
    /// Playlist card.
    pub const CARD_GAP: f32 = super::space::SM;
    pub const CARD_PAD: f32 = super::space::SM;
    pub const CARD_ART: f32 = 44.0;
    /// Play overlay on the card art (bottom-right).
    pub const OVERLAY: f32 = 20.0;
    pub const OVERLAY_INSET: f32 = 2.0;
    pub const OVERLAY_ICON: f32 = 10.0;
    /// Card action buttons.
    pub const CARD_ACTIONS_GAP: f32 = 2.0;
    pub const CARD_ICON_BTN: f32 = 26.0;
    pub const CARD_ICON: f32 = 14.0;
    /// "New playlist" buttons.
    pub const NEW_GAP: f32 = super::space::XS;
    pub const NEW_PAD_Y: f32 = super::space::SM;
    pub const NEW_PAD_X: f32 = super::space::MD;
    pub const NEW_ICON: f32 = 13.0;
    /// Badge.
    pub const BADGE_PAD_X: f32 = super::space::XS;
    pub const BADGE_LINE: f32 = 16.0;
    /// Segmented toggle segment padding.
    pub const SEG_PAD_Y: f32 = super::space::XS;
    pub const SEG_PAD_X: f32 = super::space::MD;
}
