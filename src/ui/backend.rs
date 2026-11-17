//! What the window's pages draw from, and what they can ask for.
//!
//! The pages never talk to the server themselves. They read a [`Snapshot`]
//! and send [`Action`]s through the [`Backend`] trait, so the tests can give
//! them a fake backend and check what they asked for.

use std::sync::Arc;

use crate::api::slc::dto::{ModelDto, StatusDto};
use crate::config_edit::FileSettings;
use crate::status::records::RequestRecord;

/// Whether the server is answering the window's polls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Connection {
    /// No answer yet, since the window opened.
    #[default]
    Connecting,
    Connected,
    /// Several polls in a row went unanswered: the server has stopped.
    Lost,
}

/// A one-line message for the user about something they asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub text: String,
    /// True when it reports a failure.
    pub is_error: bool,
}

/// Everything the pages show, as of the latest poll or action.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub connection: Connection,
    /// The server's latest status answer. It's kept while the connection is
    /// lost, so the pages can still say what was going on.
    pub status: Option<StatusDto>,
    /// The voice models, once they've been asked for.
    pub voices: Option<Vec<ModelDto>>,
    /// The recent requests, oldest first, at most 200.
    pub records: Vec<RequestRecord>,
    /// Whether Windows starts SLC Local TTS when the user signs in.
    pub autostart: bool,
    /// `config.toml`'s settings, or why they couldn't be read.
    pub settings: Option<Result<FileSettings, String>>,
    /// How many settings saves have finished. It goes up in the same
    /// snapshot that brings each save's re-read of the file in `settings`,
    /// so the Settings page can tell when a save it sent has come back.
    pub settings_saves: u64,
    /// The result of the latest action, when it has something to say.
    pub notice: Option<Notice>,
}

/// Something a page asks the backend to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    CopyText(String),
    /// Download the model with this stem.
    Download(String),
    /// Delete the model with this stem.
    Delete(String),
    /// Play this voice ID's preview.
    Play(String),
    StartServer,
    OpenLogFolder,
    OpenConfig,
    OpenLicences,
    OpenUrl(String),
    SetAutostart(bool),
    /// Save the fields where `new` differs from `base`, the file's values
    /// the page drew from, into `config.toml` as it is now. A field the
    /// user didn't change keeps whatever the file holds, even an edit made
    /// by hand since the page last read it.
    SaveSettings {
        base: FileSettings,
        new: FileSettings,
    },
    /// The Settings page opened: read `config.toml` and the start-with-
    /// Windows setting again, in case they changed since the last read.
    ReloadSettings,
    /// The Voices page opened (true) or closed (false), so the voice list
    /// is polled only while someone looks at it.
    WatchVoices(bool),
    ApplyUpdate,
}

/// The pages' view of the server.
///
/// `Send + Sync` because the real backend's threads share it with the
/// window's thread.
pub trait Backend: Send + Sync {
    /// The latest snapshot. An `Arc`, so handing it to a page each frame
    /// copies a pointer, not the records.
    fn snapshot(&self) -> Arc<Snapshot>;
    /// Asks for `action`, without waiting for it to finish.
    fn act(&self, action: Action);
}
