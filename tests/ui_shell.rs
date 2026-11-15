//! The settings window's shell: the tabs and the first-run note.

mod ui_common;

use eframe::egui::accesskit::{Role, Toggled};
use egui_kittest::kittest::{NodeT, Queryable};

use slc_local_tts::ui::app::Page;
use slc_local_tts::ui::backend::{Action, Notice, Snapshot};
use slc_local_tts::ui::pages::first_run::{AUTOSTART_LABEL, TEXT};
use ui_common::{actions, harness, ready};

#[test]
fn tabs_switch_pages() {
    let mut harness = harness(ready(), false);
    harness.run();
    for name in ["Status", "Voices", "Activity", "Settings"] {
        harness.get_by_role_and_label(Role::Button, name);
    }
    assert!(
        harness
            .query_by_role_and_label(Role::Heading, "Status")
            .is_some()
    );

    harness
        .get_by_role_and_label(Role::Button, "Voices")
        .click();
    harness.run();
    assert_eq!(harness.state().page, Page::Voices);
    assert!(
        harness
            .query_by_role_and_label(Role::Heading, "Voices")
            .is_some()
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Heading, "Status")
            .is_none()
    );
    assert_eq!(actions(), vec![Action::WatchVoices(true)]);

    harness
        .get_by_role_and_label(Role::Button, "Activity")
        .click();
    harness.run();
    assert!(
        harness
            .query_by_role_and_label(Role::Heading, "Activity")
            .is_some()
    );
    assert_eq!(
        actions(),
        vec![Action::WatchVoices(true), Action::WatchVoices(false)]
    );

    harness
        .get_by_role_and_label(Role::Button, "Settings")
        .click();
    harness.run();
    assert!(
        harness
            .query_by_role_and_label(Role::Heading, "Settings")
            .is_some()
    );
    // Leaving a page other than Voices says nothing about the voice list;
    // opening Settings reads the settings again.
    assert_eq!(
        actions(),
        vec![
            Action::WatchVoices(true),
            Action::WatchVoices(false),
            Action::ReloadSettings
        ]
    );
}

#[test]
fn first_run_note_offers_autostart_off() {
    let mut harness = harness(ready(), true);
    harness.run();
    assert!(harness.query_by_label(TEXT).is_some());
    let switch = harness.get_by_role_and_label(Role::Switch, AUTOSTART_LABEL);
    assert_eq!(switch.accesskit_node().toggled(), Some(Toggled::False));

    harness.get_by_label("OK").click();
    harness.run();
    assert!(harness.query_by_label(TEXT).is_none());
    assert!(actions().is_empty(), "{:?}", actions());

    // Again, with the switch turned on.
    let mut harness = ui_common::harness(ready(), true);
    harness.run();
    harness
        .get_by_role_and_label(Role::Switch, AUTOSTART_LABEL)
        .click();
    harness.run();
    let switch = harness.get_by_role_and_label(Role::Switch, AUTOSTART_LABEL);
    assert_eq!(switch.accesskit_node().toggled(), Some(Toggled::True));
    harness.get_by_label("OK").click();
    harness.run();
    assert!(harness.query_by_label(TEXT).is_none());
    assert_eq!(actions(), vec![Action::SetAutostart(true)]);
}

#[test]
fn no_first_run_note_otherwise() {
    let mut harness = harness(ready(), false);
    harness.run();
    assert!(harness.query_by_label(TEXT).is_none());
}

#[test]
fn icon_button_is_named_for_screen_readers() {
    use egui_kittest::Harness;
    use slc_local_tts::ui::fonts::{self, glyph};
    use slc_local_tts::ui::widgets::icon_button;

    let mut harness = Harness::new_ui_state(
        |ui, clicks: &mut u32| {
            if icon_button(ui, glyph::COPY, "", "Copy the address").clicked() {
                *clicks += 1;
            }
        },
        0,
    );
    fonts::install(
        &harness.ctx,
        fonts::definitions(&fonts::windows_fonts_dir()),
    );
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Copy the address")
        .click();
    harness.run();
    assert_eq!(*harness.state(), 1);
}

#[test]
fn notice_shown_under_every_page() {
    let text = "Couldn't play the preview. Activity says why.";
    let snapshot = Snapshot {
        notice: Some(Notice {
            text: text.to_string(),
            is_error: true,
        }),
        voices: Some(Vec::new()),
        ..ready()
    };
    let mut harness = harness(snapshot, false);
    harness.run();
    let on_status = harness.get_by_label(text).rect();
    let heading = harness
        .get_by_role_and_label(Role::Heading, "Status")
        .rect();
    assert!(on_status.top() > heading.bottom(), "under the page");
    // At the bottom of the 600-point screen, not just after the content.
    assert!(on_status.bottom() > 550.0, "{on_status:?}");

    harness
        .get_by_role_and_label(Role::Button, "Voices")
        .click();
    harness.run();
    assert!(harness.query_by_label(text).is_some());

    // No notice, no line.
    let mut harness = ui_common::harness(ready(), false);
    harness.run();
    assert!(harness.query_by_label(text).is_none());
}

/// Every filled rectangle the last frame painted, with its fill.
fn filled_rects(
    harness: &egui_kittest::Harness<'_, slc_local_tts::ui::app::WindowApp>,
) -> Vec<(eframe::egui::Rect, eframe::egui::Color32)> {
    use eframe::egui::Shape;

    fn walk(shape: &Shape, out: &mut Vec<(eframe::egui::Rect, eframe::egui::Color32)>) {
        match shape {
            Shape::Rect(rect) => out.push((rect.rect, rect.fill)),
            Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for clipped in &harness.output().shapes {
        walk(&clipped.shape, &mut out);
    }
    out
}

#[test]
fn background_fills_the_window_below_a_short_page() {
    use eframe::egui;

    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        for page in [Page::Status, Page::Settings] {
            // A tall screen, so every page is shorter than the window.
            let mut harness =
                ui_common::sized_harness(ready(), false, Some(egui::vec2(800.0, 1400.0)));
            harness.ctx.set_theme(theme);
            harness.state_mut().open(page);
            harness.run();
            let fill = harness.ctx.style_of(theme).visuals.panel_fill;
            // egui_kittest leaves an 8-point margin around the window.
            let window = egui::Rect::from_min_max(egui::pos2(8.0, 8.0), egui::pos2(792.0, 1392.0));
            let covered = filled_rects(&harness)
                .iter()
                .any(|(rect, color)| *color == fill && rect.contains_rect(window.shrink(0.5)));
            assert!(
                covered,
                "{theme:?} {page:?}: no {fill:?} background covers {window:?}"
            );
        }
    }
}
