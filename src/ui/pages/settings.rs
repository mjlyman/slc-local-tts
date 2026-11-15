//! The Settings page: start with Windows, check for updates, CPU threads
//! and the port, each saved as it's changed; a link to `config.toml` for
//! everything else; and About, with the licences.

use eframe::egui::accesskit::Role;
use eframe::egui::{
    self, Button, CornerRadius, Frame, Margin, RichText, ScrollArea, Stroke, TextEdit, Ui,
    WidgetInfo,
};

use super::heading;
use crate::config_edit::FileSettings;
use crate::ui::backend::{Action, Snapshot};
use crate::ui::model::settings::{
    PENDING_NOTE, SettingsView, THREADS_NOTE, parse_port, port_note, settings_view,
    settings_view_without_server, version_line_for,
};
use crate::ui::theme::{self, Palette, ROW_RADIUS};
use crate::ui::widgets::switch;

/// The port field's label, for screen readers.
const PORT_LABEL: &str = "Port";
/// The port field's width, in points: room for five digits.
const PORT_WIDTH: f32 = 64.0;

/// What the Settings page remembers between frames: the port field's text
/// while the user types in it, and why the last port typed isn't one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettingsState {
    /// What the field shows. Out of the field, it's the file's port.
    port_text: String,
    port_error: Option<&'static str>,
    /// True while the field had focus at the end of the last frame.
    editing: bool,
}

impl SettingsState {
    /// The user leaves the page while typing a port: it's saved if it's a
    /// port, as leaving the field would, and dropped otherwise. `file` is
    /// what the page drew from.
    pub fn leave(&mut self, file: Option<&FileSettings>) -> Option<Action> {
        let editing = std::mem::take(&mut self.editing);
        self.port_error = None;
        let file = file.filter(|_| editing)?;
        let port = parse_port(&self.port_text).ok()?;
        (port != file.port).then(|| Action::SaveSettings {
            base: file.clone(),
            new: FileSettings {
                port,
                ..file.clone()
            },
        })
    }
}

/// Draws the page for `snapshot`. Returns everything the user asked for in
/// this frame, in order: one click can both finish typing a port and press
/// a switch or a link.
pub fn draw(ui: &mut Ui, snapshot: &Snapshot, state: &mut SettingsState) -> Vec<Action> {
    let mut actions = Vec::new();
    let palette = theme::palette(ui.visuals().dark_mode);

    heading(ui, "Settings");
    ui.add_space(4.0);
    // Its own name, so it doesn't open at another page's scroll position.
    ScrollArea::vertical()
        .id_salt("settings page")
        .auto_shrink([false, true])
        .show(ui, |ui| {
            // Set while drawing the rows, for the note under them.
            let mut pending = false;
            rows_frame(palette).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let mut autostart = snapshot.autostart;
                if switch(ui, &mut autostart, "Start when Windows starts").changed() {
                    actions.push(Action::SetAutostart(autostart));
                }
                ui.separator();
                match &snapshot.settings {
                    Some(Ok(file)) => {
                        let view = view_of(file, snapshot);
                        pending = view.pending;
                        let new = file_rows(ui, file, &view, state, palette);
                        if new != *file {
                            actions.push(Action::SaveSettings {
                                base: file.clone(),
                                new,
                            });
                        }
                    }
                    // The rows wait until the file can be read; the link
                    // below opens it to fix.
                    Some(Err(reason)) => {
                        let text = theme::caution_text(RichText::new(reason), palette);
                        ui.add(egui::Label::new(text).wrap());
                    }
                    None => {
                        ui.label(RichText::new("Reading config.toml…").color(palette.ink_dim));
                    }
                }
            });
            ui.add_space(8.0);
            if pending {
                ui.add(egui::Label::new(PENDING_NOTE).wrap());
            }
            if ui.link("Open config.toml for advanced settings").clicked() {
                actions.push(Action::OpenConfig);
            }
            ui.add_space(16.0);
            if about(ui, snapshot) {
                actions.push(Action::OpenLicences);
            }
        });
    actions
}

/// The view for `file`, next to what the server started with once it has
/// answered.
fn view_of(file: &FileSettings, snapshot: &Snapshot) -> SettingsView {
    match &snapshot.status {
        Some(status) => settings_view(file, &status.config, &status.update, &status.version),
        None => settings_view_without_server(file),
    }
}

/// The rounded box the rows sit in.
fn rows_frame(palette: &Palette) -> Frame {
    Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.line))
        .corner_radius(CornerRadius::same(ROW_RADIUS))
        .inner_margin(Margin::same(8))
}

/// The rows that change `config.toml`. Returns `file` with every change
/// made in this frame, so changes made together are saved together.
fn file_rows(
    ui: &mut Ui,
    file: &FileSettings,
    view: &SettingsView,
    state: &mut SettingsState,
    palette: &Palette,
) -> FileSettings {
    let mut new = file.clone();

    ui.add_enabled_ui(view.updates_note.is_none(), |ui| {
        switch(ui, &mut new.check_for_updates, "Check for updates")
    });
    if let Some(note) = view.updates_note {
        dim_note(ui, note, palette);
    }
    if let Some(note) = view.held_update_note {
        dim_note(ui, note, palette);
    }
    ui.separator();

    ui.label(RichText::new("CPU threads for speech").font(theme::body_strong()));
    if let Some(threads) = threads_stepper(ui, file.num_threads, view.threads_max) {
        new.num_threads = threads;
    }
    dim_note(ui, THREADS_NOTE, palette);
    ui.separator();

    ui.label(RichText::new(PORT_LABEL).font(theme::body_strong()));
    if let Some(port) = port_field(ui, file.port, state, palette) {
        new.port = port;
    }
    new
}

/// `− n +`, from 1 to `max`. Returns the new count when a button was
/// pressed.
fn threads_stepper(ui: &mut Ui, threads: i32, max: i32) -> Option<i32> {
    let mut changed = None;
    ui.horizontal(|ui| {
        if step_button(ui, "−", "Fewer threads", threads > 1) {
            changed = Some(threads - 1);
        }
        ui.label(threads.to_string());
        if step_button(ui, "+", "More threads", threads < max) {
            changed = Some(threads + 1);
        }
    });
    changed
}

/// A stepper's button showing `text`, which screen readers (and the tests)
/// know as `accessible`. True when it was pressed.
fn step_button(ui: &mut Ui, text: &str, accessible: &str, enabled: bool) -> bool {
    let response = ui.add_enabled(enabled, Button::new(text).min_size(egui::vec2(28.0, 0.0)));
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, accessible));
    response.clicked()
}

/// The port field, with the note for its port. Typing is committed on
/// Enter or when the field loses focus. Returns a typed port to save.
///
/// The typed text shows only while the field has focus. Otherwise the
/// field and its note show the file's port, so a save that failed never
/// leaves the page telling the user to point SLC at a port that isn't in
/// use.
fn port_field(
    ui: &mut Ui,
    file_port: u16,
    state: &mut SettingsState,
    palette: &Palette,
) -> Option<u16> {
    // Checked against the last frame rather than egui's focus now: a click
    // elsewhere takes the focus during this frame, and the typed text must
    // still be there to commit.
    if !state.editing {
        state.port_text = file_port.to_string();
    }
    // A fixed id: egui otherwise numbers widgets in drawing order, so a
    // note appearing above the field would give it a new id, and the field
    // would lose focus (and save a half-typed port) mid-typing.
    let field = ui.add(
        TextEdit::singleline(&mut state.port_text)
            .id_salt("port field")
            .desired_width(PORT_WIDTH),
    );
    ui.ctx()
        .accesskit_node_builder(field.id, |node| node.set_label(PORT_LABEL));

    // A single-line field loses focus on Enter too. egui's `lost_focus`
    // can stay true for two frames, which would save twice, so the page
    // watches the focus go itself.
    let finished = state.editing && !field.has_focus();
    state.editing = field.has_focus();
    let mut save = None;
    if finished {
        match parse_port(&state.port_text) {
            Ok(port) => {
                state.port_error = None;
                save = (port != file_port).then_some(port);
            }
            Err(error) => state.port_error = Some(error),
        }
    }
    if let Some(error) = state.port_error {
        let text = theme::caution_text(RichText::new(error), palette);
        ui.add(egui::Label::new(text).wrap());
    }
    let shown = if state.editing {
        parse_port(&state.port_text).unwrap_or(file_port)
    } else {
        file_port
    };
    dim_note(ui, &port_note(shown), palette);
    save
}

/// A wrapped line of secondary text under a row.
fn dim_note(ui: &mut Ui, text: &str, palette: &Palette) {
    ui.add(egui::Label::new(RichText::new(text).small().color(palette.ink_dim)).wrap());
}

/// About: the version, the licence and a link to the licences. True when
/// the link was clicked.
fn about(ui: &mut Ui, snapshot: &Snapshot) -> bool {
    ui.label(RichText::new("About").font(theme::body_strong()));
    ui.label(version_line_for(snapshot.status.as_ref()));
    ui.label("Licence: GPL-3.0-or-later");
    ui.link("View licences").clicked()
}
