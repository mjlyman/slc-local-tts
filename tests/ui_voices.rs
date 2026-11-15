//! The Voices page: its rows, buttons, voice picker, Delete's question,
//! and the search and filter.

mod ui_common;

use eframe::egui::{self, accesskit::Role};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use slc_local_tts::api::slc::dto::ModelDto;
use slc_local_tts::models::ModelState;
use slc_local_tts::ui::app::{Page, WindowApp};
use slc_local_tts::ui::backend::{Action, Snapshot};
use ui_common::{
    actions, backend, joe, kokoro, narrow_harness, ready, sized_harness, voices_mixed,
};

/// The window on its Voices page, showing `models`. It's the window's
/// usual width (egui_kittest adds 8 points around it), and tall enough
/// for every row to show without scrolling, since the tests click by
/// position.
fn voices_page(models: Vec<ModelDto>) -> Harness<'static, WindowApp> {
    let snapshot = Snapshot {
        voices: Some(models),
        ..ready()
    };
    let mut harness = sized_harness(snapshot, false, Some(egui::vec2(456.0, 1200.0)));
    harness.state_mut().page = Page::Voices;
    harness.run();
    harness
}

/// [`voices_mixed`], with the model `stem` in `state` instead.
fn mixed_with(stem: &str, state: ModelState) -> Vec<ModelDto> {
    let mut models = voices_mixed();
    let model = models.iter_mut().find(|m| m.stem == stem).unwrap();
    model.state = state;
    models
}

#[test]
fn download_click_sends_action() {
    let mut harness = voices_page(mixed_with(kokoro().stem, ModelState::NotInstalled));
    harness
        .get_by_role_and_label(Role::Button, "Download Kokoro voices")
        .click();
    harness.run();
    assert_eq!(actions(), vec![Action::Download(kokoro().stem.to_string())]);
}

#[test]
fn delete_asks_first_then_sends() {
    let mut harness = voices_page(mixed_with(joe().stem, ModelState::Installed));
    let prompt_start = "Delete Joe (US, medium, Piper) (";
    let prompt_shown = |harness: &Harness<'static, WindowApp>| {
        harness
            .query_all_by(|node| {
                node.label()
                    .or_else(|| node.value())
                    .is_some_and(|text| text.starts_with(prompt_start))
            })
            .next()
            .is_some()
    };
    assert!(!prompt_shown(&harness));

    harness
        .get_by_role_and_label(Role::Button, "Delete Joe (US, medium, Piper)")
        .click();
    harness.run();
    assert!(prompt_shown(&harness), "the question appears");
    assert!(actions().is_empty(), "nothing is deleted yet");

    harness
        .get_by_role_and_label(Role::Button, "Cancel")
        .click();
    harness.run();
    assert!(!prompt_shown(&harness));
    assert!(actions().is_empty(), "Cancel deletes nothing");

    harness
        .get_by_role_and_label(Role::Button, "Delete Joe (US, medium, Piper)")
        .click();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Delete")
        .click();
    harness.run();
    assert_eq!(
        actions(),
        vec![Action::Delete("vits-piper-en_US-joe-medium".to_string())]
    );
    assert!(!prompt_shown(&harness));
}

#[test]
fn play_only_for_installed() {
    let harness = voices_page(voices_mixed());
    // Kokoro is still downloading.
    assert!(harness.query_by_label("Play Kokoro voices").is_none());
    assert!(harness.query_by_label("Delete Kokoro voices").is_none());
    assert!(harness.query_by_label("Download Kokoro voices").is_none());
    // Supertonic is installed.
    assert!(harness.query_by_label("Play Supertonic voices").is_some());
    assert!(harness.query_by_label("Delete Supertonic voices").is_some());
    // Joe isn't, and offers only Download.
    assert!(
        harness
            .query_by_label("Play Joe (US, medium, Piper)")
            .is_none()
    );
    assert!(
        harness
            .query_by_label("Download Joe (US, medium, Piper)")
            .is_some()
    );
}

#[test]
fn voice_picker_chooses_play_voice() {
    let mut harness = voices_page(voices_mixed());
    harness
        .get_by_role_and_label(Role::ComboBox, "Voice for Supertonic voices")
        .click();
    harness.run();
    harness.get_by_label("M3 (Supertonic)").click();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Play Supertonic voices")
        .click();
    harness.run();
    assert_eq!(actions(), vec![Action::Play("M3".to_string())]);
}

#[test]
fn play_without_picking_plays_first_voice() {
    let mut harness = voices_page(mixed_with(joe().stem, ModelState::Installed));
    // A one-voice row has nothing to pick.
    assert!(
        harness
            .query_by_label("Voice for Joe (US, medium, Piper)")
            .is_none()
    );
    harness
        .get_by_role_and_label(Role::Button, "Play Joe (US, medium, Piper)")
        .click();
    harness.run();
    assert_eq!(
        actions(),
        vec![Action::Play("en_US-joe-medium".to_string())]
    );
}

#[test]
fn progress_row_shows_megabytes() {
    let harness = voices_page(voices_mixed());
    assert!(harness.query_by_label("161 of 350 MB").is_some());
    assert!(harness.query_by_role(Role::ProgressIndicator).is_some());
    // The failed download says why, and what to do.
    assert!(
        harness
            .query_by_label(
                "Couldn't download: no internet connection. Check your connection, then press \
                 Download again."
            )
            .is_some()
    );
}

#[test]
fn search_and_filter_narrow_the_rows() {
    let mut harness = voices_page(voices_mixed());
    harness
        .get_by_role_and_label(Role::TextInput, "Search voices")
        .click();
    harness.run();
    harness
        .get_by_role_and_label(Role::TextInput, "Search voices")
        .type_text("joe");
    harness.run();
    assert!(
        harness
            .query_by_label("Download Joe (US, medium, Piper)")
            .is_some()
    );
    assert!(harness.query_by_label("Play Supertonic voices").is_none());
    // The sections with no rows left go too.
    assert!(harness.query_by_label("Supertonic").is_none());

    let mut harness = voices_page(voices_mixed());
    harness
        .get_by_role_and_label(Role::Button, "Installed")
        .click();
    harness.run();
    assert!(harness.query_by_label("Play Supertonic voices").is_some());
    assert!(
        harness
            .query_by_label("Download Joe (US, medium, Piper)")
            .is_none()
    );
    assert!(harness.query_by_label("161 of 350 MB").is_none());
}

#[test]
fn rows_fit_the_narrowest_window() {
    const WIDTH: f32 = 380.0;
    let snapshot = Snapshot {
        voices: Some(mixed_with(joe().stem, ModelState::Installed)),
        ..ready()
    };
    let mut harness = narrow_harness(snapshot, WIDTH);
    harness.state_mut().page = Page::Voices;
    harness.run();
    for label in [
        "Search voices",
        "Voice for Supertonic voices",
        "Play Supertonic voices",
        "Delete Supertonic voices",
        "Play Joe (US, medium, Piper)",
        "Delete Joe (US, medium, Piper)",
        "Couldn't download: no internet connection. Check your connection, then press Download \
         again.",
    ] {
        let right = harness.get_by_label(label).rect().right();
        assert!(right <= WIDTH, "{label} ends at {right}, past {WIDTH}");
    }
}

#[test]
fn search_preselects_matching_voice() {
    let mut harness = voices_page(mixed_with(kokoro().stem, ModelState::Installed));
    let search = |harness: &mut Harness<'static, WindowApp>, text: &str| {
        harness
            .get_by_role_and_label(Role::TextInput, "Search voices")
            .click();
        harness.run();
        harness
            .get_by_role_and_label(Role::TextInput, "Search voices")
            .type_text(text);
        harness.run();
    };
    search(&mut harness, "bm_george");
    harness
        .get_by_role_and_label(Role::Button, "Play Kokoro voices")
        .click();
    harness.run();
    assert_eq!(actions(), vec![Action::Play("bm_george".to_string())]);

    // A voice picked after the search stays picked: the picker still
    // lists every voice in the row.
    harness
        .get_by_role_and_label(Role::ComboBox, "Voice for Kokoro voices")
        .click();
    harness.run();
    harness.get_by_label("Alloy (US, Kokoro, slow)").click();
    harness.run();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Play Kokoro voices")
        .click();
    harness.run();
    assert_eq!(
        actions(),
        vec![
            Action::Play("bm_george".to_string()),
            Action::Play("af_alloy".to_string())
        ]
    );
}

/// Whether Delete's question about Joe shows.
fn joe_question_shown(harness: &Harness<'static, WindowApp>) -> bool {
    harness
        .query_all_by(|node| {
            node.label()
                .or_else(|| node.value())
                .is_some_and(|text| text.starts_with("Delete Joe (US, medium, Piper) ("))
        })
        .next()
        .is_some()
}

#[test]
fn delete_question_dropped_when_list_goes() {
    let models = mixed_with(joe().stem, ModelState::Installed);
    let mut harness = voices_page(models.clone());
    harness
        .get_by_role_and_label(Role::Button, "Delete Joe (US, medium, Piper)")
        .click();
    harness.run();
    assert!(joe_question_shown(&harness));

    // The connection drops, so the list goes, then comes back.
    backend().set_snapshot(Snapshot {
        voices: None,
        ..ready()
    });
    harness.run();
    assert!(!joe_question_shown(&harness));
    backend().set_snapshot(Snapshot {
        voices: Some(models),
        ..ready()
    });
    harness.run();
    assert!(
        !joe_question_shown(&harness),
        "an old question came back with the list"
    );
    assert!(actions().is_empty());
}

#[test]
fn delete_question_dropped_when_page_left() {
    let mut harness = voices_page(mixed_with(joe().stem, ModelState::Installed));
    harness
        .get_by_role_and_label(Role::Button, "Delete Joe (US, medium, Piper)")
        .click();
    harness.run();
    assert!(joe_question_shown(&harness));

    harness.state_mut().open(Page::Status);
    harness.run();
    harness.state_mut().open(Page::Voices);
    harness.run();
    assert!(
        !joe_question_shown(&harness),
        "an old question came back with the page"
    );
}
