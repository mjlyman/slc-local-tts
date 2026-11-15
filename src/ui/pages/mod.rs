//! The window's pages. They only draw: what to show comes from the
//! snapshot and the view models, and what the user asks for goes out as
//! actions.

pub mod activity;
pub mod first_run;
pub mod settings;
pub mod status;
pub mod voices;

use eframe::egui::accesskit::Role;
use eframe::egui::{Response, RichText, Ui, WidgetInfo};

/// A page's heading. Screen readers (and the tests) see it as a heading,
/// so it's told apart from the tab of the same name.
pub fn heading(ui: &mut Ui, text: &str) -> Response {
    let response = ui.heading(RichText::new(text));
    response.widget_info(|| WidgetInfo::labeled(Role::Heading, true, text));
    response
}
