//! Voice model files on disk: download, verify, install.
//!
//! `DiskStore` sits in front of the installer and decides *whether* to
//! download: at most one download per archive at a time, no matter how many
//! requests want it, and a back-off after failures so a dead connection isn't
//! hammered by every announcement.

pub mod install;

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;

use crate::catalog::Archive;
use install::{InstallError, Installer};

/// How long to wait after a retryable failure before downloading again.
const RETRY_AFTER: Duration = Duration::from_secs(5 * 60);

/// Why a model isn't available, in plain words for the log and the console.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct StoreError {
    pub reason: String,
}

/// What `report_load_failure` did about a model that failed to load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadRecovery {
    /// The files were bad; they were moved aside and the next
    /// `ensure_installed` downloads them again.
    Quarantined,
    /// The files match their manifest (or couldn't be read), so a download
    /// wouldn't help; nothing was changed.
    FilesOk,
    /// The files were bad, but this run already re-downloaded them once (or
    /// couldn't move them aside); nothing more is tried until a restart.
    AlreadyRetried,
}

/// Where model folders come from. A trait so the speech pipeline can be
/// tested without touching the disk or the network.
pub trait ModelStore: Send + Sync + 'static {
    /// True if the archive is installed and its marker matches.
    fn is_installed(&self, archive: &'static Archive) -> bool;

    /// Returns the model folder, downloading it first if needed.
    fn ensure_installed(
        &self,
        archive: &'static Archive,
    ) -> impl Future<Output = Result<PathBuf, StoreError>> + Send;

    /// Called after a model folder failed to load: checks its files and, at
    /// most once per run, moves bad ones aside so they're downloaded again.
    ///
    /// This may re-hash hundreds of MB, so it blocks: call it from
    /// `tokio::task::spawn_blocking`, never directly on an async thread.
    fn report_load_failure(&self, archive: &'static Archive) -> LoadRecovery;
}

/// The progress of one archive's download, broadcast to every waiter.
#[derive(Debug, Clone)]
enum SlotState {
    Idle,
    Downloading,
    Done(Result<PathBuf, StoreError>),
}

/// Bookkeeping for one archive, keyed by its stem.
struct Slot {
    /// A `watch` channel holds a single current value. Requests that want
    /// this archive `subscribe()` to it and wait until it reads `Done`; the
    /// download task sets `Done` when it ends.
    state: watch::Sender<SlotState>,
    /// When the last download failed, and whether retrying can never help.
    last_failure: Option<(Instant, bool)>,
    /// True once this run has quarantined (or tried to quarantine) the
    /// archive's files after a load failure.
    redownload_used: bool,
}

impl Slot {
    fn new() -> Slot {
        // The channel stays open as long as the sender lives (in the slot),
        // so the first receiver can be dropped straight away.
        let (state, _) = watch::channel(SlotState::Idle);
        Slot {
            state,
            last_failure: None,
            redownload_used: false,
        }
    }
}

/// Shared map of slots. A std (not tokio) mutex: every critical section is a
/// few quick map operations, and it is never held across an `.await` (a
/// std guard isn't `Send`, so the compiler would also reject that).
type Slots = Arc<Mutex<HashMap<&'static str, Slot>>>;

/// The real store: model folders under `models_dir`, fetched by `installer`.
pub struct DiskStore<I: Installer> {
    models_dir: PathBuf,
    installer: Arc<I>,
    /// In an `Arc` so download tasks can update their slot after the request
    /// that started them is gone.
    slots: Slots,
}

impl<I: Installer> DiskStore<I> {
    pub fn new(models_dir: PathBuf, installer: I) -> Arc<Self> {
        Arc::new(DiskStore {
            models_dir,
            installer: Arc::new(installer),
            slots: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Under the slots lock: either a receiver to wait on (a download is
    /// running, or was just started) or an immediate error.
    fn join_or_start(
        &self,
        archive: &'static Archive,
    ) -> Result<watch::Receiver<SlotState>, StoreError> {
        let mut slots = lock(&self.slots);
        let slot = slots.entry(archive.stem).or_insert_with(Slot::new);

        if matches!(*slot.state.borrow(), SlotState::Downloading) {
            return Ok(slot.state.subscribe());
        }
        // Re-check the disk under the lock. `ensure_installed` checked it
        // without the lock, so a download may have finished in between, and
        // starting another would be wasted work. (It would do no harm: the
        // installer re-checks under its own cross-process lock and returns
        // the fresh folder untouched.) Reading the small marker file under
        // this short lock is fine.
        if install::is_installed(archive, &self.models_dir) {
            slot.state
                .send_replace(SlotState::Done(Ok(self.models_dir.join(archive.stem))));
            slot.last_failure = None;
            return Ok(slot.state.subscribe());
        }
        if let Some((failed_at, permanent)) = slot.last_failure
            && (permanent || failed_at.elapsed() < RETRY_AFTER)
        {
            return Err(last_error(&slot.state.borrow()));
        }

        slot.state.send_replace(SlotState::Downloading);
        let receiver = slot.state.subscribe();
        self.spawn_download(archive);
        Ok(receiver)
    }

    /// Starts the download in its own task. The task owns the work: requests
    /// only hold a `watch::Receiver`, so a request that gives up (its future
    /// is dropped) leaves the download running, and the result is recorded
    /// in the slot even if nobody is waiting any more. The task is never
    /// aborted: a download that every request gave up on should still
    /// finish, so the next request finds the voice installed. (Two installs
    /// of one archive can't collide even so: the installer holds a
    /// per-archive lock file until it is completely done.)
    fn spawn_download(&self, archive: &'static Archive) {
        // `install` borrows the installer and the folder, and a spawned task
        // must own everything it uses, so it gets its own handles.
        let installer = Arc::clone(&self.installer);
        let models_dir = self.models_dir.clone();
        let slots = Arc::clone(&self.slots);
        tokio::spawn(async move {
            // The install runs in a nested task so a panic inside it comes
            // back as a `JoinError` here, and the slot still leaves
            // `Downloading` instead of keeping waiters hanging.
            let install =
                tokio::spawn(async move { installer.install(archive, &models_dir).await });
            let result = match install.await {
                Ok(result) => result,
                Err(e) => Err(InstallError::Io(format!("the install task failed: {e}"))),
            };
            record_result(&slots, archive, result);
        });
    }
}

/// Stores a finished download's outcome in its slot and wakes every waiter.
fn record_result(slots: &Slots, archive: &Archive, result: Result<PathBuf, InstallError>) {
    let outcome = match result {
        Ok(dir) => (None, Ok(dir)),
        Err(e) => {
            let permanent = e.is_permanent();
            let reason = e.user_reason();
            if permanent {
                tracing::error!(
                    stem = archive.stem,
                    error = %e,
                    "couldn't download voice model, and retrying won't help: {reason}. \
                     Voices using it stay silent until SLC Local TTS is updated"
                );
            } else {
                tracing::warn!(
                    stem = archive.stem,
                    error = %e,
                    "couldn't download voice model: {reason}. \
                     It will be tried again in 5 minutes if a voice still needs it"
                );
            }
            (
                Some((Instant::now(), permanent)),
                Err(StoreError { reason }),
            )
        }
    };
    let mut slots = lock(slots);
    let slot = slots.entry(archive.stem).or_insert_with(Slot::new);
    slot.last_failure = outcome.0;
    // `send_replace` (unlike `send`) stores the value even when no receiver
    // is left, which is exactly the "everyone gave up" case.
    slot.state.send_replace(SlotState::Done(outcome.1));
}

/// The error stored by the last failed download.
fn last_error(state: &SlotState) -> StoreError {
    match state {
        SlotState::Done(Err(e)) => e.clone(),
        _ => StoreError {
            reason: "the voice model download failed recently; try again in a few minutes"
                .to_string(),
        },
    }
}

/// Locks the slots. A panic while holding the lock can't leave a slot half
/// updated (every update is a plain field write), so a poisoned lock is safe
/// to keep using.
fn lock(slots: &Slots) -> std::sync::MutexGuard<'_, HashMap<&'static str, Slot>> {
    slots
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl<I: Installer> ModelStore for DiskStore<I> {
    fn is_installed(&self, archive: &'static Archive) -> bool {
        install::is_installed(archive, &self.models_dir)
    }

    async fn ensure_installed(&self, archive: &'static Archive) -> Result<PathBuf, StoreError> {
        if self.is_installed(archive) {
            return Ok(self.models_dir.join(archive.stem));
        }
        // The lock is taken and released inside `join_or_start`, before the
        // `.await` below.
        let mut receiver = self.join_or_start(archive)?;
        // `wait_for` checks the current value first, so a download that has
        // already finished is seen at once.
        match receiver
            .wait_for(|state| matches!(state, SlotState::Done(_)))
            .await
        {
            Ok(state) => match &*state {
                SlotState::Done(result) => result.clone(),
                _ => unreachable!("wait_for only returns once the state is Done"),
            },
            // Only if the slot's sender were dropped, which never happens.
            Err(_) => Err(StoreError {
                reason: "the voice model download stopped unexpectedly; try again".to_string(),
            }),
        }
    }

    fn report_load_failure(&self, archive: &'static Archive) -> LoadRecovery {
        let dir = self.models_dir.join(archive.stem);
        match install::verify_files(&dir) {
            Ok(true) => return LoadRecovery::FilesOk,
            Ok(false) => {}
            Err(e) => {
                // Unreadable isn't proven bad (an antivirus scan can lock a
                // file for a moment), so the files are left alone.
                tracing::warn!(
                    stem = archive.stem,
                    dir = %dir.display(),
                    error = %e,
                    "a voice model failed to load and its files couldn't be checked; \
                     leaving them in place. If this keeps happening, restart SLC Local TTS"
                );
                return LoadRecovery::FilesOk;
            }
        }

        // Claim the one re-download this run allows, under the lock so two
        // reports can't both claim it. Released before the slow rename.
        {
            let mut slots = lock(&self.slots);
            let slot = slots.entry(archive.stem).or_insert_with(Slot::new);
            if slot.redownload_used {
                return LoadRecovery::AlreadyRetried;
            }
            slot.redownload_used = true;
        }

        if let Err(e) = install::quarantine(&dir) {
            tracing::error!(
                stem = archive.stem,
                dir = %dir.display(),
                error = %e,
                "a voice model's files are damaged and couldn't be moved aside; \
                 close any program using that folder, delete it, and restart SLC Local TTS"
            );
            return LoadRecovery::AlreadyRetried;
        }
        tracing::warn!(
            stem = archive.stem,
            "a voice model's files were damaged; moved them aside and will download it again"
        );

        // Forget the last result so the next `ensure_installed` downloads
        // again. A running download is left alone; it'll record its own result.
        let mut slots = lock(&self.slots);
        let slot = slots.entry(archive.stem).or_insert_with(Slot::new);
        if !matches!(*slot.state.borrow(), SlotState::Downloading) {
            slot.state.send_replace(SlotState::Idle);
            slot.last_failure = None;
        }
        LoadRecovery::Quarantined
    }
}

#[cfg(test)]
mod tests {
    use super::install::{InstallError, Installer};
    use super::*;
    use crate::catalog::Archive;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    static ARCHIVE: Archive = Archive {
        stem: "test-voice",
        url: "http://x/test-voice.tar.bz2",
        sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        size: 1,
    };

    /// Counts calls, waits `delay`, then fails with `failure` if one is set,
    /// or installs a one-file model folder with a valid `.verified` marker.
    struct FakeInstaller {
        calls: AtomicUsize,
        delay: Duration,
        failure: std::sync::Mutex<Option<fn() -> InstallError>>,
    }

    impl FakeInstaller {
        fn new(delay: Duration, failure: Option<fn() -> InstallError>) -> FakeInstaller {
            FakeInstaller {
                calls: AtomicUsize::new(0),
                delay,
                failure: std::sync::Mutex::new(failure),
            }
        }
    }

    impl Installer for FakeInstaller {
        async fn install(
            &self,
            archive: &'static Archive,
            models_dir: &Path,
        ) -> Result<PathBuf, InstallError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let failure = *self.failure.lock().unwrap();
            tokio::time::sleep(self.delay).await;
            if let Some(make_error) = failure {
                return Err(make_error());
            }
            let dir = models_dir.join(archive.stem);
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("model.onnx"), b"model")?;
            install::write_marker(&dir, archive)?;
            Ok(dir)
        }
    }

    fn calls(store: &DiskStore<FakeInstaller>) -> usize {
        store.installer.calls.load(Ordering::SeqCst)
    }

    fn network_error() -> InstallError {
        InstallError::Network("connection refused".into())
    }

    #[tokio::test(start_paused = true)]
    async fn start_after_a_finished_download_reuses_the_install() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeInstaller::new(Duration::ZERO, None);
        let store = DiskStore::new(tmp.path().join("models"), fake);
        let dir = store.ensure_installed(&ARCHIVE).await.unwrap();

        // As if `ensure_installed` had checked the disk just before the
        // download finished and only now takes the lock.
        let mut receiver = store.join_or_start(&ARCHIVE).unwrap();
        let state = receiver
            .wait_for(|s| matches!(s, SlotState::Done(_)))
            .await
            .unwrap()
            .clone();

        assert!(matches!(state, SlotState::Done(Ok(d)) if d == dir));
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(calls(&store), 1);
        assert!(store.is_installed(&ARCHIVE));
    }

    #[tokio::test(start_paused = true)]
    async fn installer_panic_becomes_an_error_and_frees_the_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeInstaller::new(
            Duration::ZERO,
            Some(|| -> InstallError { panic!("fake installer panic") }),
        );
        let store = DiskStore::new(tmp.path().join("models"), fake);

        let err = store.ensure_installed(&ARCHIVE).await.unwrap_err();
        assert!(
            err.reason.contains("the install task failed"),
            "{}",
            err.reason
        );

        // Not stuck in `Downloading`: inside the 5-minute window the second
        // call returns the stored error at once without a new download.
        let start = tokio::time::Instant::now();
        let again = store.ensure_installed(&ARCHIVE).await.unwrap_err();
        assert_eq!(again, err);
        assert_eq!(start.elapsed(), Duration::ZERO);
        assert_eq!(calls(&store), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn joined_waiter_receives_the_download_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeInstaller::new(Duration::from_secs(1), Some(network_error));
        let store = DiskStore::new(tmp.path().join("models"), fake);

        let (a, b) = tokio::join!(
            store.ensure_installed(&ARCHIVE),
            store.ensure_installed(&ARCHIVE)
        );

        assert_eq!(a.unwrap_err().reason, network_error().user_reason());
        assert_eq!(b.unwrap_err().reason, network_error().user_reason());
        assert_eq!(calls(&store), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_ensure_installed_downloads_once() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeInstaller::new(Duration::from_secs(1), None);
        let store = DiskStore::new(tmp.path().join("models"), fake);

        let (a, b) = tokio::join!(
            store.ensure_installed(&ARCHIVE),
            store.ensure_installed(&ARCHIVE)
        );

        let dir = tmp.path().join("models").join(ARCHIVE.stem);
        assert_eq!(a.unwrap(), dir);
        assert_eq!(b.unwrap(), dir);
        assert_eq!(calls(&store), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn network_failure_retries_after_five_minutes() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeInstaller::new(Duration::ZERO, Some(network_error));
        let store = DiskStore::new(tmp.path().join("models"), fake);

        let first = store.ensure_installed(&ARCHIVE).await.unwrap_err();
        assert_eq!(first.reason, network_error().user_reason());
        assert_eq!(calls(&store), 1);

        let second = store.ensure_installed(&ARCHIVE).await.unwrap_err();
        assert_eq!(second.reason, network_error().user_reason());
        assert_eq!(calls(&store), 1);

        *store.installer.failure.lock().unwrap() = None;
        tokio::time::advance(Duration::from_secs(300)).await;
        store.ensure_installed(&ARCHIVE).await.unwrap();
        assert_eq!(calls(&store), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn http_error_also_waits_five_minutes() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeInstaller::new(Duration::ZERO, Some(|| InstallError::HttpStatus(503)));
        let store = DiskStore::new(tmp.path().join("models"), fake);

        store.ensure_installed(&ARCHIVE).await.unwrap_err();
        tokio::time::advance(Duration::from_secs(299)).await;
        let err = store.ensure_installed(&ARCHIVE).await.unwrap_err();

        assert!(err.reason.contains("HTTP 503"), "{}", err.reason);
        assert_eq!(calls(&store), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn hash_mismatch_never_retried_this_run() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeInstaller::new(
            Duration::ZERO,
            Some(|| InstallError::HashMismatch {
                expected: "a".into(),
                got: "b".into(),
            }),
        );
        let store = DiskStore::new(tmp.path().join("models"), fake);

        store.ensure_installed(&ARCHIVE).await.unwrap_err();
        tokio::time::advance(Duration::from_secs(600)).await;
        let err = store.ensure_installed(&ARCHIVE).await.unwrap_err();

        assert!(err.reason.contains("please update"), "{}", err.reason);
        assert_eq!(calls(&store), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn dropped_waiter_does_not_cancel_download() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeInstaller::new(Duration::from_secs(1), None);
        let store = DiskStore::new(tmp.path().join("models"), fake);

        // `timeout` drops the request's future after 100 ms, as an HTTP
        // handler giving up would.
        let gave_up =
            tokio::time::timeout(Duration::from_millis(100), store.ensure_installed(&ARCHIVE))
                .await;
        assert!(gave_up.is_err());
        assert!(!store.is_installed(&ARCHIVE));

        // Sleeping lets the paused clock run past the 1 s download.
        tokio::time::sleep(Duration::from_secs(2)).await;

        assert!(store.is_installed(&ARCHIVE));
        assert_eq!(calls(&store), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn load_failure_quarantines_once() {
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path().join("models");
        let fake = FakeInstaller::new(Duration::ZERO, None);
        let store = DiskStore::new(models.clone(), fake);
        let dir = store.ensure_installed(&ARCHIVE).await.unwrap();

        std::fs::write(dir.join("model.onnx"), b"corrupt").unwrap();
        assert_eq!(
            store.report_load_failure(&ARCHIVE),
            LoadRecovery::Quarantined
        );
        assert!(!dir.exists());
        assert!(
            models
                .join("test-voice.broken")
                .join("model.onnx")
                .is_file()
        );

        store.ensure_installed(&ARCHIVE).await.unwrap();
        assert_eq!(calls(&store), 2);

        std::fs::write(dir.join("model.onnx"), b"corrupt").unwrap();
        assert_eq!(
            store.report_load_failure(&ARCHIVE),
            LoadRecovery::AlreadyRetried
        );
        assert!(dir.join("model.onnx").is_file());
        store.ensure_installed(&ARCHIVE).await.unwrap();
        assert_eq!(calls(&store), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn load_failure_with_good_files_is_files_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeInstaller::new(Duration::ZERO, None);
        let store = DiskStore::new(tmp.path().join("models"), fake);
        let dir = store.ensure_installed(&ARCHIVE).await.unwrap();

        assert_eq!(store.report_load_failure(&ARCHIVE), LoadRecovery::FilesOk);

        assert!(dir.join("model.onnx").is_file());
        assert!(store.is_installed(&ARCHIVE));
    }
}
