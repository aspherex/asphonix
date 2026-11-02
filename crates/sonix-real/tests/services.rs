//! OS integration / background services of the real backend. Offline: no server, no network, no audio.
//! Each test opens its backend on its own temp dirs (`open_in`); the process environment is never touched.
use sonix_api::*;
use sonix_real::RealBackend;
use std::time::{Duration, Instant};

/// Own config/data dirs under `<build-dir>/tmp` (opened with `open_in`, no env vars), removed on drop.
struct Sandbox(std::path::PathBuf);
impl Sandbox {
    fn new(tag: &str) -> Self {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base = if std::path::Path::new("<build-dir>").exists() { std::path::PathBuf::from("<build-dir>/tmp") } else { std::env::temp_dir() };
        let dir = base.join(format!("sonix-services-{tag}-{}-{seq}", std::process::id()));
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

#[test]
fn mcp_is_off_by_default_and_enable_disable_manage_the_discovery_file() {
    let sb = Sandbox::new("mcp");
    let b = sb.open();
    let discovery = sb.config().join("mcp.json");
    let _ = std::fs::remove_file(&discovery);
    let st = b.mcp_status().unwrap();
    assert!(!st.enabled && !st.allow_control && !st.listening, "off by default: {st:?}");
    assert!(!discovery.exists(), "nothing written until enabled");
    assert!(st.register_command.as_deref().is_some_and(|c| c.starts_with("claude mcp add sonix-native -- ")));

    let st = b.set_mcp_enabled(true).unwrap();
    assert!(st.enabled && st.listening && !st.allow_control);
    let text = std::fs::read_to_string(&discovery).expect("mcp.json written on enable");
    let d: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(d["endpoint"].as_str().unwrap().starts_with(r"\\.\pipe\sonix-native-mcp-"), "own pipe name, not the installed app's");
    assert_eq!(d["token"].as_str().unwrap().len(), 64);
    assert!(b.mcp_status().unwrap().enabled);

    let st = b.set_mcp_allow_control(true).unwrap();
    assert!(st.allow_control);

    let st = b.set_mcp_enabled(false).unwrap();
    assert!(!st.enabled && !st.listening);
    assert!(!discovery.exists(), "mcp.json removed on disable");

    // Exit removes the discovery file too (reference `remove_discovery_on_exit`).
    b.set_mcp_enabled(true).unwrap();
    assert!(discovery.exists());
    b.shutdown();
    assert!(!discovery.exists());
    b.set_mcp_enabled(false).unwrap();
}

#[test]
fn mcp_end_to_end_over_the_real_pipe_with_both_gates() {
    let sb = Sandbox::new("e2e");
    let b = sb.open();
    let cfg = sonix_mcp::Config { config_dir: Some(sb.config()), call_timeout: Duration::from_secs(10) };
    // Not enabled: no discovery file, the bridge client says so and nothing listens.
    let _ = std::fs::remove_file(sb.config().join("mcp.json"));
    assert!(matches!(sonix_mcp::client::call_app(&cfg, "library_stats", &serde_json::json!({})), Err(sonix_mcp::client::CallError::NotEnabled)));

    b.set_mcp_enabled(true).unwrap();
    // Read tool over the real named pipe (handshake: server proof, then token) against this backend's own (empty) library.
    let stats = sonix_mcp::client::call_app(&cfg, "library_stats", &serde_json::json!({})).expect("library_stats");
    assert_eq!(stats["tracks"], 0, "{stats}");
    // Control tools are refused until 'Allow playback control' is on.
    let refused = sonix_mcp::client::call_app(&cfg, "play_pause", &serde_json::json!({ "action": "pause" }));
    assert!(refused.is_err(), "control is off by default");
    b.set_mcp_allow_control(true).unwrap();
    assert!(sonix_mcp::client::call_app(&cfg, "play_pause", &serde_json::json!({ "action": "pause" })).is_ok());
    // Disabling closes the listener and removes the file.
    b.set_mcp_enabled(false).unwrap();
    assert!(matches!(sonix_mcp::client::call_app(&cfg, "library_stats", &serde_json::json!({})), Err(sonix_mcp::client::CallError::NotEnabled)));
}
