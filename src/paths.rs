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

    /// The user's files in the data folder: settings, downloaded voices,
    /// logs and the unpacked espeak data. A repair or reinstall keeps
    /// exactly these (`update::reinstall`); anything else in the folder is
    /// the installer's. Anything new stored in the data folder must be
    /// added here, or a reinstall deletes it.
    pub fn user_data(&self) -> [PathBuf; 4] {
        [
            self.config_file(),
            self.models(),
            self.logs(),
            self.espeak_root(),
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
    }
}
