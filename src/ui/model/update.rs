//! What the window says about updates: the banner for a downloaded update,
//! the question "Restart and update now…" asks, the line shown just after
//! an update, and the link to that version's release notes.

use crate::update::{self, UpdateState};

/// The banner for an update that's ready to install, and whether its
/// "Restart and update now…" button shows. `None` unless an update is
/// ready. The button needs a program that can restart itself
/// (`can_restart`), which the console program can't.
pub fn banner(state: &UpdateState, can_restart: bool) -> Option<(String, bool)> {
    match state {
        // The log says the same, so the text has one source.
        UpdateState::Ready { .. } => Some((update::state_message(state), can_restart)),
        _ => None,
    }
}

/// The question asked before restarting to install `version`.
pub fn restart_prompt(version: &str) -> String {
    format!(
        "Restart SLC Local TTS to install {version}? It stops for a few seconds. \
         Announcements SLC asks for in that time get its failure sound, and pressing a \
         voice preview in SLC before it's back will crash SLC."
    )
}

/// The window's short line just after an update. The log has a longer one.
pub fn updated_line(version: &str) -> String {
    format!("Updated to {version}.")
}

/// The release notes of `version` on GitHub.
pub fn release_url(version: &str) -> String {
    format!("{}/releases/tag/v{version}", update::REPO_URL)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(version: &str) -> UpdateState {
        UpdateState::Ready {
            version: version.to_string(),
        }
    }

    #[test]
    fn banner_text_for_ready() {
        assert_eq!(
            banner(&ready("1.4.0"), true),
            Some((
                "Version 1.4.0 is ready. It installs the next time SLC Local TTS starts, so it \
                 never interrupts a flight."
                    .to_string(),
                true
            ))
        );
        // The same text, without the button, where restarting can't work.
        assert_eq!(
            banner(&ready("1.4.0"), false).map(|(_, button)| button),
            Some(false)
        );
    }

    #[test]
    fn no_banner_unless_ready() {
        for state in [
            UpdateState::NotInstalled,
            UpdateState::Off,
            UpdateState::UpToDate,
            UpdateState::Downloading,
            UpdateState::Failed {
                reason: "offline".to_string(),
            },
        ] {
            assert_eq!(banner(&state, true), None, "{state:?}");
        }
    }

    #[test]
    fn restart_prompt_text() {
        assert_eq!(
            restart_prompt("1.4.0"),
            "Restart SLC Local TTS to install 1.4.0? It stops for a few seconds. Announcements \
             SLC asks for in that time get its failure sound, and pressing a voice preview in \
             SLC before it's back will crash SLC."
        );
    }

    #[test]
    fn updated_line_text() {
        assert_eq!(updated_line("1.4.0"), "Updated to 1.4.0.");
    }

    #[test]
    fn release_url_points_at_the_tag() {
        assert_eq!(
            release_url("1.4.0"),
            "https://github.com/mjlyman/slc-local-tts/releases/tag/v1.4.0"
        );
    }
}
