//! In-memory capture of the JSON lines for tests (feature `test-support`).
//!
//! `capture()` installs the production subscriber (same layer, same schema) as the *thread-local* default
//! until the returned [`Capture`] is dropped, at `trace` for every target. Events emitted on other threads
//! (spawned threads, multi-thread tokio runtimes) are not captured: drive the code under test on the test
//! thread (e.g. a current-thread runtime).
use serde_json::Value;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

pub use crate::LogControl;

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Captured output; the capture ends when this is dropped.
pub struct Capture {
    buf: SharedBuf,
    control: LogControl,
    _guard: tracing::subscriber::DefaultGuard,
}

/// Everything at `trace`.
pub fn capture() -> Capture {
    let c = capture_with_filter(false);
    c.control.set_filter("trace");
    c
}

/// The production filter ([`crate::DEFAULT_FILTER`], or [`crate::VERBOSE_FILTER`] when `verbose`), ignoring
/// `SONIX_LOG_LEVEL`. `control()` changes it like the settings toggle does.
pub fn capture_with_filter(verbose: bool) -> Capture {
    let buf = SharedBuf::default();
    let (sub, control) = crate::build(Box::new(buf.clone()), verbose, None, None, "test-run".into());
    let guard = tracing::subscriber::set_default(sub);
    Capture { buf, control, _guard: guard }
}

impl Capture {
    /// The raw JSON lines.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.buf.0.lock().unwrap_or_else(|p| p.into_inner())).into_owned()
    }

    /// Every line parsed (panics on a line that is not valid JSON: that is a bug in the layer).
    pub fn events(&self) -> Vec<Value> {
        self.text().lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}"))).collect()
    }

    /// The first event whose message contains `needle`.
    pub fn find(&self, needle: &str) -> Option<Value> {
        self.events().into_iter().find(|e| e["message"].as_str().is_some_and(|m| m.contains(needle)))
    }

    /// The filter handle (same as the one the settings toggle drives).
    pub fn control(&self) -> &LogControl {
        &self.control
    }
}
