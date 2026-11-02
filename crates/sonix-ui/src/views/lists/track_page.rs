//! `PlaylistDetail.svelte`, `Liked.svelte` and `SmartPlaylistDetail.svelte`: three pages with the same
//! skeleton (120 px art + title block + actions, then the track table), one entity.
use sonix_api::bucket_for;
use super::menus::{self, TrackMenu};
use super::table::{Arrows, Fetch, RowItem, Table, TableCfg, TableEvent};
use super::ui::{icon_btn40, play_btn, sec_btn, status_line};
use super::{ListsEvent, hot_shadow};
use crate::app::{self, PlayerModel};
use crate::art::{self, ArtHandle};
use crate::metrics::lists as lm;
use crate::probe::pid;
use crate::router::Route;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{HEADLINE_LG, LABEL_CAPS, MONO_CODE, MONO_DATA};
use crate::widgets::context_menu::{ContextMenu, MenuItem};
use crate::widgets::hard_shadow;
use crate::widgets::icon;
use crate::widgets::ui::{Tone, badge};
use gpui::{AnyElement, ClickEvent, Context, CursorStyle, Entity, EventEmitter, IntoElement, Render, Subscription, Window, div, prelude::*, px};
use sonix_api::format as fmt;
use sonix_api::*;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Clone, PartialEq, Eq)]
pub enum PageKind {
    Playlist(String),
    Liked,
    Smart(String),
}

enum Load {
    Loading,
    Error,
    Ready,
}

impl EventEmitter<ListsEvent> for TrackPage {}

pub struct TrackPage {
    pub kind: PageKind,
    backend: Arc<dyn Backend>,
    art: ArtHandle,
    menu: Entity<ContextMenu>,
    table: Entity<Table>,
    load: Load,
    detail: Option<PlaylistDetail>,
    liked: Option<LikedSummary>,
    smart: Option<SmartSummary>,
    seed: u32,
    /// the FLOW button's bounds (its menu anchors under it)
    flow_btn: Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
    more_bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
    rev: u64,
    _subs: Vec<Subscription>,
}

fn table_cfg(kind: &PageKind, read_only: bool) -> TableCfg {
    match kind {
        PageKind::Playlist(_) => TableCfg {
            head: true,
            album_column: false,
            draggable: !read_only,
            add_btn: true,
            remove_btn: !read_only,
            multi: true,
            arrows: Arrows::CursorOnly,
            action_count: if read_only { 3 } else { 4 },
            empty: if read_only { "No tracks right now. This playlist is read-only and fills itself." } else { "No tracks yet. Use \"Add to playlist\" from any track to fill this up." },
        },
        PageKind::Liked => TableCfg {
            head: true,
            album_column: true,
            draggable: false,
            add_btn: true,
            remove_btn: false,
            multi: true,
            arrows: Arrows::Select,
            action_count: 3,
            empty: "Nothing liked yet. Press L while a track plays, or tap the heart on any track.",
        },
        PageKind::Smart(_) => TableCfg {
            head: true,
            album_column: true,
            draggable: false,
            add_btn: true,
            remove_btn: false,
            multi: true,
            arrows: Arrows::Select,
            action_count: 3,
            empty: "No tracks match these rules.",
        },
    }
}

fn initial_seed() -> u32 {
    // `Math.floor(Math.random() * 1_000_000)`; the parity harness pins Math.random() to 0.5
    #[cfg(feature = "parity")]
    {
        500_000
    }
    #[cfg(not(feature = "parity"))]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| (d.as_nanos() % 1_000_000) as u32).unwrap_or(1)
    }
}

impl TrackPage {
    pub fn new(kind: PageKind, backend: Arc<dyn Backend>, art: ArtHandle, player: &Entity<PlayerModel>, menu: &Entity<ContextMenu>, cx: &mut Context<Self>) -> Self {
        let table = cx.new(|cx| Table::new(backend.clone(), player, table_cfg(&kind, false), cx));
        let subs = vec![cx.subscribe(&table, Self::on_table)];
        let mut this = Self {
            kind,
            backend,
            art,
            menu: menu.clone(),
            table,
            load: Load::Loading,
            detail: None,
            liked: None,
            smart: None,
            seed: initial_seed(),
            flow_btn: Rc::new(std::cell::Cell::new(gpui::Bounds::default())),
            more_bounds: Default::default(),
            rev: 0,
            _subs: subs,
        };
        this.reload(cx);
        this
    }

    /// Re-read everything (route params changed, a sync landed, a star changed, an edit saved).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.rev += 1;
        let (rev, kind, b, seed) = (self.rev, self.kind.clone(), self.backend.clone(), self.seed);
        match kind {
            PageKind::Playlist(id) => {
                cx.spawn(async move |this, cx| {
                    let r = cx.background_executor().spawn(async move { b.playlists_get(&id) }).await;
                    let _ = this.update(cx, |t, cx| {
                        if t.rev != rev {
                            return;
                        }
                        match r {
                            Ok(d) => {
                                let ro = d.playlist.read_only;
                                let rows: Vec<RowItem> = d.tracks.iter().cloned().map(|t| RowItem::Track { t: Box::new(t), label: None }).collect();
                                t.table.update(cx, |tb, cx| {
                                    tb.cfg = table_cfg(&t.kind, ro);
                                    tb.set_local(rows, cx);
                                });
                                t.detail = Some(d);
                                t.load = Load::Ready;
                            }
                            Err(_) => t.load = Load::Error,
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            PageKind::Liked => {
                let fetch: Fetch = Arc::new(|b, offset, limit| {
                    let p = b.liked_get(PageRequest { offset, limit })?;
                    Ok(Paged { items: p.items.into_iter().map(|t| RowItem::Track { t: Box::new(t), label: None }).collect(), total: p.total, offset: p.offset })
                });
                self.table.update(cx, |t, cx| t.set_paged(fetch, cx));
                self.load = Load::Ready;
                cx.spawn(async move |this, cx| {
                    let r = cx.background_executor().spawn(async move { b.liked_summary() }).await;
                    let _ = this.update(cx, |t, cx| {
                        if t.rev == rev {
                            t.liked = r.ok();
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            PageKind::Smart(id) => {
                let id2 = id.clone();
                let fetch: Fetch = Arc::new(move |b, offset, limit| {
                    let p = b.smart_tracks(&id2, seed, PageRequest { offset, limit })?;
                    Ok(Paged { items: p.items.into_iter().map(|t| RowItem::Track { t: Box::new(t), label: None }).collect(), total: p.total, offset: p.offset })
                });
                self.table.update(cx, |t, cx| t.set_paged(fetch, cx));
                cx.spawn(async move |this, cx| {
                    let r = cx.background_executor().spawn(async move { b.smart_get(&id, seed) }).await;
                    let _ = this.update(cx, |t, cx| {
                        if t.rev != rev {
                            return;
                        }
                        match r {
                            Ok(s) => {
                                t.smart = Some(s);
                                t.load = Load::Ready;
                            }
                            Err(_) => t.load = Load::Error,
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    /// Stars/ratings changed elsewhere: Liked and Smart re-read (rows may appear/vanish), a playlist keeps its list.
    pub fn stars_changed(&mut self, cx: &mut Context<Self>) {
        let (b, seed, kind) = (self.backend.clone(), self.seed, self.kind.clone());
        if matches!(kind, PageKind::Playlist(_)) {
            return;
        }
        // like `pager.reset()` + the summary re-read: rows and totals refresh, the scroll position stays
        self.table.update(cx, |t, cx| t.refresh_visible(cx));
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    match kind {
                        PageKind::Liked => (b.liked_summary().ok(), None),
                        PageKind::Smart(id) => (None, b.smart_get(&id, seed).ok()),
                        PageKind::Playlist(_) => (None, None),
                    }
                })
                .await;
            let _ = this.update(cx, |t, cx| {
                if r.0.is_some() {
                    t.liked = r.0;
                }
                if r.1.is_some() {
                    t.smart = r.1;
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn release(&mut self, cx: &mut Context<Self>) {
        self.table.update(cx, |t, _| t.release());
    }

    #[cfg(feature = "parity")]
    pub(crate) fn parity_state(&self, window: &Window, cx: &gpui::App) -> serde_json::Value {
        let mut v = self.table.read(cx).parity_state(window);
        v["pageKind"] = match &self.kind {
            PageKind::Playlist(_) => "playlist",
            PageKind::Liked => "liked",
            PageKind::Smart(_) => "smart",
        }
        .into();
        v
    }

    // ------------------------------------------------------------ actions
    fn play_source(&self, start: Option<String>, shuffle: bool, cx: &mut Context<Self>) {
        self.play_with(start, shuffle, None, cx);
    }

    /// `playPlaylist|playLiked|playSmart(…, { startTrackId, shuffle, flow })`
    fn play_with(&self, start: Option<String>, shuffle: bool, flow: Option<FlowArc>, cx: &gpui::App) {
        let (kind, seed) = (self.kind.clone(), self.seed);
        app::call(cx, move |b| {
            let _ = match kind {
                PageKind::Playlist(id) => b.player_play_playlist(PlaySourceRequest { id, shuffle, start_track_id: start, flow, ..Default::default() }),
                PageKind::Liked => b.player_play_liked(PlaySourceRequest { shuffle, start_track_id: start, flow, ..Default::default() }),
                PageKind::Smart(id) => b.player_play_smart(PlaySourceRequest { id, shuffle, start_track_id: start, seed, flow }),
            };
        });
    }

    /// The header FLOW button (`openFlowMenu` with this list's pool and coverage noun).
    fn open_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (kind, id, seed) = match &self.kind {
            PageKind::Playlist(id) => (FlowPoolKind::Playlist, Some(id.clone()), None),
            PageKind::Liked => (FlowPoolKind::Liked, None, None),
            PageKind::Smart(id) => (FlowPoolKind::Smart, Some(id.clone()), Some(self.seed)),
        };
        let this = cx.entity().downgrade();
        let play: crate::flow::Play = Rc::new(move |arc, cx| {
            if let Some(t) = this.upgrade() {
                t.read(cx).play_with(None, false, Some(arc), cx);
            }
        });
        crate::flow::open_menu(self.menu.clone(), self.flow_btn.get(), crate::flow::pool(kind, id, seed), crate::flow::noun(kind), play, window, cx);
    }

    /// Tracks of the menu selection, handed to `then`. Rows the table has evicted (or never loaded, after Ctrl+A) are
    /// fetched on a background task first (`resolveTracks`), so the UI never blocks on the backend.
    fn with_selection(&mut self, ix: usize, cx: &mut Context<Self>, then: impl FnOnce(&mut Self, Vec<Track>, &mut Context<Self>) + 'static) {
        let (idx, have) = self.table.update(cx, |t, _| {
            let idx = t.menu_indices(ix);
            let have = t.tracks_at(&idx);
            (idx, have)
        });
        if have.len() == idx.len() || matches!(self.kind, PageKind::Playlist(_)) {
            then(self, have, cx);
            return;
        }
        let mut pages: Vec<usize> = idx.iter().map(|i| i / crate::pager::PAGE).collect();
        pages.sort_unstable();
        pages.dedup();
        let (b, kind, seed) = (self.backend.clone(), self.kind.clone(), self.seed);
        cx.spawn(async move |this, cx| {
            let tracks = cx
                .background_executor()
                .spawn(async move {
                    let mut fetched: std::collections::HashMap<usize, Vec<Track>> = Default::default();
                    for p in pages {
                        let req = PageRequest { offset: p * crate::pager::PAGE, limit: crate::pager::PAGE };
                        let r = match &kind {
                            PageKind::Liked => b.liked_get(req),
                            PageKind::Smart(id) => b.smart_tracks(id, seed, req),
                            PageKind::Playlist(_) => continue,
                        };
                        if let Ok(r) = r {
                            fetched.insert(p, r.items);
                        }
                    }
                    idx.iter().filter_map(|i| fetched.get(&(i / crate::pager::PAGE)).and_then(|v| v.get(i % crate::pager::PAGE)).cloned()).collect::<Vec<Track>>()
                })
                .await;
            let _ = this.update(cx, |t, cx| then(t, tracks, cx));
        })
        .detach();
    }

    fn read_only(&self) -> bool {
        self.detail.as_ref().map(|d| d.playlist.read_only).unwrap_or(false)
    }

    fn on_table(&mut self, _: Entity<Table>, ev: &TableEvent, cx: &mut Context<Self>) {
        match ev {
            TableEvent::Play(ix) => {
                let id = self.table.read(cx).track_at(*ix).map(|t| t.id.clone());
                self.play_source(id, false, cx);
            }
            TableEvent::Enqueue(ix) => {
                if let Some(id) = self.table.read(cx).track_at(*ix).map(|t| t.id.clone()) {
                    app::call(cx, move |b| {
                        let _ = b.player_enqueue(EnqueueRequest { track_ids: vec![id], position: EnqueuePosition::End });
                    });
                }
            }
            TableEvent::Star(ix) => {
                let Some(t) = self.table.read(cx).track_at(*ix).cloned() else { return };
                let starred = t.starred.is_some();
                let id = t.id.clone();
                app::call(cx, move |b| {
                    let req = StarRequest { id, kind: StarTargetType::Track };
                    let _ = if starred { b.star_remove(req) } else { b.star_add(req) };
                });
                if let PageKind::Playlist(_) = self.kind {
                    // PlaylistDetail patches its own copy: the row's heart flips at once
                    let stamp = Some(sonix_api::format::utc_iso(self.backend.now_ms()));
                    self.table.update(cx, |tb, cx| tb.patch_track(*ix, |t| t.starred = if starred { None } else { stamp.clone() }, cx));
                }
            }
            TableEvent::Menu { ix, pos } => {
                let pos = *pos;
                self.with_selection(*ix, cx, move |t, tracks, cx| {
                    if tracks.is_empty() {
                        return;
                    }
                    let items = t.track_menu(tracks, pos, cx);
                    t.show_menu(items, pos, cx);
                });
            }
            TableEvent::AddToPlaylist { ix, pos } => {
                // `openAddToPlaylistMenu(anchor, t.id)`: the row's own track, not the selection
                if let Some(id) = self.table.read(cx).track_at(*ix).map(|t| t.id.clone()) {
                    crate::overlays::add_to_playlist(cx, vec![id], *pos);
                }
            }
            TableEvent::Remove(ix) => self.remove_indices(vec![*ix], cx),
            TableEvent::Reorder { from, to } => self.reorder(*from, *to, cx),
            TableEvent::SelectionChanged => cx.notify(),
        }
    }

    fn show_menu(&mut self, items: Vec<MenuItem>, pos: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let menu = self.menu.clone();
        cx.defer(move |cx| {
            let Some(w) = cx.active_window() else { return };
            let _ = w.update(cx, |_, window, cx| menu.update(cx, |m, cx| m.show(items, pos, window, cx)));
        });
    }

    fn track_menu(&mut self, tracks: Vec<Track>, at: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let this = cx.entity().downgrade();
        let t2 = this.clone();
        let first = tracks[0].id.clone();
        let many: Vec<String> = tracks.iter().map(|t| t.id.clone()).collect();
        let play_now: Rc<dyn Fn(&mut gpui::App)> = Rc::new(move |cx| {
            if let Some(t) = t2.upgrade() {
                let (first, many) = (first.clone(), many.clone());
                t.update(cx, |p, cx| {
                    if many.len() == 1 {
                        p.play_source(Some(first), false, cx);
                    } else {
                        app::call(cx, move |b| {
                            let _ = b.player_play_queue(PlayQueueRequest { track_ids: many, start_index: 0 });
                        });
                    }
                });
            }
        });
        let remove: Option<menus::Action> = if matches!(self.kind, PageKind::Playlist(_)) && !self.read_only() {
            let t3 = this.clone();
            let sel = self.table.read(cx).sel.indices();
            Some(Rc::new(move |cx| {
                if let Some(t) = t3.upgrade() {
                    let sel = sel.clone();
                    t.update(cx, |p, cx| p.remove_indices(sel, cx));
                }
            }))
        } else {
            None
        };
        menus::track_menu_items(this, TrackMenu { tracks, at, play_now, remove_from_playlist: remove })
    }

    fn remove_indices(&mut self, mut indices: Vec<usize>, cx: &mut Context<Self>) {
        let PageKind::Playlist(id) = self.kind.clone() else { return };
        if indices.is_empty() || self.read_only() {
            return;
        }
        indices.sort_unstable();
        let req = RemovePlaylistTracksRequest { id, indices: indices.clone() };
        app::call(cx, move |b| {
            let _ = b.playlists_remove_tracks(req);
        });
        if let Some(d) = self.detail.as_mut() {
            let drop: std::collections::HashSet<usize> = indices.into_iter().collect();
            let mut i = 0;
            d.tracks.retain(|_| {
                i += 1;
                !drop.contains(&(i - 1))
            });
            d.playlist.song_count = d.tracks.len() as u32;
            d.playlist.duration = d.tracks.iter().map(|t| t.duration).sum();
            let rows: Vec<RowItem> = d.tracks.iter().cloned().map(|t| RowItem::Track { t: Box::new(t), label: None }).collect();
            self.table.update(cx, |t, cx| {
                t.clear_selection(cx);
                t.set_local_keep_scroll(rows, cx);
            });
        }
        cx.emit(ListsEvent::PlaylistsChanged);
        cx.notify();
    }

    fn reorder(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let PageKind::Playlist(id) = self.kind.clone() else { return };
        if from == to || self.read_only() {
            return;
        }
        let Some(d) = self.detail.as_mut() else { return };
        if from >= d.tracks.len() || to >= d.tracks.len() {
            return;
        }
        let moved = d.tracks.remove(from);
        d.tracks.insert(to, moved);
        let ids: Vec<String> = d.tracks.iter().map(|t| t.id.clone()).collect();
        let rows: Vec<RowItem> = d.tracks.iter().cloned().map(|t| RowItem::Track { t: Box::new(t), label: None }).collect();
        self.table.update(cx, |t, cx| t.set_local_keep_scroll(rows, cx));
        app::call(cx, move |b| {
            let _ = b.playlists_reorder(ReorderPlaylistRequest { id, track_ids: ids });
        });
    }

    fn rename(&mut self, cx: &mut Context<Self>) {
        let Some(d) = &self.detail else { return };
        if d.playlist.read_only {
            return;
        }
        let (id, name) = (d.playlist.id.clone(), d.playlist.name.clone());
        let this = cx.entity().downgrade();
        let shown = name.clone();
        crate::overlays::prompt(cx, "Rename playlist", "", "Rename", &shown, move |new, cx| {
            let Some(new) = Some(new).filter(|n| *n != name) else { return };
            let (id2, n2) = (id.clone(), new.clone());
            app::call(cx, move |b| {
                let _ = b.playlists_rename(RenamePlaylistRequest { id: id2, name: n2 });
            });
            if let Some(t) = this.upgrade() {
                t.update(cx, |p, cx| {
                    if let Some(d) = p.detail.as_mut() {
                        d.playlist.name = new;
                    }
                    cx.emit(ListsEvent::PlaylistsChanged);
                    cx.notify();
                });
            }
        });
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        let Some(d) = &self.detail else { return };
        if d.playlist.read_only {
            return;
        }
        let (id, name) = (d.playlist.id.clone(), d.playlist.name.clone());
        let this = cx.entity().downgrade();
        crate::overlays::confirm(cx, "Delete playlist", &format!("Delete \"{name}\"? This cannot be undone."), "Delete", true, move |cx| {
            let id2 = id.clone();
            app::call(cx, move |b| {
                let _ = b.playlists_delete(DeletePlaylistRequest { id: id2 });
            });
            if let Some(t) = this.upgrade() {
                t.update(cx, |_, cx| {
                    cx.emit(ListsEvent::PlaylistsChanged);
                    cx.emit(ListsEvent::Navigate(Route::Playlists));
                });
            }
        });
    }

    // ------------------------------------------------------------ render
    fn art_box(&self, th: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = *th;
        let base = div().relative().w(px(lm::ART)).h(px(lm::ART)).flex_none().flex().items_center().justify_center().overflow_hidden();
        match &self.kind {
            PageKind::Liked => div()
                .w(px(lm::ART))
                .h(px(lm::ART))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .bg(th.accent)
                .text_color(th.accent_text)
                .shadow(hard_shadow(&th, 4.0))
                .child(icon("heart-filled", lm::ICON_ART_LIKED, th.accent_text))
                .into_any_element(),
            PageKind::Smart(_) => div()
                .w(px(lm::ART))
                .h(px(lm::ART))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .bg(th.high)
                .border_1()
                .border_color(th.focus)
                .shadow(hot_shadow(&th, 2.0))
                .child(icon("bolt", lm::ICON_ART_SMART, th.accent_fg))
                .into_any_element(),
            PageKind::Playlist(_) => {
                let d = self.detail.as_ref();
                let count = d.map(|d| d.playlist.song_count).unwrap_or(0);
                let key = d.map(|d| art::mosaic_key(&d.playlist));
                let inner: AnyElement = if count > 0 {
                    art::cover_bucket(&self.art, &self.backend, key.as_deref(), lm::ART - 2.0, bucket_for((320.0 * window.scale_factor()).ceil() as u32), window, cx)
                } else {
                    icon("queue", lm::ICON_ART_EMPTY, th.muted).into_any_element()
                };
                base.bg(th.high).border_1().border_color(th.border).text_color(th.muted).child(inner).into_any_element()
            }
        }
    }
}

impl Render for TrackPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        match self.load {
            Load::Loading => {
                let msg = match self.kind {
                    PageKind::Playlist(_) => "LOADING PLAYLIST…",
                    _ => "EVALUATING…",
                };
                return status_line(&th, msg, false).into_any_element();
            }
            Load::Error => {
                let msg = match self.kind {
                    PageKind::Playlist(_) => "PLAYLIST UNAVAILABLE.",
                    _ => "SMART PLAYLIST NOT FOUND.",
                };
                return status_line(&th, msg, true).into_any_element();
            }
            Load::Ready => {}
        }
        let (total, selected) = {
            let t = self.table.read(cx);
            (t.total(), t.selected_count())
        };
        let empty = total == 0;
        let int = |n: usize| fmt::fmt_int(n as u64);

        // ---- info block
        let mut info = div().flex().flex_col().gap(px(lm::INFO_GAP)).justify_end().min_w_0();
        let name: String;
        match &self.kind {
            PageKind::Playlist(_) => {
                let d = self.detail.as_ref().unwrap();
                let pl = &d.playlist;
                name = pl.name.clone();
                if pl.read_only {
                    let note = if pl.smart { "Rule-based on the server; contents update automatically.".to_string() } else { format!("Owned by {}.", pl.owner.as_deref().unwrap_or("another user")) };
                    info = info.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(lm::INFO_GAP))
                            .children(pl.smart.then(|| badge(&th, Tone::Accent, "Smart")))
                            .child(badge(&th, Tone::Default, "Read-only"))
                            .child(txt(&th, MONO_CODE, note).color(th.muted)),
                    );
                }
            }
            PageKind::Liked => {
                name = "Liked Songs".into();
                info = info.child(div().flex().child(txt(&th, LABEL_CAPS, "Playlist // pinned").color(th.muted)));
            }
            PageKind::Smart(_) => {
                let s = self.smart.as_ref().unwrap();
                name = s.input.name.clone();
                info = info.child(div().flex().flex_row().gap(px(lm::INFO_GAP)).child(badge(&th, Tone::Accent, "Smart playlist")).child(badge(&th, Tone::Default, "Local · not on server")));
            }
        }
        let name_el = pid("pl.name", div().flex().child(txt(&th, HEADLINE_LG, name).color(th.text)));
        info = if matches!(self.kind, PageKind::Playlist(_)) { info.child(name_el.into_any_element()) } else { info.child(pid("page.title", name_el).into_any_element()) };
        // stats
        let stat = |s: String| txt(&th, MONO_DATA, s).color(th.muted);
        let dot = || txt(&th, MONO_DATA, "·").color(th.muted);
        let mut stats = div().flex().flex_row().gap(px(lm::INFO_GAP));
        match &self.kind {
            PageKind::Playlist(_) => {
                let d = self.detail.as_ref().unwrap();
                stats = stats.child(stat(format!("{} TRACKS", d.playlist.song_count))).child(dot()).child(stat(fmt::format_total_duration(d.playlist.duration)));
                if !d.tracks.is_empty() {
                    stats = stats.child(dot()).child(stat(fmt::summarize_formats(&d.tracks)));
                }
            }
            PageKind::Liked => {
                let n = self.liked.as_ref().map(|s| s.count).unwrap_or(total);
                stats = stats.child(stat(format!("{} TRACKS", int(n))));
                if let Some(s) = &self.liked {
                    stats = stats.child(dot()).child(stat(fmt::format_total_duration(s.duration)));
                }
                stats = stats.child(dot()).child(stat(if selected > 1 { format!("{} SELECTED", int(selected)) } else { "NEWEST FIRST".into() }));
            }
            PageKind::Smart(_) => {
                let s = self.smart.as_ref().unwrap();
                stats = stats.child(stat(format!("{} TRACKS", int(s.track_count)))).child(dot()).child(stat(fmt::format_total_duration(s.duration)));
                if selected > 1 {
                    stats = stats.child(dot()).child(stat(format!("{} SELECTED", int(selected))));
                }
            }
        }
        info = info.child(pid("pl.stats", stats));
        if let PageKind::Smart(_) = &self.kind {
            let s = self.smart.as_ref().unwrap();
            info = info.child(div().flex().min_w_0().child(txt(&th, MONO_CODE, super::smart_rules::describe_smart(&s.input)).color(th.muted).ellipsis()));
        }

        // ---- actions
        let ro = self.read_only();
        let mut actions = div().flex().flex_row().items_center().gap(px(lm::ACTIONS_GAP)).mt(px(lm::ACTIONS_MT));
        actions = actions
            .child(pid("pl.btn.play", play_btn("pg-play", &th, "PLAY", empty).when(!empty, |b| b.on_click(cx.listener(|this, _, _, cx| this.play_source(None, false, cx))))))
            .child(pid("pl.btn.shuffle", sec_btn("pg-shuffle", &th, "shuffle", "SHUFFLE", empty).when(!empty, |b| b.on_click(cx.listener(|this, _, _, cx| this.play_source(None, true, cx))))))
            .child(pid(
                "pl.btn.flow",
                sec_btn("pg-flow", &th, "sliders", "FLOW", empty).relative().child(crate::flow::bounds_probe(self.flow_btn.clone())).when(!empty, |b| b.on_click(cx.listener(|this, _, window, cx| this.open_flow(window, cx)))),
            ));
        match &self.kind {
            PageKind::Playlist(_) => {
                actions = actions
                    .child(pid("pl.btn.rename", icon_btn40("pg-rename", &th, "edit", ro).when(!ro, |b| b.on_click(cx.listener(|this, _, _, cx| this.rename(cx))))))
                    .child(pid("pl.btn.delete", icon_btn40("pg-delete", &th, "trash", ro).when(!ro, |b| b.on_click(cx.listener(|this, _, _, cx| this.delete(cx))))))
                    .child(pid(
                        "pl.btn.more",
                        icon_btn40("pg-more", &th, "more", false).on_click(cx.listener(|this, ev: &ClickEvent, _, cx| {
                            let Some(d) = &this.detail else { return };
                            let items = menus::playlist_menu_items(cx.entity().downgrade(), &d.playlist, true);
                            let p = ev.position();
                            this.show_menu(items, p, cx);
                        })),
                    ));
            }
            PageKind::Liked => {
                actions = actions.child(pid(
                    "pl.btn.rename",
                    div()
                        .id("pg-starred")
                        .group("hv")
                        .px(px(crate::metrics::space::SM))
                        .cursor(CursorStyle::PointingHand)
                        .text_color(th.muted)
                        .hover(move |s| s.text_color(th.accent_fg))
                        .child(txt(&th, LABEL_CAPS, "Starred albums →"))
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ListsEvent::Navigate(Route::Favorites)))),
                ));
            }
            PageKind::Smart(_) => {
                let random = self.smart.as_ref().map(|s| s.input.sort == SmartSort::Random).unwrap_or(false);
                let mut names = ["pl.btn.rename", "pl.btn.delete", "pl.btn.more", "pl.btn.x"].into_iter();
                if random {
                    actions = actions.child(pid(names.next().unwrap(), icon_btn40("pg-reroll", &th, "sync", false).on_click(cx.listener(|this, _, _, cx| {
                        this.seed = (this.seed.wrapping_mul(1_103_515_245).wrapping_add(12_345)) % 1_000_000;
                        this.reload(cx);
                    }))));
                }
                actions = actions
                    .child(pid(names.next().unwrap(), icon_btn40("pg-edit", &th, "sliders", false).on_click(cx.listener(|this, _, _, cx| {
                        if let Some(s) = this.smart.clone() {
                            cx.emit(ListsEvent::EditSmart(Some(Box::new(s))));
                        }
                    }))))
                    .child(pid(names.next().unwrap(), icon_btn40("pg-dup", &th, "add", false).on_click(cx.listener(|this, _, _, cx| {
                        let Some(s) = this.smart.clone() else { return };
                        let b = this.backend.clone();
                        cx.spawn(async move |this, cx| {
                            let r = cx.background_executor().spawn(async move { b.smart_duplicate(&s.id) }).await;
                            let _ = this.update(cx, |_, cx| {
                                if let Ok(c) = r {
                                    cx.emit(ListsEvent::SmartChanged);
                                    cx.emit(ListsEvent::Navigate(Route::Smart(c.id)));
                                }
                            });
                        })
                        .detach();
                    }))))
                    .child({
                        let mb = self.more_bounds.clone();
                        pid(
                            names.next().unwrap(),
                            icon_btn40("pg-more", &th, "more", false).relative().child(gpui::canvas(move |b, _, _| mb.set(b), |_, _, _, _| ()).absolute().size_full()).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                let Some(s) = this.smart.clone() else { return };
                                let items = menus::smart_menu_items(cx.entity().downgrade(), &s, this.seed, true);
                                // : left edge, 4 px below the button
                                let b = this.more_bounds.get();
                                this.show_menu(items, gpui::point(b.origin.x, b.origin.y + b.size.height + px(4.)), cx);
                            })),
                        )
                    });
            }
        }
        info = info.child(actions);

        let header = div().flex().flex_row().gap(px(lm::HEADER_GAP)).flex_none().child(pid("pl.art", self.art_box(&th, window, cx))).child(info);
        pid(
            "page",
            div().size_full().flex().flex_col().p(px(lm::PAGE_PAD)).gap(px(lm::PAGE_GAP)).child(header).child(pid("tp.panel", self.table.clone())),
        )
        .into_any_element()
    }
}
