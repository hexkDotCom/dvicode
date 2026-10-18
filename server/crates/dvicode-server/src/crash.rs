//! Internal errors: the panic hook, and turning a caught panic into a log entry and a notice
//! for the client (`dvicode/internalError`), which offers to report it.

use crossbeam_channel::Sender;
use lsp_server::{Message, Notification};
use serde::Serialize;
use std::cell::RefCell;
use std::collections::HashSet;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Mutex;

/// Sent to the client the first time a feature fails at a given place.
#[derive(Clone, Debug, Default, Serialize)]
pub struct InternalError {
    /// What was being done: an LSP method or a phase such as `project build`.
    pub feature: String,
    pub message: String,
    /// Source location of the panic.
    pub location: String,
    pub backtrace: String,
    pub version: &'static str,
}

thread_local! {
    /// The last panic on this thread, until the code that caught it picks it up.
    static LAST: RefCell<Option<InternalError>> = const { RefCell::new(None) };
}

/// Feature and location of errors already sent to the client.
static REPORTED: Mutex<Option<HashSet<(String, String)>>> = Mutex::new(None);

/// Logs every panic with its location and a backtrace, and keeps it for [`catch`].
pub fn install_hook() {
    panic::set_hook(Box::new(|info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown error".into());
        let location = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_default();
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        log::error!("internal error in thread '{thread}' at {location}: {message}\n{backtrace}");
        let e = InternalError { message, location, backtrace, version: crate::VERSION, ..Default::default() };
        LAST.with(|l| *l.borrow_mut() = Some(e));
    }));
}

/// Runs `f`. If it panics, logs what was being done, tells the client (once per feature and
/// place) and returns `None`.
pub fn catch<T>(feature: &str, client: Option<&Sender<Message>>, f: impl FnOnce() -> T) -> Option<T> {
    if let Ok(v) = panic::catch_unwind(AssertUnwindSafe(f)) {
        return Some(v);
    }
    let mut e = LAST.with(|l| l.borrow_mut().take()).unwrap_or_default();
    e.feature = feature.to_string();
    e.version = crate::VERSION;
    log::error!("{feature} failed: {}", e.message);
    if let Some(tx) = client {
        let first = match REPORTED.lock() {
            Ok(mut r) => r.get_or_insert_with(HashSet::new).insert((e.feature.clone(), e.location.clone())),
            Err(_) => true,
        };
        if first {
            let _ = tx.send(Message::Notification(Notification::new("dvicode/internalError".into(), e)));
        }
    }
    None
}
