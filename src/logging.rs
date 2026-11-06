//! Logging to the console and to a daily log file in `logs\`.

use std::io;
use std::path::Path;
use std::sync::Mutex;

use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::LogLevel;

/// Log files kept: today's plus the 7 days before it.
const MAX_LOG_FILES: usize = 8;

/// Our own crate's log target. The library and the binary share this name.
const OWN_TARGET: &str = "slc_local_tts";

/// Sets up `tracing`: lines on the console (coloured when `ansi` is true; see
/// `console::enable_ansi`), plain lines in `logs\slc-local-tts.<date>.log`.
/// Returns the file writer's guard; keep it alive (see [`park_guard`]) or
/// buffered lines are lost.
///
/// The folder and the file are created before any subscriber is installed,
/// so a failure here leaves global logging untouched.
pub fn init(logs_dir: &Path, level: LogLevel, ansi: bool) -> io::Result<WorkerGuard> {
    std::fs::create_dir_all(logs_dir)?;
    let appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("slc-local-tts")
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(logs_dir)
        .map_err(io::Error::other)?;
    // The file is written on a background thread, so a slow disk never holds
    // up a request. The guard is that thread's "stop and flush" handle.
    let (file_writer, guard) = tracing_appender::non_blocking(appender);

    let level = level_filter(level);
    // Other crates (hyper, reqwest) are capped at `warn`, so `debug` shows our
    // own detail instead of pages of connection chatter.
    let filter = Targets::new()
        .with_target(OWN_TARGET, level)
        .with_default(level.min(LevelFilter::WARN));

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_ansi(ansi))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file_writer),
        )
        .with(filter)
        // `try_init` returns an error instead of panicking if logging was
        // already set up.
        .try_init()
        .map_err(io::Error::other)?;
    Ok(guard)
}

/// Maps the `config.toml` level to `tracing`'s filter.
fn level_filter(level: LogLevel) -> LevelFilter {
    match level {
        LogLevel::Error => LevelFilter::ERROR,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Trace => LevelFilter::TRACE,
    }
}

/// Where the log file's guard waits until [`flush`].
///
/// Dropping a `WorkerGuard` flushes the buffered lines to the file. Normally
/// `main` would just keep it in a local variable, but the console close
/// handler (`console::install_close_handler`) runs on a thread Windows starts
/// for it and can't reach `main`'s locals. So the guard is parked here, in a
/// global, where both can get at it.
///
/// It's a `Mutex<Option<_>>`, much like a C++ `static std::optional` behind a
/// `std::mutex` or a Java `static AtomicReference`: `Option` because the slot
/// starts empty and `take()` empties it again, and `Mutex` because two
/// threads may touch it. `Mutex::new` is a `const fn`, so no lazy init is
/// needed. Rust never runs destructors of statics, so whoever ends the
/// program must call [`flush`].
static GUARD: Mutex<Option<WorkerGuard>> = Mutex::new(None);

/// Keeps the log file's guard until [`flush`] is called.
pub fn park_guard(guard: WorkerGuard) {
    *lock_guard() = Some(guard);
}

/// Writes out every buffered log line. Safe to call more than once; after
/// the first call, file logging stops.
pub fn flush() {
    // `take()` moves the guard out while the lock is held. The lock is
    // released at the end of this `let`, so the flush in `drop` (which waits
    // for the writer thread) doesn't happen under it.
    let guard = lock_guard().take();
    drop(guard);
}

/// A poisoned lock only means a panic happened while the slot was being
/// set; the slot itself is always valid, so it is safe to keep using.
fn lock_guard() -> std::sync::MutexGuard<'static, Option<WorkerGuard>> {
    GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Sends panics to the log as well as to stderr. The previous
/// hook (normally Rust's default, which prints to stderr) still runs.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `f` with a subscriber that records every log line on this thread,
    /// and returns what it wrote.
    fn capture(f: impl FnOnce()) -> String {
        #[derive(Clone, Default)]
        struct Buffer(std::sync::Arc<Mutex<Vec<u8>>>);
        impl io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        // A second live dispatcher stops tracing-core from caching a log
        // statement as "never enabled" because another test thread reached it
        // first (see the same helper in `api::error`).
        let _second = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn panic_hook_logs_message() {
        let output = capture(|| {
            install_panic_hook();
            let result = std::panic::catch_unwind(|| panic!("boom-xyz"));
            assert!(result.is_err());
        });
        // Put the default hook back so other tests see normal panics.
        let _ = std::panic::take_hook();
        assert!(output.contains("boom-xyz"), "{output}");
    }
}
