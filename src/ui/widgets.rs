//! The window's own small widgets: a switch, a button with a Segoe icon,
//! and the status lamp.

use eframe::egui::accesskit::Role;
use eframe::egui::text::LayoutJob;
use eframe::egui::{
    self, Color32, FontFamily, FontId, Response, Sense, Stroke, TextFormat, TextStyle, Ui,
    WidgetInfo, WidgetText, vec2,
};

use super::fonts::{self, ICONS};

/// The lamp's diameter, in points.
const LAMP_SIZE: f32 = 10.0;

/// An on/off switch with `label` to its right. Clicking either toggles
/// `on`. Screen readers (and the tests) see a switch named `label`.
pub fn switch(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let track_size = vec2(2.0, 1.0) * ui.spacing().interact_size.y;
    let gap = ui.spacing().item_spacing.x;
    let wrap_width = ui.available_width() - track_size.x - gap;
    let galley = WidgetText::from(label).into_galley(ui, None, wrap_width, TextStyle::Body);
    let size = vec2(
        track_size.x + gap + galley.size().x,
        track_size.y.max(galley.size().y),
    );
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let enabled = ui.is_enabled();
    let state = *on;
    response.widget_info(|| WidgetInfo::selected(Role::Switch, enabled, state, label));

    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let track = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.center().y - track_size.y / 2.0),
            track_size,
        );
        let radius = track.height() / 2.0;
        let (fill, knob) = if state {
            (visuals.selection.bg_fill, visuals.panel_fill)
        } else {
            (
                visuals.widgets.inactive.bg_fill,
                visuals.widgets.inactive.fg_stroke.color,
            )
        };
        let painter = ui.painter();
        painter.rect(
            track,
            radius,
            fill,
            visuals.widgets.noninteractive.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let knob_x = if state {
            track.right() - radius
        } else {
            track.left() + radius
        };
        painter.circle_filled(egui::pos2(knob_x, track.center().y), radius * 0.7, knob);
        let text_pos = egui::pos2(track.right() + gap, rect.center().y - galley.size().y / 2.0);
        painter.galley(text_pos, galley, visuals.text_color());
    }
    response
}

/// A button showing `glyph` from the icon font, then `text`. Without an
/// icon font it shows only the words: `text`, or `accessible` when `text`
/// is empty. Screen readers (and the tests) see a button named
/// `accessible`.
pub fn icon_button(ui: &mut Ui, glyph: char, text: &str, accessible: &str) -> Response {
    let body = ui
        .style()
        .text_styles
        .get(&TextStyle::Button)
        .map_or(14.0, |font| font.size);
    let color = ui.visuals().text_color();
    let mut job = LayoutJob::default();
    if fonts::has_icons(ui.ctx()) {
        let icon = TextFormat::simple(FontId::new(body, FontFamily::Name(ICONS.into())), color);
        job.append(&glyph.to_string(), 0.0, icon);
        if !text.is_empty() {
            let words = TextFormat::simple(FontId::proportional(body), color);
            job.append(text, 6.0, words);
        }
    } else {
        let words = if text.is_empty() { accessible } else { text };
        job.append(
            words,
            0.0,
            TextFormat::simple(FontId::proportional(body), color),
        );
    }
    let response = ui.button(job);
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, accessible));
    response
}

/// A small round lamp in `color`.
pub fn lamp(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(LAMP_SIZE, LAMP_SIZE), Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter()
            .circle(rect.center(), LAMP_SIZE / 2.0, color, Stroke::NONE);
    }
}
