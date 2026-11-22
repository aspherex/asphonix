//! The list routes: Playlists, playlist detail, Liked, smart playlist detail, History (and, in sibling
//! files, Search, Settings, Login, Favorites, the smart-playlist editor). They are plain entities
//! owned by [`ListsHost`] (created on first visit, so startup stays lean); the shell only
//! asks [`Shell::lists_stage`] for the element of a route.
pub mod favorites;
pub mod history;
pub mod login;
pub mod menus;
pub mod playlists;
pub mod search;
pub mod settings;
pub mod smart_editor;
pub mod smart_rules;
pub mod table;
pub mod track_page;
pub mod ui;

use super::shell::Shell;
use crate::app::{self, Services};
use crate::router::Route;
use crate::theme::Theme;
use gpui::{AnyElement, BoxShadow, Context, Entity, IntoElement, Subscription, Window, point, prelude::*, px};
use sonix_api::SmartSummary;
use sonix_i18n::tr;
use track_page::PageKind;

pub use crate::views::shell::SelectAll as SelectAllRows;

/// What a list page asks the shell for.
#[derive(Clone)]
pub enum ListsEvent {
    Navigate(Route),
    /// a playlist was created / renamed / deleted / changed: the sidebar and the grids re-read
    PlaylistsChanged,
    SmartChanged,
    /// open the smart-playlist editor (`None` = a new one)
    EditSmart(Option<Box<SmartSummary>>),
    /// history back, else Home (`Login` "Cancel")
    Back,
    /// Settings > "Show track preview" / its corner changed (the shell owns the track-change card)
    TrackOverlay(bool, sonix_api::OverlayCorner),
    /// Settings > Language changed (`None` = follow the OS)
    Language(Option<sonix_i18n::Lang>),
}

/// `--shadow-hot*`: accent in dark, ink in light.
pub fn hot_shadow(th: &Theme, off: f32) -> Vec<BoxShadow> {
    vec![BoxShadow { color: if th.light { th.shadow } else { th.accent }, offset: point(px(off), px(off)), blur_radius: px(0.), spread_radius: px(0.) }]
}

/// Entity that menus (which can be opened from the shell's own chrome) use as their event target.
pub struct Bridge;
impl gpui::EventEmitter<ListsEvent> for Bridge {}

/// The bridge, reachable from other routes (Home's playlist tiles use the same playlist menu).
pub struct BridgeHandle(pub gpui::WeakEntity<Bridge>);
impl gpui::Global for BridgeHandle {}

/// The playlist menu for a playlist shown outside the list routes (rename / duplicate / delete included).
pub fn playlist_menu(p: &sonix_api::Playlist, cx: &gpui::App) -> Vec<crate::widgets::context_menu::MenuItem> {
    match cx.try_global::<BridgeHandle>() {
        Some(h) => menus::playlist_menu_items(h.0.clone(), p, false),
        None => Vec::new(),
    }
}

#[derive(Default)]
pub struct ListsHost {
    pub smart_side: Vec<SmartSummary>,
    bridge: Option<Entity<Bridge>>,
    editor: Option<Entity<smart_editor::SmartEditor>>,
    track_page: Option<Entity<track_page::TrackPage>>,
    history: Option<Entity<history::HistoryView>>,
    playlists: Option<Entity<playlists::PlaylistsView>>,
    search: Option<Entity<search::SearchView>>,
    settings: Option<Entity<settings::SettingsView>>,
    pub(crate) favorites: Option<Entity<favorites::FavoritesView>>,
    pub(crate) login: Option<Entity<login::LoginView>>,
    subs: Vec<Subscription>,
}

impl ListsHost {
    pub fn handles(r: &Route) -> bool {
        matches!(r, Route::Playlists | Route::Playlist(_) | Route::Liked | Route::Smart(_) | Route::History | Route::Search | Route::Settings | Route::Login | Route::Favorites)
    }
}

fn kind_of(r: &Route) -> Option<PageKind> {
    match r {
        Route::Playlist(id) => Some(PageKind::Playlist(id.clone())),
        Route::Liked => Some(PageKind::Liked),
        Route::Smart(id) => Some(PageKind::Smart(id.clone())),
        _ => None,
    }
}

impl Shell {
    /// Shell startup: the smart editor host and the event bridge live for the whole run (dialogs are `crate::overlays`).
    pub(crate) fn lists_init(&mut self, cx: &mut Context<Self>) {
        let br = cx.new(|_| Bridge);
        cx.set_global(BridgeHandle(br.downgrade()));
        self.lists.subs.push(cx.subscribe(&br, |sh: &mut Shell, _, ev: &ListsEvent, cx| sh.lists_event(ev, cx)));
        self.lists.bridge = Some(br);
        let (b, menu) = (self.backend.clone(), self.menu.clone());
        let ed = cx.new(|cx| smart_editor::SmartEditor::new(b, &menu, cx));
        self.lists.subs.push(cx.subscribe(&ed, |sh: &mut Shell, _, ev: &ListsEvent, cx| sh.lists_event(ev, cx)));
        self.lists.editor = Some(ed);
        // the persisted track-change card setting (`settings_get` off the UI thread)
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let s = cx.background_executor().spawn(async move { b.settings_get() }).await;
            if let Ok(s) = s {
                let _ = this.update(cx, |sh, _| sh.apply_track_overlay(s.track_overlay, s.track_overlay_corner));
            }
        })
        .detach();
    }

    pub(crate) fn lists_editor_overlay(&self) -> Option<AnyElement> {
        self.lists.editor.as_ref().map(|m| m.clone().into_any_element())
    }

    pub(crate) fn lists_editor_open(&self, cx: &gpui::App) -> bool {
        self.lists.editor.as_ref().map(|m| m.read(cx).is_open()).unwrap_or(false)
    }

    pub(crate) fn lists_editor_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(m) = self.lists.editor.clone() {
            m.update(cx, |m, cx| m.close(window, cx));
        }
    }

    fn lists_art(cx: &Context<Self>) -> crate::art::ArtHandle {
        cx.global::<Services>().art.clone()
    }

    /// The stage element of a list route (the entity is created on first visit).
    pub(crate) fn lists_stage(&mut self, r: &Route, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (backend, player, menu) = (self.backend.clone(), self.player.clone(), self.menu.clone());
        match r {
            Route::Playlist(_) | Route::Liked | Route::Smart(_) => {
                let kind = kind_of(r).unwrap();
                if self.lists.track_page.as_ref().map(|p| p.read(cx).kind != kind).unwrap_or(true) {
                    let art = Self::lists_art(cx);
                    let page = cx.new(|cx| track_page::TrackPage::new(kind, backend, art, &player, &menu, cx));
                    self.lists.subs.push(cx.subscribe(&page, |sh: &mut Shell, _, ev: &ListsEvent, cx| sh.lists_event(ev, cx)));
                    self.lists.track_page = Some(page);
                }
                Self::stage_view(self.lists.track_page.clone().unwrap())
            }
            Route::History => {
                if self.lists.history.is_none() {
                    let v = cx.new(|cx| history::HistoryView::new(backend, &player, &menu, cx));
                    self.lists.subs.push(cx.subscribe(&v, |sh: &mut Shell, _, ev: &ListsEvent, cx| sh.lists_event(ev, cx)));
                    self.lists.history = Some(v);
                }
                Self::stage_view(self.lists.history.clone().unwrap())
            }
            Route::Favorites => {
                if self.lists.favorites.is_none() {
                    let lib = self.lib_ctx.clone();
                    let v = cx.new(|cx| favorites::FavoritesView::new(lib, cx));
                    self.lists.subs.push(cx.subscribe(&v, |sh: &mut Shell, _, ev: &ListsEvent, cx| sh.lists_event(ev, cx)));
                    self.lists.favorites = Some(v);
                }
                Self::stage_view(self.lists.favorites.clone().unwrap())
            }
            Route::Login => {
                // a fresh form on every visit (dropped again in `route_changed`)
                if self.lists.login.is_none() {
                    let v = cx.new(|cx| login::LoginView::new(backend, cx));
                    self.lists.subs.push(cx.subscribe(&v, |sh: &mut Shell, _, ev: &ListsEvent, cx| sh.lists_event(ev, cx)));
                    self.lists.login = Some(v);
                }
                Self::stage_view(self.lists.login.clone().unwrap())
            }
            Route::Settings => {
                if self.lists.settings.is_none() {
                    let v = cx.new(|cx| settings::SettingsView::new(backend, &menu, cx));
                    self.lists.subs.push(cx.subscribe(&v, |sh: &mut Shell, _, ev: &ListsEvent, cx| sh.lists_event(ev, cx)));
                    self.lists.settings = Some(v);
                }
                Self::stage_view(self.lists.settings.clone().unwrap())
            }
            Route::Search => {
                if self.lists.search.is_none() {
                    let art = Self::lists_art(cx);
                    let input = self.search.clone();
                    let v = cx.new(|cx| search::SearchView::new(backend, art, &menu, &input, cx));
                    self.lists.subs.push(cx.subscribe(&v, |sh: &mut Shell, _, ev: &ListsEvent, cx| sh.lists_event(ev, cx)));
                    self.lists.search = Some(v);
                }
                Self::stage_view(self.lists.search.clone().unwrap())
            }
            _ => {
                if self.lists.playlists.is_none() {
                    let art = Self::lists_art(cx);
                    let v = cx.new(|cx| playlists::PlaylistsView::new(backend, art, &menu, &player, cx));
                    self.lists.subs.push(cx.subscribe(&v, |sh: &mut Shell, _, ev: &ListsEvent, cx| sh.lists_event(ev, cx)));
                    self.lists.playlists = Some(v);
                }
                Self::stage_view(self.lists.playlists.clone().unwrap())
            }
        }
    }

    /// Window-level keys of the Search route: Up/Down move, Shift+Enter enqueues.
    pub(crate) fn lists_search_key(&mut self, ev: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        if *self.router.current() != Route::Search {
            return;
        }
        let Some(s) = self.lists.search.clone() else { return };
        let k = ev.keystroke.key.as_str();
        let m = ev.keystroke.modifiers;
        if m.control || m.alt || m.platform {
            return;
        }
        match k {
            "down" | "up" => {
                s.update(cx, |s, cx| {
                    s.arrow(k == "down", cx);
                });
            }
            "enter" if m.shift => s.update(cx, |s, cx| s.enter(true, cx)),
            _ => {}
        }
    }

    /// Escape on the Search route clears the query; returns whether it did.
    pub(crate) fn lists_search_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if *self.router.current() != Route::Search {
            return false;
        }
        match self.lists.search.clone() {
            Some(s) => {
                s.update(cx, |s, cx| s.escape(cx));
                true
            }
            None => false,
        }
    }

    fn lists_show_menu(&self, items: Vec<crate::widgets::context_menu::MenuItem>, pos: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.menu.update(cx, |m, cx| m.show(items, pos, window, cx));
    }

    pub(crate) fn lists_liked_menu(&mut self, pos: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.counts.liked.unwrap_or(0) as usize;
        self.lists_show_menu(menus::liked_menu_items(n), pos, window, cx);
        cx.stop_propagation();
    }

    pub(crate) fn lists_smart_menu(&mut self, sp: &SmartSummary, pos: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(br) = self.lists.bridge.clone() else { return };
        let on_it = *self.router.current() == Route::Smart(sp.id.clone());
        self.lists_show_menu(menus::smart_menu_items(br.downgrade(), sp, 0, on_it), pos, window, cx);
        cx.stop_propagation();
    }

    pub(crate) fn lists_playlist_menu(&mut self, p: &sonix_api::Playlist, pos: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(br) = self.lists.bridge.clone() else { return };
        self.lists_show_menu(menus::playlist_menu_items(br.downgrade(), p, true), pos, window, cx);
        cx.stop_propagation();
    }

    /// "+" in the sidebar: prompt for a name, create, open it.
    pub(crate) fn lists_new_playlist(&mut self, cx: &mut Context<Self>) {
        let Some(br) = self.lists.bridge.clone() else { return };
        let br = br.downgrade();
        crate::overlays::prompt(cx, tr!(DialogNewPlaylist), tr!(DialogPlaylistName), tr!(DialogCreate), "", move |name, cx| {
            let b = app::backend(cx);
            cx.spawn(async move |cx| {
                let r = cx.background_executor().spawn(async move { b.playlists_create(sonix_api::CreatePlaylistRequest { name, track_ids: vec![] }) }).await;
                if let (Some(t), Ok(p)) = (br.upgrade(), r) {
                    let _ = t.update(cx, |_, cx| {
                        cx.emit(ListsEvent::PlaylistsChanged);
                        cx.emit(ListsEvent::Navigate(Route::Playlist(p.id)));
                    });
                }
            })
            .detach();
        });
    }

    /// The bolt in the sidebar / Playlists header: opens the smart-playlist editor for a new list.
    pub(crate) fn lists_new_smart(&mut self, cx: &mut Context<Self>) {
        self.lists_event(&ListsEvent::EditSmart(None), cx);
    }

    /// ProfileSwitcher: the account menu (switch / remove / add / sign out). `profiles.list()` runs off the UI thread
    /// (it reads the profile store), the menu opens when it answers.
    pub(crate) fn lists_profile_menu(&mut self, pos: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let b = self.backend.clone();
        cx.spawn_in(window, async move |this, cx| {
            let list = cx.background_executor().spawn(async move { b.profiles_list().unwrap_or_default() }).await;
            let _ = this.update_in(cx, |sh, window, cx| sh.lists_profile_menu_show(list, pos, window, cx));
        })
        .detach();
    }

    fn lists_profile_menu_show(&mut self, list: Vec<sonix_api::Profile>, pos: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        use crate::widgets::context_menu::MenuItem;
        let Some(br) = self.lists.bridge.clone() else { return };
        let host = |u: &str| u.split("://").nth(1).unwrap_or(u).split('/').next().unwrap_or(u).to_string();
        let mut items: Vec<MenuItem> = list
            .iter()
            .map(|p| {
                let (id, br2) = (p.id.clone(), br.downgrade());
                let label = format!("{} @ {}", p.username, host(&p.url));
                MenuItem::action(label.clone(), move |_, cx| {
                    let (id, br2, label) = (id.clone(), br2.clone(), label.clone());
                    let b = app::backend(cx);
                    cx.spawn(async move |cx| {
                        let r = cx.background_executor().spawn(async move { b.profiles_switch(&id) }).await;
                        let ok = r.is_ok();
                        if let (Some(t), true) = (br2.upgrade(), ok) {
                            let _ = t.update(cx, |_, cx| {
                                cx.emit(ListsEvent::PlaylistsChanged);
                                cx.emit(ListsEvent::Navigate(Route::Home));
                            });
                        }
                        // toast: the new profile or the error
                        let msg = match r {
                            Ok(_) => tr!(ProfileSwitched, name = label.as_str()),
                            Err(e) => tr!(ProfileSwitchFailed, reason = crate::i18n::api_error(&e)),
                        };
                        let _ = cx.update(|cx| crate::overlays::show_toast(cx, &msg));
                    })
                    .detach();
                })
                .icon(if p.active { "check" } else { "user" })
                .disabled(p.active)
            })
            .collect();
        let others: Vec<&sonix_api::Profile> = list.iter().filter(|p| !p.active).collect();
        if !others.is_empty() {
            let subs: Vec<MenuItem> = others
                .iter()
                .map(|p| {
                    let id = p.id.clone();
                    MenuItem::action(format!("{} @ {}", p.username, host(&p.url)), move |_, cx| {
                        let id = id.clone();
                        let b = app::backend(cx);
                        cx.spawn(async move |cx| {
                            let r = cx.background_executor().spawn(async move { b.profiles_remove(&id) }).await;
                            if r.is_ok() {
                                let _ = cx.update(|cx| crate::overlays::show_toast(cx, tr!(ProfileRemoved)));
                            }
                        })
                        .detach();
                    })
                    .danger()
                })
                .collect();
            items.push(MenuItem::submenu(tr!(ProfileRemoveSaved), subs).icon("trash"));
        }
        items.push(MenuItem::separator());
        let br_add = br.downgrade();
        items.push(
            MenuItem::action(tr!(ProfileAdd), move |_, cx| {
                if let Some(t) = br_add.upgrade() {
                    t.update(cx, |_, cx| cx.emit(ListsEvent::Navigate(Route::Login)));
                }
            })
            .icon("add"),
        );
        let br_out = br.downgrade();
        items.push(
            MenuItem::action(tr!(ProfileSignOut), move |_, cx| {
                let br_out = br_out.clone();
                let b = app::backend(cx);
                cx.spawn(async move |cx| {
                    cx.background_executor().spawn(async move { let _ = b.auth_logout(); }).await;
                    if let Some(t) = br_out.upgrade() {
                        let _ = t.update(cx, |_, cx| cx.emit(ListsEvent::Navigate(Route::Login)));
                    }
                })
                .detach();
            })
            .icon("logout")
            .danger(),
        );
        // the profile menu opens at the left edge of the window, just below the button (4 px)
        let _ = pos;
        let at = gpui::point(gpui::px(0.), window.viewport_size().height - gpui::px(crate::metrics::layout::DOCK_H) + gpui::px(4.));
        self.lists_show_menu(items, at, window, cx);
    }

    pub(crate) fn lists_event(&mut self, ev: &ListsEvent, cx: &mut Context<Self>) {
        match ev {
            ListsEvent::Navigate(r) => self.navigate(r.clone(), cx),
            ListsEvent::TrackOverlay(on, corner) => self.apply_track_overlay(*on, *corner),
            ListsEvent::Language(l) => self.set_language(*l, cx),
            ListsEvent::PlaylistsChanged | ListsEvent::SmartChanged => {
                self.load_static(cx);
                if matches!(ev, ListsEvent::SmartChanged)
                    && let Some(p) = self.lists.track_page.clone()
                {
                    p.update(cx, |p, cx| {
                        if matches!(p.kind, track_page::PageKind::Smart(_)) {
                            p.reload(cx)
                        }
                    });
                }
                if let Some(p) = self.lists.playlists.clone() {
                    p.update(cx, |p, cx| p.reload(cx));
                }
            }
            ListsEvent::EditSmart(sp) => {
                let Some(ed) = self.lists.editor.clone() else { return };
                let sp = sp.clone();
                cx.defer(move |cx| {
                    let Some(w) = cx.active_window() else { return };
                    let _ = w.update(cx, |_, window, cx| ed.update(cx, |e, cx| e.show(sp.map(|b| *b), window, cx)));
                });
            }
            ListsEvent::Back => {
                if self.router.can_back() {
                    self.go_back(cx);
                } else {
                    self.navigate(Route::Home, cx);
                }
            }
        }
    }

    /// Stars or the library changed: the visible list pages re-read.
    pub(crate) fn lists_library_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.lists.track_page.clone() {
            p.update(cx, |p, cx| p.stars_changed(cx));
        }
        if let Some(h) = self.lists.history.clone() {
            h.update(cx, |h, cx| h.refresh(cx));
        }
        self.load_static(cx);
    }

    pub(crate) fn lists_release(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.lists.track_page.clone() {
            p.update(cx, |p, cx| p.release(cx));
        }
        if let Some(h) = self.lists.history.clone() {
            h.update(cx, |h, cx| h.release(cx));
        }
    }

    pub(crate) fn lists_restore(&mut self, cx: &mut Context<Self>) {
        let _ = app::backend(cx);
        if let Some(p) = self.lists.track_page.clone() {
            p.update(cx, |_, cx| cx.notify());
        }
        if let Some(h) = self.lists.history.clone() {
            h.update(cx, |_, cx| cx.notify());
        }
    }

}
