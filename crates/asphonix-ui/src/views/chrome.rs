//! Title bar, top bar, sidebar and profile switcher. All sizes come from `crate::metrics`.
use super::shell::Shell;
use crate::metrics::{layout, sidebar as sb, titlebar as tb, topbar as top};
use crate::router::Route;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{Family, HEADLINE_SM, LABEL_CAPS, MONO_CODE, MONO_DATA, TypeSpec};
use crate::widgets::button::ghost_icon;
use crate::widgets::tooltip::{Tip, tip};
use crate::widgets::{icon, icon_g};
use gpui::{AnyElement, Context, CursorStyle, Div, IntoElement, SharedString, Stateful, Window, WindowControlArea, div, prelude::*, px};
use asphonix_api::format as fmt;

/// The wordmark: Space Grotesk 18/24 600 caps with 0.04em tracking.
const LOGO: TypeSpec = TypeSpec { family: Family::Display, size: HEADLINE_SM.size, line: HEADLINE_SM.line, tracking_em: 0.04, weight: 600.0, caps: true };

/// The version chip (14 px line box) sits 7 px below the logo's line box top so both baselines
/// coincide (logo at y 9.5, chip at y 16.5).
const VERSION_DY: f32 = 7.0;

/// Host part of a server URL.
fn host(url: &str) -> String {
    let u = url.split("://").nth(1).unwrap_or(url);
    u.split('/').next().unwrap_or(u).to_string()
}

impl Shell {
    pub(crate) fn title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let th = self.th;
        let maximized = window.is_maximized();
        let ctrl = move |id: &'static str, area: WindowControlArea, name: &'static str, size: f32, danger: bool| {
            div()
                .id(id)
                .group("hv")
                .window_control_area(area)
                // occlude: otherwise the title bar's Drag hitbox (painted first, it contains the buttons) wins GPUI's
                // window-control hit test and the min/max/close buttons answer HTCAPTION
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .w(px(tb::CTRL_W))
                .h(px(tb::CTRL_H))
                .text_color(th.muted)
                .hover(move |s| if danger { s.bg(th.clip).text_color(th.text) } else { s.bg(th.high).text_color(th.text) })
                .child(icon_g(name, size, th.muted, th.text))
        };
        // the slash: accent text in dark; the light theme fills it (margin-left 2, padding 0 2).
        let slash = if th.light {
            div().ml(px(tb::SLASH_MARGIN_LIGHT)).px(px(tb::SLASH_PAD_LIGHT)).bg(th.accent).child(txt(&th, LOGO, "//").color(th.accent_text))
        } else {
            div().child(txt(&th, LOGO, "//").color(th.accent_fg))
        };
        let version = format!("v{}", env!("CARGO_PKG_VERSION"));
        div()
            .id("titlebar")
            .window_control_area(WindowControlArea::Drag)
            .flex()
            .flex_row()
            .items_center()
            .h(px(layout::TITLEBAR_H))
            .w_full()
            .flex_none()
            .bg(th.low)
            .border_b_1()
            .border_color(th.frame)
            .pl(px(tb::PAD_LEFT))
            .pr(px(tb::PAD_RIGHT))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(tb::BRAND_GAP))
                    .child(div().flex().flex_row().child(txt(&th, LOGO, "ASPHONIX").color(th.text)).child(slash))
                    .child(div().mt(px(VERSION_DY)).px(px(tb::VERSION_PAD_X)).bg(th.high).child(txt(&th, MONO_CODE, version).color(th.muted))),
            )
            .child(div().flex_1())
            .children(self.update.clone().filter(|u| u.available).map(|u| {
                // update chip: accent fill, 2 px / space-sm padding, space-sm right margin
                div()
                        .id("update-badge")
                        .occlude() // a client hitbox above the caption, so the click reaches on_click
                        .mr(px(8.))
                        .px(px(8.))
                        .py(px(2.))
                        .bg(th.accent)
                        .tip("A new version is available", th)
                        .cursor(CursorStyle::PointingHand)
                        .hover(|s| s.opacity(0.85))
                        .child(txt(&th, LABEL_CAPS, format!("Update {}", u.version.clone().unwrap_or_default())).color(th.accent_text))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let playing = this.player.read(cx).state.status == asphonix_api::PlaybackStatus::Playing;
                            crate::overlays::open_update(cx, u.clone(), playing);
                        }))
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(tb::CTRL_GAP))
                    .child(ctrl("min", WindowControlArea::Min, "minimize", tb::ICON_MIN, false))
                    .child(ctrl("max", WindowControlArea::Max, if maximized { "collapse" } else { "maximize" }, tb::ICON_MAX, false))
                    .child(ctrl("close", WindowControlArea::Close, "close", tb::ICON_CLOSE, true)),
            )
    }

    pub(crate) fn top_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let th = self.th;
        let vw = f32::from(window.viewport_size().width);
        let can_back = self.router.can_back();
        let can_fwd = self.router.can_forward();
        let connected = self.status.as_ref().map(|s| s.state == asphonix_api::ConnectionState::Connected).unwrap_or(false);
        let state_label = if connected { "CONNECTED" } else { "DISCONNECTED" };
        let ping = self.status.as_ref().and_then(|s| s.ping_ms).map(|p| format!("{p}MS")).unwrap_or_else(|| "—".into());
        let search_focused = self.search.read(cx).focus_handle.is_focused(window);
        let count = |n: Option<usize>| n.map(|n| fmt::short_count(n as u64)).unwrap_or_else(|| "—".into());
        // bold 600 number, then a text space and the label.
        let stat = move |n: String, label: &'static str| {
            div().flex().flex_row().child(txt(&th, MONO_CODE, n).weight(600.0).color(th.text)).child(txt(&th, MONO_CODE, format!(" {label}")).color(th.muted)).into_any_element()
        };
        let nav = div()
            .flex()
            .flex_row()
            .flex_none()
            .border_1()
            .border_color(th.border)
            .child(ghost_icon("nav-back", th, "chevron-left", top::ICON_NAV, top::ICON_LINK, false, !can_back).tip("Back · ALT+←", th).on_click(cx.listener(|this, _, _, cx| this.go_back(cx))))
            // the 1 px rule between the two nav buttons lives inside the second button's 28 px box.
            .child(ghost_icon("nav-fwd", th, "chevron-right", top::ICON_NAV, top::ICON_LINK, false, !can_fwd).border_l_1().border_color(th.border).tip("Forward · ALT+→", th).on_click(cx.listener(|this, _, _, cx| this.go_forward(cx))));
        let pill = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(top::PILL_GAP))
            .px(px(top::PILL_PAD_X))
            .h(px(top::PILL_H))
            
            .bg(th.mid)
            .border_1()
            .border_color(th.border)
            .text_color(if connected { th.text } else { th.muted })
            .child(div().size(px(top::DOT)).flex_none().bg(if connected { th.accent } else { th.muted }))
            .child(txt(&th, MONO_DATA, state_label))
            .when(connected, |d| d.child(txt(&th, MONO_DATA, "·").color(th.border)).child(txt(&th, MONO_DATA, ping)));
        let mut search = div()
            .flex_1()
            .max_w(px(top::SEARCH_MAX_W))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(top::SEARCH_GAP))
            .h(px(top::SEARCH_H))
            .px(px(top::SEARCH_PAD_X))
            .bg(th.low)
            .border_1()
            .border_color(if search_focused { th.focus } else { th.border })
            .text_color(th.muted)
            .child(icon("search", top::ICON_SEARCH, th.muted))
            .child(div().flex_1().min_w_0().child(self.search.clone()))
            .child(div().px(px(top::KBD_PAD_X)).py(px(top::KBD_PAD_Y)).bg(th.high).flex_none().child(txt(&th, MONO_CODE, "CTRL K").color(th.muted)));
        // the search (max 420 px) and the right-aligned collection counts: the search takes the free
        // space first (up to its max), the spacer only what is left (grow 1000:1 approximates "first").
        search.style().flex_grow = Some(1000.0);
        // tooltip of the collection counts: full numbers with thousands separators
        let full = |n: Option<usize>| n.map(|n| fmt::fmt_int(n as u64)).unwrap_or_else(|| "—".into());
        let counts_title = format!("{} tracks · {} albums · {} artists", full(self.counts.tracks), full(self.counts.albums), full(self.counts.artists));
        let collection = div()
            .id("topbar-collection")
            .tip(counts_title, th)
            .flex()
            .flex_row()
            .gap(px(top::COLLECTION_GAP))
            .flex_none()
            .child(stat(count(self.counts.tracks), "TRK"))
            .when(vw >= crate::metrics::bp::COMPACT_BELOW, |d| d.child(stat(count(self.counts.albums), "ALB")).child(stat(count(self.counts.artists), "ART")));
        let theme_btn = tip(
            ghost_icon("theme", th, if th.light { "moon" } else { "sun" }, top::ICON_NAV, top::ICON_LINK, false, false).border_1().border_color(th.border),
            if th.light { "Dark mode" } else { "Light mode" },
            th,
        )
        .on_click(cx.listener(|this, _, _, cx| this.toggle_theme(cx)));
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(top::GAP))
            .h(px(top::H))
            .w_full()
            .flex_none()
            .px(px(top::PAD_X))
            .bg(th.base)
            .border_b_1()
            .border_color(th.frame)
            .child(nav)
            .child(pill)
            .child(search)
            .child(div().flex_1())
            .child(collection)
            .child(theme_btn)
    }

    /// A sidebar row: 30 px, 12 px side padding, icon + name.
    fn nav_row(&self, id: impl Into<gpui::ElementId>, th: Theme, active: bool, icon_name: &str, label: &str) -> Stateful<Div> {
        let fg = if active { th.accent_text } else { th.muted };
        div()
            .id(id)
            .tab_index(0)
            .group("hv")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(sb::ROW_GAP))
            .w_full()
            .h(px(sb::ROW_H))
            .px(px(sb::ROW_PAD_X))
            .cursor(CursorStyle::PointingHand)
            .text_color(fg)
            .when(active, |d| d.bg(th.accent))
            .when(!active, |d| d.hover(move |s| s.bg(th.mid).text_color(th.text)))
            .child(icon_g(icon_name, sb::ICON, fg, if active { fg } else { th.text }))
            .child(div().flex_1().min_w_0().flex().child(txt(&th, LABEL_CAPS, label.to_string()).ellipsis()))
    }

    pub(crate) fn sidebar(&self, vw: f32, route: &Route, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = self.th;
        let nav_items: [(Route, &str, &str); 6] = [
            (Route::Home, "home", "Home"),
            (Route::Albums, "album", "Albums"),
            (Route::Artists, "artist", "Artists"),
            (Route::Tracks, "track", "Tracks"),
            (Route::Genres, "grid", "Genres"),
            (Route::History, "list", "History"),
        ];
        let title_pad = |d: Div| d.px(px(sb::TITLE_PAD_X)).pt(px(sb::TITLE_PAD_TOP)).pb(px(sb::TITLE_PAD_BOTTOM));
        let mut library = div().flex().flex_col().pt(px(sb::SECTION_PAD_TOP)).pb(px(sb::SECTION_PAD_BOTTOM));
        library = library.child(title_pad(div().flex().flex_row().child(txt(&th, LABEL_CAPS, "Library").color(th.muted))));
        for (r, ic, label) in nav_items {
            let active = *route == r;
            let target = r.clone();
            library = library.child(self.nav_row(SharedString::from(format!("nav-{label}")), th, active, ic, label).on_click(cx.listener(move |this, _, _, cx| this.navigate(target.clone(), cx))));
        }

        // Playlist rows: mono-data name (mixed case, ellipsis), mono-code count on the right.
        let pl_row = |id: SharedString, active: bool, icon_name: &'static str, name: String, count: String| {
            let fg = if active { th.accent_text } else { th.muted };
            div()
                .id(id)
                .tab_index(0)
                .group("hv")
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(sb::ROW_GAP))
                .w_full()
                .h(px(sb::ROW_H))
                .px(px(sb::ROW_PAD_X))
                .cursor(CursorStyle::PointingHand)
                .text_color(fg)
                .when(!active, |d| d.hover(move |s| s.bg(th.mid).text_color(th.text)))
                .when(active, |d| d.bg(th.accent))
                .child(icon_g(icon_name, sb::ICON, fg, if active { th.accent_text } else { th.text }))
                .child(div().flex_1().min_w_0().flex().child(txt(&th, MONO_DATA, name).weight(if active { 700.0 } else { 500.0 }).ellipsis()))
                .child(div().flex_none().child(txt(&th, MONO_CODE, count).weight(if active { 700.0 } else { 400.0 })))
        };
        let (pl_bar, pl_overflow) = crate::widgets::scrollbar::passive(th, &self.pl_scroll);
        let mut lists = div()
            .id("playlist-scroll")
            .track_scroll(&self.pl_scroll)
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .pt(px(sb::SECTION_PAD_TOP))
            .pb(px(sb::SECTION_PAD_BOTTOM))
            .when(pl_overflow, |d| d.pr(px(crate::metrics::tracks::SCROLLBAR_W)));
        lists = lists.child(div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .justify_between()
                .px(px(sb::TITLE_PAD_X))
                .pt(px(sb::TITLE_PAD_TOP))
                .pb(px(sb::TITLE_PAD_BOTTOM))
                .child(div().id("pl-title").tab_index(0).flex().cursor(CursorStyle::PointingHand).child(txt(&th, LABEL_CAPS, "Playlists").color(if *route == Route::Playlists { th.accent_fg } else { th.muted })).on_click(cx.listener(|this, _, _, cx| this.navigate(Route::Playlists, cx))))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(sb::ADD_GAP))
                        .child(div().id("pl-add-smart").tab_index(0).flex().cursor(CursorStyle::PointingHand).child(icon("bolt", sb::ADD_ICON, th.muted)).tip("New smart playlist (local)", th).on_click(cx.listener(|this, _, _, cx| this.lists_new_smart(cx))))
                        .child(div().id("pl-add").tab_index(0).flex().cursor(CursorStyle::PointingHand).child(icon("add", sb::ADD_ICON, th.muted)).tip("New playlist", th).on_click(cx.listener(|this, _, _, cx| this.lists_new_playlist(cx)))),
                ));
        let liked = self.counts.liked.map(|n| fmt::fmt_int(n as u64)).unwrap_or_else(|| "—".into());
        lists = lists.child(pl_row("pl-liked".into(), *route == Route::Liked, "heart-filled", "Liked Songs".into(), liked)
                .on_click(cx.listener(|this, _, _, cx| this.navigate(Route::Liked, cx)))
                .on_mouse_down(gpui::MouseButton::Right, cx.listener(|this, ev: &gpui::MouseDownEvent, window, cx| this.lists_liked_menu(ev.position, window, cx))));
        for sp in &self.lists.smart_side {
            let active = *route == Route::Smart(sp.id.clone());
            let id = sp.id.clone();
            let spc = sp.clone();
            lists = lists.child(pl_row(SharedString::from(format!("sm-{}", sp.id)), active, "bolt", sp.input.name.clone(), fmt::fmt_int(sp.track_count as u64))
                    .tip("Local smart playlist", th)
                    .on_click(cx.listener(move |this, _, _, cx| this.navigate(Route::Smart(id.clone()), cx)))
                    .on_mouse_down(gpui::MouseButton::Right, cx.listener(move |this, ev: &gpui::MouseDownEvent, window, cx| this.lists_smart_menu(&spc, ev.position, window, cx))));
        }
        lists = lists.child(div().h(px(layout::RULE)).flex_none().mx(px(sb::SEP_MARGIN_X)).my(px(sb::SEP_MARGIN_Y)).bg(th.border));
        for p in &self.playlists {
            let active = *route == Route::Playlist(p.id.clone());
            let id = p.id.clone();
            let icon_name = if p.read_only { if p.smart { "bolt" } else { "lock" } } else { "playlist" };
            lists = lists.child({
                    let pc = p.clone();
                    let row = pl_row(SharedString::from(format!("pl-{}", p.id)), active, icon_name, p.name.clone(), p.song_count.to_string())
                        .tip_opt(p.read_only.then_some(if p.smart { "Smart playlist (read-only)" } else { "Read-only playlist" }), th)
                        .relative()
                        .on_click(cx.listener(move |this, _, _, cx| this.navigate(Route::Playlist(id.clone()), cx)))
                        .on_mouse_down(gpui::MouseButton::Right, cx.listener(move |this, ev: &gpui::MouseDownEvent, window, cx| this.lists_playlist_menu(&pc, ev.position, window, cx)));
                    if p.read_only {
                        row
                    } else {
                        // drop target: read-only playlists never are; the hover frame is a dashed accent outline
                        let (pid_, pname) = (p.id.clone(), p.name.clone());
                        row.on_drop(cx.listener(move |this, d: &crate::dnd::AsphonixDrag, _, cx| this.drop_on_playlist(pid_.clone(), pname.clone(), d.0.clone(), cx))).child(
                            div().absolute().top_0().left_0().size_full().border_1().border_dashed().border_color(gpui::transparent_black()).group_drag_over::<crate::dnd::AsphonixDrag>("hv", move |s| s.border_color(th.accent_fg)),
                        )
                    }
                });
        }

        // ProfileSwitcher: avatar (2 initials + online pip), name, host, "more" icon.
        let user = self.status.as_ref().and_then(|s| s.username.clone());
        let online = self.status.as_ref().map(|s| s.state == asphonix_api::ConnectionState::Connected).unwrap_or(false);
        let initials: String = user.clone().unwrap_or_else(|| "?".into()).chars().take(2).collect::<String>().to_uppercase();
        let server = self.status.as_ref().and_then(|s| s.server_url.clone()).map(|u| host(&u)).unwrap_or_default();
        let profile = div()
            .id("profile")
            .tip("Switch profile", th)
            .tab_index(0)
            .group("hv")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(sb::PROFILE_GAP))
            .w_full()
            .px(px(sb::PROFILE_PAD_X))
            .py(px(sb::PROFILE_PAD_Y))
            .text_color(th.muted)
            .cursor(CursorStyle::PointingHand)
            .hover(move |s| s.bg(th.mid).text_color(th.text))
            .on_click(cx.listener(|this, ev: &gpui::ClickEvent, window, cx| this.lists_profile_menu(ev.position(), window, cx)))
            .child(div()
                    .relative()
                    .size(px(sb::AVATAR))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(th.high)
                    .border_1()
                    .border_color(th.frame)
                    .child(txt(&th, MONO_CODE, initials).weight(700.0).color(th.text))
                    .when(online, |d| {
                        d.child(
                            div()
                                .absolute()
                                .right(px(sb::AVATAR_PIP_OFFSET - 1.0))
                                .bottom(px(sb::AVATAR_PIP_OFFSET - 1.0))
                                .size(px(sb::AVATAR_PIP))
                                .bg(th.accent)
                                .border_1()
                                .border_color(th.frame),
                        )
                    }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().flex().child(txt(&th, MONO_DATA, user.unwrap_or_else(|| "Not signed in".into())).weight(600.0).color(th.text).ellipsis()))
                    .child(div().flex().child(txt(&th, MONO_CODE, server).ellipsis())),
            )
            .child(icon_g("more", sb::ICON, th.muted, th.text));

        let footer = div()
            .flex_none()
            .border_t_1()
            .border_color(th.frame)
            .py(px(sb::FOOTER_PAD_Y))
            .child(self.nav_row("nav-settings", th, *route == Route::Settings, "settings", "Settings").on_click(cx.listener(|this, _, _, cx| this.navigate(Route::Settings, cx))))
            .child(profile);

        div()
            .flex()
            .flex_col()
            .w(px(Self::sidebar_width(vw)))
            .h_full()
            .flex_none()
            .bg(th.chrome)
            .border_r_1()
            .border_color(th.frame)
            .child(library)
            .child(div().relative().flex_1().min_h_0().border_t_1().border_color(th.border).child(lists).child(pl_bar).child(crate::widgets::smooth::drive(window, crate::widgets::smooth::Scrollable::div(std::ptr::addr_of!(self.pl_scroll) as usize, &self.pl_scroll))))
            .child(footer)
            .into_any_element()
    }
}
