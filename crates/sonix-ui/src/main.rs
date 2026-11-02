#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! SONIX native: the GPUI desktop UI. Phase 1 runs against `sonix-mock` through the
//! `sonix_api::Backend` seam; phase 2 swaps in a real backend built on `sonix-core`.
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
mod pager;
mod playback;
mod playerkeys;
#[cfg(feature = "parity")]
mod parity;
mod perf;
mod probe;
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

/// `%LOCALAPPDATA%\dev.sonix.native\logs\crash.log` (the main window's app id).
fn crash_log_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share")))?;
    Some(base.join("dev.sonix.native").join("logs").join("crash.log"))
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

fn main() {
    T0.set(Instant::now()).ok();
    install_crash_log();
    log("main_start");
    #[cfg(not(feature = "parity"))]
    let prefs = settings::load();
    #[cfg(feature = "parity")]
    let prefs = settings::UiPrefs { light: false };
    #[cfg(not(feature = "parity"))]
    let backend: Arc<dyn Backend> = if std::env::args().any(|a| a == "--mock") || std::env::var_os("SONIX_MOCK").is_some() { Arc::new(sonix_mock::MockBackend::new()) } else { Arc::new(sonix_real::RealBackend::open()) };
    // Parity harness: deterministic mock (fixed seed, manual clock) when the runner asks for it.
    #[cfg(feature = "parity")]
    let backend: Arc<dyn Backend> = {
        let manual = Arc::new(sonix_mock::MockBackend::new_manual());
        if parity::start(manual.clone()) { manual } else { Arc::new(sonix_mock::MockBackend::new()) }
    };
    log("backend_ready");

    Application::new().with_assets(Assets).run(move |cx: &mut App| {
        log("app_run");
        cx.text_system()
            .add_fonts(fonts!(
                "inter-400", "inter-500", "inter-600", "inter-700", "jetbrains-mono-400", "jetbrains-mono-500", "jetbrains-mono-600", "jetbrains-mono-700",
                "space-grotesk-400", "space-grotesk-500", "space-grotesk-600", "space-grotesk-700"
            ))
            .expect("fonts");
        cx.set_global(if prefs.light || std::env::var("SONIX_LIGHT").is_ok() { theme::Theme::light() } else { theme::Theme::dark() });
        cx.set_global(app::Services { backend, art: art::new_handle(24 * 1024 * 1024) });
        widgets::text_input::bind_keys(cx);
        bind_keys(cx);
        open_main(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.global::<app::Services>().backend.shutdown();
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
        // the web handler ignores Shift on arrows (plain single-selection move)
        KeyBinding::new("shift-up", shell::SelUp, Some("TrackList")),
        KeyBinding::new("shift-down", shell::SelDown, Some("TrackList")),
        KeyBinding::new("enter", shell::PlaySelected, Some("TrackList")),
        // `trackList.ts: trackListKeydown`: Up/Down move a single selection, Enter plays, Shift+Enter enqueues. The Tracks
        // route passes no `selectAll`, so Ctrl+A, Shift+arrows, Home/End and Page keys do nothing there (parity-verified).
        KeyBinding::new("shift-enter", shell::EnqueueSelected, Some("TrackList")),
        // `trackListKeydown`'s `selectAll` (Album detail passes it; the Tracks route does not and ignores the action)
        KeyBinding::new("ctrl-a", shell::SelectAll, Some("TrackList")),
        KeyBinding::new("cmd-a", shell::SelectAll, Some("TrackList")),
        // album/artist cards (`tabindex=0`, Enter/Space open, Menu / Shift+F10 the context menu) and Tab traversal
        KeyBinding::new("enter", views::library::cards::CardOpen, Some("Card")),
        KeyBinding::new("space", views::library::cards::CardOpen, Some("Card")),
        KeyBinding::new("menu", views::library::cards::CardMenu, Some("Card")),
        KeyBinding::new("shift-f10", views::library::cards::CardMenu, Some("Card")),
        KeyBinding::new("tab", views::library::TabNext, Some("Root && !TextInput")),
        KeyBinding::new("shift-tab", views::library::TabPrev, Some("Root && !TextInput")),
        KeyBinding::new("menu", shell::OpenMenuKey, Some("TrackList")),
        KeyBinding::new("shift-f10", shell::OpenMenuKey, Some("TrackList")),
        KeyBinding::new("space", shell::PlayPause, Some("Root && !TextInput")),
        KeyBinding::new("n", shell::ToggleNowPlaying, Some("Root && !TextInput")),
        KeyBinding::new("escape", shell::Escape, Some("Root")),
        KeyBinding::new("ctrl-t", shell::ToggleTheme, Some("Root")),
        KeyBinding::new("ctrl-k", shell::FocusSearch, Some("Root")),
        KeyBinding::new("alt-left", shell::NavBack, Some("Root")),
        KeyBinding::new("alt-right", shell::NavForward, Some("Root")),
    ]);
}

fn open_main(cx: &mut App) {
    let (w, h) = std::env::var("SONIX_SIZE")
        .ok()
        .and_then(|s| s.split_once('x').and_then(|(a, b)| Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?))))
        .unwrap_or((1440.0, 900.0));
    let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("SONIX".into()), appears_transparent: true, traffic_light_position: None }),
            show: false,
            focus: true,
            window_background: WindowBackgroundAppearance::Opaque,
            window_min_size: Some(size(px(1024.), px(640.))),
            app_id: Some("dev.sonix.native".into()),
            ..Default::default()
        },
        |window, cx| {
            log("window_created");
            cx.new(|cx| Shell::new(window, cx))
        },
    )
    .expect("open window");
}
