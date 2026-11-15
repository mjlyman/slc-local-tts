//! Startup steps shared by the tray program (`slc-local-tts.exe`), its
//! settings window and the console program (`slc-local-tts-cli.exe`): the
//! data folder and logging, priority, the async runtime, the order of the
//! instance and update checks, and serving until told to stop.

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use windows_sys::Win32::System::Threading::{
    BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass,
};

use crate::config::{self, Config, ConfigError, LogLevel, Priority};
use crate::console;
use crate::desktop::handoff;
use crate::instance::AlreadyRunning;
use crate::logging::{self, ConsoleLog, SERVER_LOG_PREFIX, WINDOW_LOG_PREFIX};
use crate::paths::DataDir;
use crate::server::{Started, StartupError};
use crate::stop::{StopReason, StopSignal};
use crate::update::{self, UpdateError};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Async worker threads. Requests are light (synthesis runs on tokio's
/// blocking pool, one at a time), so two keep the thread count small next
/// to the flight sim.
const WORKER_THREADS: usize = 2;

/// How long to wait at exit for blocking work (an unload, an unpack) to end.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Which program is starting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The console program: log lines also go to the console.
    Cli,
    /// The tray program's server: no console.
    Gui,
    /// The settings window: no console, and a log file of its own.
    Window,
}

/// What [`boot`] found.
pub struct Boot {
    pub data: DataDir,
    /// `config.toml`, or why it couldn't be read. Reporting the error is the
    /// caller's job, so it reaches the log as well as the user.
    pub config: Result<Config, ConfigError>,
}

/// Finds the data folder, reads `config.toml` for the log level, starts
/// logging and sends panics to the log. `Err` holds the text to show the
/// user, on the console or in a message box.
pub fn boot(mode: Mode) -> Result<Boot, String> {
    let data = DataDir::from_env().map_err(|e| e.to_string())?;

    // `config.toml` is read once, here, only so logging can start at the
    // configured level (Info if the file is missing or invalid).
    let config = config::load(&data.config_file());
    let level = config.as_ref().map_or(LogLevel::Info, |c| c.log_level);

    let (prefix, console) = match mode {
        // A plain Win32 call on this thread (no threads start): colour codes
        // only where the console can show them.
        Mode::Cli if console::enable_ansi() => (SERVER_LOG_PREFIX, ConsoleLog::Ansi),
        Mode::Cli => (SERVER_LOG_PREFIX, ConsoleLog::Plain),
        Mode::Gui => (SERVER_LOG_PREFIX, ConsoleLog::Off),
        Mode::Window => (WINDOW_LOG_PREFIX, ConsoleLog::Off),
    };
    match logging::init(&log_dir(&data, mode), prefix, level, console) {
        Ok(guard) => logging::park_guard(guard),
        Err(e) => {
            let error = StartupError::DataDirNotWritable {
                path: data.root().to_path_buf(),
            };
            return Err(format!("{error}\n(Details: {e})"));
        }
    }
    logging::install_panic_hook();
    Ok(Boot { data, config })
}

/// The folder `mode`'s log files go in: `logs\` for the server, and
/// `logs\window\` for the settings window.
///
/// The window's files need a folder of their own. The log cleanup counts
/// every file in its folder whose name starts with the prefix, and the
/// server's prefix, `slc-local-tts`, is also the start of the window's,
/// `slc-local-tts-window`. In one folder, each server start would delete
/// window logs and keep fewer days of its own. The cleanup never looks
/// into subfolders.
fn log_dir(data: &DataDir, mode: Mode) -> PathBuf {
    match mode {
        Mode::Cli | Mode::Gui => data.logs(),
        Mode::Window => data.logs().join("window"),
    }
}

/// Lowers this process's CPU priority if `config.toml` asks for it. The
/// default, `normal`, leaves it alone: below-normal starved synthesis next
/// to an all-core load in testing.
pub fn set_priority(priority: Priority) {
    if priority != Priority::BelowNormal {
        return;
    }
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle for this process
    // that is always valid and never needs closing.
    let ok = unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) };
    if ok == 0 {
        tracing::warn!(
            "couldn't lower SLC Local TTS's priority as config.toml asks; it runs at normal priority"
        );
    } else {
        tracing::info!("running at below-normal priority (set in config.toml)");
    }
}

/// The async runtime the server runs on.
pub fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .enable_all()
        .build()
}

/// What to tell the user when [`runtime`] fails.
pub fn runtime_error_text(error: &io::Error) -> String {
    format!(
        "SLC Local TTS couldn't start its worker threads ({error}). Close some programs and start it again."
    )
}

/// What the server does after its startup checks.
pub enum Gate<G> {
    /// Start the server, holding the instance guard.
    Serve(G),
    /// Another copy is running; stop with this error.
    AlreadyRunning(AlreadyRunning),
    /// The updater is installing a downloaded update; exit now.
    Restarting,
}

/// The server's startup order: the instance mutex first, then a downloaded
/// update. A second copy never gets as far as the update, so it can't
/// restart the server that's already running. A failed install costs only a
/// warning: this version starts instead. Both steps are passed in, so tests
/// can check the order without a real mutex or install.
pub fn serve_gate<G>(
    acquire: impl FnOnce() -> Result<G, AlreadyRunning>,
    apply: impl FnOnce() -> Result<bool, UpdateError>,
) -> Gate<G> {
    let guard = match acquire() {
        Ok(guard) => guard,
        Err(e) => return Gate::AlreadyRunning(e),
    };
    match apply() {
        Ok(true) => Gate::Restarting,
        Ok(false) => Gate::Serve(guard),
        Err(e) => {
            tracing::warn!("{}", apply_failed_text(&e));
            Gate::Serve(guard)
        }
    }
}

/// The warning when the startup install of a downloaded update fails.
pub fn apply_failed_text(e: &UpdateError) -> String {
    match e {
        // The install couldn't even be looked at, so nothing was tried.
        UpdateError::Unreadable(_) => format!(
            "Couldn't look for a downloaded update ({e}). Starting this version; it looks again the next time SLC Local TTS starts."
        ),
        _ => format!(
            "Couldn't install the downloaded update ({e}). Starting this version instead; it tries again the next time SLC Local TTS starts."
        ),
    }
}

/// Whether the tray program may install a downloaded update at startup.
/// Turning "Check for updates" off holds back an update that was already
/// downloaded, too: someone who turns updates off before a series of
/// flights shouldn't get one anyway. A config that can't be read counts as
/// the default, which is on.
pub fn apply_allowed(config: &Result<Config, ConfigError>) -> bool {
    match config {
        Ok(config) => config.check_for_updates,
        Err(_) => true,
    }
}

/// What the tray program does once the server has stopped for `reason`,
/// and the exit code it then ends with.
///
/// For "Restart and update now" it installs the downloaded update with
/// `apply`. `Ok(true)` means the updater now waits for this process to end
/// and starts the new version itself. If there turned out to be nothing to
/// install, or installing failed (`report` tells the user why), this
/// version is started again with `restart`, so SLC never loses its server.
/// Any other reason just ends the program. The steps are passed in, so
/// tests can check every case without a real install.
pub fn after_stop(
    reason: StopReason,
    apply: impl FnOnce() -> Result<bool, String>,
    report: impl FnOnce(&str),
    restart: impl FnOnce(),
) -> ExitCode {
    if reason != StopReason::RestartToUpdate {
        return ExitCode::SUCCESS;
    }
    match apply() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            tracing::info!("no downloaded update was found after all; starting this version again");
            restart();
            ExitCode::SUCCESS
        }
        Err(e) => {
            let text = format!(
                "Couldn't install the update ({e}). SLC Local TTS will start again without it."
            );
            tracing::error!("{text}");
            report(&text);
            restart();
            ExitCode::FAILURE
        }
    }
}

/// Answers requests until `stop` is requested, then lets the requests in
/// progress finish and returns why it stopped.
///
/// While it serves, the port is in the data folder's port file, so a second
/// launch can find this server. It also logs the listening line (and, just
/// after an update, the version it updated to) and logs how this run's
/// update check goes. Neither the port file nor the update check ever stops
/// the server.
pub async fn serve_until_stopped(
    started: Started,
    data: &DataDir,
    stop: StopSignal,
) -> Result<StopReason, StartupError> {
    let port = started.port();
    // Named `_port_file`, not `_`, so it lives (and the file stays) until
    // this function returns.
    let _port_file = match handoff::write_port_file(data, port) {
        Ok(file) => Some(file),
        Err(e) => {
            tracing::warn!(
                path = %data.port_file().display(),
                "couldn't write the port file ({e}); opening SLC Local TTS again uses the port in config.toml"
            );
            None
        }
    };

    let url = format!("http://127.0.0.1:{port}");
    tracing::info!(
        version = VERSION,
        config = %data.config_file().display(),
        models = %data.models().display(),
        %url,
        "SLC Local TTS is listening; paste {url} into SLC's ElevenLabs Custom API URL"
    );
    if let Some(version) = update::updated_to() {
        tracing::info!("{}", update::updated_note(version));
    }
    // `server::start` started this run's one update check; log how it goes.
    tokio::spawn(update::log_changes(started.updates()));

    // The shutdown future. `async move { .. }` builds it without running
    // it, and takes its own clone of the signal, because `serve` keeps the
    // future and may poll it on another worker thread.
    let waiter = stop.clone();
    started
        .serve(async move {
            waiter.wait().await;
        })
        .await?;
    tracing::info!("SLC Local TTS stopped");
    // `serve` returns only after the shutdown future resolved, so a reason
    // is always set; `Quit` is only a fallback that can't happen.
    Ok(stop.requested().unwrap_or(StopReason::Quit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance;

    #[test]
    fn window_logs_survive_the_server_log_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let data = DataDir::at(tmp.path().to_path_buf());
        let window_dir = log_dir(&data, Mode::Window);
        std::fs::create_dir_all(&window_dir).unwrap();
        // More old window logs than the cleanup keeps for one prefix.
        let window_logs: Vec<PathBuf> = (1..=10)
            .map(|day| window_dir.join(format!("{WINDOW_LOG_PREFIX}.2026-01-{day:02}.log")))
            .collect();
        for path in &window_logs {
            std::fs::write(path, "window").unwrap();
        }

        // Building the server's log file runs its cleanup.
        for mode in [Mode::Gui, Mode::Cli] {
            logging::file_appender(&log_dir(&data, mode), SERVER_LOG_PREFIX).unwrap();
        }

        for path in &window_logs {
            assert!(path.is_file(), "{} was deleted", path.display());
        }
    }

    #[test]
    fn server_logs_stay_in_logs_and_window_logs_get_a_subfolder() {
        let data = DataDir::at(PathBuf::from(r"C:\data"));
        assert_eq!(log_dir(&data, Mode::Gui), data.logs());
        assert_eq!(log_dir(&data, Mode::Cli), data.logs());
        assert_eq!(log_dir(&data, Mode::Window), data.logs().join("window"));
    }

    #[test]
    fn second_copy_never_applies_an_update() {
        let mut applied = false;
        let gate = serve_gate(
            || Err::<(), _>(instance::AlreadyRunning),
            || {
                applied = true;
                Ok(true)
            },
        );
        assert!(matches!(gate, Gate::AlreadyRunning(_)));
        assert!(!applied, "a second copy must never apply an update");
    }

    #[test]
    fn pending_update_restarts_instead_of_serving() {
        let gate = serve_gate(|| Ok("guard"), || Ok(true));
        assert!(matches!(gate, Gate::Restarting));
    }

    #[test]
    fn failed_apply_still_serves() {
        let gate = serve_gate(
            || Ok("guard"),
            || {
                Err(update::UpdateError::Other(
                    "Update.exe is missing".to_string(),
                ))
            },
        );
        assert!(matches!(gate, Gate::Serve("guard")));
    }

    /// What `after_stop` did: the code, and the texts `report` got and how
    /// often `restart` and `apply` ran.
    struct AfterStop {
        code: ExitCode,
        applied: u32,
        reports: Vec<String>,
        restarts: u32,
    }

    fn run_after_stop(reason: StopReason, apply: Result<bool, String>) -> AfterStop {
        let mut applied = 0;
        let mut reports = Vec::new();
        let mut restarts = 0;
        let code = after_stop(
            reason,
            || {
                applied += 1;
                apply
            },
            |text| reports.push(text.to_string()),
            || restarts += 1,
        );
        AfterStop {
            code,
            applied,
            reports,
            restarts,
        }
    }

    #[test]
    fn after_stop_updater_waiting_exits_quietly() {
        let done = run_after_stop(StopReason::RestartToUpdate, Ok(true));
        assert_eq!(done.code, ExitCode::SUCCESS);
        assert_eq!(done.applied, 1);
        assert!(done.reports.is_empty(), "{:?}", done.reports);
        assert_eq!(done.restarts, 0);
    }

    #[test]
    fn after_stop_nothing_pending_starts_again() {
        let done = run_after_stop(StopReason::RestartToUpdate, Ok(false));
        assert_eq!(done.code, ExitCode::SUCCESS);
        assert!(done.reports.is_empty(), "{:?}", done.reports);
        assert_eq!(done.restarts, 1);
    }

    #[test]
    fn after_stop_failed_apply_reports_and_starts_again() {
        let done = run_after_stop(StopReason::RestartToUpdate, Err("disk full".to_string()));
        assert_eq!(done.code, ExitCode::FAILURE);
        assert_eq!(
            done.reports,
            ["Couldn't install the update (disk full). SLC Local TTS will start again without it."]
        );
        assert!(done.reports[0].contains("start again without it"));
        assert_eq!(done.restarts, 1);
    }

    /// Runs `f` with a subscriber that records every log line on this
    /// thread, and returns what it wrote (the same helper as in `update`).
    fn capture_logs(f: impl FnOnce()) -> String {
        #[derive(Clone, Default)]
        struct Buffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        // A second live dispatcher stops tracing-core from caching a log
        // statement as "never enabled" because another test thread reached
        // it first.
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
    fn after_stop_logs_what_happened() {
        let failed = capture_logs(|| {
            run_after_stop(StopReason::RestartToUpdate, Err("disk full".to_string()));
        });
        assert!(failed.contains("ERROR"), "{failed}");
        assert!(
            failed.contains("Couldn't install the update (disk full)"),
            "{failed}"
        );

        let nothing = capture_logs(|| {
            run_after_stop(StopReason::RestartToUpdate, Ok(false));
        });
        assert!(
            nothing.contains("no downloaded update was found after all"),
            "{nothing}"
        );
    }

    #[test]
    fn after_stop_other_reasons_never_apply() {
        for reason in [
            StopReason::Quit,
            StopReason::CtrlC,
            StopReason::SessionEnding,
        ] {
            let done = run_after_stop(reason, Ok(true));
            assert_eq!(done.code, ExitCode::SUCCESS, "{reason:?}");
            assert_eq!(done.applied, 0, "{reason:?}");
            assert!(done.reports.is_empty(), "{reason:?}");
            assert_eq!(done.restarts, 0, "{reason:?}");
        }
    }

    #[test]
    fn apply_allowed_follows_check_for_updates() {
        let on = Ok(Config {
            check_for_updates: true,
            ..Config::default()
        });
        let off = Ok(Config {
            check_for_updates: false,
            ..Config::default()
        });
        let unreadable = Err(ConfigError::Parse("expected `=`".to_string()));
        assert!(apply_allowed(&on));
        assert!(!apply_allowed(&off));
        assert!(apply_allowed(&unreadable));
    }

    #[test]
    fn apply_failed_text_per_cause() {
        assert!(
            apply_failed_text(&UpdateError::Unreadable("access denied".to_string())).starts_with(
                "Couldn't look for a downloaded update (the installation couldn't be read (access denied))"
            )
        );
        assert_eq!(
            apply_failed_text(&UpdateError::Unreadable("access denied".to_string())),
            "Couldn't look for a downloaded update (the installation couldn't be read (access denied)). Starting this version; it looks again the next time SLC Local TTS starts."
        );
        assert_eq!(
            apply_failed_text(&UpdateError::Other("disk full".to_string())),
            "Couldn't install the downloaded update (disk full). Starting this version instead; it tries again the next time SLC Local TTS starts."
        );
    }

    #[test]
    fn nothing_pending_serves() {
        let gate = serve_gate(|| Ok("guard"), || Ok(false));
        assert!(matches!(gate, Gate::Serve("guard")));
    }
}
