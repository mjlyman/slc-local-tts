//! Updates on the Status page: the banner for a downloaded update, its
//! "Restart and update now…" question, and the line shown just after an
//! update.

mod ui_common;

use eframe::egui::accesskit::Role;
use egui_kittest::kittest::Queryable;

use slc_local_tts::ui::backend::{Action, Connection, Snapshot};
use slc_local_tts::ui::model::update::{release_url, restart_prompt};
use ui_common::{actions, harness, idle_status, ready, update_ready};

const BANNER: &str = "Version 1.4.0 is ready. It installs the next time SLC Local TTS starts, so it never interrupts a flight.";
const RESTART: &str = "Restart and update now…";

/// Just updated to 1.4.0, with nothing newer waiting.
fn just_updated() -> Snapshot {
    let mut status = idle_status();
    status.updated_to = Some("1.4.0".to_string());
    Snapshot {
        status: Some(status),
        ..ready()
    }
}

#[test]
fn banner_shown_when_ready() {
    let mut harness = harness(update_ready("1.4.0"), false);
    harness.run();
    assert!(harness.query_by_label(BANNER).is_some());
    assert!(
        harness
            .query_by_role_and_label(Role::Button, RESTART)
            .is_some()
    );

    // Nothing to install, no banner.
    let mut harness = ui_common::harness(ready(), false);
    harness.run();
    assert!(harness.query_by_label(BANNER).is_none());
    assert!(
        harness
            .query_by_role_and_label(Role::Button, RESTART)
            .is_none()
    );
}

#[test]
fn restart_asks_then_applies() {
    let mut harness = harness(update_ready("1.4.0"), false);
    harness.run();
    let prompt = restart_prompt("1.4.0");
    assert!(harness.query_by_label(&prompt).is_none());

    harness.get_by_role_and_label(Role::Button, RESTART).click();
    harness.run();
    assert!(harness.query_by_label(&prompt).is_some());

    harness.get_by_role_and_label(Role::Button, "No").click();
    harness.run();
    assert!(harness.query_by_label(&prompt).is_none());
    assert!(actions().is_empty(), "No restarts nothing");

    harness.get_by_role_and_label(Role::Button, RESTART).click();
    harness.run();
    harness.get_by_role_and_label(Role::Button, "Yes").click();
    harness.run();
    assert_eq!(actions(), vec![Action::ApplyUpdate]);
    assert!(harness.query_by_label(&prompt).is_none());
}

#[test]
fn no_restart_button_under_cli() {
    let mut snapshot = update_ready("1.4.0");
    if let Some(status) = &mut snapshot.status {
        status.can_restart_to_update = false;
    }
    let mut harness = harness(snapshot, false);
    harness.run();
    // The banner still says the update installs at the next start.
    assert!(harness.query_by_label(BANNER).is_some());
    assert!(
        harness
            .query_by_role_and_label(Role::Button, RESTART)
            .is_none()
    );
}

#[test]
fn no_restart_button_once_the_server_is_gone() {
    let mut snapshot = update_ready("1.4.0");
    snapshot.connection = Connection::Lost;
    let mut harness = harness(snapshot, false);
    harness.run();
    // The last status is kept, so the banner still says what's waiting.
    assert!(harness.query_by_label(BANNER).is_some());
    assert!(
        harness
            .query_by_role_and_label(Role::Button, RESTART)
            .is_none()
    );
}

#[test]
fn updated_note_links_release() {
    let mut harness = harness(just_updated(), false);
    harness.run();
    assert!(harness.query_by_label("Updated to 1.4.0.").is_some());

    harness.get_by_label("What's new").click();
    harness.run();
    assert_eq!(actions(), vec![Action::OpenUrl(release_url("1.4.0"))]);

    harness.get_by_role_and_label(Role::Button, "OK").click();
    harness.run();
    assert!(harness.query_by_label("Updated to 1.4.0.").is_none());
    assert!(harness.query_by_label("What's new").is_none());

    // Without an update this run, there's no note.
    let mut harness = ui_common::harness(ready(), false);
    harness.run();
    assert!(harness.query_by_label("Updated to 1.4.0.").is_none());
}
