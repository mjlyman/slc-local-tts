//! The Settings page: its switches, the threads stepper, the port field,
//! the note about restarting, and the links to config.toml and the
//! licences.

mod ui_common;

use eframe::egui::{self, accesskit::Role};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use slc_local_tts::config::Config;
use slc_local_tts::config_edit::FileSettings;
use slc_local_tts::ui::app::{Page, WindowApp};
use slc_local_tts::ui::backend::{Action, Snapshot};
use slc_local_tts::update::UpdateState;
use ui_common::{actions, harness, ready};

const PENDING: &str = "Changes apply the next time SLC Local TTS starts.";
const BAD_PORT: &str = "Port must be a number from 1 to 65535.";

/// The settings the server started with, saved unchanged in the file.
fn defaults() -> FileSettings {
    FileSettings::from_config(&Config::default())
}

/// A save from the page, which drew from the defaults.
fn save(new: FileSettings) -> Action {
    Action::SaveSettings {
        base: defaults(),
        new,
    }
}

/// A connected server with `file` in `config.toml`.
fn with_file(file: FileSettings) -> Snapshot {
    Snapshot {
        settings: Some(Ok(file)),
        ..ready()
    }
}

/// The window on its Settings page, showing `snapshot`.
fn settings_page(snapshot: Snapshot) -> Harness<'static, WindowApp> {
    let mut harness = harness(snapshot, false);
    harness.state_mut().page = Page::Settings;
    harness.run();
    harness
}

/// Replaces the port field's text with `text`, leaving the field focused.
fn type_in_port(harness: &mut Harness<'static, WindowApp>, text: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, "Port")
        .click();
    harness.run();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.run();
    harness
        .get_by_role_and_label(Role::TextInput, "Port")
        .type_text(text);
    harness.run();
}

/// Replaces the port field's text with `text`, then presses Enter.
fn type_port(harness: &mut Harness<'static, WindowApp>, text: &str) {
    type_in_port(harness, text);
    harness.key_press(egui::Key::Enter);
    harness.run();
}

/// The note under the port field, for `port`.
fn port_note(port: u16) -> String {
    format!(
        "SLC must use the same port. After changing it, set SLC's Custom API URL to \
         http://127.0.0.1:{port}."
    )
}

/// The port field's text.
fn port_value(harness: &Harness<'static, WindowApp>) -> Option<String> {
    harness
        .get_by_role_and_label(Role::TextInput, "Port")
        .value()
}

#[test]
fn toggle_check_for_updates_saves() {
    let mut harness = settings_page(with_file(defaults()));
    harness
        .get_by_role_and_label(Role::Switch, "Check for updates")
        .click();
    harness.run();
    assert_eq!(
        actions(),
        vec![save(FileSettings {
            check_for_updates: false,
            ..defaults()
        })]
    );
}

#[test]
fn updates_switch_disabled_when_not_installed() {
    let mut snapshot = with_file(defaults());
    snapshot.status.as_mut().unwrap().update = UpdateState::NotInstalled;
    let mut harness = settings_page(snapshot);
    assert!(
        harness
            .query_by_label(
                "Updates aren't available in this copy, because it wasn't installed with \
                 Setup.exe."
            )
            .is_some()
    );
    harness
        .get_by_role_and_label(Role::Switch, "Check for updates")
        .click();
    harness.run();
    assert_eq!(actions(), Vec::<Action>::new());
}

#[test]
fn held_update_note_under_switch_while_off() {
    let held = "While this is off, an update that was already downloaded isn't installed either.";
    let off = FileSettings {
        check_for_updates: false,
        ..defaults()
    };
    let harness = settings_page(with_file(off));
    assert!(harness.query_by_label(held).is_some());

    let harness = settings_page(with_file(defaults()));
    assert!(harness.query_by_label(held).is_none());
}

#[test]
fn threads_stepper_saves() {
    let mut harness = settings_page(with_file(defaults()));
    assert!(harness.query_by_label("CPU threads for speech").is_some());
    assert!(
        harness
            .query_by_label(
                "More threads speak faster but take CPU from your sim. 2 suits most PCs."
            )
            .is_some()
    );
    harness
        .get_by_role_and_label(Role::Button, "More threads")
        .click();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Fewer threads")
        .click();
    harness.run();
    assert_eq!(
        actions(),
        vec![
            save(FileSettings {
                num_threads: 3,
                ..defaults()
            }),
            save(FileSettings {
                num_threads: 1,
                ..defaults()
            }),
        ]
    );
}

#[test]
fn threads_stepper_stops_at_its_ends() {
    for (threads, button) in [(4, "More threads"), (1, "Fewer threads")] {
        let mut harness = settings_page(with_file(FileSettings {
            num_threads: threads,
            ..defaults()
        }));
        harness.get_by_role_and_label(Role::Button, button).click();
        harness.run();
        assert_eq!(actions(), Vec::<Action>::new(), "{button} at {threads}");
    }
}

#[test]
fn port_saved_on_enter() {
    let mut harness = settings_page(with_file(defaults()));
    assert!(harness.query_by_label(&port_note(8765)).is_some());
    type_in_port(&mut harness, "9100");
    // While typing, the note follows the typed port.
    assert!(harness.query_by_label(&port_note(9100)).is_some());
    harness.key_press(egui::Key::Enter);
    harness.run();
    assert_eq!(
        actions(),
        vec![save(FileSettings {
            port: 9100,
            ..defaults()
        })]
    );
    // Out of the field, it shows what the file holds: still 8765 until
    // the save lands, and for good if it failed.
    assert_eq!(port_value(&harness).as_deref(), Some("8765"));
    assert!(harness.query_by_label(&port_note(8765)).is_some());

    ui_common::backend().set_snapshot(with_file(FileSettings {
        port: 9100,
        ..defaults()
    }));
    harness.run();
    assert_eq!(port_value(&harness).as_deref(), Some("9100"));
    assert!(harness.query_by_label(&port_note(9100)).is_some());
}

#[test]
fn port_saved_when_focus_leaves() {
    let mut harness = settings_page(with_file(defaults()));
    type_in_port(&mut harness, "9100");
    harness.get_by_label("CPU threads for speech").click();
    harness.run();
    assert_eq!(
        actions(),
        vec![save(FileSettings {
            port: 9100,
            ..defaults()
        })]
    );
}

#[test]
fn port_and_switch_in_one_frame_saved_together() {
    let mut harness = settings_page(with_file(defaults()));
    type_in_port(&mut harness, "9100");
    // The click takes the focus from the field and flips the switch in
    // the same frame.
    harness
        .get_by_role_and_label(Role::Switch, "Check for updates")
        .click();
    harness.run();
    assert_eq!(
        actions(),
        vec![save(FileSettings {
            check_for_updates: false,
            port: 9100,
            ..defaults()
        })]
    );
}

#[test]
fn note_appearing_above_keeps_port_field_focus() {
    let mut harness = settings_page(with_file(defaults()));
    type_in_port(&mut harness, "87");
    // A status arrives that puts a note above the field.
    let mut snapshot = with_file(defaults());
    snapshot.status.as_mut().unwrap().update = UpdateState::NotInstalled;
    ui_common::backend().set_snapshot(snapshot);
    harness.run();
    assert_eq!(actions(), Vec::<Action>::new());
    let field = harness.get_by_role_and_label(Role::TextInput, "Port");
    assert!(field.is_focused());
    assert_eq!(field.value().as_deref(), Some("87"));

    // The user finishes typing, and the whole port is saved.
    harness
        .get_by_role_and_label(Role::TextInput, "Port")
        .type_text("66");
    harness.run();
    harness.key_press(egui::Key::Enter);
    harness.run();
    assert_eq!(
        actions(),
        vec![save(FileSettings {
            port: 8766,
            ..defaults()
        })]
    );
}

#[test]
fn leaving_the_tab_commits_a_typed_port() {
    let mut harness = settings_page(with_file(defaults()));
    type_in_port(&mut harness, "9100");
    harness.get_by_label("Status").click();
    harness.run();
    assert_eq!(harness.state().page, Page::Status);
    assert_eq!(
        actions(),
        vec![save(FileSettings {
            port: 9100,
            ..defaults()
        })]
    );

    // A port that isn't one is dropped.
    let mut harness = settings_page(with_file(defaults()));
    type_in_port(&mut harness, "abc");
    harness.get_by_label("Status").click();
    harness.run();
    assert_eq!(actions(), Vec::<Action>::new());
}

#[test]
fn opening_settings_reloads_them() {
    let mut harness = harness(with_file(defaults()), false);
    harness.run();
    harness.get_by_label("Settings").click();
    harness.run();
    assert_eq!(harness.state().page, Page::Settings);
    assert_eq!(actions(), vec![Action::ReloadSettings]);
}

#[test]
fn bad_port_shows_error_and_does_not_save() {
    let mut harness = settings_page(with_file(defaults()));
    assert!(harness.query_by_label(BAD_PORT).is_none());
    type_port(&mut harness, "70000");
    assert!(harness.query_by_label(BAD_PORT).is_some());
    assert_eq!(actions(), Vec::<Action>::new());
}

#[test]
fn autostart_switch_sends_action() {
    let mut harness = settings_page(with_file(defaults()));
    harness
        .get_by_role_and_label(Role::Switch, "Start when Windows starts")
        .click();
    harness.run();
    assert_eq!(actions(), vec![Action::SetAutostart(true)]);
}

#[test]
fn open_config_and_licences_send_actions() {
    let mut harness = settings_page(with_file(defaults()));
    harness
        .get_by_role_and_label(Role::Link, "Open config.toml for advanced settings")
        .click();
    harness.run();
    harness
        .get_by_role_and_label(Role::Link, "View licences")
        .click();
    harness.run();
    assert_eq!(actions(), vec![Action::OpenConfig, Action::OpenLicences]);
    assert!(harness.query_by_label("SLC Local TTS 0.3.0").is_some());
    assert!(
        harness
            .query_by_label("Licence: GPL-3.0-or-later")
            .is_some()
    );
}

#[test]
fn pending_note_shown() {
    let harness = settings_page(with_file(defaults()));
    assert!(harness.query_by_label(PENDING).is_none());

    let harness = settings_page(with_file(FileSettings {
        num_threads: 3,
        ..defaults()
    }));
    assert!(harness.query_by_label(PENDING).is_some());
}

#[test]
fn unreadable_file_says_why() {
    let reason = "config.toml: couldn't understand the file. Fix or delete the line it points at.";
    let harness = settings_page(Snapshot {
        settings: Some(Err(reason.to_string())),
        ..ready()
    });
    assert!(harness.query_by_label(reason).is_some());
    // The rows that change the file wait until it can be read; the rest
    // of the page is still there.
    assert!(harness.query_by_label("Check for updates").is_none());
    assert!(
        harness
            .query_by_label("Start when Windows starts")
            .is_some()
    );
    assert!(
        harness
            .query_by_label("Open config.toml for advanced settings")
            .is_some()
    );
}

/// The Speech volume slider's label.
const VOLUME: &str = "Speech volume";

/// `defaults()` with the volume at `percent`.
fn at_volume(percent: u16) -> FileSettings {
    FileSettings {
        volume_percent: percent,
        ..defaults()
    }
}

/// Gives the volume slider the keyboard focus.
fn focus_volume(harness: &mut Harness<'static, WindowApp>) {
    harness.get_by_role_and_label(Role::Slider, VOLUME).focus();
    harness.run();
}

/// Drags the volume slider's handle past its right end (or left end) and
/// lets go, as the user would with the mouse.
fn drag_volume_to_end(harness: &mut Harness<'static, WindowApp>, right: bool) {
    let end = hold_volume_past_end(harness, right);
    harness.drop_at(end);
    harness.run();
}

/// Presses the volume slider's handle and drags it past the slider's
/// right end (or left end), leaving the button held. Returns where the
/// pointer is.
fn hold_volume_past_end(harness: &mut Harness<'static, WindowApp>, right: bool) -> egui::Pos2 {
    let rect = harness.get_by_role_and_label(Role::Slider, VOLUME).rect();
    harness.drag_at(rect.center());
    harness.run();
    let x = if right {
        rect.right() + 50.0
    } else {
        rect.left() - 50.0
    };
    let end = egui::pos2(x, rect.center().y);
    harness.hover_at(end);
    harness.run();
    end
}

/// The volume slider's value, as screen readers read it.
fn volume_value(harness: &Harness<'static, WindowApp>) -> Option<String> {
    harness.get_by_role_and_label(Role::Slider, VOLUME).value()
}

#[test]
fn volume_slider_shows_the_file_value() {
    let harness = settings_page(with_file(at_volume(120)));
    assert_eq!(volume_value(&harness).as_deref(), Some("120%"));
}

#[test]
fn volume_keyboard_step_saves() {
    let mut harness = settings_page(with_file(defaults()));
    focus_volume(&mut harness);
    harness.key_press(egui::Key::ArrowRight);
    harness.run();
    assert_eq!(actions(), vec![save(at_volume(105))]);
}

#[test]
fn volume_slider_stops_at_200() {
    let mut harness = settings_page(with_file(at_volume(200)));
    focus_volume(&mut harness);
    harness.key_press(egui::Key::ArrowRight);
    harness.run();
    assert_eq!(actions(), Vec::<Action>::new());
}

#[test]
fn volume_note_shown() {
    let harness = settings_page(with_file(defaults()));
    assert!(
        harness
            .query_by_label(
                "Applies to new announcements at once; press Play on the Voices page to hear \
                 it. Lines SLC has already saved keep their old volume until you clear its \
                 cache (see the README)."
            )
            .is_some()
    );
}

#[test]
fn volume_saved_once_when_a_drag_ends() {
    let mut harness = settings_page(with_file(defaults()));
    let end = hold_volume_past_end(&mut harness, true);
    // Nothing is saved while the handle is held, but it follows the drag.
    assert_eq!(actions(), Vec::<Action>::new());
    assert_eq!(volume_value(&harness).as_deref(), Some("200%"));
    harness.drop_at(end);
    harness.run();
    assert_eq!(actions(), vec![save(at_volume(200))]);
}

/// `file` in `config.toml`, after `saves` settings saves have come back.
fn after_saves(file: FileSettings, saves: u64) -> Snapshot {
    Snapshot {
        settings_saves: saves,
        ..with_file(file)
    }
}

/// A volume save from `from` to `to`, over the defaults.
fn volume_save(from: u16, to: u16) -> Action {
    Action::SaveSettings {
        base: at_volume(from),
        new: at_volume(to),
    }
}

#[test]
fn volume_shows_the_chosen_value_until_its_save_comes_back() {
    let mut harness = settings_page(with_file(defaults()));
    drag_volume_to_end(&mut harness, true);
    // The save is on its way: the slider stays where the user left it,
    // even when the file is read again (after a hand edit, say) meanwhile.
    assert_eq!(volume_value(&harness).as_deref(), Some("200%"));
    ui_common::backend().set_snapshot(with_file(at_volume(150)));
    harness.run();
    assert_eq!(volume_value(&harness).as_deref(), Some("200%"));

    // The save came back, but it failed: the slider shows the file.
    ui_common::backend().set_snapshot(after_saves(at_volume(150), 1));
    harness.run();
    assert_eq!(volume_value(&harness).as_deref(), Some("150%"));
}

#[test]
fn volume_arrow_presses_add_up() {
    let mut harness = settings_page(with_file(defaults()));
    focus_volume(&mut harness);
    harness.key_press(egui::Key::ArrowRight);
    harness.run();
    harness.key_press(egui::Key::ArrowRight);
    harness.run();
    assert_eq!(volume_value(&harness).as_deref(), Some("110%"));
    assert_eq!(
        actions(),
        vec![volume_save(100, 105), volume_save(105, 110)]
    );
}

#[test]
fn volume_dragged_back_before_the_file_catches_up_is_saved() {
    // The file holds the slider's lowest value.
    let mut harness = settings_page(with_file(at_volume(50)));
    drag_volume_to_end(&mut harness, true);
    // Back to the file's value before the first save reaches the page:
    // still saved, from what the page last asked for, so the file and
    // the server don't stay at 200.
    drag_volume_to_end(&mut harness, false);
    assert_eq!(actions(), vec![volume_save(50, 200), volume_save(200, 50)]);

    // The first save lands while the second is still on its way. Letting
    // go at the volume the file now holds is still saved, since the page
    // last asked for 50.
    ui_common::backend().set_snapshot(after_saves(at_volume(200), 1));
    harness.run();
    assert_eq!(volume_value(&harness).as_deref(), Some("50%"));
    drag_volume_to_end(&mut harness, true);
    assert_eq!(actions().len(), 3);
    assert_eq!(actions()[2], volume_save(50, 200));

    // The other two land, and the file ends at the last volume chosen.
    ui_common::backend().set_snapshot(after_saves(at_volume(50), 2));
    harness.run();
    assert_eq!(volume_value(&harness).as_deref(), Some("200%"));
    ui_common::backend().set_snapshot(after_saves(at_volume(200), 3));
    harness.run();
    assert_eq!(volume_value(&harness).as_deref(), Some("200%"));

    // Caught up: letting go at the file's volume saves nothing.
    drag_volume_to_end(&mut harness, true);
    assert_eq!(actions().len(), 3);
}
