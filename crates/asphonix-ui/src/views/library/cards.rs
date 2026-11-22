//! Album / artist grid cards (with hover play + "..." overlays) and the compact
//! tile of Home (Jump Back In, Recently Played Tracks, Playlists // Vectors).
use super::entity_link::{Kind, entity_link};
use super::parts::art_slot;
use super::{Ctx, menus, navigate};
use crate::anim;
use crate::metrics::library as m;
use crate::router::Route;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_LG, BODY_MD, LABEL_CAPS, MONO_DATA};
use crate::widgets::{hard_shadow, icon};
use gpui::{Point, 
    AnyElement, App, Bounds, ClickEvent, Context, CursorStyle, Div, IntoElement, MouseButton, MouseDownEvent, Pixels, SharedString, Stateful, Window, canvas, div, point, prelude::*, px,
};
use asphonix_api::*;
use std::cell::Cell;
use std::rc::Rc;

gpui::actions!(asphonix_library_card, [CardOpen, CardMenu]);

thread_local! {
    static FOCUS: std::cell::RefCell<std::collections::HashMap<usize, gpui::FocusHandle>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The persistent focus handle of a card: one per card id, bounded so scrolling a 5k-card grid
/// does not accumulate handles.
pub(super) fn focus_handle<V: 'static>(cx: &mut Context<V>, key: usize) -> gpui::FocusHandle {
    FOCUS.with(|m| {
        let mut m = m.borrow_mut();
        if m.len() > 256 && !m.contains_key(&key) {
            m.clear();
        }
        m.entry(key).or_insert_with(|| cx.focus_handle()).clone()
    })
}

pub enum CardItem<'a> {
    Album(&'a Album),
    Artist(&'a Artist),
}

pub(super) fn key_of(prefix: &str, id: &str) -> usize {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (prefix, id).hash(&mut h);
    h.finish() as usize
}

/// The keyboard focus ring: a 2 px rgb(16, 16, 16) ring just outside the element's border box. Add as a child of a
/// relative element. An ancestor that clips (the virtual grid's cells) hides it.
pub fn focus_ring() -> gpui::Div {
    div().absolute().top(px(-2.)).left(px(-2.)).right(px(-2.)).bottom(px(-2.)).border_2().border_color(gpui::rgb(0x101010))
}

/// Records the border box of the element it is placed in (for menus anchored under a button).
pub fn anchor_cell() -> Rc<Cell<Bounds<Pixels>>> {
    Rc::new(Cell::new(Bounds::default()))
}
fn track_bounds(cell: &Rc<Cell<Bounds<Pixels>>>) -> AnyElement {
    let cell = cell.clone();
    canvas(move |b, _, _| cell.set(b), |_, _, _, _| {}).absolute().top_0().left_0().size_full().into_any_element()
}

/// Opens a menu left-aligned, 4 px under the element.
fn open_menu_under(ctx: &Ctx, anchor: &Rc<Cell<Bounds<Pixels>>>, items: impl FnOnce(Point<Pixels>) -> Vec<crate::widgets::context_menu::MenuItem>, window: &mut Window, cx: &mut App) {
    let b = anchor.get();
    let pos = point(b.origin.x, b.origin.y + b.size.height + px(4.));
    let items = items(pos);
    ctx.menu.update(cx, |m, cx| m.show(items, pos, window, cx));
}

pub fn open_menu_at(ctx: &Ctx, items: Vec<crate::widgets::context_menu::MenuItem>, pos: gpui::Point<Pixels>, window: &mut Window, cx: &mut App) {
    ctx.menu.update(cx, |m, cx| m.show(items, pos, window, cx));
}

/// One grid card. `width` is the cell width; `height` is `Some(cell height)` inside the `VirtualGrid` (cells clip),
/// `None` for the natural height (Home "Heavy Rotation", the artist page's grids).
pub fn card<V: 'static>(
    ctx: &Ctx,
    th: &Theme,
    item: CardItem<'_>,
    width: f32,
    height: Option<f32>,
    window: &mut Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let th = *th;
    let (id, cover, title, route, is_album) = match &item {
        CardItem::Album(a) => (a.id.clone(), a.cover_art.clone(), a.name.clone(), Route::Album(a.id.clone()), true),
        CardItem::Artist(a) => (a.id.clone(), a.cover_art.clone(), a.name.clone(), Route::Artist(a.id.clone()), false),
    };
    let starred = match &item {
        CardItem::Album(a) => a.starred.is_some(),
        CardItem::Artist(a) => a.starred.is_some(),
    };
    let key = key_of("card", &id);
    let fh = focus_handle(cx, key);
    let focused = fh.contains_focused(window, cx);
    // hover or focus inside the card shows the overlays
    let hovered = anim::hovered(key) || focused;
    let t = anim::transition(window, ("card-ov", key), if hovered { 1.0 } else { 0.0 }, m::CARD_TRANSITION);
    let items_for = {
        let owned: Result<Album, Artist> = match &item {
            CardItem::Album(a) => Ok((*a).clone()),
            CardItem::Artist(a) => Err((*a).clone()),
        };
        move |at: Point<Pixels>| match &owned {
            Ok(a) => menus::album_menu(a, at),
            Err(a) => menus::artist_menu(a),
        }
    };

    // --- art slot with the hover overlays
    let mut art = art_slot(&th, &ctx.art, &ctx.backend, cover.as_deref(), width, true, 160, if is_album { "album" } else { "artist" }, 22.0, window, cx);
    if starred {
        art = art.child(div().absolute().top(px(m::CARD_STAR_INSET)).right(px(m::CARD_STAR_INSET)).child(icon("star-filled", m::CARD_STAR_ICON, th.accent_fg)));
    }
    // the play overlay of the source that is loaded shows the active state (outline, pause icon, always visible)
    let target = if let CardItem::Album(a) = &item { crate::app::PlayTarget::Album { id: id.clone(), tracks: a.song_count } } else { crate::app::PlayTarget::Artist(id.clone()) };
    let pst = ctx.player.read(cx).play_state(&target);
    let t_play = if pst.active() { 1.0 } else { t };
    let sh = m::CARD_OVERLAY_SHIFT * (1.0 - t_play);
    let play_id = id.clone();
    art = art.child(
        div()
            .id(("card-play", key))
            .absolute()
            .right(px(m::CARD_INSET))
            .bottom(px(m::CARD_INSET - sh))
            .size(px(m::CARD_PLAY))
            .flex()
            .items_center()
            .justify_center()
            .bg(if pst.active() { th.mid } else { th.accent })
            .when(pst.active(), |d| d.border_1().border_color(th.accent))
            .shadow(hard_shadow(&th, 2.0))
            .opacity(t_play)
            .cursor(CursorStyle::PointingHand)
            .child(icon(pst.icon(), m::CARD_PLAY_ICON, if pst.active() { th.accent_fg } else { th.accent_text }))
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                if pst.active() {
                    crate::playback::toggle(cx);
                } else if is_album {
                    menus::play_album(cx, play_id.clone(), false);
                } else {
                    menus::play_artist(cx, play_id.clone(), false);
                }
            }),
    );
    let anchor = anchor_cell();
    let menu_ctx = ctx.clone();
    let menu_items = items_for.clone();
    let anchor2 = anchor.clone();
    art = art.child(
        div()
            .id(("card-menu", key))
            .group("cm")
            .absolute()
            .left(px(m::CARD_INSET))
            .bottom(px(m::CARD_INSET - sh))
            .size(px(m::CARD_MENU))
            .flex()
            .items_center()
            .justify_center()
            .bg(th.mid)
            .border_1()
            .border_color(th.border)
            .text_color(th.text)
            .opacity(t)
            .cursor(CursorStyle::PointingHand)
            .hover(move |s| s.border_color(th.focus).text_color(th.accent_fg))
            .child(track_bounds(&anchor))
            .child(crate::widgets::icon_g("more", m::CARD_MENU_ICON, th.text, th.accent_fg))
            .on_click(move |_: &ClickEvent, window, cx| {
                cx.stop_propagation();
                open_menu_under(&menu_ctx, &anchor2, menu_items.clone(), window, cx);
            }),
    );

    // --- caption
    let sub: AnyElement = match &item {
        CardItem::Album(a) => div().flex().min_w_0().child(entity_link(&th, (Kind::Artist, a.artist_id.as_deref()), &a.artist, MONO_DATA, th.muted, "card", window)).into_any_element(),
        CardItem::Artist(a) => div().flex().min_w_0().child(txt(&th, MONO_DATA, format!("{} ALBUMS", a.album_count)).color(th.muted)).into_any_element(),
    };
    let meta = div()
        .flex()
        .flex_col()
        .min_w_0()
        .child(div().flex().min_w_0().child(txt(&th, BODY_MD, title).color(th.text).ellipsis()))
        .child(sub);

    let ctx_menu = ctx.clone();
    let items_ctx = items_for;
    let (ctx_key, items_key, card_anchor) = (ctx.clone(), items_ctx.clone(), anchor_cell());
    let (route_key, anchor_key) = (route.clone(), card_anchor.clone());
    let fh_menu = fh.clone();
    // drag payload: album or artist with id and name
    let drag = match &item {
        CardItem::Album(a) => crate::dnd::DragPayload::Album { id: a.id.clone(), name: a.name.clone() },
        CardItem::Artist(a) => crate::dnd::DragPayload::Artist { id: a.id.clone(), name: a.name.clone() },
    };
    let root = div()
        .id(("card", key))
        .on_drag(crate::dnd::AsphonixDrag(drag), crate::dnd::ghost(th))
        .key_context("Card")
        .track_focus(&fh)
        .tab_stop(true)
        .on_action(move |_: &CardOpen, _, cx| navigate(cx, route_key.clone()))
        .on_action(move |_: &CardMenu, window, cx| open_menu_under(&ctx_key, &anchor_key, items_key.clone(), window, cx))
        .relative()
        .flex()
        .flex_col()
        .gap(px(m::CARD_GAP))
        .w(px(width))
        .min_w_0()
        .flex_none()
        .on_hover(move |h, window, _| {
            anim::set_hovered(key, *h);
            window.refresh();
        })
        .on_click(move |_, _, cx| navigate(cx, route.clone()))
        .on_mouse_down(MouseButton::Right, move |ev: &MouseDownEvent, window, cx| {
            cx.stop_propagation();
            window.focus(&fh_menu); // a mouse down focuses the card; the menu returns focus to it on close
            open_menu_at(&ctx_menu, items_ctx(ev.position), ev.position, window, cx);
        })
        .child(track_bounds(&card_anchor))
        .when(focused, |d| d.child(focus_ring()))
        .child(art)
        .child(meta);
    let root = root.into_any_element();
    // the fixed-height cell clips the natural-height card
    match height {
        Some(h) => div().w(px(width)).h(px(h)).flex_none().overflow_hidden().child(root).into_any_element(),
        None => root,
    }
}

// ------------------------------------------------------------------ Home tiles
/// What the 56 px art slot of a tile shows.
pub enum TileArt {
    /// An album/track cover (or the empty-state icon when the item has none).
    Cover(Option<String>, &'static str),
    /// A playlist mosaic: `Some(art::mosaic_key(playlist))` when it has tracks (the backend composes it), else the empty-state icon.
    Mosaic(Option<String>),
}

pub struct Tile<'a> {
    pub id: &'a str,
    pub art: TileArt,
    /// Optional genre tag above the title (Jump Back In).
    pub genre: Option<&'a str>,
    pub title: &'a str,
    /// `Some((kind, id, text))`: an entity link; else plain `sub_text`.
    pub link: Option<(Kind, Option<&'a str>, &'a str)>,
    pub sub_text: String,
    /// Playlist tiles: play + "..." overlays.
    pub overlays: Option<TileOverlays>,
}

/// Opens a menu under the "..." button whose box is given.
pub type MenuUnder = Rc<dyn Fn(&Rc<Cell<Bounds<Pixels>>>, &mut Window, &mut App)>;

pub struct TileOverlays {
    pub play: Rc<dyn Fn(&mut App)>,
    /// Gets the anchor (the "..." button's box) to open the menu under.
    pub menu: MenuUnder,
}

/// A Home tile: a 56 px art slot + title/sub, 8 px padding, hover border -> focus. Callers attach click handlers on the result.
pub fn tile<V: 'static>(ctx: &Ctx, th: &Theme, t: Tile<'_>, width: f32, window: &mut Window, cx: &mut Context<V>) -> Stateful<Div> {
    let th = *th;
    let key = key_of("tile", t.id);
    // keyboard focusable; focus inside shows the overlays and the focus ring marks it; callers bind `CardOpen` (Enter/Space) and, for playlists, `CardMenu`.
    let fh = focus_handle(cx, key);
    let focused = fh.contains_focused(window, cx);
    let hovered = anim::hovered(key);
    let ov = anim::transition(window, ("tile-ov", key), if hovered || focused { 1.0 } else { 0.0 }, m::CARD_TRANSITION);
    let slot = div().relative().size(px(m::PL_ART)).flex_none().overflow_hidden().flex().items_center().justify_center().bg(th.high).text_color(th.muted);
    let mut slot = match &t.art {
        TileArt::Cover(Some(id), _) => slot.child(crate::art::cover_bucket(&ctx.art, &ctx.backend, Some(id), m::PL_ART, 64, window, cx)),
        TileArt::Cover(None, ic) => slot.child(icon(ic, 18.0, th.muted)),
        TileArt::Mosaic(Some(key)) => slot.child(crate::art::cover_bucket(&ctx.art, &ctx.backend, Some(key), m::PL_ART, 64, window, cx)),
        TileArt::Mosaic(None) => slot.child(icon("queue", 18.0, th.muted)),
    };
    if let Some(o) = &t.overlays {
        let play = o.play.clone();
        // the loaded playlist's tile overlay shows the active state
        let pst = ctx.player.read(cx).play_state(&crate::app::PlayTarget::Playlist(t.id.to_string()));
        let ov = if pst.active() { 1.0 } else { ov };
        slot = slot.child(
            div()
                .id(("tile-play", key))
                .absolute()
                .right(px(m::PL_PLAY_INSET))
                .bottom(px(m::PL_PLAY_INSET - 3.0 * (1.0 - ov)))
                .size(px(m::PL_PLAY))
                .flex()
                .items_center()
                .justify_center()
                .bg(if pst.active() { th.mid } else { th.accent })
                .when(pst.active(), |d| d.border_1().border_color(th.accent))
                .shadow(hard_shadow(&th, 2.0))
                .opacity(ov)
                .cursor(CursorStyle::PointingHand)
                .child(icon(pst.icon(), 12.0, if pst.active() { th.accent_fg } else { th.accent_text }))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    if pst.active() {
                        crate::playback::toggle(cx);
                    } else {
                        play(cx);
                    }
                }),
        );
    }
    let mut meta = div().flex().flex_col().min_w_0().flex_1();
    if let Some(g) = t.genre {
        meta = meta.child(div().flex().child(txt(&th, LABEL_CAPS, SharedString::from(g.to_string())).color(th.accent_fg)));
    }
    meta = meta.child(div().flex().min_w_0().child(txt(&th, BODY_LG, t.title.to_string()).color(th.text).ellipsis()));
    meta = meta.child(match &t.link {
        Some((k, id, text)) => div().flex().min_w_0().child(entity_link(&th, (*k, *id), text, MONO_DATA, th.muted, "tile", window)).into_any_element(),
        None => div().flex().min_w_0().child(txt(&th, MONO_DATA, t.sub_text.clone()).color(th.muted).ellipsis()).into_any_element(),
    });
    let mut root = div()
        .id(("tile", key))
        .track_focus(&fh)
        .tab_stop(true)
        .key_context("Card")
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(crate::metrics::space::SM))
        .w(px(width))
        .min_w_0()
        .flex_none()
        .p(px(crate::metrics::space::SM))
        .bg(th.low)
        .border_1()
        .border_color(if hovered { th.focus } else { th.border })
        .cursor(CursorStyle::PointingHand)
        .on_hover(move |h, window, _| {
            anim::set_hovered(key, *h);
            window.refresh();
        })
        .child(slot)
        .child(meta)
        .when(focused, |d| d.child(focus_ring()));
    if let Some(o) = &t.overlays {
        let (menu, anchor) = (o.menu.clone(), anchor_cell());
        let anchor2 = anchor.clone();
        root = root.child(
            div()
                .id(("tile-menu", key))
                .absolute()
                .top(px(m::PL_MENU_INSET))
                .right(px(m::PL_MENU_INSET))
                .size(px(m::PL_MENU))
                .flex()
                .items_center()
                .justify_center()
                .bg(th.mid)
                .border_1()
                .border_color(th.border)
                .opacity(ov)
                .cursor(CursorStyle::PointingHand)
                .hover(move |s| s.border_color(th.focus).text_color(th.accent_fg))
                .child(track_bounds(&anchor))
                .child(crate::widgets::icon_g("more", 13.0, th.text, th.accent_fg))
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    menu(&anchor2, window, cx);
                }),
        );
    }
    root
}

/// Skeleton of [`card`]: the same box (art square of `width`, `CARD_GAP`, title line + sub line at their
/// real line heights), so a grid that is still loading has the geometry of the loaded one.
pub fn card_skeleton(th: &Theme, width: f32) -> AnyElement {
    use crate::widgets::skeleton as sk;
    div()
        .flex()
        .flex_col()
        .gap(px(m::CARD_GAP))
        .w(px(width))
        .flex_none()
        .child(div().size(px(width)).flex_none().bg(th.high).border_1().border_color(th.border))
        .child(div().flex().flex_col().child(sk::text_slot(th, BODY_MD.line, 12.0, 0.7)).child(sk::text_slot(th, MONO_DATA.line, 10.0, 0.4)))
        .into_any_element()
}

/// Skeleton of [`tile`]: 56 px art + two text lines, 8 px padding, 1 px frame.
pub fn tile_skeleton(th: &Theme, width: f32) -> AnyElement {
    use crate::widgets::skeleton as sk;
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(crate::metrics::space::SM))
        .w(px(width))
        .flex_none()
        .p(px(crate::metrics::space::SM))
        .bg(th.low)
        .border_1()
        .border_color(th.border)
        .child(div().size(px(m::PL_ART)).flex_none().bg(th.high))
        .child(div().flex_1().min_w_0().flex().flex_col().child(sk::text_slot(th, BODY_MD.line, 12.0, 0.7)).child(sk::text_slot(th, MONO_DATA.line, 10.0, 0.45)))
        .into_any_element()
}
