//! The library routes: Home, Albums, Artists, album detail, artist detail, Genres, genre detail and the
//! components they share (cards, the virtual grid, entity links, the segmented toggle, the checkbox, the track rows
//! of the detail pages).
//!
//! Integration with the shell is deliberately thin: [`stage_for`] builds the route's view entity when the route
//! changes (a route is remounted on every navigation) and views reach back through the
//! [`NavBus`] entity (navigation, "library changed" refresh) instead of knowing the shell.
use super::shell::Shell;
use crate::app::PlayerModel;
use crate::art::ArtHandle;
use crate::router::Route;
use crate::widgets::context_menu::ContextMenu;
use gpui::{AnyView, App, AppContext, Context, Entity, EventEmitter, Global};
use asphonix_api::Backend;
use std::sync::Arc;

pub mod album_detail;
pub mod artist_detail;
pub mod cards;
pub mod entity_link;
pub mod fmtx;
pub mod genres;
pub mod grid;
pub mod home;
pub mod menus;
pub mod pages;
pub mod parts;
pub mod rows;

gpui::actions!(asphonix_library_nav, [TabNext, TabPrev]);

/// What library views tell the shell (and the shell tells them).
#[derive(Clone, Debug)]
pub enum LibEvent {
    Navigate(Route),
    /// Stars/ratings changed somewhere: views showing them re-read their data.
    Refresh,
}

/// A tiny event hub entity: entity links in rows, the dock and the queue rail navigate through it.
pub struct NavBus;
impl EventEmitter<LibEvent> for NavBus {}

pub struct NavHandle(pub Entity<NavBus>);
impl Global for NavHandle {}

/// Navigate from anywhere that has an `App`.
pub fn navigate(cx: &mut App, r: Route) {
    if let Some(h) = cx.try_global::<NavHandle>().map(|h| h.0.clone()) {
        h.update(cx, |_, cx| cx.emit(LibEvent::Navigate(r)));
    }
}

/// Everything a library view needs from the app (cheap to clone).
#[derive(Clone)]
pub struct Ctx {
    pub backend: Arc<dyn Backend>,
    pub art: ArtHandle,
    pub player: Entity<PlayerModel>,
    pub menu: Entity<ContextMenu>,
    pub nav: Entity<NavBus>,
}

/// The view entity of a library route, `None` for routes owned by other views (Tracks, Now Playing, ...).
pub fn stage_for(route: &Route, ctx: &Ctx, cx: &mut Context<Shell>) -> Option<AnyView> {
    let c = ctx.clone();
    Some(match route {
        Route::Home => cx.new(|cx| home::HomeView::new(c, cx)).into(),
        Route::Albums => cx.new(|cx| pages::AlbumsView::new(c, cx)).into(),
        Route::Artists => cx.new(|cx| pages::ArtistsView::new(c, cx)).into(),
        Route::Album(id) => {
            let id = id.clone();
            cx.new(|cx| album_detail::AlbumDetailView::new(c, id, cx)).into()
        }
        Route::Artist(id) => {
            let id = id.clone();
            cx.new(|cx| artist_detail::ArtistDetailView::new(c, id, cx)).into()
        }
        Route::Genres => cx.new(|cx| genres::GenresView::new(c, cx)).into(),
        Route::Genre(name) => {
            let name = name.clone();
            cx.new(|cx| pages::GenreDetailView::new(c, name, cx)).into()
        }
        _ => return None,
    })
}
