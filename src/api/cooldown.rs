//! The load cooldown: after a model load failure, don't reload that archive
//! for 5 minutes, unless the store quarantined it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::Instant;

use super::AppState;
use crate::catalog::{Archive, Voice};
use crate::engine::Synthesizer;
use crate::models::{LoadRecovery, ModelStore};

/// After a voice's model fails to load, requests for it fail at once for this
/// long instead of loading (and re-checking) hundreds of MB again each time.
const LOAD_RETRY_AFTER: Duration = Duration::from_secs(5 * 60);

/// When each archive last failed to load, keyed by its stem. A std mutex is
/// fine: it guards one quick map operation and is never held across `.await`.
#[derive(Debug, Default)]
pub struct LoadCooldown(Mutex<HashMap<&'static str, Instant>>);

impl LoadCooldown {
    /// Starts the cooldown for `archive`; returns the time it stored, which
    /// identifies this failure to [`LoadCooldown::clear_if`].
    pub(crate) fn record(&self, archive: &'static Archive) -> Instant {
        let now = Instant::now();
        self.lock().insert(archive.stem, now);
        now
    }

    /// True if `archive` failed to load less than `LOAD_RETRY_AFTER` ago.
    pub(crate) fn is_cooling(&self, archive: &'static Archive) -> bool {
        self.is_cooling_at(archive, Instant::now())
    }

    /// `is_cooling` as seen at `now`; separate so tests can pick the time.
    pub(crate) fn is_cooling_at(&self, archive: &'static Archive, now: Instant) -> bool {
        self.lock()
            .get(archive.stem)
            .is_some_and(|&failed_at| now.saturating_duration_since(failed_at) < LOAD_RETRY_AFTER)
    }

    /// Ends the cooldown for `archive`, but only if it still belongs to the
    /// failure recorded at `at`. A report that finishes late must not wipe
    /// the entry of a newer failure.
    pub(crate) fn clear_if(&self, archive: &'static Archive, at: Instant) {
        let mut map = self.lock();
        if map.get(archive.stem) == Some(&at) {
            map.remove(archive.stem);
        }
    }

    /// A panic can't leave the map half-updated, so a poisoned lock is safe.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<&'static str, Instant>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Starts the cooldown for `voice`'s archive and asks the store to check its
/// files. Runs on a blocking thread, so it never stalls an async worker.
pub(crate) fn on_load_failure<S: ModelStore, Y: Synthesizer>(
    state: &Arc<AppState<S, Y>>,
    voice: &'static Voice,
    detail: &str,
) {
    // Logged here, not in the request future: this runs once per real
    // failure even if SLC hung up or the budget ran out during the load.
    tracing::warn!(
        voice = voice.id,
        %detail,
        "the voice model failed to load; it won't be loaded again for 5 minutes"
    );
    // Recorded at once, so requests that arrive while the files are being
    // checked fail fast too.
    let failed_at = state.load_cooldown.record(voice.archive);
    // The check may re-hash hundreds of MB, so it gets its own blocking task
    // instead of holding up the caller (and, from `synthesize`, the
    // synthesis lock). Nobody awaits it, on purpose: dropping the
    // `JoinHandle` detaches the task, and a panic in it is reported only by
    // the panic hook.
    let state = Arc::clone(state);
    let archive = voice.archive;
    tokio::task::spawn_blocking(move || {
        let recovery = state.store.report_load_failure(archive);
        tracing::debug!(stem = archive.stem, ?recovery, "load failure reported");
        // Quarantined files are downloaded again on the next request, so
        // that request may try to load them.
        if recovery == LoadRecovery::Quarantined {
            state.load_cooldown.clear_if(archive, failed_at);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;

    fn archive() -> &'static Archive {
        catalog::find("M1").unwrap().archive
    }

    #[test]
    fn cooldown_ends_after_load_retry_after() {
        let cooldown = LoadCooldown::default();
        let at = cooldown.record(archive());
        let tick = Duration::from_millis(1);
        assert!(cooldown.is_cooling_at(archive(), at));
        assert!(cooldown.is_cooling_at(archive(), at + LOAD_RETRY_AFTER - tick));
        assert!(!cooldown.is_cooling_at(archive(), at + LOAD_RETRY_AFTER));
        assert!(!cooldown.is_cooling_at(archive(), at + LOAD_RETRY_AFTER + tick));
    }

    #[test]
    fn clear_if_only_clears_its_own_failure() {
        let cooldown = LoadCooldown::default();
        let stale = cooldown.record(archive());
        std::thread::sleep(Duration::from_millis(2));
        let current = cooldown.record(archive());
        assert_ne!(stale, current);

        cooldown.clear_if(archive(), stale);
        assert!(cooldown.is_cooling_at(archive(), current));

        cooldown.clear_if(archive(), current);
        assert!(!cooldown.is_cooling_at(archive(), current));
    }
}
