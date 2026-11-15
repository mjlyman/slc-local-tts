//! The settings window: `slc-local-tts.exe window --port <n>`, a process of
//! its own that talks to the server over HTTP.
//!
//! It's separate from the server so that closing it, or a crash in it,
//! never stops speech. It reads everything through the server's private
//! `/slc/` routes and remembers nothing between openings.

pub mod api;
pub mod app;
pub mod backend;
pub mod fonts;
pub mod model;
pub mod pages;
pub mod poller;
pub mod sound;
pub mod theme;
pub mod widgets;

use std::process::ExitCode;
use std::sync::Arc;

use eframe::egui;

use crate::desktop::dialog;
use crate::desktop::shell::{self, Raise};
use crate::instance;
use crate::logging;
use crate::startup::{self, Mode};
use crate::tray::icon::render;
use crate::tray::model::Badge;
use api::HttpApi;
use app::WindowApp;
use poller::PollingBackend;

/// The window's title, which a second launch also finds it by.
pub const TITLE: &str = "SLC Local TTS";

/// The window's own instance mutex: one settings window at a time.
const WINDOW_MUTEX: &str = r"Local\SLC-Local-TTS-window";

/// The window's icon size, in pixels.
const ICON_SIZE: u32 = 64;

/// How the window opens: 440 × 640 points, at least 380 × 480, centred on
/// the screen, with the app's icon.
pub fn native_options() -> eframe::NativeOptions {
    let icon = egui::IconData {
        rgba: render(ICON_SIZE, Badge::None),
        width: ICON_SIZE,
        height: ICON_SIZE,
    };
    eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size([440.0, 640.0])
            .with_min_inner_size([380.0, 480.0])
            .with_icon(icon),
        centered: true,
        ..Default::default()
    }
}

/// Runs the settings window for the server on `port`. `first_run` is set on
/// the tray program's very first start, to explain the tray icon.
pub fn run_window(port: u16, first_run: bool) -> ExitCode {
    let boot = match startup::boot(Mode::Window) {
        Ok(boot) => boot,
        Err(text) => {
            dialog::error(&text);
            return ExitCode::FAILURE;
        }
    };
    // Held until this function returns, when the window has closed.
    let _window_guard = match instance::acquire_named(WINDOW_MUTEX) {
        Ok(guard) => guard,
        Err(_) => {
            match shell::bring_to_front(TITLE) {
                Raise::Raised => {
                    tracing::info!("the settings window is already open; brought it to the front");
                }
                Raise::KeptBehind => tracing::info!(
                    "the settings window is already open; found it, but Windows kept another window in front"
                ),
                Raise::NotFound => tracing::warn!(
                    "the settings window is already open, but it couldn't be found to bring it to the front"
                ),
            }
            logging::flush();
            return ExitCode::SUCCESS;
        }
    };
    tracing::info!("the settings window opened for the server on port {port}");

    // Read before the window opens, so a slow disk never shows as a blank
    // first frame.
    let fonts = fonts::definitions(&fonts::windows_fonts_dir());
    let data = boot.data;
    let result = eframe::run_native(
        TITLE,
        native_options(),
        // eframe calls this once, when the window exists. `move` hands it
        // the fonts and the data folder; it runs at most once, so moving
        // them out of it again is fine.
        Box::new(move |cc| {
            theme::install(&cc.egui_ctx);
            fonts::install(&cc.egui_ctx, fonts);
            let api = HttpApi::new(port)?;
            let backend = PollingBackend::start(Arc::new(api), cc.egui_ctx.clone(), data);
            Ok(Box::new(WindowApp::new(backend, first_run)))
        }),
    );
    let code = match result {
        Ok(()) => {
            tracing::info!("the settings window closed");
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!("the settings window couldn't open ({e})");
            dialog::error(&format!(
                "The SLC Local TTS window couldn't open ({e}). The speech server is still running."
            ));
            ExitCode::FAILURE
        }
    };
    logging::flush();
    code
}
