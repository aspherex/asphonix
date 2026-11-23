//! OS integration / background services of the real backend. Offline: no server, no network, no audio.
//! Each test opens its backend on its own temp dirs (`open_in`); the process environment is never touched.
use asphonix_api::*;
use asphonix_real::RealBackend;
use std::time::{Duration, Instant};

/// Own config/data dirs under the system temp dir (override: `ASPHONIX_TEST_TMP`) (opened with `open_in`, no env vars), removed on drop.
struct Sandbox(std::path::PathBuf);
impl Sandbox {
    fn new(tag: &str) -> Self {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base = std::env::var_os("ASPHONIX_TEST_TMP").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("asphonix-services-{tag}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Sandbox(dir)
    }
    fn config(&self) -> std::path::PathBuf {
        self.0.join("config")
    }
    fn open(&self) -> RealBackend {
        RealBackend::open_in(self.config(), self.0.join("data"))
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        // background threads may still release the DB for a moment
        let t = Instant::now();
        while std::fs::remove_dir_all(&self.0).is_err() && self.0.exists() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn updates_are_disabled_by_default_and_never_install() {
    let sb = Sandbox::new("updates");
    let b = sb.open();
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
    let sb = Sandbox::new("logout");
    let b = sb.open();
    let t = Instant::now();
    b.auth_logout().unwrap(); // Stop + FlushListen before the wipe
    b.shutdown(); // persist_now (also writes the `quit` listen row when a session is open)
    assert!(t.elapsed() < Duration::from_secs(5), "flush/exit are bounded: {:?}", t.elapsed());
    assert_eq!(b.auth_session_status().unwrap().state, ConnectionState::Disconnected);
}
