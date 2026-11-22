#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! SONIX: the GPUI desktop app. The UI talks to the library, player and server only through the
//! `sonix_api::Backend` trait, implemented by `sonix-real` on top of `sonix-core`.
mod anim;
mod app;
mod art;
mod clock;
mod columns;
mod dnd;
mod flow;
mod icons_gen;
mod metrics;
mod overlays;
mod menus;
#[cfg(feature = "memprof")]
mod memprof;
mod pager;
mod playback;
mod playerkeys;
mod live;
mod part;
mod rail_dnd;
mod rcount;
mod ticker;
mod perf;
mod router;
mod selection;
mod settings;
mod theme;
mod text;
mod tracking;
mod typo;
mod views;
mod widgets;
mod winffi;

#[cfg(test)]
mod tests;

use gpui::{
    App, AppContext, Application, AssetSource, Bounds, KeyBinding, Result, SharedString, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowOptions, px,
    size,
};
use sonix_api::Backend;
use std::borrow::Cow;
use std::io::Write;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use views::shell::{self, Shell};

#[cfg(feature = "memprof")]
#[global_allocator]
static ALLOC: memprof::Counting = memprof::Counting;

static T0: OnceLock<Instant> = OnceLock::new();

/// Append "<ms since main()> <msg> @<epoch ms>" to $SONIX_LOG (if set); the perf harness reads it.
pub fn log(msg: &str) {
    let ms = T0.get().map(|t| t.elapsed().as_secs_f64() * 1000.0).unwrap_or(0.0);
    let epoch = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    if let Ok(p) = std::env::var("SONIX_LOG")
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p)
    {
        let _ = writeln!(f, "{ms:.1} {msg} @{epoch}");
    }
}

/// `logs/crash.log` in the app's data dir (`%LOCALAPPDATA%\dev.sonix.app` on Windows).
fn crash_log_path() -> Option<std::path::PathBuf> {
    Some(sonix_real::data_dir().join("logs").join("crash.log"))
}

/// Panics abort the process (`panic = "abort"`, GUI subsystem: no console, stderr goes nowhere), so this hook is
/// the only place a panic leaves evidence: message, location, thread and a forced backtrace are appended to the
/// crash log (and to $SONIX_LOG). Nothing in it may panic (a panic inside the hook aborts without a trace).
fn install_crash_log() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>").to_string();
        let bt = std::backtrace::Backtrace::force_capture();
        let epoch = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let text = format!("=== panic @{epoch} (unix s) v{} thread '{name}'\n{info}\n{bt}\n", env!("CARGO_PKG_VERSION"));
        log(&format!("PANIC thread '{name}': {info}"));
        if let Some(p) = crash_log_path() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
                let _ = f.write_all(text.as_bytes());
                let _ = f.flush();
            }
        }
        let _ = std::io::stderr().write_all(text.as_bytes());
    }));
}

struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(icons_gen::icon(path).map(Cow::Borrowed))
    }
    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![])
    }
}

macro_rules! fonts {
    ($($f:literal),*) => { vec![$(Cow::Borrowed(&include_bytes!(concat!("../../../assets/fonts/", $f, ".ttf"))[..])),*] };
}

/// Forwards the backend crates' `log` records (info and up, `sonix_*` targets only) to $SONIX_LOG. Without a
/// logger every `log::info!` in sonix-core/sonix-real (session restore skipped, engine commands, sync) went nowhere.
struct BackendLog;
impl ::log::Log for BackendLog {
    fn enabled(&self, m: &::log::Metadata) -> bool {
        m.level() <= ::log::Level::Info && m.target().starts_with("sonix")
    }
    fn log(&self, r: &::log::Record) {
        if self.enabled(r.metadata()) {
            log(&format!("[{}] {}", r.target(), r.args()));
        }
    }
    fn flush(&self) {}
}

fn install_backend_log() {
    static L: BackendLog = BackendLog;
    if std::env::var_os("SONIX_LOG").is_some() && ::log::set_logger(&L).is_ok() {
        ::log::set_max_level(::log::LevelFilter::Info);
    }
}

fn main() {
    T0.set(Instant::now()).ok();
    install_crash_log();
    rcount::init();
    install_backend_log();
    log("main_start");
    #[cfg(feature = "memprof")]
    memprof::start();
    let prefs = settings::load();
    let backend: Arc<dyn Backend> = Arc::new(sonix_real::RealBackend::open());
    log("backend_ready");

    Application::new().with_assets(Assets).run(move |cx: &mut App| {
        log("app_run");
        cx.text_system()
            .add_fonts(fonts!(
                "inter-400", "inter-500", "inter-600", "inter-700", "jetbrains-mono-400", "jetbrains-mono-500", "jetbrains-mono-600", "jetbrains-mono-700",
                "space-grotesk-400", "space-grotesk-500", "space-grotesk-600", "space-grotesk-700"
            ))
            .expect("fonts");
        // first run (no stored choice): follow the OS app theme
        let os_light = !settings::has_stored() && matches!(cx.window_appearance(), gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight);
        cx.set_global(if prefs.light || os_light { theme::Theme::light() } else { theme::Theme::dark() });
        cx.set_global(app::Services { backend, art: art::new_handle(24 * 1024 * 1024) });
        widgets::text_input::bind_keys(cx);
        bind_keys(cx);
        open_main(cx);
        {
            // Exit hook: final session write + listen `quit` row (bounded), service teardown.
            let b = cx.global::<app::Services>().backend.clone();
            cx.on_app_quit(move |_| {
                let b = b.clone();
                async move { b.shutdown() }
            })
            .detach();
        }
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        log("open_window_returned");
    });
}

fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", shell::SelUp, Some("TrackList")),
        KeyBinding::new("down", shell::SelDown, Some("TrackList")),
        // Shift+arrows move the single selection like plain arrows (no range extension)
        KeyBinding::new("shift-up", shell::SelUp, Some("TrackList")),
        KeyBinding::new("shift-down", shell::SelDown, Some("TrackList")),
        KeyBinding::new("enter", shell::PlaySelected, Some("TrackList")),
        // Up/Down move a single selection, Enter plays, Shift+Enter enqueues. Home/End and Page keys are not bound.
        KeyBinding::new("shift-enter", shell::EnqueueSelected, Some("TrackList")),
        // select all: Album detail handles it; the Tracks route ignores the action
        KeyBinding::new("ctrl-a", shell::SelectAll, Some("TrackList")),
        KeyBinding::new("cmd-a", shell::SelectAll, Some("TrackList")),
        // album/artist cards (Tab stops; Enter/Space open, Menu / Shift+F10 the context menu) and Tab traversal
        KeyBinding::new("enter", views::library::cards::CardOpen, Some("Card")),
        KeyBinding::new("space", views::library::cards::CardOpen, Some("Card")),
        KeyBinding::new("menu", views::library::cards::CardMenu, Some("Card")),
        KeyBinding::new("shift-f10", views::library::cards::CardMenu, Some("Card")),
        KeyBinding::new("tab", views::library::TabNext, Some("Root")),
        KeyBinding::new("shift-tab", views::library::TabPrev, Some("Root")),
        KeyBinding::new("menu", shell::OpenMenuKey, Some("TrackList")),
        KeyBinding::new("shift-f10", shell::OpenMenuKey, Some("TrackList")),
        KeyBinding::new("space", shell::PlayPause, Some("Root && !TextInput && !ContextMenu && !Dialog")),
        // Shift+L likes too
        KeyBinding::new("shift-l", views::shell_keys::LikeCurrent, Some("Root && !TextInput && !ContextMenu && !Dialog")),
        // N (Now Playing) lives in `shell_keys::feel_key_down`: it ignores key repeat, which a binding cannot see
        KeyBinding::new("escape", shell::Escape, Some("Root")),
        KeyBinding::new("ctrl-t", shell::ToggleTheme, Some("Root")),
        KeyBinding::new("ctrl-k", shell::FocusSearch, Some("Root")),
        KeyBinding::new("alt-left", shell::NavBack, Some("Root")),
        KeyBinding::new("alt-right", shell::NavForward, Some("Root")),
        // L = like/unlike the current track (global, not while typing)
        KeyBinding::new("l", views::shell_keys::LikeCurrent, Some("Root && !TextInput && !ContextMenu && !Dialog")),
    ]);
}

fn open_main(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("SONIX".into()), appears_transparent: true, traffic_light_position: None }),
            show: false,
            focus: true,
            window_background: WindowBackgroundAppearance::Opaque,
            window_min_size: Some(size(px(1024.), px(640.))),
            app_id: Some(sonix_real::IDENTIFIER.into()),
            ..Default::default()
        },
        |window, cx| {
            log("window_created");
            // OS media controls (SMTC): registered on this (the window's) thread, 1.5 s after creation so it never
            // competes with the first show.
            #[cfg(windows)]
            if let Some(hwnd) = winffi::hwnd_of(window) {
                let backend = cx.global::<app::Services>().backend.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(std::time::Duration::from_millis(1500)).await;
                    let _ = cx.update(|_| backend.attach_window(hwnd));
                })
                .detach();
            }
            cx.new(|cx| Shell::new(window, cx))
        },
    )
    .expect("open window");
}
