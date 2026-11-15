//! The note shown over the Status page on the very first start: where the
//! tray icon is, and an offer to start with Windows.

use eframe::egui::{Id, Modal, Ui};

use crate::ui::widgets::switch;

/// The note's text.
pub const TEXT: &str = "SLC Local TTS runs in the taskbar corner. If you can't see its icon, \
click ^ next to the clock. Start it before SLC, or let Windows start it for you:";
/// The switch's label.
pub const AUTOSTART_LABEL: &str = "Start when Windows starts";

/// Starting with Windows is off unless the user turns it on.
#[derive(Debug, Default)]
pub struct FirstRunNote {
    autostart: bool,
}

impl FirstRunNote {
    /// Draws the note over everything else. Returns `Some(autostart)` once
    /// the user presses OK, with whether they turned the switch on.
    pub fn show(&mut self, ui: &mut Ui) -> Option<bool> {
        let modal = Modal::new(Id::unique("first_run_note")).show(ui.ctx(), |ui| {
            ui.set_max_width(320.0);
            ui.label(TEXT);
            ui.add_space(8.0);
            switch(ui, &mut self.autostart, AUTOSTART_LABEL);
            ui.add_space(8.0);
            ui.button("OK").clicked()
        });
        modal.inner.then_some(self.autostart)
    }
}
