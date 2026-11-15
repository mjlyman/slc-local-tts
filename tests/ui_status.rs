//! The Status page: the state line, SLC's address, the trace, the failure
//! notice and the "Connect SLC" steps.

mod ui_common;

use eframe::egui::accesskit::Role;
use egui_kittest::kittest::Queryable;

use slc_local_tts::status::records::Route;
use slc_local_tts::timefmt::local_time;
use slc_local_tts::ui::app::Page;
use slc_local_tts::ui::backend::Action;
use slc_local_tts::ui::model::status::CONNECT_STEP_3;
use ui_common::{
    NOW_MS, actions, harness, narrow_harness, never_connected, ready, stopped, with_failures,
};

#[test]
fn copy_button_copies_address() {
    let mut harness = harness(ready(), false);
    harness.run();
    assert!(harness.query_by_label("http://127.0.0.1:8765").is_some());
    harness
        .get_by_role_and_label(Role::Button, "Copy SLC address")
        .click();
    harness.run();
    assert_eq!(
        actions(),
        vec![Action::CopyText("http://127.0.0.1:8765".to_string())]
    );
}

#[test]
fn connect_steps_shown_before_first_request() {
    let mut harness = harness(never_connected(), false);
    harness.run();
    assert_eq!(CONNECT_STEP_3, "Enter any API key; it isn't checked.");
    assert!(
        harness
            .query_by_label("Enter any API key; it isn't checked.")
            .is_some()
    );
    // Step 2's own Copy button copies the same address.
    harness
        .get_by_role_and_label(Role::Button, "Copy the address in step 2")
        .click();
    harness.run();
    assert_eq!(
        actions(),
        vec![Action::CopyText("http://127.0.0.1:8765".to_string())]
    );

    // Once SLC has asked for something, the steps are gone.
    let mut harness = ui_common::harness(ready(), false);
    harness.run();
    assert!(harness.query_by_label(CONNECT_STEP_3).is_none());
}

#[test]
fn stopped_shows_start_button() {
    let mut harness = harness(stopped(), false);
    harness.run();
    assert!(
        harness
            .query_by_label("SLC Local TTS has stopped.")
            .is_some()
    );
    harness.get_by_label("Start SLC Local TTS").click();
    harness.run();
    assert_eq!(actions(), vec![Action::StartServer]);

    // A server that answers offers no such button.
    let mut harness = ui_common::harness(ready(), false);
    harness.run();
    assert!(harness.query_by_label("Start SLC Local TTS").is_none());
}

#[test]
fn see_why_opens_activity_problems_only() {
    let mut harness = harness(with_failures(1), false);
    harness.run();
    assert!(
        harness
            .query_by_label(
                "1 announcement failed in the last hour, and SLC played its failure sound instead."
            )
            .is_some()
    );
    assert!(!harness.state().activity.problems_only);
    harness.get_by_label("See why").click();
    harness.run();
    assert_eq!(harness.state().page, Page::Activity);
    assert!(harness.state().activity.problems_only);

    // No failures, no notice.
    let mut harness = ui_common::harness(with_failures(0), false);
    harness.run();
    assert!(harness.query_by_label("See why").is_none());
}

#[test]
fn trace_bar_click_selects_record_in_activity() {
    let snapshot = with_failures(1);
    let spoken = &snapshot.records[0];
    assert_eq!(spoken.audio_seconds, Some(6.1));
    let seq = spoken.seq;
    let label = format!("{}, spoke 6.1 s", local_time(spoken.at_ms));
    let failed_label = format!("{}, failed", local_time(snapshot.records[1].at_ms));
    // The preview failed too, but previews aren't announcements.
    let preview = &snapshot.records[2];
    assert_eq!(preview.route, Route::Preview);
    let preview_label = format!("{}, failed", local_time(preview.at_ms));
    assert!(spoken.at_ms < NOW_MS);

    let mut harness = harness(snapshot, false);
    harness.run();
    // The failed line has a bar too; the preview has none.
    assert!(harness.query_by_label(&failed_label).is_some());
    assert!(harness.query_by_label(&preview_label).is_none());
    harness.get_by_role_and_label(Role::Button, &label).click();
    harness.run();
    assert_eq!(harness.state().page, Page::Activity);
    assert_eq!(harness.state().activity.highlight, Some(seq));
    assert!(!harness.state().activity.problems_only);
}

#[test]
fn bar_click_after_see_why_shows_every_line() {
    let snapshot = with_failures(1);
    let spoken = &snapshot.records[0];
    let seq = spoken.seq;
    let label = format!("{}, spoke 6.1 s", local_time(spoken.at_ms));

    let mut harness = harness(snapshot, false);
    harness.run();
    harness.get_by_label("See why").click();
    harness.run();
    assert!(harness.state().activity.problems_only);
    harness
        .get_by_role_and_label(Role::Button, "Status")
        .click();
    harness.run();
    harness.get_by_role_and_label(Role::Button, &label).click();
    harness.run();
    assert_eq!(harness.state().page, Page::Activity);
    assert_eq!(harness.state().activity.highlight, Some(seq));
    // Filtered to problems, the spoken line wouldn't be in the list.
    assert!(!harness.state().activity.problems_only);

    // And "See why" afterwards drops that line's highlight.
    harness
        .get_by_role_and_label(Role::Button, "Status")
        .click();
    harness.run();
    harness.get_by_label("See why").click();
    harness.run();
    assert!(harness.state().activity.problems_only);
    assert_eq!(harness.state().activity.highlight, None);
}

#[test]
fn connect_steps_fit_the_narrowest_window() {
    const WIDTH: f32 = 380.0;
    let mut harness = narrow_harness(never_connected(), WIDTH);
    harness.run();
    for label in [
        "Copy the address in step 2",
        "Copy SLC address",
        "Refresh the voice list and pick a voice. Press its preview button once to download it.",
    ] {
        let right = harness.get_by_label(label).rect().right();
        assert!(right <= WIDTH, "{label} ends at {right}, past {WIDTH}");
    }
}
