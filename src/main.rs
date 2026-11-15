//! `slc-local-tts` binary, the tray program. It never has a console window:
//! startup errors appear in a message box instead.
//!
//! - With no arguments, it starts the speech server, unless a copy is
//!   already running; then it opens that copy's settings window and exits.
//! - `window --port <n>` (started by the program itself, never by people)
//!   is the settings window, a process of its own.

// The "windows" subsystem means Windows opens no console for this exe.
// Test builds keep the console subsystem, so test output still shows.
#![cfg_attr(not(test), windows_subsystem = "windows")]

use std::fmt::Display;
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use clap::{Parser, Subcommand};

use slc_local_tts::api::slc::status_snapshot;
use slc_local_tts::autostart::{self, RunEntry};
use slc_local_tts::config::{self, Config};
use slc_local_tts::desktop::{dialog, handoff, shell};
use slc_local_tts::paths::DataDir;
use slc_local_tts::server::{self, ServerOptions, Started, StartupError};
use slc_local_tts::startup::{self, Boot, Gate, Mode, SHUTDOWN_GRACE};
use slc_local_tts::stop::{ServedFlag, StopReason, StopSignal};
use slc_local_tts::tray::{self, TrayContext, TrayHandle};
use slc_local_tts::{instance, logging, ui, update};

/// How many times a second launch reads the port file, and how far apart,
/// before it falls back to the port in `config.toml`.
const PORT_FILE_TRIES: u32 = 10;
const PORT_FILE_WAIT: Duration = Duration::from_millis(500);

/// How long installing an update waits for an open settings window to
/// close first.
const WINDOW_CLOSE_WAIT: Duration = Duration::from_secs(2);

/// Local, ElevenLabs-compatible text-to-speech server for Self-Loading Cargo.
#[derive(Debug, Parser)]
#[command(version)]
struct GuiCli {
    // `Option`: with no subcommand (`None`), the server starts.
    #[command(subcommand)]
    command: Option<GuiCommand>,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum GuiCommand {
    /// The settings window, for the server on `port`. Only the program
    /// itself starts it, so it's left out of `--help`.
    #[command(hide = true)]
    Window {
        #[arg(long)]
        port: u16,
        /// The very first start: the window explains the tray icon.
        #[arg(long)]
        first_run: bool,
    },
}

fn main() -> ExitCode {
    // Velopack starts this exe with hook arguments (`--veloapp-install
    // <version>` and friends) while installing, updating and uninstalling.
    // `run()` handles them and exits before anything else happens. It has to
    // come before clap, which would reject those arguments.
    update::hooks().run();

    let cli = match GuiCli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // `--help` and `--version` arrive here too, with exit code 0;
            // a real mistake has clap's usual exit code, 2.
            dialog::error(&e.to_string());
            return ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(2));
        }
    };
    if let Some(GuiCommand::Window { port, first_run }) = cli.command {
        return ui::run_window(port, first_run);
    }

    // One server at a time. The mutex is taken before logging starts, so a
    // second launch never writes to the running server's log file.
    let guard = match instance::acquire() {
        Ok(guard) => guard,
        Err(_) => {
            hand_off();
            return ExitCode::SUCCESS;
        }
    };

    let boot = match startup::boot(Mode::Gui) {
        Ok(boot) => boot,
        Err(text) => {
            dialog::error(&text);
            return ExitCode::FAILURE;
        }
    };

    // The mutex is already held, so the gate's first step just hands the
    // guard over; the gate then installs a downloaded update if there is
    // one, unless "Check for updates" is off. `instance` keeps the mutex
    // until `run` lets it go.
    let allowed = startup::apply_allowed(&boot.config);
    let apply = || {
        if allowed {
            // A settings window may be open: its "Start SLC Local TTS"
            // button starts this copy.
            update::apply_pending_and_restart_after(close_settings_window)
        } else {
            Ok(false)
        }
    };
    let instance = match startup::serve_gate(|| Ok(guard), apply) {
        Gate::Serve(guard) => guard,
        // The updater is waiting for this process to exit before it
        // replaces the program's files, so exit at once.
        Gate::Restarting => {
            logging::flush();
            return ExitCode::SUCCESS;
        }
        // Can't happen: the first step above always succeeds.
        Gate::AlreadyRunning(e) => {
            let code = fail(&e);
            logging::flush();
            return code;
        }
    };

    let code = run(boot, instance);
    // Statics are never dropped in Rust, so the parked log guard must be
    // flushed by hand before the process ends. This also covers "Restart
    // and update now": the updater waits for this process to exit, so the
    // lines about the install are all written by then.
    logging::flush();
    code
}

/// A second launch: open the running copy's settings window on the port it
/// really uses, then exit. Logging never starts here.
fn hand_off() {
    // Without a readable port file or config, the default port.
    let default_port = Config::default().port;
    let port = match DataDir::from_env() {
        Ok(data) => {
            let config_port = config::load(&data.config_file()).map_or(default_port, |c| c.port);
            handoff::resolve_port(
                || handoff::read_port_file(&data),
                config_port,
                PORT_FILE_TRIES,
                PORT_FILE_WAIT,
            )
        }
        Err(_) => default_port,
    };
    if let Err(e) = handoff::spawn_window(port, false) {
        dialog::error(&format!(
            "SLC Local TTS is already running, but its settings window couldn't open ({e}). Try again, or use its icon in the taskbar corner."
        ));
    }
}

/// Everything after the startup checks: config, priority, runtime, then the
/// server and its tray icon until the server is told to stop, and what
/// comes after that ("Restart and update now" installs the update).
/// `instance` is the held instance mutex.
fn run(boot: Boot, instance: instance::InstanceGuard) -> ExitCode {
    let Boot { data, config } = boot;
    let config: Config = match config {
        Ok(config) => config,
        Err(error) => {
            let path = data.config_file();
            return fail(&StartupError::Config { error, path });
        }
    };
    startup::set_priority(config.priority);
    refresh_autostart();

    let runtime = match startup::runtime() {
        Ok(runtime) => runtime,
        Err(e) => return fail(&startup::runtime_error_text(&e)),
    };
    let stop = StopSignal::new();
    // Set once the server has stopped, for the tray thread, which waits for
    // it when Windows ends the session.
    let served_flag = ServedFlag::default();
    let just_updated = update::updated_to().is_some();
    let options = ServerOptions {
        updated_to: update::updated_to().map(str::to_owned),
        can_restart_to_update: true,
        stop: stop.clone(),
    };

    // The tray, once it starts. It's kept outside the async block so it can
    // be shut down after `block_on` returns, on this plain thread.
    let mut tray: Option<TrayHandle> = None;
    let served = runtime.block_on(async {
        let started = server::start(data.clone(), config, options).await?;
        tray = start_tray(&started, &stop, &served_flag);
        // The very first start of the tray program opens the window once,
        // to explain where the program went; so does the first start after
        // an update, to say what it updated to.
        let first_run = handoff::take_first_run(&data);
        if (first_run || just_updated)
            && let Err(e) = handoff::spawn_window(started.port(), first_run)
        {
            tracing::warn!("couldn't open the settings window ({e})");
        }
        startup::serve_until_stopped(started, &data, stop).await
    });
    served_flag.set();
    if let Some(tray) = tray {
        tray.shutdown();
    }
    // Dropping a runtime waits for every blocking task without limit; this
    // waits a few seconds at most.
    runtime.shutdown_timeout(SHUTDOWN_GRACE);
    let reason = match served {
        Ok(reason) => reason,
        Err(error) => {
            drop(instance);
            return fail(&error);
        }
    };
    if reason == StopReason::RestartToUpdate {
        // While the mutex is still held, so no other copy can start (and
        // run its own startup install) during the wait.
        close_settings_window();
    }
    // The mutex goes before `after_stop` installs the update or starts a
    // new copy. A copy started while this one still held it would take
    // itself for a second launch, open a window and exit, leaving SLC with
    // no server.
    drop(instance);
    startup::after_stop(
        reason,
        || update::apply_pending_and_restart().map_err(|e| e.to_string()),
        dialog::error,
        start_again,
    )
}

/// Closes an open settings window and waits a moment for it to go. It runs
/// from the folder that installing an update replaces.
fn close_settings_window() {
    if !shell::close_window_and_wait(ui::TITLE, WINDOW_CLOSE_WAIT) {
        tracing::warn!(
            "the settings window didn't close within {} seconds; installing the update anyway",
            WINDOW_CLOSE_WAIT.as_secs()
        );
    }
}

/// Starts this version again after "Restart and update now" installed
/// nothing.
fn start_again() {
    if let Err(e) = shell::start_server() {
        tracing::error!("couldn't start SLC Local TTS again ({e})");
        dialog::error(&format!(
            "SLC Local TTS couldn't start again ({e}). Start it from the Start menu."
        ));
    }
}

/// Starts the tray icon, and wakes it whenever the status or the update
/// check changes. A tray that can't start is logged, and the server runs
/// on without it.
fn start_tray(started: &Started, stop: &StopSignal, served: &ServedFlag) -> Option<TrayHandle> {
    // Two handles to the same shared state, one for each closure: each
    // `move` closure owns its handle, so the tray thread can keep them.
    let state = started.state();
    let expiry_state = started.state();
    let changes = state.live.subscribe();
    let ctx = TrayContext {
        port: started.port(),
        snapshot: Box::new(move || status_snapshot(&state, SystemTime::now())),
        next_expiry: Box::new(move || expiry_state.live.next_failure_expiry(SystemTime::now())),
        stop: stop.clone(),
        served: served.clone(),
        // Without it, the icon just has no fixed identity.
        exe: std::env::current_exe().unwrap_or_default(),
    };
    match tray::spawn(ctx) {
        Ok(handle) => {
            tokio::spawn(tray::forward_changes(
                changes,
                started.updates(),
                handle.clone(),
            ));
            Some(handle)
        }
        Err(e) => {
            tracing::error!(
                "the tray icon couldn't start ({e}); the server keeps running. Open SLC Local TTS from the Start Menu to see its window."
            );
            None
        }
    }
}

/// Points an existing start-with-Windows entry at this copy, for example
/// after an update moved the program. Only an installed copy does this, so
/// a development build never takes over an installed copy's entry.
fn refresh_autostart() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    if !autostart::in_install_folder(&exe) {
        return;
    }
    if let Err(e) = autostart::refresh(&RunEntry::user(), &autostart::target_exe(&exe)) {
        tracing::warn!("couldn't update the start-with-Windows entry ({e})");
    }
}

/// Reports a fatal error once logging is up: logged, then shown in a
/// message box, since there is no console.
fn fail(error: &dyn Display) -> ExitCode {
    tracing::error!("{error}");
    dialog::error(&error.to_string());
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn gui_cli_default_is_server() {
        let cli = GuiCli::try_parse_from(["slc-local-tts"]).unwrap();
        assert_eq!(cli.command, None);
    }

    #[test]
    fn gui_cli_parses_window() {
        let cli =
            GuiCli::try_parse_from(["slc-local-tts", "window", "--port", "9000", "--first-run"])
                .unwrap();
        assert_eq!(
            cli.command,
            Some(GuiCommand::Window {
                port: 9000,
                first_run: true
            })
        );
    }

    #[test]
    fn window_subcommand_hidden_from_help() {
        let help = GuiCli::command().render_help().to_string();
        assert!(!help.contains("window"), "{help}");
    }
}
