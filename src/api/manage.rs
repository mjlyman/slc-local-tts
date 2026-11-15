//! Managing voice models on the user's behalf: deleting one from the
//! settings window.

use std::sync::Arc;

use super::AppState;
use crate::catalog::Archive;
use crate::engine::Synthesizer;
use crate::models::{DeleteError, ModelState, ModelStore};

/// Deletes `archive`'s model: waits for the synthesis in progress, unloads
/// the model, then removes its files. Refused while the model downloads.
pub async fn delete_model<S: ModelStore, Y: Synthesizer>(
    state: &Arc<AppState<S, Y>>,
    archive: &'static Archive,
) -> Result<(), DeleteError> {
    // Checked before waiting for the lock, so the answer comes at once. The
    // store checks again under its own lock, where it counts.
    if matches!(state.store.state(archive), ModelState::Downloading { .. }) {
        return Err(DeleteError::Downloading);
    }

    // The synthesis lock makes the delete wait for the line being spoken,
    // and keeps the next line from loading the model while its files go.
    // `lock_owned` gives a guard that owns an `Arc` of the mutex, so it can
    // move into the blocking task below and stay held until the work ends,
    // even if this future is dropped.
    let guard = Arc::clone(&state.synth_lock).lock_owned().await;
    let task_state = Arc::clone(state);
    let result = tokio::task::spawn_blocking(move || {
        let _guard = guard;
        // Unloaded first, so no loaded model holds the files open.
        task_state.synth.unload(archive);
        let result = task_state.store.delete(archive);
        // Unless nothing was touched, the next files are a fresh download,
        // so an old load failure no longer says anything about them. Cleared
        // here, while the lock is still held, so the request waiting for the
        // lock never sees the old cooldown.
        if result != Err(DeleteError::Downloading) {
            task_state.load_cooldown.clear(archive);
        }
        result
    })
    .await
    .unwrap_or_else(|e| {
        // Only a bug gets here (the task panicked); the detail is for the
        // log, and the user gets plain words.
        tracing::error!(stem = archive.stem, error = %e, "the delete task failed");
        Err(DeleteError::Io(
            "an internal error stopped the delete".to_string(),
        ))
    });

    state.live.notify();
    result
}
