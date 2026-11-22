//! `EntityLink.svelte`: an artist/album name that opens that entity's page. Plain text when there is no id or
//! the link would lead to the page already open. Not a tab stop; click/dblclick do not propagate, so the
//! enclosing row/card neither selects, plays nor opens. Hover: colour -> stark over 0.1 s + underline
//! (`text-underline-offset: 2px`).
use crate::widgets::tooltip::Tip;
use crate::anim;
use crate::router::Route;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::TypeSpec;
use gpui::{AnyElement, CursorStyle, Hsla, IntoElement, Window, div, prelude::*, px};
use std::cell::RefCell;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Artist,
    Album,
}

thread_local! {
    static CURRENT: RefCell<Route> = const { RefCell::new(Route::Home) };
}

/// The shell tells us which route is open (`router.current` in the Svelte component).
pub fn set_current(r: &Route) {
    CURRENT.with(|c| *c.borrow_mut() = r.clone());
}

/// `linked`: `!!id && !(router.current.name === kind && router.current.params.id === id)`.
pub fn is_linked(kind: Kind, id: Option<&str>) -> bool {
    let Some(id) = id else { return false };
    CURRENT.with(|c| match (&*c.borrow(), kind) {
        (Route::Artist(cur), Kind::Artist) | (Route::Album(cur), Kind::Album) => cur != id,
        _ => true,
    })
}

fn route_for(kind: Kind, id: &str) -> Route {
    match kind {
        Kind::Artist => Route::Artist(id.to_string()),
        Kind::Album => Route::Album(id.to_string()),
    }
}

fn key_of(kind: Kind, id: &str, salt: &str) -> usize {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (kind as u8, id, salt).hash(&mut h);
    h.finish() as usize
}

/// The name as an `EntityLink` inside a slot of type `spec` / colour `base`. `salt` distinguishes several links to the
/// same entity on screen (hover state is keyed by it). Inside the web's `button` the inherited letter-spacing is reset.
pub fn entity_link(th: &Theme, (kind, id): (Kind, Option<&str>), text: &str, spec: TypeSpec, base: Hsla, salt: &str, window: &mut Window) -> AnyElement {
    let th = *th;
    if !is_linked(kind, id) {
        return div().flex().min_w_0().child(txt(&th, spec, text.to_string()).color(base).ellipsis()).into_any_element();
    }
    let id = id.unwrap_or_default().to_string();
    let key = key_of(kind, &id, salt);
    let t = anim::transition(window, ("entity-link", key), if anim::hovered(key) { 1.0 } else { 0.0 }, 0.1);
    let color = anim::mix(base, th.text, t);
    let route = route_for(kind, &id);
    let hovered = anim::hovered(key);
    div()
        .id(("el", key))
        .tip(text.to_string(), th)
        .relative()
        .flex()
        .min_w_0()
        .cursor(CursorStyle::PointingHand)
        .child(txt(&th, spec, text.to_string()).no_tracking().color(color).ellipsis())
        // underline: 1 device px, 2 px below the baseline
        .when(hovered, |d| d.child(div().absolute().left_0().right_0().top(px(spec.line * 0.5 + spec.size * 0.35 + 2.0)).h(px(0.67)).bg(color)))
        .on_hover(move |h, window, _| {
            anim::set_hovered(key, *h);
            window.refresh();
        })
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            super::navigate(cx, route.clone());
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_without_an_id_or_on_the_entity_page_itself() {
        set_current(&Route::Tracks);
        assert!(!is_linked(Kind::Artist, None));
        assert!(is_linked(Kind::Artist, Some("ar_1")));
        set_current(&Route::Artist("ar_1".into()));
        assert!(!is_linked(Kind::Artist, Some("ar_1")));
        assert!(is_linked(Kind::Artist, Some("ar_2")));
        assert!(is_linked(Kind::Album, Some("ar_1")), "an album link on an artist page stays a link");
        set_current(&Route::Album("al_3".into()));
        assert!(!is_linked(Kind::Album, Some("al_3")));
        assert!(is_linked(Kind::Artist, Some("al_3")));
    }
}
