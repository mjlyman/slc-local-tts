//! The Activity page: its entries, the "Problems only" switch, and the
//! log folder button.

mod ui_common;

use eframe::egui::{self, Rect, accesskit::Role};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use slc_local_tts::ui::app::{Page, WindowApp};
use slc_local_tts::ui::backend::{Action, Connection, Snapshot};
use slc_local_tts::ui::model::activity::NOT_KEPT;
use ui_common::{actions, backend, harness, numbered_records, ready, voices_all, with_failures};

/// The note under the list.
const NOTE: &str =
    "Entries cover the time since SLC Local TTS last started. Older history is in the log files.";

/// The spoken line in [`ui_common::records_mixed`].
const SPOKEN_TEXT: &str = "Ladies and gentlemen, welcome aboard.";
/// The line that missed SLC's time limit there.
const LATE_TEXT: &str = "Cabin crew, please take your seats for departure.";

/// The window on its Activity page, over the mixed records.
fn activity_page() -> Harness<'static, WindowApp> {
    let mut harness = harness(with_failures(1), false);
    harness.state_mut().page = Page::Activity;
    harness.run();
    harness
}

#[test]
fn problems_only_switch_filters() {
    let mut harness = activity_page();
    assert!(harness.query_by_label(SPOKEN_TEXT).is_some());
    assert!(harness.query_by_label(LATE_TEXT).is_some());

    harness
        .get_by_role_and_label(Role::Switch, "Problems only")
        .click();
    harness.run();
    assert!(harness.state().activity.problems_only);
    assert!(harness.query_by_label(SPOKEN_TEXT).is_none());
    assert!(harness.query_by_label(LATE_TEXT).is_some());
}

#[test]
fn open_log_folder_sends_action() {
    let mut harness = activity_page();
    harness
        .get_by_role_and_label(Role::Button, "Open log folder")
        .click();
    harness.run();
    assert_eq!(actions(), vec![Action::OpenLogFolder]);
}

#[test]
fn failed_entry_shows_reason() {
    let harness = activity_page();
    // The line that missed SLC's time limit while its voice downloaded.
    assert!(
        harness
            .query_by_label("Failed: SLC played its failure sound")
            .is_some()
    );
    let late = format!(
        "The voice wasn't installed yet, and SLC waits only 10 seconds for a line. The \
         download keeps going, and later announcements will speak once it finishes. Install \
         voices on the Voices page before a flight. {NOT_KEPT}"
    );
    assert!(harness.query_by_label(&late).is_some());
    // The preview played as silence, with the same reason it would have
    // had as an announcement, but SLC never keeps a preview.
    assert!(
        harness
            .query_by_label("Preview played as silence")
            .is_some()
    );
    assert!(
        harness
            .query_by_label(
                "The voice couldn't be downloaded: no internet connection. Press Download on \
                 the Voices page to try again."
            )
            .is_some()
    );
    // The unspeakable line isn't a failure.
    assert!(
        harness
            .query_by_label("Nothing to say: the line had no letters or digits")
            .is_some()
    );
    assert!(
        harness
            .query_by_label(
                "Entries cover the time since SLC Local TTS last started. Older history is in \
                 the log files."
            )
            .is_some()
    );
}

/// The Activity page over `count` numbered lines, with `highlight` picked
/// out.
fn numbered_page(count: u64, highlight: Option<u64>) -> Harness<'static, WindowApp> {
    let snapshot = Snapshot {
        records: numbered_records(count),
        ..ready()
    };
    let mut harness = harness(snapshot, false);
    harness.state_mut().page = Page::Activity;
    harness.state_mut().activity.highlight = highlight;
    harness.run();
    harness
}

/// Where the list shows its entries on screen: between the controls above
/// it and the note below it.
fn list_area(harness: &Harness<'static, WindowApp>) -> Rect {
    let top = harness
        .get_by_role_and_label(Role::Switch, "Problems only")
        .rect()
        .bottom();
    let bottom = harness.get_by_label(NOTE).rect().top();
    Rect::from_x_y_ranges(0.0..=f32::INFINITY, top..=bottom)
}

/// Whether the entry saying `text` is wholly inside the list area.
fn in_view(harness: &Harness<'static, WindowApp>, text: &str) -> bool {
    let entry = harness.get_by_label(text).rect();
    let area = list_area(harness);
    entry.top() >= area.top() && entry.bottom() <= area.bottom()
}

/// Turns the mouse wheel by `delta_y` points over the middle of the list.
/// Positive moves the content down, showing what's above.
fn wheel(harness: &mut Harness<'static, WindowApp>, delta_y: f32) {
    let area = list_area(harness);
    harness.hover_at(egui::pos2(200.0, area.center().y));
    harness.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, delta_y),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: egui::Modifiers::NONE,
    });
    // A wheel scroll glides for a few frames, more than `run` allows.
    harness.run_steps(20);
}

#[test]
fn highlighted_entry_scrolled_into_view() {
    // Without a highlight the list starts at the newest line.
    let harness = numbered_page(40, None);
    assert!(in_view(&harness, "Line number 40."));
    assert!(
        !in_view(&harness, "Line number 20."),
        "the list is too short"
    );

    // A line from the middle, so scrolling to the end wouldn't pass.
    let harness = numbered_page(40, Some(20));
    assert!(in_view(&harness, "Line number 20."));
    assert!(!in_view(&harness, "Line number 40."));
    assert!(!in_view(&harness, "Line number 1."));
}

#[test]
fn highlight_scrolls_only_once() {
    let mut harness = numbered_page(40, Some(20));
    assert!(in_view(&harness, "Line number 20."));
    // The user scrolls back to the top; the list stays there.
    wheel(&mut harness, 5_000.0);
    harness.run();
    assert!(in_view(&harness, "Line number 40."));
    assert!(
        !in_view(&harness, "Line number 20."),
        "the list scrolled back to the highlight"
    );
}

#[test]
fn activity_opens_at_top_after_voices_scrolled() {
    let snapshot = Snapshot {
        voices: Some(voices_all()),
        records: numbered_records(40),
        ..ready()
    };
    let mut harness = harness(snapshot, false);
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Voices")
        .click();
    harness.run();
    let first_row = harness.get_by_label("Supertonic").rect().top();
    // Scroll Voices well down its list.
    harness.hover_at(egui::pos2(200.0, 400.0));
    harness.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -2_000.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: egui::Modifiers::NONE,
    });
    // A wheel scroll glides for a few frames, more than `run` allows.
    harness.run_steps(20);
    assert!(
        harness
            .query_by_label("Supertonic")
            .is_none_or(|node| node.rect().top() < first_row - 1_000.0),
        "Voices didn't scroll"
    );

    harness
        .get_by_role_and_label(Role::Button, "Activity")
        .click();
    harness.run();
    assert!(
        in_view(&harness, "Line number 40."),
        "Activity opened where Voices was scrolled to"
    );
}

#[test]
fn empty_list_says_so() {
    let mut harness = harness(ready(), false);
    harness.state_mut().page = Page::Activity;
    harness.run();
    assert!(
        harness
            .query_by_label("Nothing yet. Each line SLC asks for shows here.")
            .is_some()
    );
    assert!(
        harness
            .query_by_label("No problems since SLC Local TTS last started.")
            .is_none()
    );
}

#[test]
fn no_problems_says_so() {
    let snapshot = Snapshot {
        records: numbered_records(3),
        ..ready()
    };
    let mut harness = harness(snapshot, false);
    harness.state_mut().page = Page::Activity;
    harness.state_mut().activity.problems_only = true;
    harness.run();
    assert!(
        harness
            .query_by_label("No problems since SLC Local TTS last started.")
            .is_some()
    );
    assert!(harness.query_by_label("Line number 1.").is_none());
}

#[test]
fn lost_connection_drops_highlight() {
    let mut harness = numbered_page(40, Some(20));
    // The server stops. When it starts again its lines count from 1 again,
    // so an old highlight would pick out an unrelated line.
    backend().set_snapshot(Snapshot {
        connection: Connection::Lost,
        records: numbered_records(40),
        ..ready()
    });
    harness.run();
    assert_eq!(harness.state().activity.highlight, None);
    // Only the moment it's lost: a line picked out afterwards, from the
    // trace still showing, stays picked out.
    harness.state_mut().activity.highlight = Some(5);
    harness.run();
    assert_eq!(harness.state().activity.highlight, Some(5));
}
