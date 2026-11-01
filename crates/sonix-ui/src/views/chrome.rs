//! Title bar, top bar and sidebar (`TitleBar.svelte`, `TopBar.svelte`, `Sidebar.svelte`,
//! `ProfileSwitcher.svelte`). All sizes come from `crate::metrics` (each with its CSS source).
use super::shell::Shell;
use crate::metrics::{layout, sidebar as sb, titlebar as tb, topbar as top};
use crate::probe::pid;
use crate::router::Route;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{Family, HEADLINE_SM, LABEL_CAPS, MONO_CODE, MONO_DATA, TypeSpec};
use crate::widgets::button::ghost_icon;
use crate::widgets::tooltip::tip;
use crate::widgets::{icon, icon_g};
use gpui::{AnyElement, Context, CursorStyle, Div, IntoElement, SharedString, Stateful, Window, WindowControlArea, div, prelude::*, px};
use sonix_api::format as fmt;

/// `.logo`: Space Grotesk 18/24 600 caps with 0.04em tracking (`TitleBar.svelte:.logo` + `.type-headline-sm`).
const LOGO: TypeSpec = TypeSpec { family: Family::Display, size: HEADLINE_SM.size, line: HEADLINE_SM.line, tracking_em: 0.04, weight: 600.0, caps: true };

/// `.brand { align-items: baseline }`: the version chip (14 px line box) sits 7 px below the
/// logo's line box top so both baselines coincide (measured on the web app: logo y 9.5, chip y 16.5).
const VERSION_DY: f32 = 7.0;

/// Host part of a server URL (`ProfileSwitcher.svelte: host()`).
fn host(url: &str) -> String {
    let u = url.split("://").nth(1).unwrap_or(url);
    u.split('/').next().unwrap_or(u).to_string()
}

impl Shell {
    pub(crate) fn title_bar(&self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let th = self.th;
        let maximized = window.is_maximized();
        let ctrl = move |id: &'static str, area: WindowControlArea, name: &'static str, size: f32, danger: bool| {
            div()
                .id(id)
                .group("hv")
                .window_control_area(area)
                .flex()
                .items_center()
                .justify_center()
                .w(px(tb::CTRL_W))
                .h(px(tb::CTRL_H))
                .text_color(th.muted)
                .hover(move |s| if danger { s.bg(th.clip).text_color(th.text) } else { s.bg(th.high).text_color(th.text) })
                .child(icon_g(name, size, th.muted, th.text))
        };
        // `.slash`: accent text in dark; light theme fills it (margin-left 2, padding 0 2).
        let slash = if th.light {
            div().ml(px(tb::SLASH_MARGIN_LIGHT)).px(px(tb::SLASH_PAD_LIGHT)).bg(th.accent).child(txt(&th, LOGO, "//").color(th.accent_text))
        } else {
            div().child(txt(&th, LOGO, "//").color(th.accent_fg))
        };
        let version = format!("v{}", env!("CARGO_PKG_VERSION"));
        let mock = self.backend.mock_label();
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
                    .child(pid("titlebar.logo", div().flex().flex_row().child(txt(&th, LOGO, "SONIX").color(th.text)).child(slash)))
                    .child(pid("titlebar.version", div().mt(px(VERSION_DY)).px(px(tb::VERSION_PAD_X)).bg(th.high).child(txt(&th, MONO_CODE, version).color(th.muted))))
                    // Phase-1 honesty label (CLAUDE.md "real data only"): the data behind this UI is the mock.
                    .children(mock.map(|m| div().mt(px(VERSION_DY)).px(px(tb::VERSION_PAD_X)).bg(th.high).child(txt(&th, MONO_CODE, m).color(th.accent_fg)))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(tb::CTRL_GAP))
                    .child(pid("titlebar.ctrl.min", ctrl("min", WindowControlArea::Min, "minimize", tb::ICON_MIN, false)))
                    .child(pid("titlebar.ctrl.max", ctrl("max", WindowControlArea::Max, if maximized { "collapse" } else { "maximize" }, tb::ICON_MAX, false)))
                    .child(pid("titlebar.ctrl.close", ctrl("close", WindowControlArea::Close, "close", tb::ICON_CLOSE, true))),
            )
    }

    pub(crate) fn top_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let th = self.th;
        let vw = f32::from(window.viewport_size().width);
        let can_back = self.router.can_back();
        let can_fwd = self.router.can_forward();
        let connected = self.status.as_ref().map(|s| s.state == sonix_api::ConnectionState::Connected).unwrap_or(false);
        let state_label = if connected { "CONNECTED" } else { "DISCONNECTED" };
        let ping = self.status.as_ref().and_then(|s| s.ping_ms).map(|p| format!("{p}MS")).unwrap_or_else(|| "—".into());
        let search_focused = self.search.read(cx).focus_handle.is_focused(window);
        let count = |n: Option<usize>| n.map(|n| fmt::short_count(n as u64)).unwrap_or_else(|| "—".into());
        // `<span><b>{n}</b> LABEL</span>`: bold 600 number, then a text space and the label.
        let stat = move |n: String, label: &'static str| {
            div().flex().flex_row().child(txt(&th, MONO_CODE, n).weight(600.0).color(th.text)).child(txt(&th, MONO_CODE, format!(" {label}")).color(th.muted)).into_any_element()
        };
        let nav = div()
            .flex()
            .flex_row()
            .flex_none()
            .border_1()
            .border_color(th.border)
            .child(pid("topbar.nav.back", ghost_icon("nav-back", th, "chevron-left", top::ICON_NAV, top::ICON_LINK, false, !can_back).on_click(cx.listener(|this, _, _, cx| this.go_back(cx)))))
            // `.nav .icon-link + .icon-link { border-left: 1px }` lives inside the second button's 28 px box.
            .child(pid(
                "topbar.nav.fwd",
                ghost_icon("nav-fwd", th, "chevron-right", top::ICON_NAV, top::ICON_LINK, false, !can_fwd).border_l_1().border_color(th.border).on_click(cx.listener(|this, _, _, cx| this.go_forward(cx))),
            ));
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
            .child(pid("topbar.pill.dot", div().size(px(top::DOT)).flex_none().bg(if connected { th.accent } else { th.muted })))
            .child(pid("topbar.pill.state", txt(&th, MONO_DATA, state_label)))
            .when(connected, |d| d.child(pid("topbar.pill.sep", txt(&th, MONO_DATA, "·").color(th.border))).child(pid("topbar.pill.ping", txt(&th, MONO_DATA, ping))));
        let search = div()
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
            .child(pid("topbar.search.kbd", div().px(px(top::KBD_PAD_X)).py(px(top::KBD_PAD_Y)).bg(th.high).flex_none().child(txt(&th, MONO_CODE, "CTRL K").color(th.muted))));
        let collection = div()
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
            .child(pid("topbar.nav", nav))
            .child(pid("topbar.pill", pill))
            .child(pid("topbar.search", search))
            .child(div().flex_1())
            .child(pid("topbar.collection", collection))
            .child(pid("topbar.theme", theme_btn))
    }

    /// `.row` of the sidebar: 30 px, 12 px side padding, icon + name.
    fn nav_row(&self, id: impl Into<gpui::ElementId>, th: Theme, active: bool, icon_name: &str, label: &str) -> Stateful<Div> {
        let fg = if active { th.accent_text } else { th.muted };
        div()
            .id(id)
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

    pub(crate) fn sidebar(&self, vw: f32, route: &Route, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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
        library = library.child(pid("sidebar.library.title", title_pad(div().flex().flex_row().child(txt(&th, LABEL_CAPS, "Library").color(th.muted)))));
        for (nav_ix, (r, ic, label)) in nav_items.into_iter().enumerate() {
            let active = *route == r;
            let target = r.clone();
            library = library.child(pid(
                format!("sidebar.nav.{nav_ix}"),
                self.nav_row(SharedString::from(format!("nav-{label}")), th, active, ic, label).on_click(cx.listener(move |this, _, _, cx| this.navigate(target.clone(), cx))),
            ));
        }

        // Playlist rows: mono-data name (mixed case, ellipsis), mono-code count on the right.
        let pl_row = |id: SharedString, active: bool, icon_name: &'static str, name: String, count: String| {
            let fg = if active { th.accent_text } else { th.muted };
            div()
                .id(id)
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
                .when(!active, |d| d.hover(move |s| s.bg(th.mid).text_color(th.text)))
                .when(active, |d| d.bg(th.accent))
                .child(icon_g(icon_name, sb::ICON, fg, if active { th.accent_text } else { th.text }))
                .child(div().flex_1().min_w_0().flex().child(txt(&th, MONO_DATA, name).weight(if active { 700.0 } else { 500.0 }).ellipsis()))
                .child(div().flex_none().child(txt(&th, MONO_CODE, count).weight(if active { 700.0 } else { 400.0 })))
        };
        let mut lists = div()
            .id("playlist-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .border_t_1()
            .border_color(th.border)
            .flex()
            .flex_col()
            .pt(px(sb::SECTION_PAD_TOP))
            .pb(px(sb::SECTION_PAD_BOTTOM));
        lists = lists.child(pid(
            "sidebar.playlists.title",
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px(px(sb::TITLE_PAD_X))
                .pt(px(sb::TITLE_PAD_TOP))
                .pb(px(sb::TITLE_PAD_BOTTOM))
                .child(div().flex().child(txt(&th, LABEL_CAPS, "Playlists").color(if *route == Route::Playlists { th.accent_fg } else { th.muted })))
                .child(div().flex().flex_row().gap(px(sb::ADD_GAP)).child(icon("bolt", sb::ADD_ICON, th.muted)).child(icon("add", sb::ADD_ICON, th.muted))),
        ));
        let liked = self.counts.liked.map(|n| fmt::fmt_int(n as u64)).unwrap_or_else(|| "—".into());
        lists = lists.child(pid("sidebar.pl.liked", pl_row("pl-liked".into(), *route == Route::Liked, "heart-filled", "Liked Songs".into(), liked).on_click(cx.listener(|this, _, _, cx| this.navigate(Route::Liked, cx)))));
        lists = lists.child(div().h(px(layout::RULE)).mx(px(sb::SEP_MARGIN_X)).my(px(sb::SEP_MARGIN_Y)).bg(th.border));
        for (i, p) in self.playlists.iter().enumerate() {
            let active = *route == Route::Playlist(p.id.clone());
            let id = p.id.clone();
            let icon_name = if p.read_only { if p.smart { "bolt" } else { "lock" } } else { "playlist" };
            lists = lists.child(pid(
                format!("sidebar.pl.s{i}"),
                pl_row(SharedString::from(format!("pl-{}", p.id)), active, icon_name, p.name.clone(), p.song_count.to_string()).on_click(cx.listener(move |this, _, _, cx| this.navigate(Route::Playlist(id.clone()), cx))),
            ));
        }

        // ProfileSwitcher: avatar (2 initials + online pip), name, host, "more" icon.
        let user = self.status.as_ref().and_then(|s| s.username.clone());
        let online = self.status.as_ref().map(|s| s.state == sonix_api::ConnectionState::Connected).unwrap_or(false);
        let initials: String = user.clone().unwrap_or_else(|| "?".into()).chars().take(2).collect::<String>().to_uppercase();
        let server = self.status.as_ref().and_then(|s| s.server_url.clone()).map(|u| host(&u)).unwrap_or_default();
        let profile = div()
            .id("profile")
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
            .child(pid(
                "sidebar.profile.avatar",
                div()
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
                    }),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(pid("sidebar.profile.name", div().flex().child(txt(&th, MONO_DATA, user.unwrap_or_else(|| "Not signed in".into())).weight(600.0).color(th.text).ellipsis())))
                    .child(pid("sidebar.profile.server", div().flex().child(txt(&th, MONO_CODE, server).ellipsis()))),
            )
            .child(icon_g("more", sb::ICON, th.muted, th.text));

        let footer = div()
            .flex_none()
            .border_t_1()
            .border_color(th.frame)
            .py(px(sb::FOOTER_PAD_Y))
            .child(pid("sidebar.settings", self.nav_row("nav-settings", th, *route == Route::Settings, "settings", "Settings").on_click(cx.listener(|this, _, _, cx| this.navigate(Route::Settings, cx)))))
            .child(pid("sidebar.profile", profile));

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
            .child(pid("sidebar.playlists", lists))
            .child(pid("sidebar.footer", footer))
            .into_any_element()
    }
}
