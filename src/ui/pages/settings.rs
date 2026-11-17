//! The Settings page: start with Windows, check for updates, the speech
//! volume, CPU threads and the port, each saved as it's changed; a link to
//! `config.toml` for everything else; and About, with the licences.

use eframe::egui::accesskit::Role;
use eframe::egui::{
    self, Button, CornerRadius, Frame, Margin, RichText, ScrollArea, Slider, Stroke, TextEdit, Ui,
    WidgetInfo,
};

use super::heading;
use crate::config::{VOLUME_MAX, VOLUME_MIN};
use crate::config_edit::FileSettings;
use crate::ui::backend::{Action, Snapshot};
use crate::ui::model::settings::{
    PENDING_NOTE, SettingsView, THREADS_NOTE, VOLUME_LABEL, VOLUME_NOTE, parse_port, port_note,
    settings_view, settings_view_without_server, version_line_for,
};
use crate::ui::theme::{self, Palette, ROW_RADIUS};
use crate::ui::widgets::switch;

/// The port field's label, for screen readers.
const PORT_LABEL: &str = "Port";
/// The port field's width, in points: room for five digits.
const PORT_WIDTH: f32 = 64.0;
/// How far one step of the volume slider moves, in percent.
const VOLUME_STEP: f64 = 5.0;

/// What the Settings page remembers between frames: the port field's text
/// while the user types in it, why the last port typed isn't one, the
/// volume slider's drag and the volume still being saved.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettingsState {
    /// What the field shows. Out of the field, it's the file's port.
    port_text: String,
    port_error: Option<&'static str>,
    /// True while the field had focus at the end of the last frame.
    editing: bool,
    /// The volume under the slider's handle while the user drags it.
    volume_drag: Option<u16>,
    /// The last volume the page asked to save, until that save comes back.
    volume_pending: Option<PendingVolume>,
    /// How many saves the page has sent. Every settings save comes from
    /// this page, so it matches the snapshot's `settings_saves` once all of
    /// them have come back.
    saves_sent: u64,
}

/// A volume the page asked to save, and the number that save has in
/// [`SettingsState::saves_sent`].
#[derive(Clone, Copy, Debug, PartialEq)]
struct PendingVolume {
    volume: u16,
    save: u64,
}

impl SettingsState {
    /// Notes that the page sent `action`. A volume it saves stays on the
    /// slider until the save comes back.
    fn sent(&mut self, action: Action) -> Action {
        self.saves_sent += 1;
        if let Action::SaveSettings { base, new } = &action
            && new.volume_percent != base.volume_percent
        {
            self.volume_pending = Some(PendingVolume {
                volume: new.volume_percent,
                save: self.saves_sent,
            });
        }
        action
    }

    /// The user leaves the page while typing a port: it's saved if it's a
    /// port, as leaving the field would, and dropped otherwise. `file` is
    /// what the page drew from.
    pub fn leave(&mut self, file: Option<&FileSettings>) -> Option<Action> {
        let editing = std::mem::take(&mut self.editing);
        self.port_error = None;
        let file = file.filter(|_| editing)?;
        let port = parse_port(&self.port_text).ok()?;
        let save = (port != file.port).then(|| Action::SaveSettings {
            base: file.clone(),
            new: FileSettings {
                port,
                ..file.clone()
            },
        })?;
        Some(self.sent(save))
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
                        let saves_done = snapshot.settings_saves;
                        let (base, new) = file_rows(ui, file, saves_done, &view, state, palette);
                        if new != base {
                            actions.push(state.sent(Action::SaveSettings { base, new }));
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

/// The rows that change `config.toml`, after `saves_done` of the page's
/// saves have come back. Returns what the change is from (`file`, unless
/// the volume slider says otherwise) and `file` with every change made in
/// this frame, so changes made together are saved together.
fn file_rows(
    ui: &mut Ui,
    file: &FileSettings,
    saves_done: u64,
    view: &SettingsView,
    state: &mut SettingsState,
    palette: &Palette,
) -> (FileSettings, FileSettings) {
    let mut base = file.clone();
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

    let label = ui.label(RichText::new(VOLUME_LABEL).font(theme::body_strong()));
    if let Some(change) = volume_slider(ui, file.volume_percent, saves_done, label.id, state) {
        base.volume_percent = change.from;
        new.volume_percent = change.to;
    }
    dim_note(ui, VOLUME_NOTE, palette);
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
    (base, new)
}

/// A volume to save, and the volume it replaces.
struct VolumeChange {
    from: u16,
    to: u16,
}

/// The volume slider, named by the label `label_id`. It shows the
/// handle's volume while it's dragged, then the volume last chosen until
/// its save has come back (`saves_done` counts those), then the file's: so
/// a save that failed, or a hand edit, ends up shown. Returns a volume to
/// save when the user lets go of the handle or presses an arrow key, never
/// mid-drag.
fn volume_slider(
    ui: &mut Ui,
    file_volume: u16,
    saves_done: u64,
    label_id: egui::Id,
    state: &mut SettingsState,
) -> Option<VolumeChange> {
    if state
        .volume_pending
        .is_some_and(|pending| saves_done >= pending.save)
    {
        state.volume_pending = None;
    }
    // What the page last asked for: what a change is from, and what the
    // arrow keys step from.
    let latest = state
        .volume_pending
        .map_or(file_volume, |pending| pending.volume);
    let mut value = state.volume_drag.unwrap_or(latest);
    let response = ui
        .add(
            Slider::new(&mut value, VOLUME_MIN..=VOLUME_MAX)
                .step_by(VOLUME_STEP)
                .suffix("%")
                // A number typed in the box beside it is taken on Enter,
                // not at each keystroke, which would save each partial number.
                .update_while_editing(false),
        )
        .labelled_by(label_id);
    // Screen readers (and the tests) read the value as it's shown.
    ui.ctx()
        .accesskit_node_builder(response.id, |node| node.set_value(format!("{value}%")));

    // Kept only while the handle is held, so a drag that ends unseen (the
    // window losing the pointer) doesn't leave the slider stuck.
    state.volume_drag = response.dragged().then_some(value);
    let finished = response.drag_stopped() || (response.changed() && !response.dragged());
    // Compared with what the page last asked for, not with the file: the
    // file may not hold that yet, and letting go at its old volume must
    // still be saved.
    (finished && value != latest).then_some(VolumeChange {
        from: latest,
        to: value,
    })
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
