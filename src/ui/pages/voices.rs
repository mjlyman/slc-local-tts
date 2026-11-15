//! The Voices page: a search box and filter, then one section per engine
//! with a row per download, each with Play, Download or Delete. Delete asks
//! first.

use std::collections::HashMap;

use eframe::egui::{
    self, ComboBox, CornerRadius, Frame, Id, Margin, Modal, ProgressBar, RichText, ScrollArea,
    Stroke, TextEdit, Ui,
};

use super::heading;
use crate::api::slc::dto::ModelDto;
use crate::ui::backend::{Action, Snapshot};
use crate::ui::fonts::glyph;
use crate::ui::model::voices::{
    Filter, Row, delete_prompt, delete_target, play_voice, search_pick, sections,
};
use crate::ui::theme::{self, Palette, ROW_RADIUS};
use crate::ui::widgets::icon_button;

/// The search box's label, for screen readers, and its hint.
const SEARCH_LABEL: &str = "Search voices";

/// The voice picker's width, in points: enough for most voice names.
const PICKER_WIDTH: f32 = 190.0;

/// What the Voices page remembers between frames.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VoicesState {
    pub search: String,
    pub filter: Filter,
    /// The voice each many-voice row's Play button plays, by the row's
    /// stem. A row not in here plays its first voice.
    picked: HashMap<String, String>,
    /// The stem of the model Delete asked about, while its question shows.
    confirm_delete: Option<String>,
    /// The search the picks were last set from. A new search moves each
    /// picker to the voice it names; until the search changes again, the
    /// user's own picks are left alone.
    searched: String,
}

impl VoicesState {
    /// Drops Delete's question, if it's asked, without deleting anything.
    pub fn cancel_delete(&mut self) {
        self.confirm_delete = None;
    }
}

/// Draws the page for `snapshot`. Returns what the user asked for, if
/// anything.
pub fn draw(ui: &mut Ui, snapshot: &Snapshot, state: &mut VoicesState) -> Option<Action> {
    // A frame carries at most one click, so keeping the first is enough.
    let mut action = None;
    let palette = theme::palette(ui.visuals().dark_mode);

    heading(ui, "Voices");
    ui.add_space(4.0);
    search_and_filter(ui, state);
    ui.add_space(8.0);

    let Some(models) = &snapshot.voices else {
        // The list went with the connection. Its question goes too, so it
        // can't pop up again when the list comes back.
        state.cancel_delete();
        ui.label(RichText::new("Waiting for SLC Local TTS to answer.").color(palette.ink_dim));
        return None;
    };
    let shown = sections(models, &state.search, state.filter);
    if state.search != state.searched {
        // Searching for "bm_george" should make Kokoro's Play play George.
        for row in shown.iter().flat_map(|section| &section.rows) {
            if let Some(voice) = search_pick(row, &state.search) {
                state.picked.insert(row.stem.clone(), voice);
            }
        }
        state.searched = state.search.clone();
    }
    if shown.is_empty() {
        ui.label(
            RichText::new("No voices match. Change the search, or pick All.")
                .color(palette.ink_dim),
        );
    }
    // Its own name, so the Activity list doesn't open at this list's
    // scroll position (egui keys a scroll position on the area's ID).
    ScrollArea::vertical()
        .id_salt("voices list")
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for section in &shown {
                ui.label(RichText::new(section.title).font(theme::body_strong()));
                ui.add(
                    egui::Label::new(RichText::new(section.description).color(palette.ink_dim))
                        .wrap(),
                );
                ui.add_space(4.0);
                for row in &section.rows {
                    if let Some(clicked) = row_frame(ui, row, palette, state) {
                        action.get_or_insert(clicked);
                    }
                    ui.add_space(6.0);
                }
                ui.add_space(8.0);
            }
        });

    if let Some(confirmed) = delete_question(ui, models, state) {
        action.get_or_insert(confirmed);
    }
    action
}

/// The search box, then the filter as three selectable labels.
fn search_and_filter(ui: &mut Ui, state: &mut VoicesState) {
    let search = ui.add(
        TextEdit::singleline(&mut state.search)
            .hint_text(SEARCH_LABEL)
            .desired_width(f32::INFINITY),
    );
    // The hint disappears once the user types, so the box gets a name of
    // its own for screen readers.
    ui.ctx()
        .accesskit_node_builder(search.id, |node| node.set_label(SEARCH_LABEL));
    ui.horizontal(|ui| {
        for filter in Filter::EVERY {
            if ui
                .selectable_label(state.filter == filter, filter.label())
                .clicked()
            {
                state.filter = filter;
            }
        }
    });
}

/// One row in its rounded box: title, size, state, progress and buttons.
/// Returns the action a button asked for.
fn row_frame(ui: &mut Ui, row: &Row, palette: &Palette, state: &mut VoicesState) -> Option<Action> {
    Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.line))
        .corner_radius(CornerRadius::same(ROW_RADIUS))
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(&row.title).font(theme::body_strong()));
            ui.label(RichText::new(&row.subline).small().color(palette.ink_dim));
            status_line(ui, row, palette);
            if let Some(progress) = row.progress {
                ui.add(ProgressBar::new(progress).desired_height(6.0));
            }
            buttons(ui, row, state)
        })
        .inner
}

/// The row's state. A failed download is in the caution colour, on its
/// pale background where the theme has one.
fn status_line(ui: &mut Ui, row: &Row, palette: &Palette) {
    let mut text = RichText::new(&row.status_text);
    text = if row.problem {
        theme::caution_text(text, palette)
    } else {
        text.color(palette.ink_dim)
    };
    ui.add(egui::Label::new(text).wrap());
}

/// The voice picker (for a row of many voices) and the buttons the row's
/// state allows.
fn buttons(ui: &mut Ui, row: &Row, state: &mut VoicesState) -> Option<Action> {
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        if row.can_play {
            let voice = played_voice(ui, row, state);
            if icon_button(ui, glyph::PLAY, "Play", &format!("Play {}", row.title)).clicked()
                && let Some(voice) = voice
            {
                action = Some(Action::Play(voice));
            }
        }
        if row.can_download
            && icon_button(
                ui,
                glyph::DOWNLOAD,
                "Download",
                &format!("Download {}", row.title),
            )
            .clicked()
        {
            action = Some(Action::Download(row.stem.clone()));
        }
        if row.can_delete
            && icon_button(
                ui,
                glyph::DELETE,
                "Delete",
                &format!("Delete {}", row.title),
            )
            .clicked()
        {
            state.confirm_delete = Some(row.stem.clone());
        }
    });
    action
}

/// The voice Play plays: the one picked for the row, or its only voice.
/// A row of many voices draws its picker here.
fn played_voice(ui: &mut Ui, row: &Row, state: &mut VoicesState) -> Option<String> {
    if row.voices.len() < 2 {
        return play_voice(row, None);
    }
    let mut picked = play_voice(row, state.picked.get(&row.stem).map(String::as_str))?;
    let name = row
        .voices
        .iter()
        .find(|(id, _)| *id == picked)
        .map_or(picked.as_str(), |(_, name)| name.as_str())
        .to_string();
    let picker = ComboBox::from_id_salt(("voice picker", &row.stem))
        .width(PICKER_WIDTH)
        .selected_text(name)
        .show_ui(ui, |ui| {
            for (id, name) in &row.voices {
                ui.selectable_value(&mut picked, id.clone(), name);
            }
        })
        .response;
    let label = format!("Voice for {}", row.title);
    ui.ctx()
        .accesskit_node_builder(picker.id, |node| node.set_label(label));
    state.picked.insert(row.stem.clone(), picked.clone());
    Some(picked)
}

/// Delete's question, over everything else, while it's asked. Returns the
/// delete once the user confirms it.
fn delete_question(ui: &mut Ui, models: &[ModelDto], state: &mut VoicesState) -> Option<Action> {
    let stem = state.confirm_delete.as_ref()?;
    let Some(model) = delete_target(models, stem) else {
        state.confirm_delete = None;
        return None;
    };
    let modal = Modal::new(Id::unique("delete_voices")).show(ui.ctx(), |ui| {
        ui.set_max_width(320.0);
        ui.add(egui::Label::new(delete_prompt(model)).wrap());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let delete = ui.button("Delete").clicked();
            let cancel = ui.button("Cancel").clicked();
            if delete {
                Some(true)
            } else if cancel {
                Some(false)
            } else {
                None
            }
        })
        .inner
    });
    // Escape, or a click outside the question, cancels it too.
    let answer = modal
        .inner
        .or_else(|| modal.should_close().then_some(false));
    match answer {
        Some(true) => {
            let stem = model.stem.clone();
            state.confirm_delete = None;
            Some(Action::Delete(stem))
        }
        Some(false) => {
            state.confirm_delete = None;
            None
        }
        None => None,
    }
}
