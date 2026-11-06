//! SLC Local TTS: a local, ElevenLabs-compatible text-to-speech server for
//! Self-Loading Cargo. The binary in `main.rs` is a thin shell over this
//! library; every module lives here.
pub mod api;
pub mod audio;
pub mod catalog;
pub mod config;
pub mod console;
pub mod engine;
pub mod frontend;
pub mod instance;
pub mod logging;
pub mod models;
pub mod paths;
pub mod server;
pub mod update;
