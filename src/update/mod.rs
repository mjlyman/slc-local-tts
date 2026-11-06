//! The Velopack installer's hooks, the background update check, and the
//! startup apply of a downloaded update.

mod check;
mod reinstall;
mod velopack_updater;

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use velopack::VelopackApp;

use crate::config::Config;
use velopack_updater::VelopackUpdater;

pub use velopack_updater::{FEED_ENV, REPO_URL};

/// The version Velopack just updated us to, set by the `on_restarted` hook.
/// A `OnceLock` is a static that can be written once, at run time, and read
/// from any thread afterwards; `serve` reads it to print a note.
static UPDATED_TO: OnceLock<String> = OnceLock::new();

/// Velopack's startup handler, set up for this app. `main` calls `.run()` on
/// it before anything else.
///
/// - **Automatic apply is off.** Velopack's default applies any newer
///   downloaded update at once, in whichever copy of the exe starts,
///   including a `voices` or `download` started mid-flight. Instead, only
///   `serve` applies one, at startup.
/// - **The install hook keeps a repair or reinstall from deleting the
///   user's voices and settings** (`reinstall`).
/// - **After an update,** the restarted copy records its new version for
///   `updated_to`.
///
/// An uninstall callback, when one is needed, goes here too.
pub fn hooks() -> VelopackApp<'static> {
    VelopackApp::build()
        .set_auto_apply_on_startup(false)
        .on_after_install_fast_callback(|_| reinstall::keep_user_data())
        .on_restarted(|version| {
            // `set` fails only if the value is already set; the hook runs
            // once per process, so there is nothing to handle.
            let _ = UPDATED_TO.set(version.to_string());
        })
}

/// The version this copy was just updated to, if Velopack started it right
/// after applying an update.
pub fn updated_to() -> Option<&'static str> {
    UPDATED_TO.get().map(String::as_str)
}

/// The line `serve` prints after an update.
pub fn updated_note(version: &str) -> String {
    format!("SLC Local TTS updated itself to {version}.")
}

/// Where this run's update check stands. It serializes to
/// JSON, for example `{"state":"ready","version":"1.4.0"}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdateState {
    /// This copy wasn't installed with Setup.exe, so it can't update itself.
    NotInstalled,
    /// `check_for_updates = false` in `config.toml`.
    Off,
    /// Nothing newer. Also the value while the check is still running.
    UpToDate,
    Downloading,
    /// Downloaded; it installs the next time `serve` starts.
    Ready {
        version: String,
    },
    Failed {
        reason: String,
    },
}

/// Why an update step failed, in words for the log.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("this copy wasn't installed with Setup.exe")]
    NotInstalled,
    #[error("the update server couldn't be reached ({0})")]
    Network(String),
    #[error("the downloaded update was damaged ({0})")]
    Damaged(String),
    #[error("{0}")]
    Other(String),
}

/// Starts this run's update check in the background and returns a receiver
/// that always holds its latest state. It must be called inside the tokio
/// runtime.
pub fn spawn_check(config: &Config) -> watch::Receiver<UpdateState> {
    check::spawn_check_with(config.check_for_updates, VelopackUpdater::from_env())
}

/// Starts installing an update that an earlier run downloaded, if there is
/// one. `Ok(false)` means there's nothing to install (or this copy wasn't
/// installed with Setup.exe), so carry on.
///
/// `Ok(true)` means the updater is now waiting for this process to exit, so
/// the caller must return from `main` promptly: the updater gives up after
/// 60 s.
pub fn apply_pending_and_restart() -> Result<bool, UpdateError> {
    match VelopackUpdater::from_env() {
        Err(UpdateError::NotInstalled) => Ok(false),
        Err(e) => Err(e),
        Ok(updater) => check::apply_pending(&updater),
    }
}

/// The plain-English line for each update state.
pub fn state_message(state: &UpdateState) -> String {
    match state {
        UpdateState::NotInstalled => {
            "Updates are off in this copy, because it wasn't installed with Setup.exe.".to_string()
        }
        UpdateState::Off => {
            "Update checks are off (check_for_updates = false in config.toml).".to_string()
        }
        UpdateState::UpToDate => "SLC Local TTS is up to date.".to_string(),
        UpdateState::Downloading => "Downloading an update in the background. It installs the next time SLC Local TTS starts.".to_string(),
        UpdateState::Ready { version } => format!(
            "Version {version} is ready. It installs the next time SLC Local TTS starts, so it never interrupts a flight."
        ),
        UpdateState::Failed { reason } => format!(
            "Couldn't check for updates: {reason}. SLC Local TTS keeps working and tries again the next time it starts."
        ),
    }
}

/// Logs the update check's progress until the check ends.
///
/// First, the current value is logged:
/// - **always,** if the check published it, which means the check has
///   already finished (so even "up to date" gets its line);
/// - **otherwise,** unless it's `UpToDate`. That's the channel's starting
///   value, which only means the check is still running. So `NotInstalled`,
///   `Off` and an early `Failed`, which never come from a running check,
///   each get their line.
///
/// Then each later change is logged, until the sender is dropped.
pub async fn log_changes(mut updates: watch::Receiver<UpdateState>) {
    // `borrow_and_update` reads the value and marks it seen, so `changed()`
    // below wakes only for later values. `has_changed()` says whether this
    // value was new to us, which here means the check published it rather
    // than it being the starting value. The block ends the borrow before any
    // `.await`: holding it would block the sender.
    let (first, published) = {
        let current = updates.borrow_and_update();
        (current.clone(), current.has_changed())
    };
    if published || first != UpdateState::UpToDate {
        log_state(&first);
    }
    while updates.changed().await.is_ok() {
        let state = updates.borrow_and_update().clone();
        log_state(&state);
    }
}

fn log_state(state: &UpdateState) {
    let message = state_message(state);
    match state {
        UpdateState::NotInstalled => tracing::debug!("{message}"),
        UpdateState::Failed { .. } => tracing::warn!("{message}"),
        _ => tracing::info!("{message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updated_note_text() {
        assert_eq!(
            updated_note("0.2.1"),
            "SLC Local TTS updated itself to 0.2.1."
        );
    }

    #[test]
    fn not_restarted_under_cargo_test() {
        assert_eq!(updated_to(), None);
    }

    fn all_states() -> Vec<UpdateState> {
        vec![
            UpdateState::NotInstalled,
            UpdateState::Off,
            UpdateState::UpToDate,
            UpdateState::Downloading,
            UpdateState::Ready {
                version: "1.4.0".to_string(),
            },
            UpdateState::Failed {
                reason: "the update server couldn't be reached (timed out)".to_string(),
            },
        ]
    }

    #[test]
    fn update_state_json_shape() {
        let ready = UpdateState::Ready {
            version: "1.4.0".to_string(),
        };
        assert_eq!(
            serde_json::to_string(&ready).unwrap(),
            r#"{"state":"ready","version":"1.4.0"}"#
        );
        assert_eq!(
            serde_json::to_string(&UpdateState::NotInstalled).unwrap(),
            r#"{"state":"not_installed"}"#
        );
        for state in all_states() {
            let json = serde_json::to_string(&state).unwrap();
            let back: UpdateState = serde_json::from_str(&json).unwrap();
            assert_eq!(back, state, "{json}");
        }
    }

    #[test]
    fn state_messages() {
        let expected = [
            "Updates are off in this copy, because it wasn't installed with Setup.exe.",
            "Update checks are off (check_for_updates = false in config.toml).",
            "SLC Local TTS is up to date.",
            "Downloading an update in the background. It installs the next time SLC Local TTS starts.",
            "Version 1.4.0 is ready. It installs the next time SLC Local TTS starts, so it never interrupts a flight.",
            "Couldn't check for updates: the update server couldn't be reached (timed out). SLC Local TTS keeps working and tries again the next time it starts.",
        ];
        for (state, text) in all_states().iter().zip(expected) {
            assert_eq!(state_message(state), text);
        }
    }

    #[test]
    fn apply_pending_and_restart_is_false_when_not_installed() {
        assert!(matches!(apply_pending_and_restart(), Ok(false)));
    }

    #[tokio::test]
    async fn spawn_check_is_not_installed_under_cargo_test() {
        let updates = spawn_check(&Config::default());
        assert_eq!(*updates.borrow(), UpdateState::NotInstalled);
    }

    /// Runs `f` with a subscriber that records every log line on this thread,
    /// and returns what it wrote (the same helper as in `logging` and
    /// `api::error`).
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
        // statement as "never enabled" because another test thread reached it
        // first.
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

    /// Runs `log_changes` to the end on this thread, so the thread's capture
    /// subscriber sees its lines. The senders are already dropped, so it
    /// never waits.
    fn logged(updates: watch::Receiver<UpdateState>) -> String {
        capture_logs(|| {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(log_changes(updates));
        })
    }

    #[test]
    fn log_changes_reports_up_to_date_only_once_the_check_published_it() {
        let up_to_date = "SLC Local TTS is up to date.";

        // The check finished (published `UpToDate`) before logging started.
        let (sender, updates) = watch::channel(UpdateState::UpToDate);
        sender.send_replace(UpdateState::UpToDate);
        drop(sender);
        let output = logged(updates);
        assert_eq!(output.matches(up_to_date).count(), 1, "{output}");

        // Only the initial placeholder: the check never said anything.
        let (sender, updates) = watch::channel(UpdateState::UpToDate);
        drop(sender);
        let output = logged(updates);
        assert_eq!(output, "", "{output}");
    }
}
