//! SLC Local TTS: a local, ElevenLabs-compatible text-to-speech server for
//! Self-Loading Cargo. The two binaries are thin shells over this library:
//! `main.rs` is the tray program, `slc-local-tts.exe`, and `bin/cli.rs` the
//! console program, `slc-local-tts-cli.exe`. Every module lives here.
pub mod api;
pub mod audio;
pub mod autostart;
pub mod catalog;
pub mod config;
pub mod config_edit;
pub mod console;
pub mod desktop;
pub mod engine;
pub mod frontend;
pub mod instance;
pub mod logging;
pub mod models;
pub mod names;
pub mod paths;
pub mod server;
pub mod startup;
pub mod status;
pub mod stop;
pub mod timefmt;
pub mod tray;
pub mod ui;
pub mod update;
