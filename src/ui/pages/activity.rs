//! The Activity page: the requests the server remembers, newest first,
//! with why any of them failed, a "Problems only" switch and a button that
//! opens the log folder.

use eframe::egui::style::ScrollAnimation;
use eframe::egui::{self, Align, CornerRadius, Frame, Margin, RichText, ScrollArea, Stroke, Ui};

use super::heading;
use crate::timefmt;
use crate::ui::backend::{Action, Snapshot};
use crate::ui::fonts::glyph;
use crate::ui::model::activity::{Entry, entries};
use crate::ui::theme::{self, Palette, ROW_RADIUS};
use crate::ui::widgets::{icon_button, switch};

/// The note under the list. The server keeps its records in memory only.
const NOTE: &str =
    "Entries cover the time since SLC Local TTS last started. Older history is in the log files.";

/// What the Activity page shows besides the records themselves. The Status
/// page sets it too, when it opens Activity on one record or on the
/// problems only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivityState {
    /// The `seq` of the record to pick out, if any.
    pub highlight: Option<u64>,
    /// True to list only the failures.
    pub problems_only: bool,
    /// The highlight the list has already scrolled to. The list scrolls to
    /// a highlight once, when it first shows, and then leaves the scrolling
    /// to the user. A new state from the Status page starts this at `None`,
    /// so picking the same line again scrolls to it again.
    pub(crate) scrolled_to: Option<u64>,
}

/// Draws the page for `snapshot`. Returns what the user asked for, if
/// anything.
pub fn draw(ui: &mut Ui, snapshot: &Snapshot, state: &mut ActivityState) -> Option<Action> {
    let mut action = None;
    let palette = theme::palette(ui.visuals().dark_mode);

    heading(ui, "Activity");
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        switch(ui, &mut state.problems_only, "Problems only");
        if icon_button(ui, glyph::FOLDER, "Open log folder", "Open log folder").clicked() {
            action = Some(Action::OpenLogFolder);
        }
    });
    ui.add_space(8.0);

    // The note goes first, as a panel along the bottom, so it stays under
    // the list however long the list grows.
    egui::Panel::bottom("activity_note")
        .show_separator_line(false)
        .frame(egui::Frame::new().inner_margin(Margin {
            top: 6,
            ..Margin::ZERO
        }))
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(NOTE).small().color(palette.ink_dim)).wrap());
        });

    let shown = entries(&snapshot.records, state.problems_only, timefmt::local_time);
    if shown.is_empty() {
        let empty = if snapshot.records.is_empty() {
            "Nothing yet. Each line SLC asks for shows here."
        } else {
            "No problems since SLC Local TTS last started."
        };
        ui.label(RichText::new(empty).color(palette.ink_dim));
    }
    // Its own name, so it doesn't open at the Voices list's scroll
    // position (egui keys a scroll position on the area's ID).
    ScrollArea::vertical()
        .id_salt("activity list")
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for entry in &shown {
                let highlighted = state.highlight == Some(entry.seq);
                let response = entry_frame(ui, entry, highlighted, palette);
                if highlighted && state.scrolled_to != state.highlight {
                    // A jump, not a glide: nothing on the window moves
                    // except a download's progress bar.
                    response.scroll_to_me_animation(Some(Align::Center), ScrollAnimation::none());
                    state.scrolled_to = state.highlight;
                }
                ui.add_space(6.0);
            }
        });
    action
}

/// One entry in its rounded box: when and with which voice, the start of
/// the text, what happened, and why it failed. The highlighted entry is
/// framed in the colour of what's selected.
fn entry_frame(ui: &mut Ui, entry: &Entry, highlighted: bool, palette: &Palette) -> egui::Response {
    let edge = if highlighted {
        palette.act
    } else {
        palette.line
    };
    Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, edge))
        .corner_radius(CornerRadius::same(ROW_RADIUS))
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    RichText::new(format!("{}, {}", entry.time, entry.voice))
                        .small()
                        .color(palette.ink_dim),
                )
                .wrap(),
            );
            ui.add(egui::Label::new(&entry.text_start).wrap());
            let summary = RichText::new(&entry.summary);
            let summary = if entry.problem {
                theme::caution_text(summary, palette)
            } else {
                summary.color(palette.ink_dim)
            };
            ui.add(egui::Label::new(summary).wrap());
            if let Some(reason) = &entry.reason {
                ui.add(egui::Label::new(reason).wrap());
            }
        })
        .response
}
