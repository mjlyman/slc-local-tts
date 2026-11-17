//! What the Settings page shows: how far the threads stepper goes, whether
//! saved changes still wait for a restart, why updates may be unavailable
//! or held back, the version line, and what a typed port means.

use crate::api::slc::dto::StatusDto;
use crate::config::Config;
use crate::config_edit::FileSettings;
use crate::update::UpdateState;

/// Under the threads stepper.
pub const THREADS_NOTE: &str =
    "More threads speak faster but take CPU from your sim. 2 suits most PCs.";
/// The volume slider's label.
pub const VOLUME_LABEL: &str = "Speech volume";
/// Under the volume slider.
pub const VOLUME_NOTE: &str = "Applies to new announcements at once; press Play on the Voices \
     page to hear it. Lines SLC has already saved keep their old volume until you clear its \
     cache (see the README).";
/// Shown while `config.toml` holds settings the server hasn't started with.
pub const PENDING_NOTE: &str = "Changes apply the next time SLC Local TTS starts.";
/// Under "Check for updates" in a copy that can't update itself.
pub const UPDATES_NOT_INSTALLED: &str =
    "Updates aren't available in this copy, because it wasn't installed with Setup.exe.";
/// Under "Check for updates" while the file has it off: an update that was
/// downloaded earlier waits too.
pub const HELD_UPDATE_NOTE: &str =
    "While this is off, an update that was already downloaded isn't installed either.";
/// What a port that isn't one says.
pub const BAD_PORT: &str = "Port must be a number from 1 to 65535.";
/// The highest the threads stepper goes, unless the file already says more.
const THREADS_USUAL_MAX: i32 = 4;

/// The Settings page's rows, worked out from the file and the server.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsView {
    /// The threads stepper runs from 1 to this.
    pub threads_max: i32,
    /// True when the file says something the server didn't start with.
    pub pending: bool,
    /// Why "Check for updates" is switched off and can't be changed.
    pub updates_note: Option<&'static str>,
    /// Under "Check for updates" while it's off in the file.
    pub held_update_note: Option<&'static str>,
    /// The About section's first line.
    pub version_line: String,
}

/// The view for the settings in `file`, next to the `effective` config the
/// server started with, its `update` state and its `version`.
pub fn settings_view(
    file: &FileSettings,
    effective: &Config,
    update: &UpdateState,
    version: &str,
) -> SettingsView {
    SettingsView {
        threads_max: threads_max(file),
        pending: *file != FileSettings::from_config(effective),
        updates_note: matches!(update, UpdateState::NotInstalled).then_some(UPDATES_NOT_INSTALLED),
        held_update_note: held_update_note(file, update),
        version_line: version_line(version),
    }
}

/// The view before the server's first answer, when nothing is known about
/// what it started with: no change is shown as pending, and the version is
/// this program's own (the window and the server are the same exe).
pub fn settings_view_without_server(file: &FileSettings) -> SettingsView {
    SettingsView {
        threads_max: threads_max(file),
        pending: false,
        updates_note: None,
        // Whether this copy can update at all isn't known yet.
        held_update_note: None,
        version_line: version_line_for(None),
    }
}

/// [`HELD_UPDATE_NOTE`] while the file has "Check for updates" off, in a
/// copy that can update itself. It reads the file, not what the server
/// started with, because the next start decides from the file too.
fn held_update_note(file: &FileSettings, update: &UpdateState) -> Option<&'static str> {
    let can_update = !matches!(update, UpdateState::NotInstalled);
    (can_update && !file.check_for_updates).then_some(HELD_UPDATE_NOTE)
}

/// The stepper goes to 4, or to the file's value if someone set it higher
/// by hand, so opening the page never lowers it.
fn threads_max(file: &FileSettings) -> i32 {
    THREADS_USUAL_MAX.max(file.num_threads)
}

/// "SLC Local TTS 0.3.0".
fn version_line(version: &str) -> String {
    format!("SLC Local TTS {version}")
}

/// The version line for the server that answered with `status`, or for
/// this program itself before any answer (the window and the server are
/// the same exe).
pub fn version_line_for(status: Option<&StatusDto>) -> String {
    version_line(status.map_or(env!("CARGO_PKG_VERSION"), |status| status.version.as_str()))
}

/// The note under the port field, for the port the user typed.
pub fn port_note(port: u16) -> String {
    format!(
        "SLC must use the same port. After changing it, set SLC's Custom API URL to \
         http://127.0.0.1:{port}."
    )
}

/// The port in `text`, or why it isn't one. Spaces around it are ignored.
pub fn parse_port(text: &str) -> Result<u16, &'static str> {
    match text.trim().parse::<u16>() {
        Ok(port) if port >= 1 => Ok(port),
        _ => Err(BAD_PORT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(num_threads: i32) -> FileSettings {
        FileSettings {
            num_threads,
            ..FileSettings::from_config(&Config::default())
        }
    }

    fn view(file: &FileSettings, update: &UpdateState) -> SettingsView {
        settings_view(file, &Config::default(), update, "0.3.0")
    }

    #[test]
    fn pending_when_file_differs_from_effective() {
        let same = FileSettings::from_config(&Config::default());
        assert!(!view(&same, &UpdateState::UpToDate).pending);

        let changes = [
            FileSettings {
                check_for_updates: !same.check_for_updates,
                ..same.clone()
            },
            FileSettings {
                num_threads: same.num_threads + 1,
                ..same.clone()
            },
            FileSettings {
                port: same.port + 1,
                ..same.clone()
            },
            // Only when the server couldn't be told at once: it reports
            // the volume it plays at.
            FileSettings {
                volume_percent: same.volume_percent + 30,
                ..same.clone()
            },
        ];
        for changed in changes {
            assert!(
                view(&changed, &UpdateState::UpToDate).pending,
                "{changed:?}"
            );
        }
    }

    #[test]
    fn threads_range() {
        assert_eq!(view(&file(2), &UpdateState::UpToDate).threads_max, 4);
        assert_eq!(view(&file(6), &UpdateState::UpToDate).threads_max, 6);
        assert_eq!(settings_view_without_server(&file(6)).threads_max, 6);
    }

    #[test]
    fn port_parse() {
        assert_eq!(parse_port("8765"), Ok(8765));
        assert_eq!(parse_port(" 9100 "), Ok(9100));
        for bad in ["0", "70000", "abc", "", "-1"] {
            assert_eq!(
                parse_port(bad),
                Err("Port must be a number from 1 to 65535."),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn updates_note_when_not_installed() {
        let settings = file(2);
        assert_eq!(
            view(&settings, &UpdateState::NotInstalled).updates_note,
            Some(
                "Updates aren't available in this copy, because it wasn't installed with Setup.exe."
            )
        );
        assert_eq!(view(&settings, &UpdateState::Off).updates_note, None);
        assert_eq!(view(&settings, &UpdateState::UpToDate).updates_note, None);
    }

    #[test]
    fn held_update_note_when_checks_off() {
        let off = FileSettings {
            check_for_updates: false,
            ..file(2)
        };
        // The server started with checks on; the file now says off.
        let effective = Config {
            check_for_updates: true,
            ..Config::default()
        };
        let held = settings_view(&off, &effective, &UpdateState::UpToDate, "0.3.0");
        assert_eq!(
            held.held_update_note,
            Some(
                "While this is off, an update that was already downloaded isn't installed either."
            )
        );

        let on = FileSettings {
            check_for_updates: true,
            ..file(2)
        };
        assert_eq!(view(&on, &UpdateState::UpToDate).held_update_note, None);
        assert_eq!(
            view(&off, &UpdateState::NotInstalled).held_update_note,
            None
        );
    }

    #[test]
    fn version_line_has_one_source() {
        let mut status = crate::api::slc::dto::StatusDto {
            version: "0.3.0".to_string(),
            ..test_status()
        };
        assert_eq!(version_line_for(Some(&status)), "SLC Local TTS 0.3.0");
        assert_eq!(
            version_line_for(None),
            format!("SLC Local TTS {}", env!("CARGO_PKG_VERSION"))
        );
        // The view says the same.
        status.version = "1.4.0".to_string();
        assert_eq!(
            settings_view(&file(2), &status.config, &status.update, &status.version).version_line,
            version_line_for(Some(&status))
        );
        assert_eq!(
            settings_view_without_server(&file(2)).version_line,
            version_line_for(None)
        );
    }

    fn test_status() -> StatusDto {
        StatusDto {
            app: "SLC Local TTS".to_string(),
            version: "0.0.0".to_string(),
            port: 8765,
            state: crate::api::slc::dto::ActivityDto::Idle,
            last_request_ms: None,
            ever_connected: true,
            recent_failures: 0,
            failed_downloads: 0,
            downloads: Vec::new(),
            latest_seq: 0,
            update: UpdateState::UpToDate,
            can_restart_to_update: false,
            updated_to: None,
            config: Config::default(),
        }
    }

    #[test]
    fn version_and_port_lines() {
        assert_eq!(
            view(&file(2), &UpdateState::UpToDate).version_line,
            "SLC Local TTS 0.3.0"
        );
        assert_eq!(
            port_note(9100),
            "SLC must use the same port. After changing it, set SLC's Custom API URL to \
             http://127.0.0.1:9100."
        );
    }
}
