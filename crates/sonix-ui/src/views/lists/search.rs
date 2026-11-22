//! The global search page. The query lives in the top bar's input (shared entity); results
//! come from one `SearchType::All` request after an 80 ms debounce, tabs show the real totals, arrows move a
//! flat selection, Enter opens / plays, Shift+Enter enqueues a track.
use super::menus::{self, TrackMenu};
use super::ui::scroll_page;
use super::ListsEvent;
use crate::app;
use crate::art::{self, ArtHandle};
use crate::metrics::{lists as lm, space};
use crate::router::Route;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_MD, BODY_SM, HEADLINE_MD, HEADLINE_SM, LABEL_CAPS, MONO_CODE, MONO_DATA};
use crate::widgets::context_menu::{ContextMenu, MenuItem};
use crate::widgets::text_input::{InputEvent, TextInput};
use crate::widgets::{icon, hard_shadow};
use gpui::{AnyElement, Context, CursorStyle, Entity, EventEmitter, IntoElement, MouseButton, MouseDownEvent, Render, ScrollHandle, Subscription, Timer, Window, div, prelude::*, px};
use sonix_api::format as fmt;
use sonix_api::*;
use sonix_i18n::tr;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

impl EventEmitter<ListsEvent> for SearchView {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    All,
    Track,
    Album,
    Artist,
    Playlist,
}

#[derive(Clone)]
enum Flat {
    Track(usize),
    Album(usize),
    Artist(usize),
    Playlist(usize),
}

pub struct SearchView {
    backend: Arc<dyn Backend>,
    art: ArtHandle,
    menu: Entity<ContextMenu>,
    input: Entity<TextInput>,
    query: String,
    tab: Tab,
    results: Option<SearchResults>,
    loading: bool,
    /// the answer for the current query arrived (before that an empty result is "Searching…", never "No matches")
    settled: bool,
    selected: usize,
    seq: u64,
    scroll: ScrollHandle,
    _sub: Subscription,
}

impl SearchView {
    pub fn new(backend: Arc<dyn Backend>, art: ArtHandle, menu: &Entity<ContextMenu>, input: &Entity<TextInput>, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(input, |this: &mut Self, i, ev: &InputEvent, cx| match ev {
            InputEvent::Changed => {
                let q = i.read(cx).content.to_string();
                this.set_query(q, cx);
            }
            InputEvent::Submit => this.enter(false, cx),
        });
        let query = input.read(cx).content.to_string();
        let mut this = Self {
            backend,
            art,
            menu: menu.clone(),
            input: input.clone(),
            query: String::new(),
            tab: Tab::All,
            results: None,
            loading: false,
            settled: false,
            selected: 0,
            seq: 0,
            scroll: ScrollHandle::new(),
            _sub: sub,
        };
        this.set_query(query, cx);
        this
    }

    pub fn set_query(&mut self, q: String, cx: &mut Context<Self>) {
        self.query = q.clone();
        self.selected = 0;
        self.seq += 1;
        self.settled = false;
        let seq = self.seq;
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            Timer::after(Duration::from_millis(80)).await;
            let alive = this.update(cx, |t, _| t.seq == seq).unwrap_or(false);
            if !alive {
                return;
            }
            if q.trim().is_empty() {
                let _ = this.update(cx, |t, cx| {
                    t.results = None;
                    cx.notify();
                });
                return;
            }
            let _ = this.update(cx, |t, cx| {
                t.loading = true;
                cx.notify();
            });
            let r = cx.background_executor().spawn(async move { b.search_run(SearchRequest { text: q, search_type: SearchType::All, offset: 0, limit: 40 }) }).await;
            let _ = this.update(cx, |t, cx| {
                if t.seq == seq {
                    t.results = r.ok();
                    t.loading = false;
                    t.settled = true;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn flat(&self) -> Vec<Flat> {
        let Some(r) = &self.results else { return vec![] };
        let t = (0..r.tracks.items.len()).map(Flat::Track);
        let a = (0..r.albums.items.len()).map(Flat::Album);
        let ar = (0..r.artists.items.len()).map(Flat::Artist);
        let p = (0..r.playlists.items.len()).map(Flat::Playlist);
        match self.tab {
            Tab::Track => t.collect(),
            Tab::Album => a.collect(),
            Tab::Artist => ar.collect(),
            Tab::Playlist => p.collect(),
            Tab::All => t.chain(a).chain(ar).chain(p).collect(),
        }
    }


    fn open(&mut self, item: &Flat, cx: &mut Context<Self>) {
        let Some(r) = &self.results else { return };
        match item {
            Flat::Track(i) => {
                let ids: Vec<String> = r.tracks.items.iter().map(|t| t.id.clone()).collect();
                let start = *i;
                app::call(cx, move |b| {
                    let _ = b.player_play_queue(PlayQueueRequest { track_ids: ids, start_index: start });
                });
            }
            Flat::Album(i) => {
                if let Some(a) = r.albums.items.get(*i) {
                    cx.emit(ListsEvent::Navigate(Route::Album(a.id.clone())))
                }
            }
            Flat::Artist(i) => {
                if let Some(a) = r.artists.items.get(*i) {
                    cx.emit(ListsEvent::Navigate(Route::Artist(a.id.clone())))
                }
            }
            Flat::Playlist(i) => {
                if let Some(p) = r.playlists.items.get(*i) {
                    cx.emit(ListsEvent::Navigate(Route::Playlist(p.id.clone())))
                }
            }
        }
    }

    /// Enter / Shift+Enter on the selection.
    pub fn enter(&mut self, shift: bool, cx: &mut Context<Self>) {
        let flat = self.flat();
        let Some(item) = flat.get(self.selected).cloned() else { return };
        if shift && let (Flat::Track(i), Some(r)) = (&item, &self.results) {
            let Some(id) = r.tracks.items.get(*i).map(|t| t.id.clone()) else { return };
            app::call(cx, move |b| {
                let _ = b.player_enqueue(EnqueueRequest { track_ids: vec![id], position: EnqueuePosition::End });
            });
        } else {
            self.open(&item, cx);
        }
    }

    /// Up / Down from the window-level key handler. Returns whether the key was used.
    pub fn arrow(&mut self, down: bool, cx: &mut Context<Self>) -> bool {
        let n = self.flat().len();
        if n == 0 {
            return false;
        }
        self.selected = if down { (self.selected + 1).min(n - 1) } else { self.selected.saturating_sub(1) };
        cx.notify();
        true
    }

    /// Escape: clear the query and the results.
    pub fn escape(&mut self, cx: &mut Context<Self>) {
        self.input.update(cx, |i, cx| i.set_content("", cx));
        self.results = None;
        self.query.clear();
        cx.notify();
    }

    fn show_menu(&self, items: Vec<MenuItem>, pos: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let menu = self.menu.clone();
        cx.defer(move |cx| {
            let Some(w) = cx.active_window() else { return };
            let _ = w.update(cx, |_, window, cx| menu.update(cx, |m, cx| m.show(items, pos, window, cx)));
        });
    }

    fn context_menu(&mut self, item: Flat, pos: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let Some(r) = &self.results else { return };
        // the row listener captured an index into the results of the frame it was painted in; a search that
        // answered since then may have fewer rows (indexing would panic = abort)
        let len = match item {
            Flat::Track(_) => r.tracks.items.len(),
            Flat::Album(_) => r.albums.items.len(),
            Flat::Artist(_) => r.artists.items.len(),
            Flat::Playlist(_) => r.playlists.items.len(),
        };
        let (Flat::Track(i) | Flat::Album(i) | Flat::Artist(i) | Flat::Playlist(i)) = item;
        if i >= len {
            return;
        }
        let this = cx.entity().downgrade();
        let items = match item {
            Flat::Track(i) => {
                let ids: Vec<String> = r.tracks.items.iter().map(|t| t.id.clone()).collect();
                let t2 = this.clone();
                let play: Rc<dyn Fn(&mut gpui::App)> = Rc::new(move |cx| {
                    let ids = ids.clone();
                    app::call(cx, move |b| {
                        let _ = b.player_play_queue(PlayQueueRequest { track_ids: ids, start_index: i });
                    });
                    let _ = &t2;
                });
                menus::track_menu_items(this, TrackMenu { tracks: vec![r.tracks.items[i].clone()], at: pos, play_now: play, remove_from_playlist: None })
            }
            Flat::Playlist(i) => menus::playlist_menu_items(this, &r.playlists.items[i], false),
            Flat::Album(i) => crate::views::library::menus::album_menu(&r.albums.items[i], pos),
            Flat::Artist(i) => crate::views::library::menus::artist_menu(&r.artists.items[i]),
        };
        self.show_menu(items, pos, cx);
    }

    fn row(&mut self, th: Theme, flat_ix: usize, item: Flat, last: bool, cx: &mut Context<Self>) -> AnyElement {
        let r = self.results.as_ref().unwrap();
        let selected = self.selected == flat_ix;
        let (title, sub, format, time): (String, String, Option<String>, Option<String>) = match &item {
            Flat::Track(i) => {
                let t = &r.tracks.items[*i];
                (t.title.clone(), t.artist.clone(), Some(fmt::format_track_format(&t.format)), Some(fmt::format_duration(Some(t.duration))))
            }
            Flat::Album(i) => {
                let a = &r.albums.items[*i];
                (a.name.clone(), a.artist.clone(), Some(a.year.map(|y| y.to_string()).unwrap_or_else(|| "—".into())), None)
            }
            Flat::Artist(i) => {
                let a = &r.artists.items[*i];
                (a.name.clone(), tr!(CountAlbums, count = a.album_count).to_uppercase(), None, None)
            }
            Flat::Playlist(i) => {
                let p = &r.playlists.items[*i];
                (p.name.clone(), tr!(CountTracks, count = p.song_count).to_uppercase(), None, None)
            }
        };
        let is_track = matches!(item, Flat::Track(_));
        // tracks / album / artist rows are drag sources (playlists are not)
        let drag = match &item {
            Flat::Track(i) => Some(crate::dnd::DragPayload::Tracks(vec![r.tracks.items[*i].id.clone()])),
            Flat::Album(i) => Some(crate::dnd::DragPayload::Album { id: r.albums.items[*i].id.clone(), name: r.albums.items[*i].name.clone() }),
            Flat::Artist(i) => Some(crate::dnd::DragPayload::Artist { id: r.artists.items[*i].id.clone(), name: r.artists.items[*i].name.clone() }),
            Flat::Playlist(_) => None,
        };
        let (it_click, it_dbl, it_menu) = (item.clone(), item.clone(), item.clone());
        div()
            .id(("sr", flat_ix))
            .when_some(drag, |d, p| d.on_drag(crate::dnd::SonixDrag(p), crate::dnd::ghost(th)))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(space::MD))
            .w_full()
            .h(px(36.))
            .px(px(space::MD))
            .py(px(space::XS))
            .cursor(CursorStyle::PointingHand)
            .when(!last, |d| d.border_b_1().border_color(th.mid))
            .when(selected, |d| d.bg(th.mid))
            .when(!selected, |d| d.hover(move |s| s.bg(th.mid)))
            .child(div().flex_1().min_w_0().flex().child(txt(&th, BODY_MD, title).color(th.text).ellipsis()))
            .child(div().max_w(px(240.)).flex().min_w_0().child(txt(&th, MONO_DATA, sub).color(th.muted).ellipsis()))
            .children(format.map(|f| div().w(px(96.)).flex_none().flex().justify_end().child(txt(&th, MONO_DATA, f).color(th.muted))))
            .children(time.map(|t| div().w(px(60.)).flex_none().flex().justify_end().child(txt(&th, MONO_CODE, t).color(th.muted))))
            .when(selected, |d| d.child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(th.accent_fg)))
            .relative()
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                if is_track {
                    // tracks: single click selects, double click plays
                    if ev.click_count() >= 2 {
                        this.open(&it_dbl, cx);
                    } else {
                        this.selected = flat_ix;
                    }
                } else {
                    this.open(&it_click, cx);
                }
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    this.context_menu(it_menu.clone(), ev.position, cx);
                    cx.stop_propagation();
                }),
            )
            .into_any_element()
    }
}

impl Render for SearchView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        let handle = self.scroll.clone();
        scroll_page(th, "search-scroll", &handle, std::ptr::addr_of!(self.scroll) as usize, window, |_, window| {
            let flat = self.flat();
            let (counts, total) = match &self.results {
                Some(r) => ([r.tracks.total, r.albums.total, r.artists.total, r.playlists.total], true),
                None => ([0; 4], false),
            };
            let _ = total;
            let all = counts.iter().sum::<usize>();
            let tabs: [(Tab, &str, usize); 5] = [(Tab::All, tr!(SearchTabAll), all), (Tab::Track, tr!(NavTracks), counts[0]), (Tab::Album, tr!(NavAlbums), counts[1]), (Tab::Artist, tr!(NavArtists), counts[2]), (Tab::Playlist, tr!(NavPlaylists), counts[3])];
            let mut tabs_row = div().flex().flex_row().gap(px(2.));
            for (i, (t, label, n)) in tabs.into_iter().enumerate() {
                let active = self.tab == t;
                let (bg, fg, bc) = if active { (th.accent, th.accent_text, th.accent_fg) } else { (th.mid, th.muted, th.border) };
                tabs_row = tabs_row.child(div()
                        .id(("sr-tab", i))
                        .px(px(space::MD))
                        .py(px(space::XS))
                        .bg(bg)
                        .border_1()
                        .border_color(bc)
                        .text_color(fg)
                        .cursor(CursorStyle::PointingHand)
                        .child(txt(&th, LABEL_CAPS, format!("{label} // {n}")).color(fg))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.tab = t;
                            this.selected = 0;
                            cx.notify();
                        })));
            }
            let title = if self.query.is_empty() { tr!(SearchTitle).to_string() } else { format!("// {}", self.query) };
            let header = div().flex().flex_col().gap(px(space::SM)).child(div().flex().child(txt(&th, HEADLINE_MD, title).color(th.text))).child(tabs_row);
            let mut page = div().flex().flex_col().gap(px(lm::PAGE_GAP)).p(px(lm::PAGE_PAD)).w_full().child(header);
            let hint = |s: String| txt(&th, BODY_SM, s).color(th.muted);
            if self.query.is_empty() {
                page = page.child(hint(tr!(SearchStart).into()));
            } else if (self.loading || !self.settled) && self.results.is_none() {
                page = page.child(hint(tr!(SearchSearching).into()));
            } else if flat.is_empty() {
                page = page.child(hint(tr!(SearchNoMatches, query = self.query.as_str())));
            } else {
                let r = self.results.clone().unwrap();
                if self.tab == Tab::All {
                    // hero: best match = first flat item
                    let (tag, name, sub, cover, ic): (&str, String, String, Option<String>, &str) = match &flat[0] {
                        Flat::Track(i) => {
                            let t = &r.tracks.items[*i];
                            ("track", t.title.clone(), format!("{} · {} · {}", t.artist, fmt::format_track_format(&t.format), fmt::format_duration(Some(t.duration))), t.cover_art.clone(), "track")
                        }
                        Flat::Album(i) => {
                            let a = &r.albums.items[*i];
                            ("album", a.name.clone(), format!("{} · {}", a.artist, tr!(CountTracks, count = a.song_count).to_uppercase()), a.cover_art.clone(), "album")
                        }
                        Flat::Artist(i) => {
                            let a = &r.artists.items[*i];
                            ("artist", a.name.clone(), tr!(CountAlbums, count = a.album_count).to_uppercase(), a.cover_art.clone(), "artist")
                        }
                        Flat::Playlist(i) => {
                            let p = &r.playlists.items[*i];
                            ("playlist", p.name.clone(), tr!(CountTracks, count = p.song_count).to_uppercase(), None, "queue")
                        }
                    };
                    let selected = self.selected == 0;
                    let art_inner: AnyElement =
                        if cover.is_some() { art::cover(&self.art, &self.backend, cover.as_deref(), 64.0, window, cx) } else { icon(ic, 22., th.muted).into_any_element() };
                    let first = flat[0].clone();
                    let hero = div()
                        .id("sr-hero")
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(space::LG))
                        .w_full()
                        .p(px(space::LG))
                        .bg(th.low)
                        .border_1()
                        .border_color(if selected { th.focus } else { th.border })
                        .when(selected, |d| d.shadow(super::hot_shadow(&th, 2.0)))
                        .cursor(CursorStyle::PointingHand)
                        .child(div().size(px(64.)).flex_none().flex().items_center().justify_center().overflow_hidden().bg(th.high).text_color(th.muted).child(art_inner))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .min_w_0()
                                .child(div().flex().child(txt(&th, LABEL_CAPS, format!("{} // {}", tr!(SearchBestMatch), search_tag(tag))).color(th.accent_fg)))
                                .child(div().flex().child(txt(&th, HEADLINE_SM, name).color(th.text)))
                                .child(div().flex().child(txt(&th, MONO_DATA, sub).color(th.muted))),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.open(&first, cx)));
                    page = page.child(hero);
                }
                // groups
                let groups: [(&str, Tab, usize, usize); 4] = [(tr!(NavTracks), Tab::Track, r.tracks.items.len(), 0), (tr!(NavAlbums), Tab::Album, r.albums.items.len(), 1), (tr!(NavArtists), Tab::Artist, r.artists.items.len(), 2), (tr!(NavPlaylists), Tab::Playlist, r.playlists.items.len(), 3)];
                let mut base = 0usize;
                for (label, tab, n, kind) in groups {
                    if n == 0 || !(self.tab == Tab::All || self.tab == tab) {
                        continue;
                    }
                    let mut list = div().flex().flex_col().border_1().border_color(th.border).bg(th.low);
                    for k in 0..n {
                        let item = match kind {
                            0 => Flat::Track(k),
                            1 => Flat::Album(k),
                            2 => Flat::Artist(k),
                            _ => Flat::Playlist(k),
                        };
                        list = list.child(self.row(th, base + k, item, k + 1 == n, cx));
                    }
                    base += n;
                    page = page.child(div().flex().flex_col().gap(px(space::XS)).child(div().flex().child(txt(&th, LABEL_CAPS, label).color(th.muted))).child(list));
                }
            }
            let _ = hard_shadow;
            page.into_any_element()
        })
    }
}

/// The best-match kind ("track", "album", ...) as shown text.
fn search_tag(tag: &str) -> &'static str {
    match tag {
        "album" => tr!(SearchKindAlbum),
        "artist" => tr!(SearchKindArtist),
        "playlist" => tr!(SearchKindPlaylist),
        _ => tr!(SearchKindTrack),
    }
}
