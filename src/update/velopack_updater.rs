//! The real [`Updater`]: Velopack's `UpdateManager`, reading GitHub
//! Releases, or the feed that [`FEED_ENV`] names.

use std::ffi::OsStr;
use std::path::PathBuf;

use velopack::sources::{FileSource, GithubSource, HttpSource, UpdateSource};
use velopack::{UpdateCheck, UpdateInfo, UpdateManager, VelopackAsset};

use super::UpdateError;
use super::check::Updater;

/// The GitHub repository whose releases are the update feed.
pub const REPO_URL: &str = "https://github.com/mjlyman/slc-local-tts";

/// Set this to an `http://` or `https://` URL, or to a folder, to read
/// updates from there instead of GitHub (for testing a release).
pub const FEED_ENV: &str = "SLC_LOCAL_TTS_UPDATE_FEED";

/// Where updates come from.
#[derive(Debug, PartialEq)]
pub(crate) enum Feed {
    Github,
    Http(String),
    Folder(PathBuf),
}

/// Picks the feed from [`FEED_ENV`]'s value: unset or empty means GitHub, a
/// URL means an HTTP feed, and anything else is a folder.
pub(crate) fn feed_choice(value: Option<&OsStr>) -> Feed {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Feed::Github;
    };
    // A value that isn't valid Unicode can't be a URL, so it's a folder.
    match value.to_str() {
        Some(text)
            if starts_with_ignore_ascii_case(text, "http://")
                || starts_with_ignore_ascii_case(text, "https://") =>
        {
            Feed::Http(text.to_string())
        }
        _ => Feed::Folder(PathBuf::from(value)),
    }
}

/// Compares bytes, so it never slices a string inside a multi-byte letter.
fn starts_with_ignore_ascii_case(text: &str, prefix: &str) -> bool {
    text.as_bytes()
        .get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// Velopack's update manager for this install.
pub(crate) struct VelopackUpdater {
    manager: UpdateManager,
}

impl VelopackUpdater {
    /// Fails with [`UpdateError::NotInstalled`] unless this exe is inside an
    /// install made by Setup.exe, so always under `cargo run` and `cargo test`.
    pub(crate) fn from_env() -> Result<VelopackUpdater, UpdateError> {
        let source: Box<dyn UpdateSource> = match feed_choice(std::env::var_os(FEED_ENV).as_deref())
        {
            Feed::Github => Box::new(GithubSource::new(REPO_URL, None, false)),
            Feed::Http(url) => {
                tracing::info!("checking for updates at {url} ({FEED_ENV} is set)");
                Box::new(HttpSource::new(url))
            }
            Feed::Folder(path) => {
                tracing::info!(
                    "checking for updates in {} ({FEED_ENV} is set)",
                    path.display()
                );
                Box::new(FileSource::new(path))
            }
        };
        let manager = UpdateManager::new_boxed(source, None, None)?;
        Ok(VelopackUpdater { manager })
    }
}

impl Updater for VelopackUpdater {
    fn pending(&self) -> Option<VelopackAsset> {
        self.manager.get_update_pending_restart()
    }

    fn check(&self) -> Result<Option<UpdateInfo>, UpdateError> {
        match self.manager.check_for_updates()? {
            // `*boxed` moves the `UpdateInfo` out of its `Box`.
            UpdateCheck::UpdateAvailable(boxed) => Ok(Some(*boxed)),
            UpdateCheck::NoUpdateAvailable | UpdateCheck::RemoteIsEmpty => Ok(None),
        }
    }

    fn download(&self, update: &UpdateInfo) -> Result<(), UpdateError> {
        Ok(self.manager.download_updates(update, None)?)
    }

    fn apply_after_exit(&self, pending: &VelopackAsset) -> Result<(), UpdateError> {
        // The restart arguments are this run's own, so `serve` restarts as
        // `serve`. Not silent: Velopack may show its progress window.
        Ok(self.manager.wait_exit_then_apply_updates(
            pending,
            false,
            true,
            std::env::args_os().skip(1),
        )?)
    }
}

/// Lets `?` turn Velopack's errors into ours.
impl From<velopack::Error> for UpdateError {
    fn from(e: velopack::Error) -> Self {
        use velopack::Error as V;
        match e {
            V::NotInstalled(_) => UpdateError::NotInstalled,
            V::Network(_) => UpdateError::Network(e.to_string()),
            V::ChecksumInvalid(..) | V::SizeInvalid(..) | V::Zip(_) | V::InvalidPackage(_) => {
                UpdateError::Damaged(e.to_string())
            }
            _ => UpdateError::Other(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use super::*;
    use crate::update::UpdateError;

    #[test]
    fn feed_choice_defaults_to_github() {
        assert_eq!(feed_choice(None), Feed::Github);
        assert_eq!(feed_choice(Some(OsStr::new(""))), Feed::Github);
    }

    #[test]
    fn feed_choice_url_is_http() {
        assert_eq!(
            feed_choice(Some(OsStr::new("HTTPS://example.test/feed"))),
            Feed::Http("HTTPS://example.test/feed".to_string())
        );
    }

    #[test]
    fn feed_choice_path_is_folder() {
        let path = r"C:\feeds\José Ü @feed";
        assert_eq!(
            feed_choice(Some(OsStr::new(path))),
            Feed::Folder(PathBuf::from(path))
        );
    }

    #[test]
    fn velopack_errors_map_to_plain_reasons() {
        let not_installed = velopack::Error::NotInstalled("no Update.exe".to_string());
        assert!(matches!(
            UpdateError::from(not_installed),
            UpdateError::NotInstalled
        ));

        let checksum = velopack::Error::ChecksumInvalid(
            PathBuf::from("SLC-Local-TTS-0.3.0-full.nupkg"),
            "aaaa".to_string(),
            "bbbb".to_string(),
        );
        match UpdateError::from(checksum) {
            UpdateError::Damaged(reason) => assert!(reason.contains("aaaa"), "{reason}"),
            other => panic!("expected Damaged, got {other:?}"),
        }

        let other = velopack::Error::Other("something odd".to_string());
        match UpdateError::from(other) {
            UpdateError::Other(reason) => assert_eq!(reason, "something odd"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn real_updater_is_not_installed_under_cargo_test() {
        assert!(matches!(
            VelopackUpdater::from_env(),
            Err(UpdateError::NotInstalled)
        ));
    }
}
