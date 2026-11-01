//! `TrackRow.svelte` as used by the detail pages (Album: 36 px rows with Year/Added/Format/Time + star, add, "..."; the
//! artist page's "Popular" list: 34 px rows without the artist column and without "add"). Header and rows share the same
//! flex columns, and the Year/Added columns drop out by the `track-table` container query, like the Tracks route.
use super::cards::anchor_cell;
use super::entity_link::{Kind, entity_link};
use crate::anim;
use crate::columns::Columns;
use crate::metrics::{anim as m_anim, tracks as tr};
use crate::probe::pid;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_MD, LABEL_CAPS, MONO_CODE, MONO_DATA};
use crate::views::tracks::{flex_cell, now_playing_bars};
use crate::widgets::{icon, icon_g};
use gpui::{AnyElement, App, Bounds, CursorStyle, Div, IntoElement, Pixels, Point, Stateful, Window, div, point, prelude::*, px};
use sonix_api::Track;
use sonix_api::format as fmt;
use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone, Copy)]
pub struct RowOpts {
    pub show_artist: bool,
    pub cols: Columns,
    /// Trailing icon buttons: star + "..." (2) or star + add + "..." (3).
    pub actions: usize,
}

fn spacer() -> Div {
    div().w(px(tr::ICON_BTN)).h(px(tr::ICON_BTN)).flex_none()
}

fn right(w: f32) -> Div {
    div().w(px(w)).flex_none().flex().justify_end()
}

/// `<TrackRow head actionCount={n}/>`: plain (not sortable) caps labels.
pub fn head(th: &Theme, o: RowOpts, prefix: &str) -> AnyElement {
    let label = |s: &'static str| txt(th, LABEL_CAPS, s).color(th.muted);
    let mut r = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(tr::ROW_GAP))
        .pl(px(tr::ROW_PAD_LEFT))
        .pr(px(tr::ROW_PAD_RIGHT))
        .py(px(tr::HEAD_PAD_Y))
        .flex_none()
        .border_b_1()
        .border_color(th.border)
        .child(div().w(px(tr::IDX_W)).flex_none())
        .child(flex_cell(tr::TITLE_GROW).flex().child(label("Title")));
    if o.show_artist {
        r = r.child(flex_cell(tr::ARTIST_GROW).flex().child(label("Artist")));
    }
    if o.cols.year {
        r = r.child(right(tr::YEAR_W).child(label("Year")));
    }
    if o.cols.added {
        r = r.child(right(tr::ADDED_W).child(label("Added")));
    }
    r = r.child(right(tr::FORMAT_W).child(label("Format"))).child(right(tr::TIME_W).child(label("Time")));
    for _ in 0..o.actions {
        r = r.child(spacer());
    }
    pid(format!("{prefix}.head"), r).into_any_element()
}

type WinCb = Rc<dyn Fn(&mut Window, &mut App)>;
type PosCb = Rc<dyn Fn(Point<Pixels>, &mut Window, &mut App)>;

pub struct RowCb {
    pub star: WinCb,
    pub add: Option<PosCb>,
    /// Gets the menu position (under the "..." button).
    pub more: PosCb,
    /// The row's index button (`.idx-play`): play from this row.
    pub play: WinCb,
}

pub struct RowState {
    pub ix: usize,
    pub active: bool,
    pub playing: bool,
    pub selected: bool,
    /// Records the row's box (menu key anchors the menu under the cursor row).
    pub anchor: Option<Rc<Cell<Bounds<Pixels>>>>,
}

fn key_of(prefix: &str, ix: usize) -> usize {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (prefix, ix).hash(&mut h);
    h.finish() as usize
}

fn icon_button(th: &Theme, id: (&'static str, usize), name: &'static str, color: gpui::Hsla, hover: gpui::Hsla, anchor: Option<Rc<Cell<Bounds<Pixels>>>>) -> Stateful<Div> {
    let th = *th;
    let _ = th;
    let mut d = div().id(id).group("hv").relative().size(px(tr::ICON_BTN)).flex_none().flex().items_center().justify_center().cursor(CursorStyle::PointingHand).text_color(color).hover(move |s| s.text_color(hover));
    if let Some(a) = anchor {
        d = d.child(gpui::canvas(move |b, _, _| a.set(b), |_, _, _, _| {}).absolute().size_full());
    }
    d.child(icon_g(name, tr::ROW_ICON, color, hover))
}

/// One track row. `ids` = parity id prefix for this row (`album.row.3`). The caller attaches click / right-click handlers.
#[allow(clippy::too_many_arguments)]
pub fn row(th: &Theme, t: &Track, st: &RowState, o: RowOpts, height: f32, ids: Option<String>, cb: RowCb, window: &mut Window) -> Stateful<Div> {
    let th = *th;
    let ix = st.ix;
    let key = key_of(ids.as_deref().unwrap_or("row"), ix);
    let hovered = anim::hovered(key);
    let idx_opacity = 1.0 - anim::transition(window, ("idx-fade", key), if hovered { 1.0 } else { 0.0 }, m_anim::IDX_FADE);
    let starred = t.starred.is_some();
    let p = |suffix: &str, el: AnyElement| -> AnyElement {
        match &ids {
            Some(i) => pid(format!("{i}.{suffix}"), el).into_any_element(),
            None => el,
        }
    };
    let play_cb = cb.play.clone();
    let idx_cell = p(
        "idx",
        div()
            .id(("didx", key))
            .relative()
            .w(px(tr::IDX_W))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(2.))
            .text_color(th.muted)
            .cursor(CursorStyle::PointingHand)
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                play_cb(window, cx);
            })
            .child(if st.active {
                now_playing_bars(&th, st.playing).into_any_element()
            } else {
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().opacity(idx_opacity).child(txt(&th, MONO_CODE, format!("{:02}", ix + 1))))
                    .when(hovered, |d| d.child(div().absolute().child(icon("play", tr::ROW_PLAY_ICON, th.muted))))
                    .into_any_element()
            })
            .into_any_element(),
    );
    let title_txt = txt(&th, BODY_MD, t.title.clone()).color(if st.active { th.accent_fg } else { th.text }).ellipsis();
    let title = flex_cell(tr::TITLE_GROW).flex().child(p("title", div().flex().min_w_0().child(title_txt).into_any_element()));
    let mut r = div()
        .id(("drow", key))
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(tr::ROW_GAP))
        .pl(px(tr::ROW_PAD_LEFT))
        .pr(px(tr::ROW_PAD_RIGHT))
        .h(px(height))
        .w_full()
        .border_b_1()
        .border_color(th.mid)
        .when(st.active || st.selected, |d| d.bg(th.mid))
        .when(!st.active && !st.selected, |d| d.hover(move |s| s.bg(th.mid)))
        .child(idx_cell)
        .child(title);
    if o.show_artist {
        r = r.child(p(
            "artist",
            flex_cell(tr::ARTIST_GROW).flex().child(entity_link(&th, (Kind::Artist, t.artist_id.as_deref()), &t.artist, MONO_DATA, th.muted, &format!("row{ix}"), window)).into_any_element(),
        ));
    }
    if o.cols.year {
        r = r.child(p("year", right(tr::YEAR_W).child(txt(&th, MONO_DATA, fmt::track_year(t)).color(th.muted)).into_any_element()));
    }
    if o.cols.added {
        r = r.child(p("added", right(tr::ADDED_W).child(txt(&th, MONO_DATA, t.date_added.as_deref().map(fmt::date_part).unwrap_or("—").to_string()).color(th.muted)).into_any_element()));
    }
    r = r
        .child(p("format", right(tr::FORMAT_W).child(txt(&th, MONO_DATA, fmt::format_track_format(&t.format)).color(th.muted)).into_any_element()))
        .child(p("time", right(tr::TIME_W).child(txt(&th, MONO_DATA, fmt::format_duration(Some(t.duration))).color(th.muted)).into_any_element()));
    let (star_col, star_hover) = if starred { (th.accent_fg, th.accent_fg) } else { (th.muted, th.text) };
    let star = icon_button(&th, ("dstar", key), if starred { "heart-filled" } else { "heart" }, star_col, star_hover, None).on_click({
        let f = cb.star.clone();
        move |_, window, cx| {
            cx.stop_propagation();
            f(window, cx);
        }
    });
    r = r.child(p("star", star.into_any_element()));
    if let Some(add) = cb.add.clone() {
        let a = anchor_cell();
        let a2 = a.clone();
        r = r.child(
            icon_button(&th, ("dadd", key), "add", th.muted, th.text, Some(a)).on_click(move |_, window, cx| {
                cx.stop_propagation();
                let b = a2.get();
                add(point(b.origin.x, b.origin.y + b.size.height + px(4.)), window, cx);
            }),
        );
    }
    let a = anchor_cell();
    let a2 = a.clone();
    let more = cb.more.clone();
    r = r.child(p(
        "more",
        icon_button(&th, ("dmore", key), "more", th.muted, th.text, Some(a))
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                let b = a2.get();
                more(point(b.origin.x, b.origin.y + b.size.height + px(4.)), window, cx);
            })
            .into_any_element(),
    ));
    if let Some(a) = st.anchor.clone() {
        r = r.child(gpui::canvas(move |b, _, _| a.set(b), |_, _, _, _| {}).absolute().size_full());
    }
    r = r
        .when(st.active, |d| d.child(div().absolute().left_0().top_0().bottom_0().w(px(tr::ACTIVE_BAR_W)).bg(th.accent_fg)))
        .when(st.selected, |d| d.child(div().absolute().top_0().left_0().size_full().border_1().border_color(th.accent_fg)))
        .on_hover(move |h, window, _| {
            anim::set_hovered(key, *h);
            window.refresh();
        });
    r
}
