//! OS integration / background services of the real backend. Offline: no server, no network, no audio.
//! One shared sandbox dir for APPDATA/LOCALAPPDATA (the backend reads them on every call), set once.
use sonix_api::*;
use sonix_real::RealBackend;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

fn sandbox() -> &'static Mutex<()> {
    static S: OnceLock<Mutex<()>> = OnceLock::new();
    S.get_or_init(|| {
        let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("services-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        unsafe {
            std::env::set_var("APPDATA", &dir);
            std::env::set_var("LOCALAPPDATA", &dir);
        }
        Mutex::new(())
    })
}

#[test]
fn updates_are_disabled_by_default_and_never_install() {
    let _g = sandbox().lock().unwrap_or_else(|p| p.into_inner());
    let b = RealBackend::open();
    let rx = b.subscribe();
    let info = b.updates_check().unwrap();
    assert!(!info.available);
    assert!(!info.current_version.is_empty(), "current version is reported even without a channel");
    assert!(rx.try_recv().is_err(), "no UpdateAvailable event");
    // Install without an update (the built-in channel is empty): refused, nothing downloaded or run.
    assert!(b.updates_install().is_err());
    // Auto-check on a disabled channel starts nothing.
    b.set_auto_check_updates(true);
}

#[test]
fn logout_and_exit_flush_the_listen_writer_promptly() {
    let _g = sandbox().lock().unwrap_or_else(|p| p.into_inner());
    let b = RealBackend::open();
    let t = Instant::now();
    b.auth_logout().unwrap(); // Stop + FlushListen before the wipe
    b.shutdown(); // persist_now (also writes the `quit` listen row when a session is open)
    assert!(t.elapsed() < Duration::from_secs(5), "flush/exit are bounded: {:?}", t.elapsed());
    assert_eq!(b.auth_session_status().unwrap().state, ConnectionState::Disconnected);
}
