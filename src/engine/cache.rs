//! A small cache of loaded models that unloads the ones left idle.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Loaded models keyed by a `'static` name, each with the time it was last used.
///
/// Values are handed out as `Arc<T>` so a caller can use a model after the
/// cache's lock is released. Unloading just drops the cache's `Arc`; the
/// model itself is freed when the last `Arc` goes away.
pub struct ModelCache<T> {
    entries: Mutex<HashMap<&'static str, (Arc<T>, Instant)>>,
}

impl<T> Default for ModelCache<T> {
    fn default() -> Self {
        ModelCache {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl<T> ModelCache<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the model for `key`, calling `load` only if it isn't loaded.
    ///
    /// The lock is held while `load` runs, so two callers can never load the
    /// same model twice. In practice there is no contention anyway: every
    /// caller already holds the global synthesis lock.
    pub fn get_or_load(
        &self,
        key: &'static str,
        load: impl FnOnce() -> Result<T, String>,
    ) -> Result<Arc<T>, String> {
        let mut entries = self.lock();
        if let Some((model, _)) = entries.get(key) {
            return Ok(Arc::clone(model));
        }
        let model = Arc::new(load()?);
        entries.insert(key, (Arc::clone(&model), Instant::now()));
        Ok(model)
    }

    /// Records that `key` was used at `now`, restarting its idle timer.
    pub fn mark_used(&self, key: &'static str, now: Instant) {
        if let Some((_, last_used)) = self.lock().get_mut(key) {
            *last_used = now;
        }
    }

    /// Drops every model unused for at least `idle_for` as of `now`; returns
    /// how many were dropped.
    ///
    /// No `Arc::strong_count` check is needed to avoid unloading a model in
    /// use: every `synthesize` and `unload_idle` call runs under the global
    /// synthesis lock (Tasks 14/15), so nothing else holds a model's `Arc`
    /// while this runs, and dropping ours frees it straight away.
    pub fn unload_idle(&self, now: Instant, idle_for: Duration) -> usize {
        let mut entries = self.lock();
        let before = entries.len();
        // `saturating_duration_since` gives zero if `last_used` is after `now`.
        entries.retain(|_, (_, last_used)| now.saturating_duration_since(*last_used) < idle_for);
        before - entries.len()
    }

    /// Locks the map. A panic while the lock was held (say, in a loader)
    /// leaves the map itself intact, so a poisoned lock is safe to reuse.
    fn lock(&self) -> MutexGuard<'_, HashMap<&'static str, (Arc<T>, Instant)>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    #[test]
    fn cache_loads_once_and_unloads_idle() {
        let cache: ModelCache<String> = ModelCache::new();
        let loads = Cell::new(0);
        let load = || {
            loads.set(loads.get() + 1);
            Ok("model".to_string())
        };
        assert_eq!(*cache.get_or_load("joe", load).unwrap(), "model");
        assert_eq!(*cache.get_or_load("joe", load).unwrap(), "model");
        assert_eq!(loads.get(), 1);

        let t0 = Instant::now();
        let minute = Duration::from_secs(60);
        cache.mark_used("joe", t0);
        assert_eq!(cache.unload_idle(t0 + 9 * minute, 10 * minute), 0);
        assert_eq!(cache.unload_idle(t0 + 11 * minute, 10 * minute), 1);
    }
}
