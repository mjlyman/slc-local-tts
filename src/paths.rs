//! Where SLC Local TTS keeps its files on disk.

use std::path::{Path, PathBuf};

/// Environment variable that overrides the data folder (handy for tests and
/// portable setups).
const HOME_OVERRIDE: &str = "SLC_LOCAL_TTS_HOME";

#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error(
        "Windows didn't report a LocalAppData folder, so SLC Local TTS doesn't know where to keep its files."
    )]
    NoLocalAppData,
}

/// The data folder and the fixed layout inside it.
#[derive(Debug, Clone)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    /// `SLC_LOCAL_TTS_HOME` if set, else `%LOCALAPPDATA%\SLC-Local-TTS`.
    /// Thin on purpose: the testable logic lives in `at()` and the getters.
    pub fn from_env() -> Result<DataDir, PathsError> {
        if let Some(home) = std::env::var_os(HOME_OVERRIDE).filter(|v| !v.is_empty()) {
            return Ok(DataDir::at(PathBuf::from(home)));
        }
        let local = std::env::var_os("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .ok_or(PathsError::NoLocalAppData)?;
        Ok(DataDir::at(PathBuf::from(local).join("SLC-Local-TTS")))
    }

    pub fn at(root: PathBuf) -> DataDir {
        DataDir { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn models(&self) -> PathBuf {
        self.root.join("models")
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn espeak_root(&self) -> PathBuf {
        self.root.join("espeak")
    }

    /// An empty file whose presence means SLC has sent this server a
    /// request at least once, so the settings window stops offering its
    /// "Connect SLC" steps for good.
    pub fn slc_seen_marker(&self) -> PathBuf {
        self.root.join("slc-connected")
    }

    /// An empty file whose presence means the tray program has started at
    /// least once, so the window that explains the tray icon opens only on
    /// the very first start.
    pub fn intro_marker(&self) -> PathBuf {
        self.root.join("tray-intro-shown")
    }

    /// The port the running server is bound to, written while it serves and
    /// removed when it stops. A second launch reads it to open the settings
    /// window on the right port, even after `config.toml` changed. It
    /// describes one run only, so it isn't part of [`user_data`].
    ///
    /// [`user_data`]: DataDir::user_data
    pub fn port_file(&self) -> PathBuf {
        self.root.join("running-port.txt")
    }

    /// The user's files in the data folder: settings, downloaded voices,
    /// logs, the unpacked espeak data, the "SLC has connected" marker and
    /// the "tray explained" marker. A repair or reinstall keeps exactly
    /// these (`update::reinstall`); anything else in the folder is the
    /// installer's. Anything new stored in the data folder must be added
    /// here, or a reinstall deletes it.
    pub fn user_data(&self) -> [PathBuf; 6] {
        [
            self.config_file(),
            self.models(),
            self.logs(),
            self.espeak_root(),
            self.slc_seen_marker(),
            self.intro_marker(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_override_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::at(tmp.path().to_path_buf());
        assert_eq!(dir.models(), tmp.path().join("models"));
        assert_eq!(dir.config_file(), tmp.path().join("config.toml"));
        assert_eq!(dir.logs(), tmp.path().join("logs"));
        assert_eq!(dir.espeak_root(), tmp.path().join("espeak"));
        assert_eq!(dir.root(), tmp.path());
        assert_eq!(dir.slc_seen_marker(), tmp.path().join("slc-connected"));
    }

    #[test]
    fn user_data_includes_slc_connected() {
        let dir = DataDir::at(PathBuf::from(r"C:\data"));
        assert!(dir.user_data().contains(&dir.slc_seen_marker()));
    }

    #[test]
    fn user_data_includes_intro_marker_not_port_file() {
        let dir = DataDir::at(PathBuf::from(r"C:\data"));
        assert_eq!(
            dir.intro_marker(),
            PathBuf::from(r"C:\data\tray-intro-shown")
        );
        assert_eq!(dir.port_file(), PathBuf::from(r"C:\data\running-port.txt"));
        assert!(dir.user_data().contains(&dir.intro_marker()));
        assert!(!dir.user_data().contains(&dir.port_file()));
    }
}
