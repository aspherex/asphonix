//! The list routes: Playlists, PlaylistDetail, Liked, SmartPlaylistDetail, History (and, in sibling
//! files, Search, Settings, Login, Favorites, the smart-playlist editor). They are plain entities
//! owned by [`ListsHost`] (created on first visit, so startup stays as lean as before); the shell only
//! asks [`Shell::lists_stage`] for the element of a route.
pub mod history;
pub mod menus;
pub mod modal;
pub mod playlists;
pub mod smart_rules;
pub mod table;
pub mod track_page;
pub mod ui;

use super::shell::Shell;
use crate::app::{self, Services};
use crate::router::Route;
use crate::theme::Theme;
use gpui::{AnyElement, BoxShadow, Context, Entity, IntoElement, Subscription, Window, actions, point, prelude::*, px};
use sonix_api::SmartSummary;
use track_page::PageKind;

actions!(sonix_lists, [SelectAllRows]);

/// What a list page asks the shell for.
#[derive(Clone)]
pub enum ListsEvent {
    Navigate(Route),
    /// a playlist was created / renamed / deleted / changed: the sidebar and the grids re-read
    PlaylistsChanged,
    SmartChanged,
    /// open the smart-playlist editor (`None` = a new one)
    EditSmart(Option<Box<SmartSummary>>),
}

/// `--shadow-hot*`: accent in dark, ink in light.
pub fn hot_shadow(th: &Theme, off: f32) -> Vec<BoxShadow> {
    vec![BoxShadow { color: if th.light { th.shadow } else { th.accent }, offset: point(px(off), px(off)), blur_radius: px(0.), spread_radius: px(0.) }]
}

#[derive(Default)]
pub struct ListsHost {
    pub modal: Option<Entity<modal::Modal>>,
    track_page: Option<Entity<track_page::TrackPage>>,
    history: Option<Entity<history::HistoryView>>,
    playlists: Option<Entity<playlists::PlaylistsView>>,
    subs: Vec<Subscription>,
}

impl ListsHost {
    pub fn handles(r: &Route) -> bool {
        matches!(r, Route::Playlists | Route::Playlist(_) | Route::Liked | Route::Smart(_) | Route::History)
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
    /// Shell startup: the (stand-in) dialog host lives for the whole run.
    pub(crate) fn lists_init(&mut self, cx: &mut Context<Self>) {
        let th = self.th;
        let m = cx.new(|cx| modal::Modal::new(&th, cx));
        cx.set_global(modal::ModalHandle(m.clone()));
        self.lists.modal = Some(m);
    }

    pub(crate) fn lists_modal_overlay(&self) -> Option<AnyElement> {
        self.lists.modal.as_ref().map(|m| m.clone().into_any_element())
    }

    pub(crate) fn lists_modal_open(&self, cx: &gpui::App) -> bool {
        self.lists.modal.as_ref().map(|m| m.read(cx).is_open()).unwrap_or(false)
    }

    pub(crate) fn lists_modal_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(m) = self.lists.modal.clone() {
            m.update(cx, |m, cx| m.cancel(window, cx));
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
                self.lists.track_page.clone().unwrap().into_any_element()
            }
            Route::History => {
                if self.lists.history.is_none() {
                    let v = cx.new(|cx| history::HistoryView::new(backend, &player, &menu, cx));
                    self.lists.subs.push(cx.subscribe(&v, |sh: &mut Shell, _, ev: &ListsEvent, cx| sh.lists_event(ev, cx)));
                    self.lists.history = Some(v);
                }
                self.lists.history.clone().unwrap().into_any_element()
            }
            _ => {
                if self.lists.playlists.is_none() {
                    let art = Self::lists_art(cx);
                    let v = cx.new(|cx| playlists::PlaylistsView::new(backend, art, &menu, cx));
                    self.lists.subs.push(cx.subscribe(&v, |sh: &mut Shell, _, ev: &ListsEvent, cx| sh.lists_event(ev, cx)));
                    self.lists.playlists = Some(v);
                }
                self.lists.playlists.clone().unwrap().into_any_element()
            }
        }
    }

    pub(crate) fn lists_event(&mut self, ev: &ListsEvent, cx: &mut Context<Self>) {
        match ev {
            ListsEvent::Navigate(r) => self.navigate(r.clone(), cx),
            ListsEvent::PlaylistsChanged | ListsEvent::SmartChanged => {
                self.load_static(cx);
                if let Some(p) = self.lists.playlists.clone() {
                    p.update(cx, |p, cx| p.reload(cx));
                }
            }
            ListsEvent::EditSmart(_) => {}
        }
    }

    /// `uiState.starsRev` / `libraryRev`: the visible list pages re-read.
    pub(crate) fn lists_library_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.lists.track_page.clone() {
            p.update(cx, |p, cx| p.stars_changed(cx));
        }
        if let Some(h) = self.lists.history.clone() {
            h.update(cx, |h, cx| h.reload(cx));
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

    #[cfg(feature = "parity")]
    pub(crate) fn lists_parity_state(&self, window: &Window, cx: &gpui::App) -> serde_json::Value {
        match self.router.current() {
            Route::Playlist(_) | Route::Liked | Route::Smart(_) => self.lists.track_page.as_ref().map(|p| p.read(cx).parity_state(window, cx)).unwrap_or_default(),
            Route::History => self.lists.history.as_ref().map(|p| p.read(cx).parity_state(window, cx)).unwrap_or_default(),
            Route::Playlists => self.lists.playlists.as_ref().map(|p| p.read(cx).parity_state()).unwrap_or_default(),
            _ => serde_json::Value::Null,
        }
    }
}
