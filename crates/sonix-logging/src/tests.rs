use super::*;
use crate::test_support::{capture, capture_with_filter};
use std::io::Write as _;
use std::sync::{Arc, Mutex};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sonix-logging-{tag}-{}-{}", std::process::id(), new_run_id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn every_line_is_json_with_the_schema_fields() {
    let c = capture_with_filter(false);
    let span = tracing::info_span!(target: "sonix_core::audio", "playback", track_id = "t-1");
    span.in_scope(|| {
        tracing::info!(target: "sonix_core::audio::engine", track_id = "t-1", duration_ms = 12u64, ok = true, "track \"started\"");
        tracing::warn!(target: "sonix_core::library", error = %"disk full", "sync failed");
    });
    let ev = c.events();
    assert_eq!(ev.len(), 2, "{}", c.text());
    let e = &ev[0];
    let ts = e["ts"].as_str().unwrap();
    assert_eq!(ts.len(), 24, "{ts}");
    assert!(ts.ends_with('Z') && ts.as_bytes()[10] == b'T', "{ts}");
    assert_eq!(e["level"], "INFO");
    assert_eq!(e["target"], "sonix_core::audio::engine");
    assert_eq!(e["message"], "track \"started\"");
    assert_eq!(e["fields"]["track_id"], "t-1");
    assert_eq!(e["fields"]["duration_ms"], 12);
    assert_eq!(e["fields"]["ok"], true);
    assert!(e["fields"].get("message").is_none());
    assert_eq!(e["spans"][0]["name"], "playback");
    assert_eq!(e["spans"][0]["track_id"], "t-1");
    assert_eq!(e["run_id"], "test-run");
    assert_eq!(ev[1]["level"], "WARN");
    assert_eq!(ev[1]["fields"]["error"], "disk full");
}

#[test]
fn an_event_without_fields_still_has_a_fields_object() {
    let c = capture_with_filter(false);
    tracing::info!(target: "sonix_real", "plain");
    let e = c.find("plain").unwrap();
    assert!(e["fields"].as_object().unwrap().is_empty());
    assert!(e.get("spans").is_none());
}

#[test]
fn debug_is_dropped_by_default_and_kept_after_switching_to_verbose() {
    let c = capture_with_filter(false);
    tracing::debug!(target: "sonix_core::scrobble", "quiet");
    tracing::info!(target: "sonix_core::scrobble", "loud");
    tracing::info!(target: "hyper::client", "dependency info");
    tracing::warn!(target: "hyper::client", "dependency warn");
    assert!(c.find("quiet").is_none());
    assert!(c.find("loud").is_some());
    assert!(c.find("dependency info").is_none(), "other crates log at warn and up only");
    assert!(c.find("dependency warn").is_some());

    c.control().set_verbose(true);
    tracing::debug!(target: "sonix_core::scrobble", "now visible");
    tracing::debug!(target: "hyper::client", "dependency debug");
    assert!(c.find("now visible").is_some(), "{}", c.text());
    assert!(c.find("dependency debug").is_none(), "verbose only widens our crates");

    c.control().set_verbose(false);
    tracing::debug!(target: "sonix_core::scrobble", "hidden again");
    assert!(c.find("hidden again").is_none());
}

#[test]
fn the_sonix_directive_covers_every_crate_prefix() {
    let c = capture_with_filter(false);
    tracing::info!(target: "probe", "not ours");
    tracing::info!(target: "sonix_core::x", "a");
    tracing::info!(target: "sonix_real::art", "b");
    tracing::info!(target: "sonix_mcp::stdio", "c");
    tracing::info!(target: "sonix::panic", "d");
    let msgs: Vec<String> = c.events().iter().map(|e| e["message"].as_str().unwrap().to_string()).collect();
    assert_eq!(msgs, ["a", "b", "c", "d"]);
}

#[test]
fn the_env_override_wins_over_the_setting_and_a_bad_one_falls_back() {
    let f = filter_for(false, Some("sonix_core=trace"));
    assert_eq!(f.max_level_hint(), Some(tracing_subscriber::filter::LevelFilter::TRACE));
    let f = filter_for(true, Some("sonix_core=error"));
    assert_eq!(f.max_level_hint(), Some(tracing_subscriber::filter::LevelFilter::ERROR));
    // unparsable -> the setting decides
    let f = filter_for(false, Some("sonix=[{"));
    assert_eq!(f.max_level_hint(), Some(tracing_subscriber::filter::LevelFilter::INFO));
    let f = filter_for(true, None);
    assert_eq!(f.max_level_hint(), Some(tracing_subscriber::filter::LevelFilter::DEBUG));
}

#[test]
fn capture_sees_everything_at_trace() {
    let c = capture();
    tracing::trace!(target: "anything", "deep");
    assert!(c.find("deep").is_some());
}

#[test]
fn forwarding_sends_info_and_up_from_our_crates_regardless_of_the_file_filter() {
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let l2 = lines.clone();
    let sink: LineSink = Box::new(move |s| l2.lock().unwrap().push(s.to_string()));
    let buf = Arc::new(Mutex::new(Vec::new()));
    struct W(Arc<Mutex<Vec<u8>>>);
    impl io::Write for W {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let (sub, control) = build(Box::new(W(buf.clone())), false, None, Some(sink), "r".into());
    tracing::subscriber::with_default(sub, || {
        control.set_filter("off");
        tracing::info!(target: "sonix_core::audio::engine", track_id = "42", gain = 1.5, "engine: play");
        tracing::debug!(target: "sonix_core::audio::engine", "not forwarded: debug");
        tracing::warn!(target: "rustls", "not forwarded: other crate");
        tracing::error!(target: "sonix_real", "boom");
    });
    let got = lines.lock().unwrap().clone();
    assert_eq!(got, ["[sonix_core::audio::engine] engine: play track_id=42 gain=1.5", "[sonix_real] boom"]);
    assert!(buf.lock().unwrap().is_empty(), "file filter off");
}

#[test]
fn prune_keeps_the_newest_files_of_its_own_prefix_only() {
    let d = temp_dir("prune");
    let mut all = Vec::new();
    for day in 1..=10 {
        let n = file_name("sonix", &format!("2026-11-{day:02}"));
        std::fs::write(d.join(&n), "{}\n").unwrap();
        all.push(n);
    }
    for other in ["sonix-mcp.2026-10-01.jsonl", "sonix-mcp.2026-10-02.jsonl", "crash.log", "sonix.notadate.jsonl", "sonix.2026-11-01.json"] {
        std::fs::write(d.join(other), "x").unwrap();
    }
    let removed = prune(&d, "sonix", MAX_LOG_FILES).unwrap();
    assert_eq!(removed.len(), 3);
    let mut left: Vec<String> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    left.sort();
    let mut want: Vec<String> = all[3..].to_vec();
    want.extend(["crash.log", "sonix-mcp.2026-10-01.jsonl", "sonix-mcp.2026-10-02.jsonl", "sonix.2026-11-01.json", "sonix.notadate.jsonl"].map(String::from));
    want.sort();
    assert_eq!(left, want);
    // idempotent
    assert!(prune(&d, "sonix", MAX_LOG_FILES).unwrap().is_empty());
    // the other binary's prefix is handled independently
    assert!(prune(&d, "sonix-mcp", 1).unwrap().len() == 1);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_file_writer_creates_todays_file_with_our_name() {
    let d = temp_dir("file");
    let (mut nb, guard, path) = open_file(&d, "sonix").unwrap();
    nb.write_all(b"{\"message\":\"hello\"}\n").unwrap();
    drop(nb);
    drop(guard); // flushes
    assert_eq!(path.file_name().unwrap().to_str().unwrap(), file_name("sonix", &ts_date()));
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text, "{\"message\":\"hello\"}\n");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn opening_twice_prunes_old_files_at_startup() {
    let d = temp_dir("startup");
    for day in 1..=9 {
        std::fs::write(d.join(file_name("sonix", &format!("2020-01-{day:02}"))), "").unwrap();
    }
    let (_nb, _guard, today) = open_file(&d, "sonix").unwrap();
    let n = std::fs::read_dir(&d).unwrap().filter(|e| dated(e.as_ref().unwrap().file_name().to_str().unwrap(), "sonix").is_some()).count();
    assert!(n <= MAX_LOG_FILES, "{n} files left");
    assert!(today.exists(), "today's file is the newest and is kept");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn run_ids_differ() {
    assert_ne!(new_run_id(), new_run_id());
    assert_eq!(new_run_id().len(), 16);
}

#[test]
fn rotation_over_many_days_keeps_at_most_seven_files() {
    let d = temp_dir("rotate");
    let day = Arc::new(Mutex::new(1u32));
    let d2 = day.clone();
    let clock: rolling::Clock = Box::new(move || format!("2026-03-{:02}", *d2.lock().unwrap()));
    let mut f = rolling::DailyFile::open(d.clone(), "sonix", MAX_LOG_FILES, clock).unwrap();
    std::fs::write(d.join("sonix-mcp.2026-01-01.jsonl"), "x").unwrap();
    for n in 1..=12u32 {
        *day.lock().unwrap() = n;
        writeln!(f, "{{\"day\":{n}}}").unwrap();
        writeln!(f, "{{\"day\":{n},\"second\":true}}").unwrap();
        let ours = std::fs::read_dir(&d).unwrap().filter(|e| dated(e.as_ref().unwrap().file_name().to_str().unwrap(), "sonix").is_some()).count();
        assert!(ours <= MAX_LOG_FILES, "day {n}: {ours} files");
    }
    f.flush().unwrap();
    let mut left: Vec<String> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    left.sort();
    let mut want: Vec<String> = (6..=12).map(|n| file_name("sonix", &format!("2026-03-{n:02}"))).collect();
    want.push("sonix-mcp.2026-01-01.jsonl".into());
    want.sort();
    assert_eq!(left, want, "the newest seven days of ours, the other binary's file untouched");
    assert_eq!(f.path(), d.join(file_name("sonix", "2026-03-12")));
    assert_eq!(std::fs::read_to_string(f.path()).unwrap().lines().count(), 2, "both lines of the day in one file");
    drop(f);
    let _ = std::fs::remove_dir_all(&d);
}
