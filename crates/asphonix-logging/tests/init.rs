//! `init` end to end in its own process (the global subscriber can be installed once per process).
use std::sync::{Arc, Mutex};

#[test]
fn init_writes_json_lines_to_the_dated_file_bridges_log_and_reloads() {
    let dir = std::env::temp_dir().join(format!("asphonix-logging-init-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let fwd = Arc::new(Mutex::new(Vec::<String>::new()));
    let f2 = fwd.clone();
    let mut cfg = asphonix_logging::LogConfig::new(Some(dir.clone()), "asphonix", "9.9.9");
    cfg.forward = Some(Box::new(move |l| f2.lock().unwrap().push(l.to_string())));
    let guard = asphonix_logging::init(cfg);
    let file = guard.file().expect("file").to_path_buf();
    let root = guard.root().clone().entered();

    tracing::info!(target: "asphonix_core::audio::engine", track_id = "7", "playback started");
    tracing::debug!(target: "asphonix_core::audio::engine", "debug before verbose");
    log::warn!(target: "symphonia_core::probe", "a dependency still on log");
    log::debug!(target: "symphonia_core::probe", "filtered log debug");
    log::error!(target: "", "a record without a target");
    asphonix_logging::set_verbose(true);
    tracing::debug!(target: "asphonix_core::audio::engine", "debug after verbose");
    std::thread::Builder::new().name("worker".into()).spawn(|| tracing::info!(target: "asphonix_real", "from another thread")).unwrap().join().unwrap();
    asphonix_logging::set_verbose(false);
    tracing::debug!(target: "asphonix_core::audio::engine", "debug after quiet");
    drop(root);
    drop(guard); // flush

    let text = std::fs::read_to_string(&file).unwrap();
    let ev: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let find = |m: &str| ev.iter().find(|e| e["message"] == m).cloned();
    let start = find("logging started").expect("startup event");
    assert_eq!(start["spans"][0]["name"], "app");
    assert_eq!(start["spans"][0]["version"], "9.9.9");
    assert!(start["spans"][0]["os"].is_string() && start["spans"][0]["run_id"].is_string());
    let run_id = start["run_id"].clone();
    let play = find("playback started").unwrap();
    assert_eq!(play["fields"]["track_id"], "7");
    assert_eq!(play["spans"][0]["name"], "app", "root span on the main thread");
    let dep = find("a dependency still on log").expect("log bridge");
    assert_eq!(dep["target"], "symphonia_core::probe");
    assert_eq!(dep["level"], "WARN");
    assert!(dep["fields"].as_object().unwrap().is_empty(), "log.* bridge fields are dropped: {dep}");
    assert!(find("filtered log debug").is_none());
    assert_eq!(find("a record without a target").unwrap()["target"], "init", "empty target -> module path");
    assert!(find("debug before verbose").is_none());
    assert!(find("debug after verbose").is_some());
    assert!(find("debug after quiet").is_none());
    let other = find("from another thread").unwrap();
    assert_eq!(other["run_id"], run_id, "run_id on lines of every thread");
    assert_eq!(other["thread"], "worker");

    let fwd = fwd.lock().unwrap();
    assert!(fwd.contains(&"[asphonix_core::audio::engine] playback started track_id=7".to_string()), "{fwd:?}");
    assert!(!fwd.iter().any(|l| l.contains("debug")), "{fwd:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
