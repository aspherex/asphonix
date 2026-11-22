//! The app shell (`App.svelte`): title bar, top bar, sidebar, stage (route view), queue
//! rail / drawer, dock, the shared context-menu overlay, plus suspend/resume on minimize.
use super::dock::{DockEvent, DockView};
use super::library::{self, LibEvent, NavBus, NavHandle};
use super::nowplaying::{NowPlayingView, NpEvent};
use super::queue_rail::QueueRail;
use super::tracks::{TracksEvent, TracksView};
use crate::app::{self, PlayerModel, Services};
use crate::perf::FrameStats;
use crate::router::{Route, Router};
#[cfg(not(feature = "parity"))]
use crate::settings::{self, UiPrefs};
use crate::theme::{MONO, Theme};
use crate::tracking::tracked;
use crate::typo::ts;
use crate::widgets::context_menu::ContextMenu;
use crate::widgets::text_input::{InputEvent, TextInput};
use futures::StreamExt;
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, ImageSource, IntoElement, ObjectFit, Render, RenderImage, SharedString, StyledImage, Subscription, Window, actions,
    div, img, prelude::*, px,
};
use sonix_api::{Backend, Event, Playlist, SessionStatus, format as fmt};
use std::sync::Arc;
use std::time::{Duration, Instant};

actions!(
    sonix,
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
    /// `updateStore.info`: the newer release found by `updates_check` (title-bar badge, UpdateDialog).
    pub(crate) update: Option<sonix_api::UpdateInfo>,
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
    /// `uiState.queueRailCollapsed`: the docked rail (>= 1440 px) hidden by the dock's queue button.
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
        let search = cx.new(|cx| TextInput::new(cx, "SEARCH ARCHIVE", &th, MONO, 12.0, 16.0, 500.0));
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
                // `uiState.toggleQueueRail`: collapse the docked rail, or open/close the compact drawer
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
            // `playFromQueue` reloads the list, which drops the focus it held: back to the page
            super::queue_rail::RailEvent::Played => {
                let f = this.focus.clone();
                window.focus(&f);
            }
        }));
        subs.push(cx.observe_window_activation(window, |this: &mut Shell, window, _cx| this.main_active = window.is_window_active()));
        subs.push(cx.subscribe(&np, |this: &mut Shell, _, ev: &NpEvent, cx| this.on_np_event(ev, cx)));
        // `TopBar.svelte: onSearchFocus`: focusing the search box navigates to the Search route
        let search_focus = search.read(cx).focus_handle.clone();
        subs.push(cx.on_focus(&search_focus, window, |this: &mut Shell, _, cx| this.navigate(Route::Search, cx)));
        subs.push(cx.subscribe(&tracks, |this: &mut Shell, _, ev: &TracksEvent, cx| match ev {
            TracksEvent::Navigate(r) => this.navigate(r.clone(), cx),
        }));

        let mut this = Self {
            th,
            router: Router::new(Route::Tracks),
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
            track_overlay: !cfg!(feature = "parity"),
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
            fs: FrameStats::new(std::env::var("SONIX_FPSLOG").is_ok() || std::env::var("SONIX_SCRIPT").is_ok()),
            _subs: subs,
        };
        this.lists_init(cx);
        this.load_backdrop();
        this.start_event_pump(cx);
        this.load_static(cx);
        this.startup_login_check(cx);
        this.install_minimize_hook(window, cx);
        if let Some(r) = std::env::var("SONIX_ROUTE").ok().and_then(|r| Route::parse(&r)) {
            this.router.navigate(r);
            this.route_changed(cx);
        }
        this.debug_hooks(window, cx);
        #[cfg(feature = "parity")]
        this.parity_attach(window, cx);
        this
    }

    // -------------------------------------------------------------- scripted / debug hooks
    /// Env-driven hooks for screenshots and the perf harness (never set in normal use).
    fn debug_hooks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::env::var("SONIX_PLAY").is_ok() {
            app::call(cx, |b| {
                let _ = b.player_play_all(sonix_api::PlayAllRequest {
                    order: sonix_api::TrackOrder::Sort { field: sonix_api::TrackSortField::Title, dir: sonix_api::SortDir::Asc },
                    filter: None,
                    start_track_id: Some("tr_7".into()),
                    flow: None,
                });
            });
        }
        if let Ok(n) = std::env::var("SONIX_MENU") {
            let ix: usize = n.parse().unwrap_or(2);
            cx.spawn_in(window, async move |this, cx| {
                gpui::Timer::after(Duration::from_millis(1500)).await;
                let _ = this.update_in(cx, |sh, window, cx| {
                    sh.tracks.update(cx, |t, cx| t.debug_menu(ix, window, cx));
                });
            })
            .detach();
        }
        if std::env::var("SONIX_SCRIPT").is_ok() {
            self.run_script(cx);
        }
    }

    fn log_frames(&mut self, name: &str) {
        if let Some(s) = self.fs.summary() {
            crate::log(&s.line(name));
        }
        self.fs.reset();
    }

    /// idle 5 s -> scroll the whole list -> idle 4 s -> Now Playing 60 s -> back to Tracks.
    fn run_script(&mut self, cx: &mut Context<Self>) {
        if std::env::var("SONIX_SCRIPT").is_ok_and(|v| v == "real") {
            return self.run_script_real(cx);
        }
        cx.spawn(async move |this, cx| {
            let t = |ms: u64| gpui::Timer::after(Duration::from_millis(ms));
            t(1500).await;
            crate::log("phase idle_start");
            t(5000).await;
            crate::log("phase idle_end");
            let _ = this.update(cx, |sh, _| sh.fs.reset());
            crate::log("phase scroll_start");
            let total = 54_515usize;
            let mut ix = 0usize;
            while ix < total - 1 {
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
                    let _ = b.player_play_all(sonix_api::PlayAllRequest {
                        order: sonix_api::TrackOrder::Sort { field: sonix_api::TrackSortField::Title, dir: sonix_api::SortDir::Asc },
                        filter: None,
                        start_track_id: Some("tr_7".into()),
                        flow: None,
                    });
                });
                sh.navigate(Route::NowPlaying, cx);
                sh.fs.reset();
            });
            crate::log("phase np_start");
            t(10_000).await;
            let _ = this.update(cx, |sh, _| sh.fs.reset());
            t(50_000).await;
            let _ = this.update(cx, |sh, _| sh.log_frames("np"));
            crate::log("phase np_60s");
            let _ = this.update(cx, |sh, cx| sh.navigate(Route::Tracks, cx));
            t(500).await;
            crate::log("phase done");
        })
        .detach();
    }

    /// Real-backend memory run (`SONIX_SCRIPT=real`, no input synthesis, no focus needed): idle -> scroll the library ->
    /// play (queue restored from the session) -> Now Playing -> back; the harness minimizes/restores the window itself.
    fn run_script_real(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            for _ in 0..50 {
                gpui::Timer::after(Duration::from_millis(1000)).await;
                let n = this.update(cx, |sh, _| sh.fs.intervals_ms.len()).unwrap_or(0);
                let _ = this.update(cx, |sh, _| sh.fs.reset());
                crate::log(&format!("root renders/s={n}"));
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let t = |ms: u64| gpui::Timer::after(Duration::from_millis(ms));
            t(8000).await;
            crate::log("phase idle_start");
            t(5000).await;
            crate::log("phase idle_end");
            crate::log("phase scroll_start");
            let mut ix = 0usize;
            while ix < 2000 && std::env::var_os("SONIX_NOSCROLL").is_none() {
                ix += 12;
                if this.update(cx, |sh, cx| sh.tracks.update(cx, |t, cx| t.script_scroll(ix, cx))).is_err() {
                    return;
                }
                t(16).await;
            }
            crate::log("phase scroll_done");
            t(3000).await;
            crate::log("phase post_scroll_idle");
            t(2000).await;
            let _ = this.update(cx, |_, cx| {
                app::call(cx, |b| {
                    let _ = b.player_play();
                });
            });
            t(4000).await;
            crate::log("phase playing");
            t(2000).await;
            let _ = this.update(cx, |sh, cx| sh.navigate(Route::NowPlaying, cx));
            t(4000).await;
            crate::log("phase np_start");
            t(12_000).await;
            crate::log("phase np_end");
            let _ = this.update(cx, |sh, cx| sh.navigate(Route::Tracks, cx));
            t(2500).await;
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
            let q = cx.background_executor().spawn(async move { b.player_get_queue(sonix_api::PageRequest { offset: 0, limit: 100 }) }).await;
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
                // subscriber never ran: the card must still show then, that is when it is most useful (overlay.rs).
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
                        self.queue.update(cx, |_, cx| cx.notify());
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

    /// `App.svelte` onMount: only a session without stored credentials (`disconnected`), or a failing
    /// status call, opens the login screen; `connecting`/`error` keep browsing the local library.
    fn startup_login_check(&mut self, cx: &mut Context<Self>) {
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let st = cx.background_executor().spawn(async move { b.auth_session_status() }).await;
            let to_login = st.map(|s| s.state == sonix_api::ConnectionState::Disconnected).unwrap_or(true);
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
                        b.browse_get_albums(sonix_api::GetAlbumsRequest { offset: 0, limit: 1, sort: sonix_api::AlbumSort::Alphabetical, filter: None }),
                        b.browse_get_artists(sonix_api::GetArtistsRequest { offset: 0, limit: 1, sort: None }),
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
    pub fn navigate(&mut self, r: Route, cx: &mut Context<Self>) {
        if self.router.navigate(r) {
            self.route_changed(cx);
        }
    }
    pub fn go_back(&mut self, cx: &mut Context<Self>) {
        if self.router.back() {
            self.route_changed(cx);
        }
    }
    pub fn go_forward(&mut self, cx: &mut Context<Self>) {
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
        // Starred Crates remounts on every visit (`onMount(() => pager.reset())`)
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
                    // The web remounts the route (fresh queries); the parked page re-queries in place instead (no
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
        #[cfg(not(feature = "parity"))]
        settings::save(&UiPrefs { light: th.light });
        self.search.update(cx, |i, _| i.set_theme(&th));
        self.menu.update(cx, |m, _| m.set_theme(th));
        self.tracks.update(cx, |t, _| t.set_theme(&th));
        if !self.suspended {
            self.load_backdrop();
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
        // `onVisibleSpectrumFrame`: the backend computes the spectrum only while Now Playing or the rail is on screen.
        let want_spectrum = !self.suspended && (on_np || docked || (self.queue_drawer && !on_np));
        if want_spectrum != self.spectrum_on {
            self.spectrum_on = want_spectrum;
            app::call(cx, move |b| {
                let _ = b.player_set_spectrum_active(want_spectrum);
            });
        }

        self.fs.tick();
        crate::probe::begin_frame();
        if !self.shown {
            self.shown = true;
            crate::log("first_render");
            window.activate_window();
            #[cfg(not(feature = "parity"))]
            self.start_boot(window, cx);
        }
        if let Some(t) = self.resumed_at.take() {
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            crate::log(&format!("restore_first_render {ms:.1}ms after resume"));
        }

        let stage: AnyElement = match &route {
            Route::Tracks => self.tracks.clone().into_any_element(),
            Route::NowPlaying => self.np.clone().into_any_element(),
            r if super::lists::ListsHost::handles(r) => self.lists_stage(r, window, cx),
            other => match &self.lib_stage {
                Some(v) => v.clone().into_any_element(),
                None => self.placeholder(other, window),
            },
        };

        let title = crate::probe::pid("titlebar", self.title_bar(window, cx));
        let top = crate::probe::pid("topbar", self.top_bar(window, cx));
        let side = crate::probe::pid("sidebar", self.sidebar(vw, &route, window, cx));

        // `App.svelte`: the login route has only the title bar and its own stage (no top bar, sidebar, rail, dock)
        let is_login = route == Route::Login;
        let stage_id = if is_login { "login.stage" } else { "stage" };
        let body = div()
            .flex_1()
            .flex()
            .flex_row()
            .min_h_0()
            .relative()
            .children((!is_login).then_some(side))
            .child(crate::probe::pid(stage_id, div().flex_1().min_w_0().h_full().overflow_hidden().relative().child(stage_probe()).child(stage)))
            .when(docked, |d| d.child(self.queue.clone()))
            .when(!docked && !on_np && self.queue_drawer, |d| {
                d.child(div().absolute().top_0().right_0().bottom_0().child(self.queue.clone()))
            });

        let root = div()
            .id("root")
            .capture_any_mouse_down(cx.listener(|_, _, _, _| crate::widgets::set_key_nav(false)))
            .key_context("Root")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                let m = ev.keystroke.modifiers;
                // `shortcuts.ts`: not while typing (any text field, e.g. a prompt opened over Now Playing), not inside a
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
            // `shortcuts.ts`: L likes the current track, `/` opens search, mouse side buttons walk the history
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
                        // the canvas starts below title bar + top bar (`--chrome-top`): on the login route there is no top bar
                        div().absolute().top(px(if is_login { crate::metrics::topbar::H } else { 0.0 })).left_0().right_0().bottom_0().child(img(ImageSource::Render(b)).size_full().object_fit(ObjectFit::Fill))
                    }))
                    .child(crate::probe::pid(if is_login { "login.body" } else { "shell.body" }, body))
                    .when(!on_np && !is_login, |d| d.child(self.dock.clone())),
            )
            .child(self.menu.clone())
            .children(self.lists_editor_overlay())
            .child(self.overlays.clone())
            .children(self.boot.clone())
            .child(crate::widgets::smooth::host());
        #[cfg(feature = "parity")]
        let root = root.child(crate::probe::frame_marker());
        root
    }
}

impl Shell {
    /// Stage for routes that are not ported yet: honest, named, with the porting status.
    fn placeholder(&self, r: &Route, window: &mut Window) -> AnyElement {
        let th = self.th;
        let name = r.path();
        let head: SharedString = format!("{} // NOT PORTED YET", name.to_uppercase()).into();
        let heading = tracked(&head, crate::tracking::HEADLINE_MD, th.body_font, th.text, window);
        div()
            .size_full()
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(heading)
            .child(ts(&th, crate::tracking::BODY_SM).text_color(th.muted).child("This route exists in the Svelte app and is tracked in docs/PORTING.md; phase 1 ports Tracks and Now Playing."))
            .child(ts(&th, crate::tracking::MONO_CODE).text_color(th.muted).child(SharedString::from(format!("{} tracks in the mock library", self.counts.tracks.map(|n| fmt::fmt_int(n as u64)).unwrap_or_else(|| "—".into())))))
            .into_any_element()
    }
}


impl Shell {
    fn on_np_event(&mut self, ev: &NpEvent, cx: &mut Context<Self>) {
        match ev {
            NpEvent::Close => self.go_back(cx),
            NpEvent::Navigate(r) => self.navigate(r.clone(), cx),
            NpEvent::Rate { id, rating, title } => {
                let (id, rating) = (id.clone(), *rating);
                let set = move |cx: &mut gpui::App| {
                    let id = id.clone();
                    app::call(cx, move |b| {
                        let _ = b.rating_set(sonix_api::SetRatingRequest { id, kind: sonix_api::StarTargetType::Track, rating });
                    });
                };
                if rating == 1 {
                    // 1 star files are purged nightly: never a one-click misfire
                    crate::overlays::confirm(cx, "Rate 1 star?", &format!("\"{title}\" will be purged from the library by the nightly cleanup."), "Rate 1★", true, set);
                } else {
                    set(cx);
                }
            }
            NpEvent::AddToPlaylist { id, at } => crate::overlays::add_to_playlist(cx, vec![id.clone()], *at),
            NpEvent::TrackInfo(t) => crate::overlays::track_info(cx, (**t).clone()),
        }
    }
}

impl Shell {
    /// `App.svelte showIntro`: the launch animation, once per app start (parity: only on request, the web side loads with `?resumed=1`).
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

/// The open track-change card (`overlay.rs`): at most one window, re-used for a newer track.
pub(crate) struct CardHandle {
    pub(crate) win: gpui::WindowHandle<super::overlay_card::CardView>,
    _sub: Subscription,
}

impl Shell {
    /// `overlay.rs show`: open (or re-use) the card for `track` and arm the hide timer.
    pub fn show_card(&mut self, track: sonix_api::Track, cx: &mut Context<Self>) {
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
                titlebar: Some(gpui::TitlebarOptions { title: Some("SONIX".into()), appears_transparent: true, traffic_light_position: None }),
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
        if cfg!(feature = "parity") {
            return; // the runner drives time itself
        }
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

    /// `dismiss`: destroy the card window now (nothing stays resident).
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

    /// `PlayerEvent` hook: `should_show` (track change into playback while SONIX is not the focused window).
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
    /// Settings > "Show track preview when SONIX is in the background" and its corner (`settings.trackOverlay*`).
    /// Parity builds never open the second window (the harness captures one window only).
    pub(crate) fn apply_track_overlay(&mut self, enabled: bool, corner: sonix_api::OverlayCorner) {
        self.track_overlay = enabled && !cfg!(feature = "parity");
        self.track_overlay_corner = match corner {
            sonix_api::OverlayCorner::TopLeft => 0,
            sonix_api::OverlayCorner::TopRight => 1,
            sonix_api::OverlayCorner::BottomLeft => 2,
            sonix_api::OverlayCorner::BottomRight => 3,
        };
    }

    /// `Sidebar.svelte onPlaylistDrop`: resolve the dragged tracks/album/artist, append them through `playlists_add_tracks`, toast.
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
                    Some(match b.playlists_add_tracks(sonix_api::AddToPlaylistRequest { id: id2, track_ids: ids }) {
                        Ok(()) => format!("Added {} to \"{}\"", crate::dnd::describe(&p2, count), n2),
                        Err(_) => format!("Could not add to \"{}\"", n2),
                    })
                })
                .await;
            let _ = this.update(cx, |sh, cx| {
                if let Some(m) = msg {
                    crate::overlays::show_toast(cx, &m);
                    // `notifyPlaylistsChanged()`: the sidebar counts and open playlist pages re-read
                    sh.lists_event(&super::lists::ListsEvent::PlaylistsChanged, cx);
                }
            });
        })
        .detach();
    }
}
