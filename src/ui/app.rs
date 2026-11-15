//! The window itself: the tab bar, the page under it, and the first-run
//! note.

use std::sync::Arc;
use std::time::SystemTime;

use eframe::egui;

use super::backend::{Action, Backend, Connection, Notice};
use super::pages::activity::ActivityState;
use super::pages::settings::SettingsState;
use super::pages::status::{StatusEvent, StatusState};
use super::pages::voices::VoicesState;
use super::pages::{self, first_run::FirstRunNote};
use super::theme;

/// The window's tabs, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Status,
    Voices,
    Activity,
    Settings,
}

impl Page {
    pub const ALL: [Page; 4] = [Page::Status, Page::Voices, Page::Activity, Page::Settings];

    /// The tab's name.
    pub fn title(self) -> &'static str {
        match self {
            Page::Status => "Status",
            Page::Voices => "Voices",
            Page::Activity => "Activity",
            Page::Settings => "Settings",
        }
    }
}

/// The settings window's state between frames. The pages keep their own
/// state here as they gain controls that need it.
pub struct WindowApp {
    pub page: Page,
    /// The Status page's open question and dismissed update line.
    pub status: StatusState,
    /// What Activity shows besides its records, which the Status page can
    /// set.
    pub activity: ActivityState,
    /// The Voices page's search, filter, voice picks and open question.
    pub voices: VoicesState,
    /// The Settings page's port field.
    pub settings: SettingsState,
    /// Where the pages get the time from: the system clock, or a fixed time
    /// in tests.
    pub clock: fn() -> SystemTime,
    /// The connection as of the last frame, to notice the moment it's lost.
    connection: Connection,
    backend: Arc<dyn Backend>,
    /// The first-run note, until the user presses its OK.
    first_run: Option<FirstRunNote>,
}

impl WindowApp {
    /// The window on its Status page, showing the first-run note if
    /// `first_run`.
    pub fn new(backend: Arc<dyn Backend>, first_run: bool) -> WindowApp {
        WindowApp {
            page: Page::Status,
            status: StatusState::default(),
            activity: ActivityState::default(),
            voices: VoicesState::default(),
            settings: SettingsState::default(),
            clock: SystemTime::now,
            connection: Connection::default(),
            backend,
            first_run: first_run.then(FirstRunNote::default),
        }
    }

    /// Draws one frame.
    pub fn draw(&mut self, ui: &mut egui::Ui) {
        let snapshot = self.backend.snapshot();
        let now = (self.clock)();
        if snapshot.connection == Connection::Lost && self.connection != Connection::Lost {
            // A restarted server counts its lines from 1 again, so the old
            // highlight would pick out an unrelated line.
            self.activity.highlight = None;
            self.activity.scrolled_to = None;
        }
        self.connection = snapshot.connection;
        // A central panel stretches its background over the whole window,
        // however short the page; a bare frame would stop under the content
        // and leave the rest of the window unpainted.
        egui::CentralPanel::default_margins().show(ui, |ui| {
            ui.horizontal(|ui| {
                for page in Page::ALL {
                    if ui
                        .selectable_label(self.page == page, page.title())
                        .clicked()
                    {
                        self.open(page);
                    }
                }
            });
            ui.separator();
            // The notice goes first, as a panel along the bottom, so it
            // keeps its place under whatever the page draws, a long list
            // of voices included.
            if let Some(notice) = &snapshot.notice {
                notice_line(ui, notice);
            }
            match self.page {
                Page::Status => {
                    if let Some(event) = pages::status::draw(ui, &snapshot, now, &mut self.status) {
                        self.on_status(event);
                    }
                }
                Page::Voices => {
                    if let Some(action) = pages::voices::draw(ui, &snapshot, &mut self.voices) {
                        self.backend.act(action);
                    }
                }
                Page::Activity => {
                    if let Some(action) = pages::activity::draw(ui, &snapshot, &mut self.activity) {
                        self.backend.act(action);
                    }
                }
                Page::Settings => {
                    for action in pages::settings::draw(ui, &snapshot, &mut self.settings) {
                        self.backend.act(action);
                    }
                }
            }
        });

        if let Some(note) = &mut self.first_run
            && let Some(autostart) = note.show(ui)
        {
            if autostart {
                self.backend.act(Action::SetAutostart(true));
            }
            self.first_run = None;
        }
    }

    /// Carries out what the user clicked on the Status page.
    fn on_status(&mut self, event: StatusEvent) {
        match event {
            StatusEvent::Act(action) => self.backend.act(action),
            // Each sets the whole Activity state, so an earlier click never
            // hides what this one asks for: a spoken line under "Problems
            // only", say.
            StatusEvent::ShowRecord(seq) => {
                self.activity = ActivityState {
                    highlight: Some(seq),
                    problems_only: false,
                    ..ActivityState::default()
                };
                self.open(Page::Activity);
            }
            StatusEvent::ShowProblems => {
                self.activity = ActivityState {
                    highlight: None,
                    problems_only: true,
                    ..ActivityState::default()
                };
                self.open(Page::Activity);
            }
        }
    }

    /// Switches to `page`. Every change of page, from the tabs or from a
    /// page, goes through here. The voice list is polled only while Voices is
    /// open, so the backend is told when it opens and closes. Settings reads
    /// `config.toml` again each time it opens, in case it was edited by hand.
    pub fn open(&mut self, page: Page) {
        if page == self.page {
            return;
        }
        if self.page == Page::Settings {
            // The tab click arrives before the page draws again, so a port
            // still being typed is committed here instead.
            let snapshot = self.backend.snapshot();
            let file = match &snapshot.settings {
                Some(Ok(file)) => Some(file),
                _ => None,
            };
            if let Some(save) = self.settings.leave(file) {
                self.backend.act(save);
            }
        }
        if page == Page::Settings {
            self.backend.act(Action::ReloadSettings);
        }
        if self.page == Page::Status {
            self.status.cancel_restart();
        }
        if self.page == Page::Voices {
            self.backend.act(Action::WatchVoices(false));
            // A question left open would pop up again whenever the user
            // came back, long after they stopped expecting it.
            self.voices.cancel_delete();
        }
        if page == Page::Voices {
            self.backend.act(Action::WatchVoices(true));
        }
        self.page = page;
    }
}

/// The latest action's notice, in one wrapped line at the bottom of the
/// page area: the caution colour for a failure, the secondary text colour
/// otherwise.
fn notice_line(ui: &mut egui::Ui, notice: &Notice) {
    let palette = theme::palette(ui.visuals().dark_mode);
    let mut text = egui::RichText::new(&notice.text);
    text = if notice.is_error {
        theme::caution_text(text, palette)
    } else {
        text.color(palette.ink_dim)
    };
    egui::Panel::bottom("notice")
        .show_separator_line(false)
        .frame(egui::Frame::new().inner_margin(egui::Margin {
            top: 6,
            ..egui::Margin::ZERO
        }))
        .show(ui, |ui| {
            ui.add(egui::Label::new(text).wrap());
        });
}

impl eframe::App for WindowApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }

    /// What the window shows before the first frame, and for a moment while
    /// it's resized: the panel's own colour, rather than eframe's
    /// near-black, which would flash in the light theme.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }
}
