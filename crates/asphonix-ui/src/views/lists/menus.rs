//! Context-menu item lists of the list routes: tracks (with "Remove from this playlist"),
//! playlists, Liked Songs and smart playlists. Items talk to the page through [`ListsEvent`]s and fire-and-forget
//! backend calls, like the Tracks route's menu.
use super::ListsEvent;
use crate::app;
use crate::router::Route;
use crate::widgets::context_menu::MenuItem;
use gpui::{App, EventEmitter, WeakEntity};
use asphonix_api::*;
use std::rc::Rc;

pub type Action = Rc<dyn Fn(&mut App)>;

pub struct TrackMenu {
    pub tracks: Vec<Track>,
    /// where the menu opened (the add-to-playlist popover anchors here)
    pub at: gpui::Point<gpui::Pixels>,
    pub play_now: Action,
    pub remove_from_playlist: Option<Action>,
}

fn nav<V: EventEmitter<ListsEvent> + 'static>(this: &WeakEntity<V>, cx: &mut App, r: Route) {
    if let Some(t) = this.upgrade() {
        t.update(cx, |_, cx| cx.emit(ListsEvent::Navigate(r)));
    }
}

/// Track menu: the shared builder (`crate::menus`) with this page's play / remove-from-playlist context.
pub fn track_menu_items<V: EventEmitter<ListsEvent> + 'static>(this: WeakEntity<V>, o: TrackMenu) -> Vec<MenuItem> {
    crate::menus::track_menu_items(crate::menus::TrackMenu {
        tracks: o.tracks,
        at: o.at,
        on_play_now: o.play_now,
        on_play_next: None,
        on_remove_from_queue: None,
        on_remove_from_playlist: o.remove_from_playlist,
        on_navigate: Rc::new(move |r, cx| nav(&this, cx, r)),
    })
}

fn play_pl(id: String, shuffle: bool) -> impl Fn(&mut gpui::Window, &mut App) {
    move |_, cx| crate::playback::play_playlist(cx, id.clone(), shuffle, None)
}

/// Playlist menu.
pub fn playlist_menu_items<V: EventEmitter<ListsEvent> + 'static>(this: WeakEntity<V>, p: &Playlist, on_deleted_nav: bool) -> Vec<MenuItem> {
    let empty = p.song_count == 0;
    let ro_hint = "Read-only playlist.";
    let queue_item = |label: &'static str, pos: EnqueuePosition, icon: &'static str| {
        let id = p.id.clone();
        MenuItem::action(label, move |_, cx| {
            let id = id.clone();
            app::call(cx, move |b| {
                if let Ok(d) = b.playlists_get(&id) {
                    let _ = b.player_enqueue(EnqueueRequest { track_ids: d.tracks.into_iter().map(|t| t.id).collect(), position: pos });
                }
            });
        })
        .icon(icon)
        .disabled(empty)
    };
    let (id_r, name_r, this_r) = (p.id.clone(), p.name.clone(), this.clone());
    let rename = MenuItem::action("Rename", move |_, cx| {
        let (id, this) = (id_r.clone(), this_r.clone());
        crate::overlays::prompt(cx, "Rename playlist", "", "Rename", &name_r, move |name, cx| {
            let id2 = id.clone();
            app::call(cx, move |b| {
                let _ = b.playlists_rename(RenamePlaylistRequest { id: id2, name });
            });
            if let Some(t) = this.upgrade() {
                t.update(cx, |_, cx| cx.emit(ListsEvent::PlaylistsChanged));
            }
        });
    })
    .icon("edit")
    .disabled(p.read_only);
    let (id_d, this_d) = (p.id.clone(), this.clone());
    let dup = MenuItem::action("Duplicate", move |_, cx| {
        let id = id_d.clone();
        app::call(cx, move |b| {
            let _ = b.playlists_duplicate(&id);
        });
        if let Some(t) = this_d.upgrade() {
            t.update(cx, |_, cx| cx.emit(ListsEvent::PlaylistsChanged));
        }
    })
    .icon("add");
    let (id_x, name_x, this_x) = (p.id.clone(), p.name.clone(), this);
    let delete = MenuItem::action("Delete", move |_, cx| {
        let (id, this) = (id_x.clone(), this_x.clone());
        crate::overlays::confirm(cx, "Delete playlist", &format!("Delete \"{name_x}\"? This cannot be undone."), "Delete", true, move |cx| {
            let id2 = id.clone();
            app::call(cx, move |b| {
                let _ = b.playlists_delete(DeletePlaylistRequest { id: id2 });
            });
            if let Some(t) = this.upgrade() {
                t.update(cx, |_, cx| {
                    cx.emit(ListsEvent::PlaylistsChanged);
                    if on_deleted_nav {
                        cx.emit(ListsEvent::Navigate(Route::Playlists));
                    }
                });
            }
        });
    })
    .icon("trash")
    .danger()
    .disabled(p.read_only);
    let (mut rename, mut delete) = (rename, delete);
    if p.read_only {
        rename = rename.hint(ro_hint);
        delete = delete.hint(ro_hint);
    }
    vec![
        MenuItem::action("Play", play_pl(p.id.clone(), false)).icon("play").disabled(empty),
        MenuItem::action("Shuffle", play_pl(p.id.clone(), true)).icon("shuffle").disabled(empty),
        {
            let id = p.id.clone();
            crate::flow::menu_entry(Rc::new(move |arc, cx: &mut App| crate::playback::play_playlist(cx, id.clone(), false, Some(arc))), empty)
        },
        queue_item("Play next", EnqueuePosition::Next, "next"),
        queue_item("Add to queue", EnqueuePosition::End, "queue"),
        MenuItem::separator(),
        rename,
        dup,
        delete,
    ]
}

/// Liked Songs menu.
pub fn liked_menu_items(count: usize) -> Vec<MenuItem> {
    let play = |shuffle: bool| move |_: &mut gpui::Window, cx: &mut App| crate::playback::play_liked(cx, shuffle, None);
    vec![
        MenuItem::action("Play", play(false)).icon("play").disabled(count == 0),
        MenuItem::action("Shuffle", play(true)).icon("shuffle").disabled(count == 0),
        crate::flow::menu_entry(Rc::new(|arc, cx: &mut App| crate::playback::play_liked(cx, false, Some(arc))), count == 0),
    ]
}

/// Smart playlist menu.
pub fn smart_menu_items<V: EventEmitter<ListsEvent> + 'static>(this: WeakEntity<V>, sp: &SmartSummary, seed: u32, delete_nav: bool) -> Vec<MenuItem> {
    let empty = sp.track_count == 0;
    let play = |id: String, shuffle: bool| move |_: &mut gpui::Window, cx: &mut App| crate::playback::play_smart(cx, id.clone(), shuffle, seed, None);
    let (sp_e, this_e) = (sp.clone(), this.clone());
    let edit = MenuItem::action("Edit rules…", move |_, cx| {
        if let Some(t) = this_e.upgrade() {
            let sp = sp_e.clone();
            t.update(cx, |_, cx| cx.emit(ListsEvent::EditSmart(Some(Box::new(sp)))));
        }
    })
    .icon("sliders");
    let (sp_r, this_r) = (sp.clone(), this.clone());
    let rename = MenuItem::action("Rename", move |_, cx| {
        let (sp, this) = (sp_r.clone(), this_r.clone());
        crate::overlays::prompt(cx, "Rename smart playlist", "", "Rename", &sp_r.input.name, move |name, cx| {
            let (id, mut input) = (sp.id.clone(), sp.input.clone());
            input.name = name;
            app::call(cx, move |b| {
                let _ = b.smart_update(&id, input);
            });
            if let Some(t) = this.upgrade() {
                t.update(cx, |_, cx| cx.emit(ListsEvent::SmartChanged));
            }
        });
    })
    .icon("edit");
    let (id_d, this_d) = (sp.id.clone(), this.clone());
    let dup = MenuItem::action("Duplicate", move |_, cx| {
        let id = id_d.clone();
        let this = this_d.clone();
        // the copy opens right away
        let b = app::backend(cx);
        cx.spawn(async move |cx| {
            let copy = cx.background_executor().spawn(async move { b.smart_duplicate(&id) }).await;
            if let (Some(t), Ok(c)) = (this.upgrade(), copy) {
                let _ = t.update(cx, |_, cx| {
                    cx.emit(ListsEvent::SmartChanged);
                    if delete_nav {
                        cx.emit(ListsEvent::Navigate(Route::Smart(c.id)));
                    }
                });
            }
        })
        .detach();
    })
    .icon("add");
    let (sp_x, this_x) = (sp.clone(), this);
    let delete = MenuItem::action("Delete", move |_, cx| {
        let (sp, this) = (sp_x.clone(), this_x.clone());
        crate::overlays::confirm(cx, "Delete smart playlist", &format!("Delete \"{}\"? Only the rules are removed, never any tracks.", sp.input.name), "Delete", true, move |cx| {
            let id = sp.id.clone();
            app::call(cx, move |b| {
                let _ = b.smart_delete(&id);
            });
            if let Some(t) = this.upgrade() {
                t.update(cx, |_, cx| {
                    cx.emit(ListsEvent::SmartChanged);
                    if delete_nav {
                        cx.emit(ListsEvent::Navigate(Route::Playlists));
                    }
                });
            }
        });
    })
    .icon("trash")
    .danger();
    vec![
        MenuItem::action("Play", play(sp.id.clone(), false)).icon("play").disabled(empty),
        MenuItem::action("Shuffle", play(sp.id.clone(), true)).icon("shuffle").disabled(empty),
        {
            let id = sp.id.clone();
            crate::flow::menu_entry(Rc::new(move |arc, cx: &mut App| crate::playback::play_smart(cx, id.clone(), false, seed, Some(arc))), empty)
        },
        MenuItem::separator(),
        edit,
        rename,
        dup,
        delete,
    ]
}
