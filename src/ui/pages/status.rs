//! The Status page: the line saying it just updated, the state line, SLC's
//! address, the trace of the last hour's announcements (or the "Connect
//! SLC" steps before SLC's first request), the failure notice, and the
//! banner for a downloaded update.

use std::time::{Duration, SystemTime};

use eframe::egui::accesskit::Role;
use eframe::egui::{
    self, Color32, CornerRadius, Frame, Id, Margin, Modal, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui, WidgetInfo, pos2, vec2,
};

use super::heading;
use crate::ui::backend::{Action, Connection, Snapshot};
use crate::ui::fonts::glyph;
use crate::ui::model::status::{
    BarKind, CONNECT_STEP_1, CONNECT_STEP_3, CONNECT_STEP_4, StatusView, TraceBar, connect_step_2,
    status_view,
};
use crate::ui::model::update::{banner, release_url, restart_prompt, updated_line};
use crate::ui::theme::{self, Palette, ROW_RADIUS};
use crate::ui::widgets::{icon_button, lamp};
use crate::update::UpdateState;

/// A trace bar's width, the gap between bars, and the trace's full height,
/// in points.
const BAR_WIDTH: f32 = 4.0;
const BAR_GAP: f32 = 2.0;
const TRACE_HEIGHT: f32 = 48.0;

/// How often the page redraws while it shows an "ago" time, so the minutes
/// tick over between the server's changes.
const TICK: Duration = Duration::from_secs(30);

/// The update button's text.
const RESTART_BUTTON: &str = "Restart and update now…";

/// What the Status page remembers between frames.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusState {
    /// True while "Restart and update now…" asks its question.
    confirm_restart: bool,
    /// True once the user pressed OK on "Updated to …".
    updated_seen: bool,
}

impl StatusState {
    /// Drops the restart question, for when the user leaves the page: a
    /// question left open would pop up again when they came back.
    pub fn cancel_restart(&mut self) {
        self.confirm_restart = false;
    }
}

/// What the user clicked on the page: an action for the backend, or a
/// change of page for the window.
#[derive(Clone, Debug, PartialEq)]
pub enum StatusEvent {
    Act(Action),
    /// Show this record (by `seq`) in Activity.
    ShowRecord(u64),
    /// Show only the problems in Activity.
    ShowProblems,
}

/// Draws the page for `snapshot` at time `now`. Returns what the user
/// clicked, if anything.
pub fn draw(
    ui: &mut Ui,
    snapshot: &Snapshot,
    now: SystemTime,
    state: &mut StatusState,
) -> Option<StatusEvent> {
    // A frame carries at most one click, so keeping the first is enough.
    let mut event = None;
    let view = status_view(snapshot, now);
    let palette = theme::palette(ui.visuals().dark_mode);

    heading(ui, "Status");
    ui.add_space(4.0);
    if let Some(version) = snapshot
        .status
        .as_ref()
        .and_then(|s| s.updated_to.as_deref())
        && !state.updated_seen
    {
        updated_note(ui, version, palette, state, &mut event);
        ui.add_space(8.0);
    }
    state_line(ui, &view, palette, &mut event);
    ui.add_space(12.0);
    address(ui, &view, &mut event);
    ui.add_space(12.0);
    if view.connect_steps {
        connect_steps(ui, &view.address, &mut event);
    } else {
        trace(ui, &view.trace, palette, &mut event);
    }
    if let Some(notice) = &view.failure_notice {
        ui.add_space(12.0);
        failure_notice(ui, notice, palette, &mut event);
    }
    update_banner(ui, snapshot, palette, state, &mut event);

    // Only while an "ago" time shows: otherwise an untouched window paints
    // nothing until a poll finds a change.
    if view.ticking {
        ui.ctx().request_repaint_after(TICK);
    }
    event
}

/// The lamp and headline, the detail under them, and the start button
/// once the server has stopped.
fn state_line(ui: &mut Ui, view: &StatusView, palette: &Palette, event: &mut Option<StatusEvent>) {
    ui.horizontal(|ui| {
        lamp(ui, view.lamp.color(palette));
        ui.label(RichText::new(&view.headline).heading());
    });
    // Once the server has stopped, the detail warns that SLC is losing
    // speech.
    let detail_color = if view.start_button {
        palette.warn
    } else {
        palette.ink_dim
    };
    ui.add(egui::Label::new(RichText::new(&view.detail).color(detail_color)).wrap());
    if view.start_button {
        ui.add_space(4.0);
        if ui.button("Start SLC Local TTS").clicked() {
            event.get_or_insert(StatusEvent::Act(Action::StartServer));
        }
    }
}

/// "SLC's Custom API URL", the address in large type, and Copy.
fn address(ui: &mut Ui, view: &StatusView, event: &mut Option<StatusEvent>) {
    ui.label(RichText::new("SLC's Custom API URL").font(theme::body_strong()));
    ui.horizontal(|ui| {
        ui.label(RichText::new(&view.address).heading());
        if icon_button(ui, glyph::COPY, "Copy", "Copy SLC address").clicked() {
            event.get_or_insert(StatusEvent::Act(Action::CopyText(view.address.clone())));
        }
    });
}

/// The four numbered setup steps, with a Copy button on step 2.
fn connect_steps(ui: &mut Ui, address: &str, event: &mut Option<StatusEvent>) {
    ui.label(RichText::new("Connect SLC").font(theme::body_strong()));
    // Each number is a label of its own, so a step's text reads (and is
    // found) without it.
    let step = |ui: &mut Ui, number: &str, text: &str| {
        ui.horizontal_top(|ui| {
            ui.label(number);
            ui.add(egui::Label::new(text).wrap());
        });
    };
    step(ui, "1.", CONNECT_STEP_1);
    // The address is long, so the Copy button goes under the step's text:
    // side by side, they run past the narrowest window.
    ui.horizontal_top(|ui| {
        ui.label("2.");
        ui.vertical(|ui| {
            ui.add(egui::Label::new(connect_step_2(address)).wrap());
            if icon_button(ui, glyph::COPY, "Copy", "Copy the address in step 2").clicked() {
                event.get_or_insert(StatusEvent::Act(Action::CopyText(address.to_string())));
            }
        });
    });
    step(ui, "3.", CONNECT_STEP_3);
    step(ui, "4.", CONNECT_STEP_4);
}

/// The trace: one bar per announcement, oldest on the left. When there are
/// more than fit, the oldest are left out.
fn trace(ui: &mut Ui, bars: &[TraceBar], palette: &Palette, event: &mut Option<StatusEvent>) {
    let width = ui.available_width();
    let (area, _) = ui.allocate_exact_size(vec2(width, TRACE_HEIGHT), Sense::hover());
    let step = BAR_WIDTH + BAR_GAP;
    let fits = ((width + BAR_GAP) / step).floor().max(0.0) as usize;
    let shown = &bars[bars.len().saturating_sub(fits)..];

    for (i, bar) in shown.iter().enumerate() {
        let left = area.left() + i as f32 * step;
        // The whole column, gap included, takes the click: a 4-point bar
        // alone is hard to hit.
        let column = Rect::from_min_size(pos2(left, area.top()), vec2(step, TRACE_HEIGHT));
        let id = ui.make_persistent_id(("trace bar", bar.seq));
        // The bar being spoken has no record yet, so there's nothing to
        // open.
        let (sense, role) = if bar.seq.is_some() {
            (Sense::click(), Role::Button)
        } else {
            (Sense::hover(), Role::Label)
        };
        let response = ui.interact(column, id, sense).on_hover_text(&bar.label);
        response.widget_info(|| WidgetInfo::labeled(role, true, &bar.label));
        if response.clicked()
            && let Some(seq) = bar.seq
        {
            event.get_or_insert(StatusEvent::ShowRecord(seq));
        }

        if ui.is_rect_visible(column) {
            let height = TRACE_HEIGHT * bar.height;
            let rect =
                Rect::from_min_size(pos2(left, area.bottom() - height), vec2(BAR_WIDTH, height));
            let painter = ui.painter();
            match bar.kind {
                BarKind::Spoken => {
                    painter.rect_filled(rect, CornerRadius::ZERO, palette.ink_dim);
                }
                BarKind::Speaking => {
                    painter.rect_filled(rect, CornerRadius::ZERO, palette.act);
                }
                BarKind::Failed => {
                    painter.rect_stroke(
                        rect,
                        CornerRadius::ZERO,
                        Stroke::new(1.0, palette.caution),
                        StrokeKind::Inside,
                    );
                }
            }
        }
    }
}

/// The amber line saying announcements failed, with "See why".
fn failure_notice(ui: &mut Ui, notice: &str, palette: &Palette, event: &mut Option<StatusEvent>) {
    // The light theme puts caution text on a pale amber background, so it
    // stays readable.
    let fill = palette.caution_bg.unwrap_or(Color32::TRANSPARENT);
    rounded(fill).show(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(notice).color(palette.caution)).wrap());
        if ui.link("See why").clicked() {
            event.get_or_insert(StatusEvent::ShowProblems);
        }
    });
}

/// "Updated to 1.4.0.", with a link to what's new, until the user presses
/// OK.
fn updated_note(
    ui: &mut Ui,
    version: &str,
    palette: &Palette,
    state: &mut StatusState,
    event: &mut Option<StatusEvent>,
) {
    rounded(palette.surface)
        .stroke(Stroke::new(1.0, palette.line))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(updated_line(version)).font(theme::body_strong()));
            ui.horizontal(|ui| {
                if ui.link("What's new").clicked() {
                    event.get_or_insert(StatusEvent::Act(Action::OpenUrl(release_url(version))));
                }
                if ui.button("OK").clicked() {
                    state.updated_seen = true;
                }
            });
        });
}

/// The banner for a downloaded update, with a caution-coloured edge, and
/// its button and question where this program can restart itself.
fn update_banner(
    ui: &mut Ui,
    snapshot: &Snapshot,
    palette: &Palette,
    state: &mut StatusState,
    event: &mut Option<StatusEvent>,
) {
    let banner = snapshot
        .status
        .as_ref()
        .and_then(|status| banner(&status.update, status.can_restart_to_update));
    let Some((text, can_restart)) = banner else {
        // The update went away (the server restarted, say): so does the
        // question.
        state.confirm_restart = false;
        return;
    };
    // Restarting needs a server to ask.
    let button = can_restart && snapshot.connection == Connection::Connected;
    ui.add_space(12.0);
    rounded(palette.surface)
        .stroke(Stroke::new(1.0, palette.caution))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(text).wrap());
            if button {
                ui.add_space(4.0);
                if ui.button(RESTART_BUTTON).clicked() {
                    state.confirm_restart = true;
                }
            }
        });
    if !button {
        state.confirm_restart = false;
        return;
    }
    if let Some(UpdateState::Ready { version }) = snapshot.status.as_ref().map(|s| &s.update)
        && restart_question(ui, version, state)
    {
        event.get_or_insert(StatusEvent::Act(Action::ApplyUpdate));
    }
}

/// The restart question, over everything else, while it's asked. True once
/// the user says Yes.
fn restart_question(ui: &mut Ui, version: &str, state: &mut StatusState) -> bool {
    if !state.confirm_restart {
        return false;
    }
    let modal = Modal::new(Id::unique("restart_to_update")).show(ui.ctx(), |ui| {
        ui.set_max_width(320.0);
        ui.add(egui::Label::new(restart_prompt(version)).wrap());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let yes = ui.button("Yes").clicked();
            let no = ui.button("No").clicked();
            if yes {
                Some(true)
            } else if no {
                Some(false)
            } else {
                None
            }
        })
        .inner
    });
    // Escape, or a click outside the question, means No.
    let answer = modal
        .inner
        .or_else(|| modal.should_close().then_some(false));
    match answer {
        Some(yes) => {
            state.confirm_restart = false;
            yes
        }
        None => false,
    }
}

/// A box with the grouped rows' rounded corners, filled with `fill`.
fn rounded(fill: Color32) -> Frame {
    Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(ROW_RADIUS))
        .inner_margin(Margin::symmetric(8, 6))
}
