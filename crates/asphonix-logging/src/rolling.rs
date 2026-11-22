//! Daily file writer with pruning, run on the non-blocking writer thread.
//!
//! Not `tracing_appender::rolling::RollingFileAppender`: its `max_log_files` pruning matches files by
//! `starts_with(prefix)`, so the app's `asphonix` prefix would count (and delete) `asphonix-mcp.*.jsonl` too, and it
//! prunes only on rollover, never at startup. This one prunes its exact `<prefix>.YYYY-MM-DD.jsonl` files when
//! it opens a file (startup and every day change), with an injectable clock for tests.
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

/// Today's UTC date as `YYYY-MM-DD`.
pub(crate) type Clock = Box<dyn Fn() -> String + Send>;

pub(crate) struct DailyFile {
    dir: PathBuf,
    prefix: String,
    keep: usize,
    today: Clock,
    date: String,
    file: Option<File>,
}

impl DailyFile {
    /// Opens (appends to) today's file right away, so an unusable directory fails here, not later.
    pub(crate) fn open(dir: PathBuf, prefix: &str, keep: usize, today: Clock) -> io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let mut f = Self { dir, prefix: prefix.to_string(), keep, today, date: String::new(), file: None };
        let date = (f.today)();
        f.roll(date)?;
        Ok(f)
    }

    /// Path of the file currently written.
    pub(crate) fn path(&self) -> PathBuf {
        self.dir.join(crate::file_name(&self.prefix, &self.date))
    }

    fn roll(&mut self, date: String) -> io::Result<()> {
        let path = self.dir.join(crate::file_name(&self.prefix, &date));
        self.file = Some(OpenOptions::new().create(true).append(true).open(path)?);
        self.date = date;
        let _ = crate::prune(&self.dir, &self.prefix, self.keep);
        Ok(())
    }
}

impl Write for DailyFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let now = (self.today)();
        if now != self.date || self.file.is_none() {
            self.roll(now)?;
        }
        match self.file.as_mut() {
            Some(f) => f.write(buf),
            None => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().map_or(Ok(()), |f| f.flush())
    }
}
