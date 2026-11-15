//! `slc-local-tts-cli` binary, the console program: parses the command line,
//! sets up logging and the console, and hands off to the library. Running
//! with no subcommand starts the server.
//!
//! Unlike the tray program, it never installs a downloaded update: that
//! happens only when the tray program starts.

use std::fmt::{Display, Write as _};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use slc_local_tts::catalog::{self, Archive, Voice};
use slc_local_tts::config::{Config, ConfigError};
use slc_local_tts::instance::AlreadyRunning;
use slc_local_tts::models::install::{self, HttpInstaller};
use slc_local_tts::models::{DiskStore, ModelStore};
use slc_local_tts::paths::DataDir;
use slc_local_tts::server::{self, ServerOptions, StartupError};
use slc_local_tts::startup::{self, Mode, SHUTDOWN_GRACE};
use slc_local_tts::stop::{StopReason, StopSignal};
use slc_local_tts::{console, instance, logging, update};

const VERSION: &str = env!("CARGO_PKG_VERSION");

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
    // Velopack starts the exe with hook arguments (`--veloapp-install
    // <version>` and friends) while installing, updating and uninstalling.
    // `run()` handles them and exits before anything else happens. It has to
    // come before clap, which would reject those arguments.
    update::hooks().run();

    let cli = Cli::parse();
    let owns_console = console::owns_console();

    let boot = match startup::boot(Mode::Cli) {
        Ok(boot) => boot,
        Err(text) => return fail_before_logging(&text, owns_console),
    };

    let code = run(
        cli.command.unwrap_or(Command::Serve),
        boot.data,
        boot.config,
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
    startup::set_priority(config.priority);

    // In a terminal the window belongs to the shell, so its title and
    // settings are left alone. The close handler is always installed:
    // closing a terminal window ends us just the same.
    if owns_console {
        console::disable_quick_edit();
        console::set_title("SLC Local TTS");
    }
    console::install_close_handler();

    let runtime = match startup::runtime() {
        Ok(runtime) => runtime,
        Err(e) => return fail(&startup::runtime_error_text(&e), owns_console),
    };

    let code = match command {
        Command::Serve => {
            // One server at a time: take the instance mutex before binding
            // the port or cleaning anything up. This program never installs
            // a downloaded update, so the mutex is the only check. It's
            // named `_instance`, not `_`, on purpose: `let _ = ..` drops the
            // guard at once, which would release the mutex straight away. A
            // named binding lives to the end of this block, so until `serve`
            // returns.
            let _instance = match instance::acquire() {
                Ok(guard) => guard,
                Err(e) => return fail(&already_running_text(&e), owns_console),
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

/// What `serve` says when another copy already runs. That copy is most
/// likely the tray program, which has no window of its own to look for.
fn already_running_text(error: &AlreadyRunning) -> String {
    format!("{error}\nSLC Local TTS is already running in the taskbar corner.")
}

/// `serve`: start, show the banner, answer requests until Ctrl+C.
async fn serve(data: DataDir, config: Config) -> Result<(), StartupError> {
    let stop = StopSignal::new();
    let options = ServerOptions {
        updated_to: update::updated_to().map(str::to_owned),
        // This program never installs updates: the tray program does.
        can_restart_to_update: false,
        stop: stop.clone(),
    };
    let started = server::start(data.clone(), config, options).await?;
    println!("\n{}\n", server::banner(VERSION, started.port()));
    if let Some(version) = update::updated_to() {
        println!("{}\n", update::updated_note(version));
    }

    // The first Ctrl+C asks the server to stop taking requests, finish the
    // ones in progress and return. The listener is spawned as a task of its
    // own, so it runs alongside the server; `async move` hands it its own
    // clone of the signal.
    let on_ctrl_c = stop.clone();
    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                tracing::info!(
                    "Stopping after the requests in progress finish. Press Ctrl+C again to quit now."
                );
                on_ctrl_c.request(StopReason::CtrlC);
            }
            // Without a Ctrl+C handler, Windows' default one still ends the
            // process. Never requesting a stop keeps the server running.
            Err(e) => tracing::warn!(error = %e, "couldn't listen for Ctrl+C"),
        }
    });
    startup::serve_until_stopped(started, &data, stop).await?;
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
        eprintln!("No voice called '{id}'. Run `slc-local-tts-cli voices` to see the list.");
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
    fn already_running_mentions_taskbar() {
        let text = already_running_text(&AlreadyRunning);
        assert!(
            text.starts_with(&AlreadyRunning.to_string()),
            "the mutex text comes first: {text}"
        );
        assert!(
            text.ends_with("\nSLC Local TTS is already running in the taskbar corner."),
            "{text}"
        );
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
