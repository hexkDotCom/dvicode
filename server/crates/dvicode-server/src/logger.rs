//! Minimal logger to stderr (which the VS Code client shows in its output channel) or a file.

use std::io::Write;
use std::sync::Mutex;

struct Logger {
    level: log::LevelFilter,
    out: Mutex<Box<dyn Write + Send>>,
}

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= self.level
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let secs = t as u64 % 86400;
        let line = format!(
            "[{:02}:{:02}:{:02}.{:03} {:<5}] {}\n",
            secs / 3600,
            secs / 60 % 60,
            secs % 60,
            ((t.fract()) * 1000.0) as u32,
            r.level(),
            r.args()
        );
        if let Ok(mut o) = self.out.lock() {
            let _ = o.write_all(line.as_bytes());
            let _ = o.flush();
        }
    }

    fn flush(&self) {}
}

/// The file the log goes to, if not stderr.
static FILE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

pub fn file() -> Option<&'static std::path::Path> {
    FILE.get().map(|p| p.as_path())
}

pub fn init(level: log::LevelFilter, file: Option<&str>) {
    if let Some(f) = file {
        let _ = FILE.set(std::fs::canonicalize(f).unwrap_or_else(|_| f.into()));
    }
    let out: Box<dyn Write + Send> = match file.and_then(|f| std::fs::OpenOptions::new().create(true).append(true).open(f).ok()) {
        Some(f) => Box::new(f),
        None => Box::new(std::io::stderr()),
    };
    let _ = log::set_boxed_logger(Box::new(Logger { level, out: Mutex::new(out) }));
    log::set_max_level(level);
}
