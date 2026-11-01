//! Title bar, top bar and sidebar (`TitleBar.svelte`, `TopBar.svelte`, `Sidebar.svelte`).
use super::shell::Shell;
use crate::probe::pid;
use crate::router::Route;
use crate::theme::{MONO, Theme};
use crate::tracking::{Family, HEADLINE_SM, LABEL_CAPS, MONO_CODE, MONO_DATA, TypeSpec, tracked};
use crate::typo::{ellipsis, ts};
use crate::widgets::button::ghost_icon;
use crate::widgets::tooltip::tip;
use crate::widgets::{icon, icon_g};
use gpui::{
    AnyElement, Context, CursorStyle, Div, FontWeight, IntoElement, MouseButton, SharedString, Stateful, Window, WindowControlArea, div, prelude::*,
    px, transparent_black,
};
use sonix_api::format as fmt;

/// `.logo`: Space Grotesk 18/24 600 caps with 0.04em tracking.
const LOGO: TypeSpec = TypeSpec { family: Family::Display, size: HEADLINE_SM.size, line: HEADLINE_SM.line, tracking_em: 0.04, weight: 600.0, caps: true };

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
                .w(px(32.))
                .h(px(28.))
                .text_color(th.muted)
                .hover(move |s| if danger { s.bg(th.clip).text_color(th.text) } else { s.bg(th.high).text_color(th.text) })
                .child(icon_g(name, size, th.muted, th.text))
        };
        let slash = if th.light {
            div().ml(px(2.)).px(px(2.)).bg(th.accent).child(tracked("//", LOGO, th.body_font, th.accent_text, window))
        } else {
            div().child(tracked("//", LOGO, th.body_font, th.accent_fg, window))
        };
        let mock = self.backend.mock_label();
        let version = format!("v{}{}", env!("CARGO_PKG_VERSION"), mock.map(|m| format!(" · {m}")).unwrap_or_default());
        div()
            .id("titlebar")
            .window_control_area(WindowControlArea::Drag)
            .flex()
            .flex_row()
            .items_center()
            .h(px(44.))
            .w_full()
            .flex_none()
            .bg(th.low)
            .border_b_1()
            .border_color(th.frame)
            .pl(px(12.))
            .pr(px(8.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap(px(8.))
                    .child(pid("titlebar.logo", div().flex().flex_row().child(tracked("SONIX", LOGO, th.body_font, th.text, window)).child(slash)))
                    .child(pid("titlebar.version", ts(&th, MONO_CODE).px(px(4.)).bg(th.high).text_color(th.muted).child(SharedString::from(version)))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(2.))
                    .child(pid("titlebar.ctrl.min", ctrl("min", WindowControlArea::Min, "minimize", 12., false)))
                    .child(pid("titlebar.ctrl.max", ctrl("max", WindowControlArea::Max, if maximized { "collapse" } else { "maximize" }, 11., false)))
                    .child(pid("titlebar.ctrl.close", ctrl("close", WindowControlArea::Close, "close", 12., true))),
            )
    }

    pub(crate) fn top_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let th = self.th;
        let vw = f32::from(window.viewport_size().width);
        let can_back = self.router.can_back();
        let can_fwd = self.router.can_forward();
        let connected = self.status.as_ref().map(|s| s.state == sonix_api::ConnectionState::Connected).unwrap_or(false);
        let ping = self.status.as_ref().and_then(|s| s.ping_ms).map(|p| format!("{p} MS")).unwrap_or_else(|| "-- MS".into());
        let search_focused = self.search.read(cx).focus_handle.is_focused(window);
        let count = |n: Option<usize>| n.map(|n| fmt::short_count(n as u64)).unwrap_or_else(|| "—".into());
        let stat = move |n: String, label: &'static str| {
            div()
                .flex()
                .flex_row()
                .gap(px(4.))
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(th.text).child(SharedString::from(n)))
                .child(label)
        };
        let nav = div()
            .flex()
            .flex_row()
            .flex_none()
            .border_1()
            .border_color(th.border)
            .child(pid("topbar.nav.back", ghost_icon("nav-back", th, "chevron-left", 16., 28., false, !can_back).on_click(cx.listener(|this, _, _, cx| this.go_back(cx)))))
            .child(div().w(px(1.)).bg(th.border))
            .child(pid("topbar.nav.fwd", ghost_icon("nav-fwd", th, "chevron-right", 16., 28., false, !can_fwd).on_click(cx.listener(|this, _, _, cx| this.go_forward(cx)))));
        let pill = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .px(px(8.))
            .h(px(26.))
            .flex_none()
            .bg(th.mid)
            .border_1()
            .border_color(th.border)
            .text_color(if connected { th.text } else { th.muted })
            .child(pid("topbar.pill.dot", div().size(px(6.)).flex_none().bg(if connected { th.accent } else { th.muted })))
            .child(ts(&th, MONO_DATA).child(if connected { "CONNECTED" } else { "DISCONNECTED" }))
            .when(connected, |d| d.child(ts(&th, MONO_DATA).text_color(th.border).child("·")).child(ts(&th, MONO_DATA).child(SharedString::from(ping))));
        let search = div()
            .flex_1()
            .max_w(px(420.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .h(px(28.))
            .px(px(8.))
            .bg(th.low)
            .border_1()
            .border_color(if search_focused { th.focus } else { th.border })
            .text_color(th.muted)
            .child(icon("search", 14., th.muted))
            .child(div().flex_1().min_w_0().child(self.search.clone()))
            .child(pid("topbar.search.kbd", ts(&th, MONO_CODE).px(px(4.)).py(px(1.)).bg(th.high).flex_none().text_color(th.muted).child("CTRL K")));
        let collection = ts(&th, MONO_CODE)
            .ml_auto()
            .flex()
            .flex_row()
            .gap(px(12.))
            .flex_none()
            .text_color(th.muted)
            .child(stat(count(self.counts.tracks), "TRK"))
            .when(vw >= 1280.0, |d| d.child(stat(count(self.counts.albums), "ALB")).child(stat(count(self.counts.artists), "ART")));
        let theme_btn = tip(
            ghost_icon("theme", th, if th.light { "moon" } else { "sun" }, 16., 28., false, false).border_1().border_color(th.border),
            if th.light { "Dark mode" } else { "Light mode" },
            th,
        )
        .on_click(cx.listener(|this, _, _, cx| this.toggle_theme(cx)));
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .h(px(44.))
            .w_full()
            .flex_none()
            .px(px(12.))
            .bg(th.base)
            .border_b_1()
            .border_color(th.frame)
            .child(pid("topbar.nav", nav))
            .child(pid("topbar.pill", pill))
            .child(pid("topbar.search", search))
            .child(pid("topbar.collection", collection))
            .child(pid("topbar.theme", theme_btn))
    }

    fn nav_row(&self, id: impl Into<gpui::ElementId>, th: Theme, active: bool, window: &mut Window, icon_name: &str, label: &str) -> Stateful<Div> {
        let fg = if active { th.accent_text } else { th.muted };
        div()
            .id(id)
            .group("hv")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .w_full()
            .h(px(30.))
            .px(px(12.))
            .cursor(CursorStyle::PointingHand)
            .text_color(fg)
            .when(active, |d| d.bg(th.accent))
            .when(!active, |d| d.hover(move |s| s.bg(th.mid).text_color(th.text)))
            .child(icon_g(icon_name, 14., fg, if active { fg } else { th.text }))
            .child(div().flex_1().min_w_0().child(tracked(label, LABEL_CAPS, th.body_font, if active { th.accent_text } else { th.muted }, window)))
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
        let mut library = div().flex().flex_col().pt(px(4.)).pb(px(8.));
        library = library.child(pid("sidebar.library.title", div().px(px(12.)).pt(px(12.)).pb(px(4.)).child(tracked("Library", LABEL_CAPS, th.body_font, th.muted, window))));
        for (nav_ix, (r, ic, label)) in nav_items.into_iter().enumerate() {
            let active = *route == r;
            let target = r.clone();
            library = library.child(pid(
                format!("sidebar.nav.{nav_ix}"),
                self.nav_row(SharedString::from(format!("nav-{label}")), th, active, window, ic, label).on_click(cx.listener(move |this, _, _, cx| this.navigate(target.clone(), cx))),
            ));
        }

        let pl_row = |id: SharedString, active: bool, icon_name: &'static str, name: String, count: String| {
            div()
                .id(id)
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .w_full()
                .h(px(30.))
                .px(px(12.))
                .cursor(CursorStyle::PointingHand)
                .text_color(if active { th.accent_text } else { th.muted })
                .when(active, |d| d.bg(th.accent).font_weight(FontWeight::BOLD))
                .when(!active, |d| d.hover(move |s| s.bg(th.mid).text_color(th.text)))
                .font_family(MONO)
                .text_size(px(12.))
                .line_height(px(16.))
                .child(icon_g(icon_name, 14., if active { th.accent_text } else { th.muted }, if active { th.accent_text } else { th.text }))
                .child(ellipsis(div().flex_1()).child(SharedString::from(name)))
                .child(div().flex_none().text_size(px(11.)).child(SharedString::from(count)))
        };
        let mut lists = div().id("playlist-scroll").flex_1().min_h_0().overflow_y_scroll().border_t_1().border_color(th.border).flex().flex_col();
        lists = lists.child(pid("sidebar.playlists.title",
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px(px(12.))
                .pt(px(12.))
                .pb(px(4.))
                .child(tracked("Playlists", LABEL_CAPS, th.body_font, if *route == Route::Playlists { th.accent_fg } else { th.muted }, window))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(8.))
                        .text_color(th.muted)
                        .child(icon("bolt", 12., th.muted))
                        .child(icon("add", 12., th.muted)),
                ),
        ));
        lists = lists.child(pid(
            "sidebar.pl.liked",
            pl_row("pl-liked".into(), *route == Route::Liked, "heart-filled", "Liked Songs".into(), "2".into()).on_click(cx.listener(|this, _, _, cx| this.navigate(Route::Liked, cx))),
        ));
        lists = lists.child(div().h(px(1.)).mx(px(12.)).my(px(4.)).bg(th.border));
        for p in &self.playlists {
            let active = *route == Route::Playlist(p.id.clone());
            let id = p.id.clone();
            lists = lists.child(
                pl_row(SharedString::from(format!("pl-{}", p.id)), active, if p.read_only { "lock" } else { "playlist" }, p.name.clone(), p.song_count.to_string())
                    .on_click(cx.listener(move |this, _, _, cx| this.navigate(Route::Playlist(id.clone()), cx))),
            );
        }

        let footer = div()
            .flex_none()
            .border_t_1()
            .border_color(th.frame)
            .py(px(4.))
            .child(pid("sidebar.settings", self.nav_row("nav-settings", th, *route == Route::Settings, window, "settings", "Settings").on_click(cx.listener(|this, _, _, cx| this.navigate(Route::Settings, cx)))))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .h(px(30.))
                    .px(px(12.))
                    .text_color(th.muted)
                    .child(icon("user", 14., th.muted))
                    .child(ts(&th, MONO_DATA).child(SharedString::from(self.status.as_ref().and_then(|s| s.username.clone()).unwrap_or_else(|| "—".into())))),
            );

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
            .child(lists)
            .child(footer)
            .into_any_element()
    }
}

#[allow(dead_code)]
fn _unused(_: MouseButton) -> gpui::Hsla {
    transparent_black()
}
