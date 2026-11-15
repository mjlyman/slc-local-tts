//! Deleting a voice model from the settings window: it waits for the
//! synthesis in progress, unloads the model, and a line that was waiting for
//! that voice downloads it again instead of loading files that are gone.

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde_json::json;

use common::{FakeStore, FakeSynth, post_json, state_with, test_app};
use slc_local_tts::api::manage::delete_model;
use slc_local_tts::catalog::{self, Voice};
use slc_local_tts::engine::SynthError;
use slc_local_tts::models::{DeleteError, ModelState, ModelStore};

const URI: &str = "/v1/text-to-speech/M1/stream";

fn voice() -> &'static Voice {
    catalog::find("M1").unwrap()
}

fn speech_body(text: &str) -> serde_json::Value {
    json!({
        "text": text,
        "model_id": "eleven_multilingual_v2",
        "voice_settings": {"speed": 1.0}
    })
}

#[tokio::test]
async fn delete_model_waits_for_synthesis_and_unloads() {
    let synth = FakeSynth {
        delay: Duration::from_millis(300),
        ..Default::default()
    };
    let st = state_with(FakeStore::with_installed(voice()), synth, 2_000);
    let app = test_app(st.clone());
    let speech = tokio::spawn(async move { post_json(&app, URI, speech_body("One")).await });

    tokio::time::sleep(Duration::from_millis(50)).await;
    let result = delete_model(&st, voice().archive).await;
    let deleted_at = Instant::now();

    assert_eq!(result, Ok(()));
    assert_eq!(speech.await.unwrap().status, StatusCode::OK);
    let calls = st.synth.calls();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].ended <= deleted_at,
        "the delete finished before the synthesis did"
    );
    assert_eq!(*st.synth.unloads.lock().unwrap(), [voice().archive.stem]);
    assert_eq!(st.store.delete_calls.load(Ordering::SeqCst), 1);
    assert!(!st.store.is_installed(voice().archive));
}

#[tokio::test]
async fn delete_model_refuses_while_downloading() {
    let store = FakeStore::default();
    store.states.lock().unwrap().insert(
        voice().archive.stem,
        ModelState::Downloading {
            done: 10,
            total: 100,
        },
    );
    let st = state_with(store, FakeSynth::default(), 2_000);

    let result = delete_model(&st, voice().archive).await;

    assert_eq!(result, Err(DeleteError::Downloading));
    assert_eq!(st.store.delete_calls.load(Ordering::SeqCst), 0);
    assert!(st.synth.unloads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn deleted_while_waiting_redownloads_without_cooldown() {
    // Installed at first; once deleted, "downloading" it takes far longer
    // than the 1 s budget.
    let store = FakeStore {
        download_delay: Duration::from_secs(10),
        ..FakeStore::with_installed(voice())
    };
    let synth = FakeSynth {
        delay: Duration::from_millis(300),
        ..Default::default()
    };
    let st = state_with(store, synth, 1_000);
    let app = test_app(st.clone());

    // A speaks for 300 ms.
    let a_app = app.clone();
    let a = tokio::spawn(async move { post_json(&a_app, URI, speech_body("One")).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(st.store.ensure_calls.load(Ordering::SeqCst), 1);

    // The delete queues behind A for the synthesis lock...
    let delete_state = st.clone();
    let delete = tokio::spawn(async move { delete_model(&delete_state, voice().archive).await });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // ...and B, for the same voice, queues behind the delete.
    let b = post_json(&app, URI, speech_body("Two")).await;

    assert_eq!(b.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(b.json()["detail"]["status"], "budget_exceeded");
    let message = b.json()["detail"]["message"].as_str().unwrap().to_string();
    assert!(message.contains("still downloading"), "{message}");
    // A asked once; B asked twice: before waiting, and after the delete.
    assert_eq!(st.store.ensure_calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        st.synth.calls().len(),
        1,
        "B synthesized from deleted files"
    );
    assert!(!st.load_cooldown.is_cooling(voice().archive));
    assert_eq!(delete.await.unwrap(), Ok(()));
    assert_eq!(a.await.unwrap().status, StatusCode::OK);
}

#[tokio::test]
async fn cooldown_is_cleared_before_the_lock_is_released() {
    // The voice fails to load once, which starts its load cooldown.
    let synth = FakeSynth {
        synth_result: Err(SynthError::LoadFailed("bad model".to_string())),
        ..Default::default()
    };
    let st = state_with(FakeStore::with_installed(voice()), synth, 2_000);
    let app = test_app(st.clone());
    let failed = post_json(&app, URI, speech_body("One")).await;
    assert_eq!(failed.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(st.load_cooldown.is_cooling(voice().archive));

    // Hold the synthesis lock so the delete, then a request, queue for it in
    // that order.
    let held = st.synth_lock.lock().await;
    let delete_state = st.clone();
    let delete = tokio::spawn(async move { delete_model(&delete_state, voice().archive).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let waiter_state = st.clone();
    let waiter = tokio::spawn(async move {
        let _lock = waiter_state.synth_lock.lock().await;
        // What the next request sees the moment it gets the lock.
        waiter_state.load_cooldown.is_cooling(voice().archive)
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(held);

    assert!(
        !waiter.await.unwrap(),
        "the request after the delete saw the old load cooldown"
    );
    assert_eq!(delete.await.unwrap(), Ok(()));
}
