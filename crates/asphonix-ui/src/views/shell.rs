//! The app shell: title bar, top bar, sidebar, stage (route view), queue
//! rail / drawer, dock, the shared context-menu overlay, plus suspend/resume on minimize.
use super::dock::{DockEvent, DockView};
use super::library::{self, LibEvent, NavBus, NavHandle};
use super::nowplaying::{NowPlayingView, NpEvent};
use super::queue_rail::QueueRail;
use super::tracks::{TracksEvent, TracksView};
use crate::app::{self, PlayerModel, Services};
use crate::perf::FrameStats;
use crate::router::{Route, Router, initial_route};
use crate::settings;
use crate::theme::{MONO, Theme};
use crate::widgets::context_menu::ContextMenu;
use crate::widgets::text_input::{InputEvent, TextInput};
use futures::StreamExt;
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, ImageSource, IntoElement, ObjectFit, Render, RenderImage, StyledImage, Subscription, Window, actions,
    div, img, prelude::*, px,
};
use asphonix_api::{Backend, Event, Playlist, SessionStatus};
use std::sync::Arc;
use std::time::{Duration, Instant};

actions!(
    asphonix,
    [PlayPause, ToggleNowPlaying, Escape, ToggleTheme, FocusSearch, NavBack, NavForward, SelUp, SelDown, PlaySelected, EnqueueSelected, OpenMenuKey, SelectAll]
);

pub struct Counts {
    pub tracks: Option<usize>,
    pub albums: Option<usize>,
    pub artists: Option<usize>,
    pub liked: Option<u32>,
}

pub struct Shell {
    pub(crate) th: Theme,
    pub(crate) router: Router,
    pub(crate) backend: Arc<dyn Backend>,
    pub(crate) player: Entity<PlayerModel>,
    pub(crate) tracks: Entity<TracksView>,
    /// list routes (Playlists, PlaylistDetail, Liked, Smart, History, ...): see `views/lists`
    pub(crate) lists: super::lists::ListsHost,
    pub(crate) np: Entity<NowPlayingView>,
    pub(crate) dock: Entity<DockView>,
    pub(crate) queue: Entity<QueueRail>,
    pub(crate) menu: Entity<ContextMenu>,
    pub(crate) overlays: Entity<crate::overlays::Overlays>,
    pub(crate) boot: Option<Entity<super::boot::BootIntro>>,
    /// The newer release found by `updates_check` (title-bar badge, update dialog).
    pub(crate) update: Option<asphonix_api::UpdateInfo>,
    pub(crate) card: Option<CardHandle>,
    card_gen: u64,
    prev_track_id: Option<String>,
    track_overlay: bool,
    track_overlay_corner: u8,
    main_active: bool,
    main_hwnd: Option<isize>,
    scale: f32,
    _boot_sub: Option<Subscription>,
    pub(crate) search: Entity<TextInput>,
    pub(crate) focus: FocusHandle,
    pub(crate) playlists: Vec<Playlist>,
    pub(crate) status: Option<SessionStatus>,
    pub(crate) counts: Counts,
    pub(crate) queue_drawer: bool,
    pub(crate) pl_scroll: gpui::ScrollHandle,
    /// The docked rail (>= 1440 px) hidden by the dock's queue button.
    pub(crate) rail_collapsed: bool,
    /// Library routes (Home, Albums, ..., `views/library`): the view entity of the open route and what it needs.
    pub(crate) lib_ctx: library::Ctx,
    lib_stage: Option<gpui::AnyView>,
    /// The route `lib_stage` was built for, and the page parked while Now Playing is open: closing Now Playing brings the
    /// same view entity back (no remount, no re-query, scroll and loaded data intact) instead of rebuilding it.
    lib_stage_route: Option<Route>,
    lib_parked: Option<(Route, gpui::AnyView)>,
    last_vw: f32,
    spectrum_on: bool,
    pub(crate) suspended: bool,
    /// ordering of the queue-source refreshes (a slow older answer never overwrites a newer one)
    source_seq: u64,
    backdrop: Option<Arc<RenderImage>>,
    shown: bool,
    resumed_at: Option<Instant>,
    fs: FrameStats,
    _subs: Vec<Subscription>,
    /// CPU work (`crate::live`, `crate::part`): the animated-elements layer and the cached chrome parts
    pub(crate) live: Entity<crate::live::LiveLayer>,
    pub(crate) live_stage: Entity<crate::live::LiveLayer>,
    pub(crate) parts: Option<super::shell_cache::ShellParts>,
    /// what the live layer's slots were recorded for (stage, rail shown, suspended): a change re-records them all
    live_key: Option<(Route, bool, bool, bool)>,
}

impl Shell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let svc = cx.global::<Services>();
        let (backend, art_cache) = (svc.backend.clone(), svc.art.clone());
        let th = app::theme(cx);
        let player = cx.new(|_| PlayerModel::default());
        let menu = cx.new(|cx| ContextMenu::new(th, cx));
        let overlays = cx.new(crate::overlays::Overlays::new);
        cx.set_global(crate::overlays::OverlayHandle(overlays.clone()));
        let tracks = cx.new(|cx| TracksView::new(backend.clone(), art_cache.clone(), &player, &menu, window, cx));
        let np = cx.new(|cx| NowPlayingView::new(backend.clone(), art_cache.clone(), &player, cx));
        let dock = cx.new(|cx| DockView::new(backend.clone(), art_cache.clone(), &player, cx));
        let queue = cx.new(|cx| QueueRail::new(backend.clone(), art_cache.clone(), &player, &menu, cx));
        let search = cx.new(|cx| TextInput::new(cx, &search_placeholder(), &th, MONO, 12.0, 16.0, 500.0));
        let focus = cx.focus_handle();
        window.focus(&focus);
        let nav = cx.new(|_| NavBus);
        cx.set_global(NavHandle(nav.clone()));
        cx.set_global(crate::widgets::text_input::InputMenu(menu.downgrade()));
        let lib_ctx = library::Ctx { backend: backend.clone(), art: art_cache.clone(), player: player.clone(), menu: menu.clone(), nav: nav.clone() };

        let mut subs = Vec::new();
        subs.push(cx.subscribe(&nav, |this: &mut Shell, _, ev: &LibEvent, cx| {
            if let LibEvent::Navigate(r) = ev {
                this.navigate(r.clone(), cx);
            }
        }));
        subs.push(cx.subscribe(&dock, |this: &mut Shell, _, ev: &DockEvent, cx| match ev {
            DockEvent::OpenNowPlaying => this.toggle_now_playing(cx),
            DockEvent::ToggleQueue => {
                // collapse the docked rail, or open/close the compact drawer
                if this.last_vw >= crate::metrics::bp::RAIL_DOCKED_FROM {
                    this.rail_collapsed = !this.rail_collapsed;
                } else {
                    this.queue_drawer = !this.queue_drawer;
                }
                cx.notify();
            }
        }));
        subs.push(cx.subscribe(&search, |this: &mut Shell, input, ev: &InputEvent, cx| {
            if *ev == InputEvent::Submit || matches!(ev, InputEvent::Changed) {
                let q = input.read(cx).content.to_string();
                if !q.trim().is_empty() {
                    this.navigate(Route::Search, cx);
                }
            }
        }));
        subs.push(cx.subscribe(&player, |this: &mut Shell, _, _: &crate::app::PlayerEvent, cx| this.card_on_player(cx)));
        subs.push(cx.subscribe_in(&queue, window, |this: &mut Shell, _, ev: &super::queue_rail::RailEvent, window, cx| match ev {
            super::queue_rail::RailEvent::Navigate(r) => this.navigate(r.clone(), cx),
            // playing from the queue reloads the list, which drops the focus it held: back to the page
            super::queue_rail::RailEvent::Played => {
                let f = this.focus.clone();
                window.focus(&f);
            }
        }));
        subs.push(cx.observe_window_activation(window, |this: &mut Shell, window, _cx| this.main_active = window.is_window_active()));
        subs.push(cx.subscribe(&np, |this: &mut Shell, _, ev: &NpEvent, cx| this.on_np_event(ev, cx)));
        // focusing the search box navigates to the Search route
        let search_focus = search.read(cx).focus_handle.clone();
        subs.push(cx.on_focus(&search_focus, window, |this: &mut Shell, _, cx| this.navigate(Route::Search, cx)));
        subs.push(cx.subscribe(&tracks, |this: &mut Shell, _, ev: &TracksEvent, cx| match ev {
            TracksEvent::Navigate(r) => this.navigate(r.clone(), cx),
        }));

        let live = cx.new(|_| crate::live::LiveLayer::new(true, &player, np.downgrade()));
        let live_stage = cx.new(|_| crate::live::LiveLayer::new(false, &player, np.downgrade()));
        let mut this = Self {
            th,
            router: Router::new(initial_route(std::env::var_os("ASPHONIX_SCRIPT").is_some())),
            backend: backend.clone(),
            player,
            tracks,
            lists: Default::default(),
            np,
            dock,
            queue,
            menu,
            overlays,
            boot: None,
            update: None,
            card: None,
            card_gen: 0,
            prev_track_id: None,
            track_overlay: true,
            track_overlay_corner: 0,
            main_active: true,
            main_hwnd: crate::winffi::hwnd_of(window),
            scale: window.scale_factor(),
            _boot_sub: None,
            search,
            focus,
            playlists: Vec::new(),
            status: None,
            counts: Counts { tracks: None, albums: None, artists: None, liked: None },
            queue_drawer: false,
            pl_scroll: gpui::ScrollHandle::new(),
            rail_collapsed: false,
            lib_ctx,
            lib_stage: None,
            lib_stage_route: None,
            lib_parked: None,
            last_vw: 1440.0,
            spectrum_on: false,
            suspended: false,
            source_seq: 0,
            backdrop: None,
            shown: false,
            resumed_at: None,
            fs: FrameStats::new(std::env::var("ASPHONIX_FPSLOG").is_ok() || std::env::var("ASPHONIX_SCRIPT").is_ok()),
            _subs: subs,
            live,
            live_stage,
            parts: None,
            live_key: None,
        };
        this.lists_init(cx);
        this.load_backdrop();
        this.start_event_pump(cx);
        this.load_static(cx);
        this.startup_login_check(cx);
        this.install_minimize_hook(window, cx);
        // Mount the initial route's page (Home builds its stage here; Tracks has none).
        crate::log(&format!("initial route {}", this.router.current().path()));
        this.route_changed(cx);
        this.debug_hooks(cx);
        this
    }

    // -------------------------------------------------------------- scripted / debug hooks
    /// Env-driven hooks for the perf harness (`scripts/perf.ps1`); never set in normal use.
    fn debug_hooks(&mut self, cx: &mut Context<Self>) {
        // `ASPHONIX_PAUSE_MS=<ms>`: toggle play/pause once after <ms> (CPU measurements of a real pause, no input needed)
        if let Some(ms) = std::env::var("ASPHONIX_PAUSE_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
            cx.spawn(async move |_, cx| {
                gpui::Timer::after(Duration::from_millis(ms)).await;
                let _ = cx.update(|cx| {
                    app::call(cx, |b| {
                        let _ = b.player_play_pause();
                    })
                });
            })
            .detach();
        }
        if std::env::var("ASPHONIX_SCRIPT").is_ok() {
            self.run_script(cx);
        }
    }

    fn log_frames(&mut self, name: &str) {
        if let Some(s) = self.fs.summary() {
            crate::log(&s.line(name));
        }
        self.fs.reset();
    }

    /// Perf timeline (`ASPHONIX_SCRIPT=1`, needs a signed-in profile, no input synthesis, no focus needed): idle 5 s ->
    /// scroll the whole Tracks list (`ASPHONIX_NOSCROLL` skips it) -> idle -> play all from the top -> Now Playing 40 s ->
    /// back to Tracks. Each step logs a `phase <name>` line to $ASPHONIX_LOG; the harness minimizes/restores the window itself.
    fn run_script(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let t = |ms: u64| gpui::Timer::after(Duration::from_millis(ms));
            t(3000).await;
            crate::log("phase idle_start");
            t(5000).await;
            crate::log("phase idle_end");
            let _ = this.update(cx, |sh, _| sh.fs.reset());
            crate::log("phase scroll_start");
            let total = this.update(cx, |sh, _| sh.counts.tracks.unwrap_or(0)).unwrap_or(0);
            let mut ix = 0usize;
            while ix + 1 < total && std::env::var_os("ASPHONIX_NOSCROLL").is_none() {
                ix = (ix + 100).min(total - 1);
                if this.update(cx, |sh, cx| sh.tracks.update(cx, |t, cx| t.script_scroll(ix, cx))).is_err() {
                    return;
                }
                t(16).await;
            }
            let _ = this.update(cx, |sh, _| sh.log_frames("scroll"));
            crate::log("phase scroll_done");
            t(4000).await;
            crate::log("phase post_scroll_idle");
            let _ = this.update(cx, |sh, cx| {
                sh.tracks.update(cx, |t, cx| t.script_scroll(0, cx));
                app::call(cx, |b| {
                    let _ = b.player_play_all(asphonix_api::PlayAllRequest {
                        order: asphonix_api::TrackOrder::Sort { field: asphonix_api::TrackSortField::Title, dir: asphonix_api::SortDir::Asc },
                        filter: None,
                        start_track_id: None,
                        flow: None,
                    });
                });
                sh.navigate(Route::NowPlaying, cx);
                sh.fs.reset();
            });
            crate::log("phase np_start");
            t(10_000).await;
            let _ = this.update(cx, |sh, _| sh.fs.reset());
            t(30_000).await;
            let _ = this.update(cx, |sh, _| sh.log_frames("np"));
            crate::log("phase np_end");
            let _ = this.update(cx, |sh, cx| sh.navigate(Route::Tracks, cx));
            t(500).await;
            crate::log("phase done");
        })
        .detach();
    }

    // -------------------------------------------------------------- data
    fn start_event_pump(&mut self, cx: &mut Context<Self>) {
        let rx = self.backend.subscribe();
        let (tx, mut frx) = futures::channel::mpsc::unbounded::<Event>();
        std::thread::Builder::new()
            .name("ui-events".into())
            .spawn(move || {
                while let Ok(ev) = rx.recv() {
                    if tx.unbounded_send(ev).is_err() {
                        break;
                    }
                }
            })
            .ok();
        cx.spawn(async move |this, cx| {
            while let Some(ev) = frx.next().await {
                if this.update(cx, |sh, cx| sh.on_event(ev, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        // initial state (a late listener asks, like `player_get_state`)
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let st = cx.background_executor().spawn(async move { b.player_get_state() }).await;
            if let Ok(st) = st {
                let _ = this.update(cx, |sh, cx| {
                    sh.player.update(cx, |p, cx| p.apply_state(st, true, cx));
                    sh.refresh_source(cx);
                });
            }
        })
        .detach();
    }

    /// The queue's source identity into the player model (`PlayerModel::source`: which play button is "playing").
    fn refresh_source(&mut self, cx: &mut Context<Self>) {
        let b = self.backend.clone();
        self.source_seq += 1;
        let seq = self.source_seq;
        cx.spawn(async move |this, cx| {
            let q = cx.background_executor().spawn(async move { b.player_get_queue(asphonix_api::PageRequest { offset: 0, limit: 100 }) }).await;
            if let Ok(q) = q {
                let _ = this.update(cx, |sh, cx| {
                    if seq != sh.source_seq {
                        return; // a newer refresh is in flight / done
                    }
                    let live = !sh.suspended;
                    // a static queue that is entirely one album (all rows loaded)
                    let album = (q.source.is_none() && !q.items.is_empty() && q.items.len() == q.total)
                        .then(|| q.items[0].track.album_id.clone())
                        .flatten()
                        .filter(|a| q.items.iter().all(|it| it.track.album_id.as_deref() == Some(a.as_str())))
                        .map(|a| (a, q.total as u32));
                    sh.player.update(cx, |p, cx| p.set_source(q.source, album, live, cx));
                });
            }
        })
        .detach();
    }

    fn on_event(&mut self, ev: Event, cx: &mut Context<Self>) {
        match ev {
            Event::PlayerState(s) => {
                let live = !self.suspended;
                let changed = self.player.read(cx).current_id() != s.track.as_ref().map(|t| t.id.as_str()) || self.player.read(cx).state.status != s.status;
                if changed {
                    crate::log(&format!("player status={:?} track={:?} err={:?}", s.status, s.track.as_ref().map(|t| t.id.as_str()), s.error));
                }
                self.player.update(cx, |p, cx| p.apply_state(*s, live, cx));
                // Minimized, apply_state emits no PlayerEvent (nothing re-renders), so the track-change card's
                // subscriber never ran: the card must still show then, that is when it is most useful.
                if changed && !live {
                    self.card_on_player(cx);
                }
                if changed {
                    self.refresh_source(cx);
                }
            }
            Event::Spectrum(f) => {
                if !self.suspended {
                    self.player.update(cx, |p, _| p.apply_spectrum(f));
                    if self.spectrum_on && *self.router.current() != Route::NowPlaying {
                        // the rail spectrum is painted by the live layer (`crate::live`): only it re-renders
                        // both layers render with every root frame: a pending bars / FFT redraw takes the new frame along
                        if !self.live_stage.read(cx).pending() {
                            self.live.update(cx, |l, cx| l.poke(cx));
                        }
                    }
                }
            }
            Event::QueueChanged => {
                self.refresh_source(cx);
                if !self.suspended {
                    self.queue.update(cx, |q, cx| q.refresh(cx));
                }
            }
            Event::LibraryChanged => {
                if !self.suspended {
                    self.tracks.update(cx, |t, cx| t.refresh_rows(cx));
                    self.lib_ctx.nav.update(cx, |_, cx| cx.emit(LibEvent::Refresh));
                    self.lists_library_changed(cx);
                }
            }
            Event::UpdateAvailable(i) => {
                if i.available {
                    self.update = Some(i);
                    cx.notify();
                }
            }
            Event::UpdateProgress(p) => crate::overlays::update_progress(cx, p),
            Event::SyncProgress(_) | Event::Connection(_) => {}
        }
    }

    /// At startup only a session without stored credentials (`disconnected`), or a failing
    /// status call, opens the login screen; `connecting`/`error` keep browsing the local library.
    fn startup_login_check(&mut self, cx: &mut Context<Self>) {
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let st = cx.background_executor().spawn(async move { b.auth_session_status() }).await;
            let to_login = st.map(|s| s.state == asphonix_api::ConnectionState::Disconnected).unwrap_or(true);
            crate::log(&format!("startup_login_check to_login={to_login}"));
            let r = this.update(cx, |sh, cx| {
                if to_login && *sh.router.current() != Route::Login {
                    sh.navigate(Route::Login, cx);
                }
            });
            crate::log(&format!("startup_login_check applied={}", r.is_ok()));
        })
        .detach();
    }

    pub(crate) fn load_static(&mut self, cx: &mut Context<Self>) {
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let (pl, st, albums, artists, liked, smart) = cx
                .background_executor()
                .spawn(async move {
                    (
                        b.playlists_list(),
                        b.auth_session_status(),
                        b.browse_get_albums(asphonix_api::GetAlbumsRequest { offset: 0, limit: 1, sort: asphonix_api::AlbumSort::Alphabetical, filter: None }),
                        b.browse_get_artists(asphonix_api::GetArtistsRequest { offset: 0, limit: 1, sort: None }),
                        b.liked_count(),
                        b.smart_list(),
                    )
                })
                .await;
            let _ = this.update(cx, |sh, cx| {
                sh.playlists = pl.unwrap_or_default();
                sh.lists.smart_side = smart.unwrap_or_default();
                sh.counts = Counts { tracks: st.as_ref().ok().and_then(|s| s.track_count).map(|n| n as usize), albums: albums.ok().map(|p| p.total), artists: artists.ok().map(|p| p.total), liked: liked.ok() };
                sh.status = st.ok();
                cx.notify();
            });
        })
        .detach();
    }

    fn load_backdrop(&mut self) {
        let bytes: &[u8] = if self.th.light { include_bytes!("../../../../assets/bg/light.png") } else { include_bytes!("../../../../assets/bg/dark.png") };
        self.backdrop = image::load_from_memory(bytes).ok().map(|i| {
            let mut rgba = i.to_rgba8();
            for p in rgba.chunks_mut(4) {
                p.swap(0, 2);
            }
            Arc::new(RenderImage::new(vec![image::Frame::new(rgba)]))
        });
    }

    // -------------------------------------------------------------- routing
    /// `ASPHONIX_LOG` names every route change with its call site (startup diagnostics: who navigated).
    #[track_caller]
    pub fn navigate(&mut self, r: Route, cx: &mut Context<Self>) {
        let at = std::panic::Location::caller();
        crate::log(&format!("navigate {} -> {} ({}:{})", self.router.current().path(), r.path(), at.file(), at.line()));
        if self.router.navigate(r) {
            self.route_changed(cx);
        }
    }
    pub fn go_back(&mut self, cx: &mut Context<Self>) {
        crate::log("navigate back");
        if self.router.back() {
            self.route_changed(cx);
        }
    }
    pub fn go_forward(&mut self, cx: &mut Context<Self>) {
        crate::log("navigate forward");
        if self.router.forward() {
            self.route_changed(cx);
        }
    }
    pub fn toggle_now_playing(&mut self, cx: &mut Context<Self>) {
        if *self.router.current() == Route::NowPlaying {
            self.go_back(cx);
        } else {
            self.navigate(Route::NowPlaying, cx);
        }
    }
    fn route_changed(&mut self, cx: &mut Context<Self>) {
        if *self.router.current() != Route::Login {
            self.lists.login = None;
        }
        // Starred Crates starts fresh on every visit
        if *self.router.current() != Route::Favorites {
            self.lists.favorites = None;
        }
        self.queue_drawer = false;
        let route = self.router.current().clone();
        library::entity_link::set_current(&route);
        let prev = self.lib_stage.take().zip(self.lib_stage_route.take());
        if route == Route::NowPlaying {
            self.lib_parked = prev.map(|(v, r)| (r, v));
        } else {
            self.lib_stage = match self.lib_parked.take() {
                Some((r, v)) if r == route => {
                    // The parked page re-queries in place (no
                    // skeletons, scroll kept), so what changed while Now Playing was open (Home's recently played,
                    // a LibraryChanged skipped while minimized) shows up.
                    self.lib_ctx.nav.update(cx, |_, cx| cx.emit(LibEvent::Refresh));
                    Some(v)
                }
                _ => library::stage_for(&route, &self.lib_ctx, cx),
            };
            self.lib_stage_route = self.lib_stage.as_ref().map(|_| route.clone());
        }
        let on_np = *self.router.current() == Route::NowPlaying;
        self.np.update(cx, |n, cx| n.set_visible(on_np, cx));
        cx.notify();
    }

    pub fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.th = self.th.toggled();
        let th = self.th;
        cx.set_global(th);
        settings::update(|p| p.light = th.light);
        self.search.update(cx, |i, _| i.set_theme(&th));
        self.menu.update(cx, |m, _| m.set_theme(th));
        self.tracks.update(cx, |t, _| t.set_theme(&th));
        // cached views (`crate::part`) repaint only on their own notify: the theme reaches them through a full refresh
        cx.refresh_windows();
        if !self.suspended {
            self.load_backdrop();
        }
        cx.notify();
    }

    /// Settings > Language: persist the choice (with the current theme, so a first-run user who followed the OS theme
    /// keeps it), switch the catalog, refresh text held in state (the search placeholder), then re-render every
    /// window with view caching bypassed (`refresh_windows`), so no cached part keeps the old language.
    pub fn set_language(&mut self, pref: Option<asphonix_i18n::Lang>, cx: &mut Context<Self>) {
        let light = self.th.light;
        settings::update(|p| {
            p.light = light;
            p.language = pref.map(|l| l.tag().to_string());
        });
        if crate::i18n::switch(cx, pref) {
            crate::log(&format!("language: {}", asphonix_i18n::lang().tag()));
            self.search.update(cx, |i, cx| {
                i.placeholder = search_placeholder();
                cx.notify();
            });
            cx.refresh_windows();
        }
        cx.notify();
    }
    // -------------------------------------------------------------- background mode
    fn install_minimize_hook(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(windows)]
        {
            let Some(hwnd) = crate::winffi::hwnd_of(window) else {
                crate::log("minimize hook: no HWND");
                return;
            };
            let ent = cx.weak_entity();
            let wh = window.window_handle();
            let app = cx.to_async();
            let fe = cx.foreground_executor().clone();
            let ok = crate::winffi::size_hook::install(
                hwnd,
                Box::new(move |min: bool| {
                    let (ent, app) = (ent.clone(), app.clone());
                    fe.spawn(async move {
                        let _ = app.update(|cx| {
                            let _ = wh.update(cx, |_, window, cx| {
                                let _ = ent.update(cx, |s, cx| s.on_minimize_change(min, window, cx));
                            });
                        });
                    })
                    .detach();
                }),
            );
            crate::log(&format!("minimize hook installed: {ok}"));
        }
        #[cfg(not(windows))]
        {
            let _ = (window, cx);
        }
    }

    fn on_minimize_change(&mut self, minimized: bool, window: &mut Window, cx: &mut Context<Self>) {
        crate::log(&format!("minimize change: minimized={minimized}"));
        if minimized {
            self.suspend(window, cx);
        } else if self.suspended {
            self.resume(window, cx);
        }
    }

    /// Minimize: stop animations/timers (gated on `suspended`), tell the backend nobody
    /// listens to the spectrum, drop every cache (cover images + GPU atlas tiles, paged
    /// rows, backdrop), then trim the working set once the frees have landed.
    fn suspend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::log("suspend: releasing caches");
        self.suspended = true;
        self.backend.set_suspended(true); // pauses background cover prefetch
        self.spectrum_on = false;
        crate::widgets::smooth::clear();
        crate::anim::clear(); // no transition may keep requesting frames while minimized
        crate::live::clear_all(); // nor the live layer (bars, spectrum): no slot, no frame
        self.live.update(cx, |l, _| l.stop());
        self.live_stage.update(cx, |l, _| l.stop());
        app::call(cx, |b| {
            let _ = b.player_set_spectrum_active(false);
        });
        self.np.update(cx, |n, cx| n.set_suspended(true, cx));
        self.tracks.update(cx, |t, _| t.release());
        self.lists_release(cx);
        self.queue.update(cx, |q, _| q.release());
        let handle = cx.global::<Services>().art.clone();
        for im in handle.borrow_mut().drain_all() {
            let _ = window.drop_image(im);
        }
        if let Some(b) = self.backdrop.take() {
            let _ = window.drop_image(b);
        }
        cx.spawn(async move |_, _| {
            gpui::Timer::after(Duration::from_millis(300)).await;
            crate::winffi::empty_working_set();
            crate::log("EmptyWorkingSet done");
        })
        .detach();
    }

    fn resume(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::log("resume: restore");
        self.suspended = false;
        self.backend.set_suspended(false);
        self.resumed_at = Some(Instant::now());
        self.load_backdrop();
        let st = self.backend.player_get_state();
        if let Ok(st) = st {
            self.player.update(cx, |p, cx| p.apply_state(st, true, cx));
        }
        // track / status changes while minimized were applied without an event (`apply_state(.., false, ..)`): the views that
        // keep their own copy (Tracks, tables, library pages: playing row, bars, play buttons) learn them now
        self.player.update(cx, |_, cx| {
            cx.emit(crate::app::PlayerEvent::TrackOrStatus);
            cx.notify();
        });
        self.refresh_source(cx);
        self.np.update(cx, |n, cx| n.set_suspended(false, cx));
        self.tracks.update(cx, |t, cx| t.restore(cx));
        self.lists_restore(cx);
        self.queue.update(cx, |q, cx| q.refresh(cx));
        window.refresh();
        cx.notify();
    }

    // -------------------------------------------------------------- layout helpers
    pub(crate) fn sidebar_width(vw: f32) -> f32 {
        if vw < crate::metrics::bp::COMPACT_BELOW { crate::metrics::layout::SIDEBAR_W_COMPACT } else { crate::metrics::layout::SIDEBAR_W }
    }
    /// Queue rail docked beside the stage (>= 1440 px, hidden on Now Playing).
    pub(crate) fn rail_docked(vw: f32, route: &Route) -> bool {
        vw >= crate::metrics::bp::RAIL_DOCKED_FROM && *route != Route::NowPlaying && *route != Route::Login
    }
}

impl Focusable for Shell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Keeps `library::parts` told the stage width, so views created on navigation start at their real width.
fn stage_probe() -> gpui::AnyElement {
    gpui::canvas(|b, _, _| super::library::parts::set_stage_width(f32::from(b.size.width)), |_, _, _, _| {})
        .absolute()
        .size_full()
        .into_any_element()
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = self.th;
        let vw = f32::from(window.viewport_size().width);
        let route = self.router.current().clone();
        let on_np = route == Route::NowPlaying;
        let docked = Self::rail_docked(vw, &route) && !self.rail_collapsed;
        self.last_vw = vw;
        // the backend computes the spectrum only while Now Playing or the rail is on screen.
        let want_spectrum = !self.suspended && (on_np || docked || (self.queue_drawer && !on_np));
        if want_spectrum != self.spectrum_on {
            self.spectrum_on = want_spectrum;
            app::call(cx, move |b| {
                let _ = b.player_set_spectrum_active(want_spectrum);
            });
        }

        self.fs.tick();
        crate::rcount::frame_start();
        crate::rcount::hit("root");
        if !self.shown {
            self.shown = true;
            crate::log("first_render");
            // test-only guard: exactly `ASPHONIX_NO_ACTIVATE=1` parks the window; unset (or any other value) activates as always
            if std::env::var_os("ASPHONIX_NO_ACTIVATE").is_none_or(|v| v != "1") {
                window.activate_window();
            } else if let Some(h) = crate::winffi::hwnd_of(window) {
                crate::winffi::show_parked_no_activate(h);
            }
            self.start_boot(window, cx);
        }
        if let Some(t) = self.resumed_at.take() {
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            crate::log(&format!("restore_first_render {ms:.1}ms after resume"));
        }

        let stage: AnyElement = match &route {
            Route::Tracks => Self::stage_view(self.tracks.clone()),
            Route::NowPlaying => Self::stage_view(self.np.clone()),
            r if super::lists::ListsHost::handles(r) => self.lists_stage(r, window, cx),
            _ => match &self.lib_stage {
                Some(v) => Self::stage_view(v.clone()),
                None => self.placeholder(),
            },
        };

        // cached parts (`shell_cache.rs`): re-rendered when the shell notifies, reused on live-layer frames
        let [title, top, side] = self.chrome_views(vw, window, cx);
        let live_key = (route.clone(), docked, !docked && !on_np && self.queue_drawer, self.suspended);
        if self.live_key.as_ref() != Some(&live_key) {
            // pages that left the screen keep no animated slots; the next frame re-renders everything uncached, so the
            // views still on screen record theirs again
            crate::live::clear_all();
            crate::widgets::smooth::forget_targets();
            cx.refresh_windows();
            self.live_key = Some(live_key);
        }

        // the login route has only the title bar and its own stage (no top bar, sidebar, rail, dock)
        let is_login = route == Route::Login;
        let body = div()
            .flex_1()
            .flex()
            .flex_row()
            .min_h_0()
            .relative()
            .children((!is_login).then_some(side))
            .child(div().flex_1().min_w_0().h_full().overflow_hidden().relative().child(stage_probe()).child(stage).child(self.live_stage.clone()))
            .when(docked, |d| d.child(self.rail_view()))
            .when(!docked && !on_np && self.queue_drawer, |d| {
                d.child(div().absolute().top_0().right_0().bottom_0().child(self.rail_view()))
            });

        div()
            .id("root")
            .capture_any_mouse_down(cx.listener(|_, _, _, _| crate::widgets::set_key_nav(false)))
            .key_context("Root")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                let m = ev.keystroke.modifiers;
                // not while typing (any text field, e.g. a prompt opened over Now Playing), not inside a
                // menu/dialog, not while a popover/modal is open
                if *this.router.current() == Route::NowPlaying && this.focus.contains_focused(window, cx) && !this.search.read(cx).focus_handle.is_focused(window) && !super::shell_keys::typing_or_in_overlay(window) && !this.overlay_open(cx) && !m.control && !m.alt && !m.platform && !m.shift {
                    let key = ev.keystroke.key.clone();
                    if this.np.update(cx, |n, cx| n.key(&key, cx)) {
                        cx.stop_propagation();
                    }
                }
            }))
            .on_action(cx.listener(|_, _: &PlayPause, _, cx| {
                app::call(cx, |b| {
                    let _ = b.player_play_pause();
                })
            }))
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| this.lists_search_key(ev, cx)))
            .on_action(cx.listener(|this, _: &ToggleNowPlaying, _, cx| this.toggle_now_playing(cx)))
            .on_action(cx.listener(|this, _: &ToggleTheme, _, cx| this.toggle_theme(cx)))
            .on_action(cx.listener(|this, _: &NavBack, _, cx| this.go_back(cx)))
            .on_action(cx.listener(|this, _: &NavForward, _, cx| this.go_forward(cx)))
            // L likes the current track, `/` opens search, mouse side buttons walk the history
            .on_action(cx.listener(|this, _: &super::shell_keys::LikeCurrent, _, cx| this.like_current(cx)))
            .on_action(cx.listener(|this, _: &super::shell_keys::SlashSearch, window, cx| this.slash_search(window, cx)))
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| this.feel_key_down(ev, window, cx)))
            .on_mouse_up(gpui::MouseButton::Navigate(gpui::NavigationDirection::Back), cx.listener(|this, _, _, cx| this.go_back(cx)))
            .on_mouse_up(gpui::MouseButton::Navigate(gpui::NavigationDirection::Forward), cx.listener(|this, _, _, cx| this.go_forward(cx)))
            .on_action(cx.listener(|_, _: &library::TabNext, window, _| {
                crate::widgets::text_input::note_tab_nav();
                crate::widgets::set_key_nav(true);
                window.focus_next()
            }))
            .on_action(cx.listener(|_, _: &library::TabPrev, window, _| {
                crate::widgets::text_input::note_tab_nav();
                crate::widgets::set_key_nav(true);
                window.focus_prev()
            }))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                let f = this.search.read(cx).focus_handle.clone();
                window.focus(&f);
                // Ctrl+K with an old query selects it so typing starts a new search
                let q = this.search.read(cx).content.to_string();
                this.search.update(cx, |i, cx| i.set_content(&q, cx));
            }))
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                if this.overlays.update(cx, |o, cx| o.escape(cx)) {
                    return;
                }
                if this.lists_editor_open(cx) {
                    this.lists_editor_close(window, cx);
                } else if this.menu.read(cx).is_open() {
                    this.menu.update(cx, |m, cx| m.close(window, cx));
                } else if this.lists_search_escape(cx) {
                } else if *this.router.current() == Route::NowPlaying {
                    this.go_back(cx);
                } else if this.queue_drawer {
                    this.queue_drawer = false;
                    cx.notify();
                } else {
                    let f = this.focus.clone();
                    window.focus(&f);
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(th.base)
            .text_color(th.text)
            .font_family(th.body_font)
            .text_size(px(13.))
            .line_height(px(18.))
            .child(crate::rcount::frame_phase_probe())
            .child(title)
            .children((!is_login).then_some(top))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .flex()
                    .flex_col()
                    .children(self.backdrop.clone().map(|b| {
                        // the canvas starts below title bar + top bar: on the login route there is no top bar
                        div().absolute().top(px(if is_login { crate::metrics::topbar::H } else { 0.0 })).left_0().right_0().bottom_0().child(img(ImageSource::Render(b)).size_full().object_fit(ObjectFit::Fill))
                    }))
                    .child(body)
                    .when(!on_np && !is_login, |d| d.child(self.dock_view())),
            )
            .child(self.live_view())
            .child(self.menu.clone())
            .children(self.lists_editor_overlay())
            .child(self.overlays.clone())
            .children(self.boot.clone())
            .child(crate::widgets::smooth::host())
            .child(crate::rcount::frame_end_probe())
    }
}

impl Shell {
    /// Empty stage for a library route whose page is not mounted yet (it is built on the next frame).
    fn placeholder(&self) -> AnyElement {
        div().size_full().into_any_element()
    }
}


impl Shell {
    fn on_np_event(&mut self, ev: &NpEvent, cx: &mut Context<Self>) {
        match ev {
            NpEvent::Close => self.go_back(cx),
            NpEvent::Navigate(r) => self.navigate(r.clone(), cx),
            NpEvent::Rate { id, rating } => {
                let (id, rating) = (id.clone(), *rating);
                app::call(cx, move |b| {
                    let _ = b.rating_set(asphonix_api::SetRatingRequest { id, kind: asphonix_api::StarTargetType::Track, rating });
                });
            }
            NpEvent::AddToPlaylist { id, at } => crate::overlays::add_to_playlist(cx, vec![id.clone()], *at),
            NpEvent::TrackInfo(t) => crate::overlays::track_info(cx, (**t).clone()),
        }
    }
}

impl Shell {
    /// The launch animation, once per app start.
    pub fn start_boot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.boot.is_some() {
            return;
        }
        let (th, bd) = (self.th, self.backdrop.clone());
        let e = cx.new(|_| super::boot::BootIntro::new(th, bd));
        let sub = cx.subscribe(&e, |this: &mut Shell, _, _: &super::boot::BootEvent, cx| {
            this.boot = None;
            this._boot_sub = None;
            cx.notify();
        });
        self._boot_sub = Some(sub);
        self.boot = Some(e);
        let _ = window;
        cx.notify();
    }
}

/// The open track-change card: at most one window, re-used for a newer track.
pub(crate) struct CardHandle {
    pub(crate) win: gpui::WindowHandle<super::overlay_card::CardView>,
    _sub: Subscription,
}

impl Shell {
    /// Opens (or re-uses) the card for `track` and arms the hide timer.
    pub fn show_card(&mut self, track: asphonix_api::Track, cx: &mut Context<Self>) {
        use super::overlay_card::{CardEvent, CardView, Corner, HEIGHT, MARGIN, WIDTH};
        self.card_gen += 1;
        let gen_ = self.card_gen;
        if let Some(c) = &self.card {
            let _ = c.win.update(cx, |v, _, cx| v.set_track(track, cx));
            self.schedule_card_hide(gen_, cx);
            return;
        }
        let Some(main) = self.main_hwnd else { return };
        let scale = self.scale.max(0.5);
        let (w, h_px, m) = ((WIDTH * scale) as i32, (HEIGHT * scale) as i32, (MARGIN * scale) as i32);
        let work = crate::winffi::work_area_of(main).unwrap_or((0, 0, 1920, 1040));
        let (x, y) = Corner::from_u8(self.track_overlay_corner).place(work, w, h_px, m);
        let (backend, art) = (self.backend.clone(), cx.global::<Services>().art.clone());
        let bounds = gpui::Bounds::new(gpui::point(px(x as f32 / scale), px(y as f32 / scale)), gpui::size(px(WIDTH), px(HEIGHT)));
        let opened = cx.open_window(
            gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions { title: Some("ASPHONIX".into()), appears_transparent: true, traffic_light_position: None }),
                focus: false,
                show: true,
                kind: gpui::WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: gpui::WindowBackgroundAppearance::Opaque,
                ..Default::default()
            },
            move |_, cx| cx.new(|_| CardView::new(backend, art, track, (x, y, w, h_px))),
        );
        let Ok(win) = opened else { return };
        let entity = win.entity(cx).ok();
        let sub = match entity {
            Some(e) => cx.subscribe(&e, |this: &mut Shell, _, _: &CardEvent, cx| this.open_main_from_card(cx)),
            None => return,
        };
        // show it now (hidden windows do not paint): no activation, topmost; the card itself starts at opacity 0
        let _ = win.update(cx, |_, window, _| {
            if let Some(h) = crate::winffi::hwnd_of(window) {
                crate::winffi::show_no_activate(h, x, y, w, h_px);
            }
        });
        self.card = Some(CardHandle { win, _sub: sub });
        self.schedule_card_hide(gen_, cx);
    }

    fn schedule_card_hide(&mut self, gen_: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(super::overlay_card::SHOW_FOR_MS)).await;
            let alive = this.update(cx, |t, cx| {
                if t.card_gen != gen_ {
                    return false;
                }
                if let Some(c) = &t.card {
                    let _ = c.win.update(cx, |v, _, cx| v.hide(cx));
                }
                true
            });
            if !matches!(alive, Ok(true)) {
                return;
            }
            cx.background_executor().timer(Duration::from_millis(super::overlay_card::FADE_MS)).await;
            let _ = this.update(cx, |t, cx| {
                if t.card_gen == gen_ {
                    t.close_card(cx);
                }
            });
        })
        .detach();
    }

    /// Destroys the card window now (nothing stays resident).
    pub fn close_card(&mut self, cx: &mut Context<Self>) {
        self.card_gen += 1;
        if let Some(c) = self.card.take() {
            let _ = c.win.update(cx, |_, window, _| window.remove_window());
        }
    }

    fn open_main_from_card(&mut self, cx: &mut Context<Self>) {
        self.close_card(cx);
        if let Some(h) = self.main_hwnd {
            crate::winffi::restore_foreground(h);
        }
    }

    /// `PlayerEvent` hook: `should_show` (track change into playback while ASPHONIX is not the focused window).
    fn card_on_player(&mut self, cx: &mut Context<Self>) {
        let st = self.player.read(cx).state.clone();
        let focused = self.main_active && !self.suspended;
        if super::overlay_card::should_show(self.prev_track_id.as_deref(), &st, self.track_overlay, focused)
            && let Some(t) = st.track.clone()
        {
            self.show_card(t, cx);
        }
        self.prev_track_id = st.track.as_ref().map(|t| t.id.clone());
    }
}

impl Shell {
    /// Settings > "Show track preview when ASPHONIX is in the background" and its corner.
    pub(crate) fn apply_track_overlay(&mut self, enabled: bool, corner: asphonix_api::OverlayCorner) {
        self.track_overlay = enabled;
        self.track_overlay_corner = match corner {
            asphonix_api::OverlayCorner::TopLeft => 0,
            asphonix_api::OverlayCorner::TopRight => 1,
            asphonix_api::OverlayCorner::BottomLeft => 2,
            asphonix_api::OverlayCorner::BottomRight => 3,
        };
    }

    /// Drop on a sidebar playlist: resolve the dragged tracks/album/artist, append them through `playlists_add_tracks`, toast.
    pub fn drop_on_playlist(&mut self, playlist_id: String, name: String, payload: crate::dnd::DragPayload, cx: &mut Context<Self>) {
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let p2 = payload.clone();
            let (n2, id2) = (name.clone(), playlist_id.clone());
            let msg = cx
                .background_executor()
                .spawn(async move {
                    let ids = crate::dnd::resolve_ids(&p2, &*b);
                    if ids.is_empty() {
                        return None;
                    }
                    let count = ids.len();
                    Some(match b.playlists_add_tracks(asphonix_api::AddToPlaylistRequest { id: id2, track_ids: ids }) {
                        Ok(()) => asphonix_i18n::tr!(ToastAddedToPlaylist, what = crate::dnd::describe(&p2, count), playlist = n2.as_str()),
                        Err(_) => asphonix_i18n::tr!(ToastAddFailed, playlist = n2.as_str()),
                    })
                })
                .await;
            let _ = this.update(cx, |sh, cx| {
                if let Some(m) = msg {
                    crate::overlays::show_toast(cx, &m);
                    // playlists changed: the sidebar counts and open playlist pages re-read
                    sh.lists_event(&super::lists::ListsEvent::PlaylistsChanged, cx);
                }
            });
        })
        .detach();
    }
}

/// The top bar search placeholder (upper-cased at render time, like the label-caps specs).
fn search_placeholder() -> gpui::SharedString {
    asphonix_i18n::tr!(SearchPlaceholder).to_uppercase().into()
}
#[cfg(test)]
#[path = "shell_tests.rs"]
mod gpui_tests;
