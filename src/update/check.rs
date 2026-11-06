//! The update check and the startup apply, as plain logic over a small
//! [`Updater`] trait, so tests can drive them without a real install.

use tokio::sync::watch;
use velopack::{UpdateInfo, VelopackAsset};

use super::{UpdateError, UpdateState};

/// What the check and the apply need from Velopack.
///
/// The real implementation (`VelopackUpdater`) only works inside an app that
/// Setup.exe installed, so under `cargo test` it can't even be created. The
/// logic below is therefore written against this trait, and the tests pass a
/// scripted fake instead (like coding to an interface in Java so a test can
/// hand in a mock).
pub(crate) trait Updater {
    /// A newer package already downloaded by an earlier run, if any.
    fn pending(&self) -> Option<VelopackAsset>;
    /// Asks the feed for a newer version; `None` means nothing new.
    fn check(&self) -> Result<Option<UpdateInfo>, UpdateError>;
    /// Downloads and verifies the package for `update`.
    fn download(&self, update: &UpdateInfo) -> Result<(), UpdateError>;
    /// Starts the updater, which waits for this process to exit, then
    /// installs `pending` and starts the new version.
    fn apply_after_exit(&self, pending: &VelopackAsset) -> Result<(), UpdateError>;
}

/// One update check: finds a package left by an earlier run, or asks the
/// feed and downloads what's new. It returns nothing; its outcome is the last
/// state it publishes.
///
/// Every state goes out with `send_replace`, which always stores the value.
/// Plain `send` drops the value, with an error, once every receiver is gone,
/// and then the outcome would be lost.
pub(crate) fn run_check(updater: &impl Updater, publish: &watch::Sender<UpdateState>) {
    // A package from an earlier run needs no network.
    if let Some(asset) = updater.pending() {
        publish.send_replace(UpdateState::Ready {
            version: asset.Version,
        });
        return;
    }
    let state = match updater.check() {
        Err(e) => UpdateState::Failed {
            reason: e.to_string(),
        },
        Ok(None) => UpdateState::UpToDate,
        Ok(Some(info)) => {
            let version = info.TargetFullRelease.Version.clone();
            tracing::info!("found version {version}");
            publish.send_replace(UpdateState::Downloading);
            match updater.download(&info) {
                Ok(()) => UpdateState::Ready { version },
                Err(e) => UpdateState::Failed {
                    reason: e.to_string(),
                },
            }
        }
    };
    publish.send_replace(state);
}

/// Starts installing a downloaded update, if there is one. `Ok(true)` means
/// the updater is now waiting for this process to exit.
pub(crate) fn apply_pending(updater: &impl Updater) -> Result<bool, UpdateError> {
    let Some(asset) = updater.pending() else {
        return Ok(false);
    };
    tracing::info!(
        "Installing version {}. SLC Local TTS closes now and starts again by itself in a few seconds.",
        asset.Version
    );
    updater.apply_after_exit(&asset)?;
    Ok(true)
}

/// Starts the background check (if it should run) and returns a receiver
/// that always holds the latest [`UpdateState`]. It returns at once; the
/// check runs on tokio's blocking pool, because Velopack's network calls
/// block. It must be called inside the tokio runtime.
pub(crate) fn spawn_check_with<U: Updater + Send + 'static>(
    enabled: bool,
    updater: Result<U, UpdateError>,
) -> watch::Receiver<UpdateState> {
    // `watch::channel(..).1` keeps only the receiver. The sender is dropped
    // at once, but a receiver keeps the last value, so these fixed states
    // stay readable for as long as anyone asks.
    let updater = match updater {
        Err(UpdateError::NotInstalled) => return watch::channel(UpdateState::NotInstalled).1,
        Err(e) => {
            let reason = e.to_string();
            return watch::channel(UpdateState::Failed { reason }).1;
        }
        Ok(updater) => updater,
    };
    if !enabled {
        return watch::channel(UpdateState::Off).1;
    }
    let (sender, receiver) = watch::channel(UpdateState::UpToDate);
    // `move` hands the updater and the sender to the blocking thread, which
    // owns them from then on. When the check ends, the sender is dropped
    // there; receivers still keep the last value it published.
    tokio::task::spawn_blocking(move || run_check(&updater, &sender));
    receiver
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tokio::sync::watch;
    use velopack::{UpdateInfo, VelopackAsset};

    use super::*;
    use crate::update::{UpdateError, UpdateState};

    fn asset(version: &str) -> VelopackAsset {
        VelopackAsset {
            Version: version.to_string(),
            ..Default::default()
        }
    }

    fn info(version: &str) -> UpdateInfo {
        UpdateInfo {
            TargetFullRelease: asset(version),
            ..Default::default()
        }
    }

    /// A scripted stand-in for Velopack. Each result is used once; an
    /// unscripted call succeeds with "nothing new". Every call is recorded
    /// in `calls`, which the test keeps a clone of.
    #[derive(Default)]
    struct FakeUpdater {
        pending: Option<VelopackAsset>,
        check: Mutex<Option<Result<Option<UpdateInfo>, UpdateError>>>,
        download: Mutex<Option<Result<(), UpdateError>>>,
        apply: Mutex<Option<Result<(), UpdateError>>>,
        calls: Arc<Mutex<Vec<&'static str>>>,
        /// When set, `check` waits for the test to send `()` (or drop it).
        release_check: Option<Mutex<mpsc::Receiver<()>>>,
        /// When set, `download` records the published state at that moment.
        watch_state: Option<watch::Receiver<UpdateState>>,
        state_at_download: Mutex<Option<UpdateState>>,
        downloaded_version: Mutex<Option<String>>,
        applied_version: Mutex<Option<String>>,
    }

    impl FakeUpdater {
        fn log(&self, call: &'static str) {
            self.calls.lock().unwrap().push(call);
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Updater for FakeUpdater {
        fn pending(&self) -> Option<VelopackAsset> {
            self.log("pending");
            self.pending.clone()
        }

        fn check(&self) -> Result<Option<UpdateInfo>, UpdateError> {
            self.log("check");
            if let Some(release) = &self.release_check {
                // An error means the test dropped its sender: carry on.
                let _ = release.lock().unwrap().recv();
            }
            self.check.lock().unwrap().take().unwrap_or(Ok(None))
        }

        fn download(&self, update: &UpdateInfo) -> Result<(), UpdateError> {
            self.log("download");
            if let Some(state) = &self.watch_state {
                *self.state_at_download.lock().unwrap() = Some(state.borrow().clone());
            }
            *self.downloaded_version.lock().unwrap() =
                Some(update.TargetFullRelease.Version.clone());
            self.download.lock().unwrap().take().unwrap_or(Ok(()))
        }

        fn apply_after_exit(&self, pending: &VelopackAsset) -> Result<(), UpdateError> {
            self.log("apply");
            *self.applied_version.lock().unwrap() = Some(pending.Version.clone());
            self.apply.lock().unwrap().take().unwrap_or(Ok(()))
        }
    }

    /// Runs one check against `fake` and returns the last published state.
    fn final_state(fake: &FakeUpdater) -> UpdateState {
        let (sender, receiver) = watch::channel(UpdateState::UpToDate);
        run_check(fake, &sender);
        receiver.borrow().clone()
    }

    #[test]
    fn nothing_new_is_up_to_date() {
        let fake = FakeUpdater::default();
        assert_eq!(final_state(&fake), UpdateState::UpToDate);
        assert_eq!(fake.calls(), ["pending", "check"]);
    }

    #[test]
    fn newer_version_downloads_then_ready() {
        let (sender, receiver) = watch::channel(UpdateState::UpToDate);
        let fake = FakeUpdater {
            check: Mutex::new(Some(Ok(Some(info("0.3.0"))))),
            watch_state: Some(sender.subscribe()),
            ..Default::default()
        };
        run_check(&fake, &sender);

        assert_eq!(
            *fake.state_at_download.lock().unwrap(),
            Some(UpdateState::Downloading)
        );
        assert_eq!(
            *receiver.borrow(),
            UpdateState::Ready {
                version: "0.3.0".to_string()
            }
        );
        assert_eq!(
            fake.downloaded_version.lock().unwrap().as_deref(),
            Some("0.3.0")
        );
    }

    #[test]
    fn check_failure_is_failed_without_download() {
        let fake = FakeUpdater {
            check: Mutex::new(Some(Err(UpdateError::Network("timed out".to_string())))),
            ..Default::default()
        };
        match final_state(&fake) {
            UpdateState::Failed { reason } => {
                assert!(reason.contains("couldn't be reached"), "{reason}");
                assert!(reason.contains("timed out"), "{reason}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(!fake.calls().contains(&"download"), "{:?}", fake.calls());
    }

    #[test]
    fn download_failure_is_failed() {
        let fake = FakeUpdater {
            check: Mutex::new(Some(Ok(Some(info("0.3.0"))))),
            download: Mutex::new(Some(Err(UpdateError::Damaged(
                "checksum mismatch".to_string(),
            )))),
            ..Default::default()
        };
        assert!(
            matches!(final_state(&fake), UpdateState::Failed { .. }),
            "{:?}",
            fake.calls()
        );
    }

    #[test]
    fn pending_package_is_ready_without_checking() {
        let fake = FakeUpdater {
            pending: Some(asset("0.2.5")),
            ..Default::default()
        };
        assert_eq!(
            final_state(&fake),
            UpdateState::Ready {
                version: "0.2.5".to_string()
            }
        );
        assert!(!fake.calls().contains(&"check"), "{:?}", fake.calls());
    }

    #[test]
    fn apply_without_pending_is_false() {
        let fake = FakeUpdater::default();
        assert!(matches!(apply_pending(&fake), Ok(false)));
        assert!(!fake.calls().contains(&"apply"), "{:?}", fake.calls());
    }

    #[test]
    fn apply_with_pending_launches_the_updater() {
        let fake = FakeUpdater {
            pending: Some(asset("0.2.5")),
            ..Default::default()
        };
        assert!(matches!(apply_pending(&fake), Ok(true)));
        assert_eq!(
            fake.applied_version.lock().unwrap().as_deref(),
            Some("0.2.5")
        );
    }

    #[test]
    fn apply_launch_failure_is_an_error() {
        let fake = FakeUpdater {
            pending: Some(asset("0.2.5")),
            apply: Mutex::new(Some(Err(UpdateError::Other(
                "Update.exe is missing".to_string(),
            )))),
            ..Default::default()
        };
        assert!(matches!(apply_pending(&fake), Err(UpdateError::Other(_))));
    }

    #[test]
    fn not_installed_skips_quietly() {
        let updates = spawn_check_with::<FakeUpdater>(true, Err(UpdateError::NotInstalled));
        assert_eq!(*updates.borrow(), UpdateState::NotInstalled);
    }

    #[test]
    fn off_when_disabled_and_never_checks() {
        let fake = FakeUpdater::default();
        let calls = Arc::clone(&fake.calls);
        let updates = spawn_check_with(false, Ok(fake));
        assert_eq!(*updates.borrow(), UpdateState::Off);
        assert!(
            calls.lock().unwrap().is_empty(),
            "{:?}",
            calls.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn spawn_check_returns_before_the_check_finishes() {
        let (release, blocked) = mpsc::channel();
        let fake = FakeUpdater {
            check: Mutex::new(Some(Ok(Some(info("0.3.0"))))),
            release_check: Some(Mutex::new(blocked)),
            ..Default::default()
        };
        let mut updates = spawn_check_with(true, Ok(fake));
        assert_eq!(*updates.borrow(), UpdateState::UpToDate);

        release.send(()).unwrap();
        let ready = tokio::time::timeout(
            Duration::from_secs(5),
            updates.wait_for(|state| matches!(state, UpdateState::Ready { .. })),
        )
        .await;
        assert!(matches!(ready, Ok(Ok(_))), "the check never became Ready");
    }
}
