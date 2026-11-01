//! Context-menu item lists of the list routes (`menus.ts`): tracks (with "Remove from this playlist"),
//! playlists, Liked Songs and smart playlists. Items talk to the page through [`ListsEvent`]s and fire-and-forget
//! backend calls, like the Tracks route's menu.
use super::ListsEvent;
use super::modal;
use crate::app;
use crate::router::Route;
use crate::widgets::context_menu::MenuItem;
use gpui::{App, ClipboardItem, EventEmitter, WeakEntity};
use sonix_api::*;
use std::rc::Rc;

pub type Action = Rc<dyn Fn(&mut App)>;

pub struct TrackMenu {
    pub tracks: Vec<Track>,
    pub playlists: Vec<Playlist>,
    pub play_now: Action,
    pub remove_from_playlist: Option<Action>,
}

fn enqueue(label: String, pos: EnqueuePosition, ids: Vec<String>, icon: &'static str) -> MenuItem {
    MenuItem::action(label, move |_, cx| {
        let ids = ids.clone();
        app::call(cx, move |b| {
            let _ = b.player_enqueue(EnqueueRequest { track_ids: ids, position: pos });
        });
    })
    .icon(icon)
}

fn nav<V: EventEmitter<ListsEvent> + 'static>(this: &WeakEntity<V>, cx: &mut App, r: Route) {
    if let Some(t) = this.upgrade() {
        t.update(cx, |_, cx| cx.emit(ListsEvent::Navigate(r)));
    }
}

/// `trackMenuItems`
pub fn track_menu_items<V: EventEmitter<ListsEvent> + 'static>(this: WeakEntity<V>, o: TrackMenu) -> Vec<MenuItem> {
    let tracks = o.tracks;
    let n = tracks.len();
    let suffix = if n > 1 { format!(" ({n} tracks)") } else { String::new() };
    let ids: Vec<String> = tracks.iter().map(|t| t.id.clone()).collect();
    let first = tracks[0].clone();
    let all_starred = tracks.iter().all(|t| t.starred.is_some());

    let play = o.play_now.clone();
    let mut items = vec![
        MenuItem::action("Play now", move |_, cx| play(cx)).icon("play"),
        enqueue(format!("Play next{suffix}"), EnqueuePosition::Next, ids.clone(), "next"),
        enqueue(format!("Add to queue{suffix}"), EnqueuePosition::End, ids.clone(), "queue"),
    ];
    if n == 1 {
        items.push(MenuItem::action("Start radio", |_, _| {}).icon("equalizer"));
    }
    let pls: Vec<MenuItem> = o
        .playlists
        .iter()
        .filter(|p| !p.read_only)
        .map(|p| {
            let (pid, ids, this) = (p.id.clone(), ids.clone(), this.clone());
            MenuItem::action(p.name.clone(), move |_, cx| {
                let (pid, ids) = (pid.clone(), ids.clone());
                app::call(cx, move |b| {
                    let _ = b.playlists_add_tracks(AddToPlaylistRequest { id: pid, track_ids: ids });
                });
                if let Some(t) = this.upgrade() {
                    t.update(cx, |_, cx| cx.emit(ListsEvent::PlaylistsChanged));
                }
            })
        })
        .collect();
    items.push(MenuItem::submenu(format!("Add to playlist{suffix}"), pls).icon("add"));
    let ids_l = ids.clone();
    items.push(
        MenuItem::action(format!("{}{suffix}", if all_starred { "Unlike" } else { "Like" }), move |_, cx| {
            let ids = ids_l.clone();
            app::call(cx, move |b| {
                for id in ids {
                    let req = StarRequest { id, kind: StarTargetType::Track };
                    let _ = if all_starred { b.star_remove(req) } else { b.star_add(req) };
                }
            });
        })
        .icon(if all_starred { "heart-filled" } else { "heart" }),
    );
    let rate = |stars: u8, ids: Vec<String>| {
        let label = if stars == 0 { "Clear rating".to_string() } else { format!("{}★", stars) };
        let mut it = MenuItem::action(label, move |_, cx| {
            let ids = ids.clone();
            app::call(cx, move |b| {
                for id in ids {
                    let _ = b.rating_set(SetRatingRequest { id, kind: StarTargetType::Track, rating: stars });
                }
            });
        });
        if stars == 1 {
            it = it.danger().hint("1★ tracks are purged by the NAS nightly.");
        }
        it
    };
    items.push(MenuItem::submenu(format!("Rate{suffix}"), [0u8, 5, 4, 3, 2, 1].into_iter().map(|s| rate(s, ids.clone())).collect()).icon("star"));
    if n == 1 {
        if let Some(al) = first.album_id.clone() {
            let this = this.clone();
            items.push(MenuItem::action("Go to album", move |_, cx| nav(&this, cx, Route::Album(al.clone()))).icon("album"));
        }
        if let Some(ar) = first.artist_id.clone() {
            let this = this.clone();
            items.push(MenuItem::action("Go to artist", move |_, cx| nav(&this, cx, Route::Artist(ar.clone()))).icon("artist"));
        }
    }
    if let Some(rm) = o.remove_from_playlist {
        items.push(MenuItem::separator());
        items.push(MenuItem::action(format!("Remove from this playlist{suffix}"), move |_, cx| rm(cx)).icon("trash").danger());
    }
    if n == 1 {
        items.push(MenuItem::separator());
        items.push(MenuItem::action("Track info", |_, _| {}).icon("eye"));
        let text = format!("{} – {}", first.artist, first.title);
        items.push(MenuItem::action("Copy \"Artist – Title\"", move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))));
    }
    items
}

/// `flowMenuEntry`: the "Flow ▸" submenu (Flow sequencing is not ported: the entries are inert).
fn flow_entry(disabled: bool) -> MenuItem {
    MenuItem::submenu("Flow", vec![MenuItem::action("Steady", |_, _| {}), MenuItem::action("Build", |_, _| {}), MenuItem::action("Peak", |_, _| {})]).icon("sliders").disabled(disabled)
}

fn play_pl(id: String, shuffle: bool) -> impl Fn(&mut gpui::Window, &mut App) {
    move |_, cx| {
        let id = id.clone();
        app::call(cx, move |b| {
            let _ = b.player_play_playlist(PlaySourceRequest { id, shuffle, start_track_id: None, seed: 0 });
        });
    }
}

/// `playlistMenuItems`
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
        modal::prompt(cx, "Rename playlist", &name_r, "", "Rename", move |name, _, cx| {
            let Some(name) = name else { return };
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
        modal::confirm(cx, "Delete playlist", &format!("Delete \"{name_x}\"? This cannot be undone."), "Delete", true, move |ok, _, cx| {
            if ok.is_none() {
                return;
            }
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
        flow_entry(empty),
        queue_item("Play next", EnqueuePosition::Next, "next"),
        queue_item("Add to queue", EnqueuePosition::End, "queue"),
        MenuItem::separator(),
        rename,
        dup,
        delete,
    ]
}

/// `likedMenuItems`
pub fn liked_menu_items(count: usize) -> Vec<MenuItem> {
    let play = |shuffle: bool| {
        move |_: &mut gpui::Window, cx: &mut App| {
            app::call(cx, move |b| {
                let _ = b.player_play_liked(PlaySourceRequest { shuffle, ..Default::default() });
            });
        }
    };
    vec![MenuItem::action("Play", play(false)).icon("play").disabled(count == 0), MenuItem::action("Shuffle", play(true)).icon("shuffle").disabled(count == 0), flow_entry(count == 0)]
}

/// `smartMenuItems`
pub fn smart_menu_items<V: EventEmitter<ListsEvent> + 'static>(this: WeakEntity<V>, sp: &SmartSummary, seed: u32, delete_nav: bool) -> Vec<MenuItem> {
    let empty = sp.track_count == 0;
    let play = |id: String, shuffle: bool| {
        move |_: &mut gpui::Window, cx: &mut App| {
            let id = id.clone();
            app::call(cx, move |b| {
                let _ = b.player_play_smart(PlaySourceRequest { id, shuffle, start_track_id: None, seed });
            });
        }
    };
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
        modal::prompt(cx, "Rename smart playlist", &sp_r.input.name, "", "Rename", move |name, _, cx| {
            let Some(name) = name else { return };
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
        // the copy opens right away (SmartPlaylistDetail's `onChanged` navigates to it)
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
        modal::confirm(cx, "Delete smart playlist", &format!("Delete \"{}\"? Only the rules are removed, never any tracks.", sp.input.name), "Delete", true, move |ok, _, cx| {
            if ok.is_none() {
                return;
            }
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
        flow_entry(empty),
        MenuItem::separator(),
        edit,
        rename,
        dup,
        delete,
    ]
}
