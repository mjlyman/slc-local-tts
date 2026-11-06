//! `slc-local-tts` binary: parses the command line, sets up logging and the
//! console, and hands off to the library. Running with no subcommand starts
//! the server.

use std::fmt::{Display, Write as _};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use windows_sys::Win32::System::Threading::{
    BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass,
};

use slc_local_tts::catalog::{self, Archive, Voice};
use slc_local_tts::config::{self, Config, ConfigError, LogLevel, Priority};
use slc_local_tts::instance::AlreadyRunning;
use slc_local_tts::models::install::{self, HttpInstaller};
use slc_local_tts::models::{DiskStore, ModelStore};
use slc_local_tts::paths::DataDir;
use slc_local_tts::server::{self, StartupError};
use slc_local_tts::update::UpdateError;
use slc_local_tts::{console, instance, logging, update};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Async worker threads. Requests are light (synthesis runs on tokio's
/// blocking pool, one at a time), so two keep the thread count small next
/// to the flight sim.
const WORKER_THREADS: usize = 2;

/// How long to wait at exit for blocking work (an unload, an unpack) to end.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Exit code for `download` with a voice ID the catalog doesn't know.
const EXIT_UNKNOWN_VOICE: u8 = 2;

/// Local, ElevenLabs-compatible text-to-speech server for Self-Loading Cargo.
// `#[derive(Parser)]` generates the argument parser from this struct; the doc
// comments become the `--help` text.
#[derive(Debug, Parser)]
#[command(version)]
struct Cli {
    /// What to do. With no subcommand, the server starts (same as `serve`).
    // `Option` makes the subcommand optional: `None` means the user typed none.
    #[command(subcommand)]
    command: Option<Command>,
}

// `PartialEq` lets the tests compare parsed commands with `assert_eq!`.
#[derive(Debug, PartialEq, Eq, Subcommand)]
enum Command {
    /// Start the speech server (the default).
    Serve,
    /// List the voices this server offers.
    Voices,
    /// Download the model for one voice ahead of time.
    Download {
        /// Voice ID, for example `M1` or `en_US-joe-medium`.
        voice: String,
    },
}

fn main() -> ExitCode {
    // Velopack starts this exe with hook arguments (`--veloapp-install
    // <version>` and friends) while installing, updating and uninstalling.
    // `run()` handles them and exits before anything else happens. It has to
    // come before clap, which would reject those arguments.
    update::hooks().run();

    let cli = Cli::parse();
    let owns_console = console::owns_console();

    let data = match DataDir::from_env() {
        Ok(data) => data,
        Err(e) => return fail_before_logging(&e, owns_console),
    };

    // `config.toml` is read once, here, only so logging can start at the
    // configured level (Info if the file is missing or invalid). A config
    // error is reported once logging is up, so it reaches the log file too.
    let config = config::load(&data.config_file());
    let level = config.as_ref().map_or(LogLevel::Info, |c| c.log_level);
    // A plain Win32 call on this thread (no threads start): colour codes only
    // where the console can show them.
    let ansi = console::enable_ansi();
    match logging::init(&data.logs(), level, ansi) {
        Ok(guard) => logging::park_guard(guard),
        Err(e) => {
            let error = StartupError::DataDirNotWritable {
                path: data.root().to_path_buf(),
            };
            return fail_before_logging(&format!("{error}\n(Details: {e})"), owns_console);
        }
    }
    logging::install_panic_hook();

    let code = run(
        cli.command.unwrap_or(Command::Serve),
        data,
        config,
        owns_console,
    );
    // Statics are never dropped in Rust, so the parked log guard must be
    // flushed by hand before the process ends.
    logging::flush();
    code
}

/// Everything after logging is up: config, priority, console, then the
/// command itself.
fn run(
    command: Command,
    data: DataDir,
    config: Result<Config, ConfigError>,
    owns_console: bool,
) -> ExitCode {
    let config = match config {
        Ok(config) => config,
        Err(error) => {
            let path = data.config_file();
            return fail(&StartupError::Config { error, path }, owns_console);
        }
    };
    set_priority(config.priority);

    // In a terminal the window belongs to the shell, so its title and
    // settings are left alone. The close handler is always installed:
    // closing a terminal window ends us just the same.
    if owns_console {
        console::disable_quick_edit();
        console::set_title("SLC Local TTS");
    }
    console::install_close_handler();

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            let message = format!(
                "SLC Local TTS couldn't start its worker threads ({e}). Close some programs and start it again."
            );
            return fail(&message, owns_console);
        }
    };

    let code = match command {
        Command::Serve => {
            // One server at a time: take the instance mutex before binding
            // the port or cleaning anything up, then install any update an
            // earlier run downloaded. It's named `_instance`, not `_`, on
            // purpose: `let _ = ..` drops the guard at once, which would
            // release the mutex straight away. A named binding lives to the
            // end of this block, so until `serve` returns.
            let _instance = match serve_gate(instance::acquire, update::apply_pending_and_restart) {
                Gate::Serve(guard) => guard,
                Gate::AlreadyRunning(e) => return fail(&e, owns_console),
                // `Ok(true)` from the apply: the updater is waiting for this
                // process to exit, so return from `main` now, without
                // "Press Enter".
                Gate::Restarting => return ExitCode::SUCCESS,
            };
            match runtime.block_on(serve(data, config)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(&error, owns_console),
            }
        }
        Command::Voices => {
            let models = data.models();
            print!(
                "{}",
                voices_table(catalog::voices(), |archive| install::is_installed(
                    archive, &models
                ))
            );
            ExitCode::SUCCESS
        }
        Command::Download { voice } => runtime.block_on(download(&data, &voice)),
    };
    // Dropping a runtime waits for every blocking task without limit; this
    // waits a few seconds at most.
    runtime.shutdown_timeout(SHUTDOWN_GRACE);
    code
}

/// What `serve` does after its startup checks.
enum Gate<G> {
    /// Start the server, holding the instance guard.
    Serve(G),
    /// Another copy is running; stop with this error.
    AlreadyRunning(AlreadyRunning),
    /// The updater is installing a downloaded update; exit now.
    Restarting,
}

/// `serve`'s startup order: the instance mutex first, then a downloaded
/// update. A second copy never gets as far as the update, so it can't
/// restart the server that's already running. A failed install costs only a
/// warning: this version starts instead. Both steps are passed in, so tests
/// can check the order without a real mutex or install.
fn serve_gate<G>(
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
            tracing::warn!(
                "Couldn't install the downloaded update ({e}). Starting this version instead; it tries again the next time SLC Local TTS starts."
            );
            Gate::Serve(guard)
        }
    }
}

/// `serve`: start, show the banner, check for updates in the background,
/// answer requests until Ctrl+C.
async fn serve(data: DataDir, config: Config) -> Result<(), StartupError> {
    let config_path = data.config_file();
    let models_dir = data.models();
    // `server::start` takes ownership of `config`, so keep a copy for the
    // update check.
    let config_copy = config.clone();
    let started = server::start(data, config).await?;
    let port = started.port();
    let url = format!("http://127.0.0.1:{port}");
    tracing::info!(
        version = VERSION,
        config = %config_path.display(),
        models = %models_dir.display(),
        %url,
        "SLC Local TTS is listening; paste {url} into SLC's ElevenLabs Custom API URL"
    );
    println!("\n{}\n", server::banner(VERSION, port));
    if let Some(version) = update::updated_to() {
        let note = update::updated_note(version);
        println!("{note}\n");
        tracing::info!("{note}");
    }
    // Now that the server is listening: check for updates once, on a
    // blocking thread, and log how it goes. Neither ever stops the server.
    tokio::spawn(update::log_changes(update::spawn_check(&config_copy)));

    // The shutdown future: it completes on the first Ctrl+C, which tells
    // `serve` to stop taking requests, finish the ones in progress and
    // return. `async { .. }` builds the future without running it; `serve`
    // polls it alongside the connections.
    let shutdown = async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => tracing::info!(
                "Stopping after the requests in progress finish. Press Ctrl+C again to quit now."
            ),
            Err(e) => {
                // Without a Ctrl+C handler, Windows' default one still ends
                // the process. Never resolving keeps the server running.
                tracing::warn!(error = %e, "couldn't listen for Ctrl+C");
                std::future::pending::<()>().await;
            }
        }
    };
    started.serve(shutdown).await?;
    tracing::info!("SLC Local TTS stopped");
    Ok(())
}

/// `download <voice>`: installs one voice's model now. Exit code 0 when it
/// is installed, 2 for an unknown voice, 1 when the download fails.
///
/// It works whether or not a server is running. Both install through the
/// same per-archive lock file, so if the server is already downloading this
/// voice, this waits for it and then reports the voice installed. It doesn't
/// sweep leftover download files the way the server does at startup; the
/// installer clears its own archive's stale scratch files under the lock.
async fn download(data: &DataDir, id: &str) -> ExitCode {
    let Some(voice) = catalog::find(id) else {
        eprintln!("No voice called '{id}'. Run `slc-local-tts voices` to see the list.");
        return ExitCode::from(EXIT_UNKNOWN_VOICE);
    };
    let store = DiskStore::new(data.models(), HttpInstaller::new());
    if store.is_installed(voice.archive) {
        println!("{} is already installed.", voice.id);
        return ExitCode::SUCCESS;
    }
    println!(
        "Downloading {} ({} MB). This can take a few minutes...",
        voice.name,
        voice.archive.size.div_ceil(1_000_000)
    );
    match store.ensure_installed(voice.archive).await {
        Ok(dir) => {
            println!("Installed {} in {}.", voice.id, dir.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Couldn't download {}: {}.", voice.id, e.reason);
            ExitCode::FAILURE
        }
    }
}

/// Lowers this process's CPU priority if `config.toml` asks for it. The
/// default, `normal`, leaves it alone: below-normal starved synthesis next
/// to an all-core load in testing.
fn set_priority(priority: Priority) {
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

/// Reports a fatal error once logging is up: logged (console and file), then
/// "Press Enter to exit" if closing would make the window vanish.
fn fail(error: &dyn Display, owns_console: bool) -> ExitCode {
    tracing::error!("{error}");
    if owns_console {
        console::wait_for_enter();
    }
    ExitCode::FAILURE
}

/// Reports a fatal error from before logging could start: console only.
fn fail_before_logging(error: &dyn Display, owns_console: bool) -> ExitCode {
    eprintln!("{error}");
    if owns_console {
        console::wait_for_enter();
    }
    ExitCode::FAILURE
}

/// The `voices` command output: one row per voice, columns padded with
/// spaces so they line up.
fn voices_table(voices: &[Voice], installed: impl Fn(&Archive) -> bool) -> String {
    let header = ["ID", "ENGINE", "INSTALLED", "NAME"];
    let rows: Vec<[&str; 4]> = voices
        .iter()
        .map(|voice| {
            let is_installed = if installed(voice.archive) {
                "yes"
            } else {
                "no"
            };
            [voice.id, voice.engine.as_str(), is_installed, voice.name]
        })
        .collect();

    // Each column is as wide as its widest cell, header included.
    let mut widths = header.map(|cell| cell.chars().count());
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }

    let mut table = String::new();
    for [id, engine, is_installed, name] in std::iter::once(&header).chain(&rows) {
        // The last column isn't padded, so lines have no trailing spaces.
        // `writeln!` into a `String` can't fail, so its result is ignored.
        let _ = writeln!(
            table,
            "{id:<w0$}  {engine:<w1$}  {is_installed:<w2$}  {name}",
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
        );
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_defaults_to_serve() {
        let cli = Cli::try_parse_from(["slc-local-tts"]).unwrap();
        assert_eq!(cli.command, None);
    }

    #[test]
    fn voices_table_lists_joe() {
        let table = voices_table(slc_local_tts::catalog::voices(), |_| false);
        let mut lines = table.lines();
        assert!(lines.next().unwrap().starts_with("ID"), "{table}");
        let joe = table
            .lines()
            .find(|line| line.starts_with("en_US-joe-medium "))
            .unwrap_or_else(|| panic!("no joe line in:\n{table}"));
        let mut rest = joe;
        for part in ["en_US-joe-medium", "piper", "no", "Joe (US, medium, Piper)"] {
            let at = rest
                .find(part)
                .unwrap_or_else(|| panic!("{part} missing or out of order in: {joe}"));
            rest = &rest[at + part.len()..];
        }
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

    #[test]
    fn nothing_pending_serves() {
        let gate = serve_gate(|| Ok("guard"), || Ok(false));
        assert!(matches!(gate, Gate::Serve("guard")));
    }

    #[test]
    fn cli_parses_download() {
        let cli = Cli::try_parse_from(["slc-local-tts", "download", "M1"]).unwrap();
        assert_eq!(
            cli.command,
            Some(Command::Download {
                voice: "M1".to_string()
            })
        );
    }
}
