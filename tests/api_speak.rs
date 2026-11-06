//! The speech and preview routes: 200 WAV on success,
//! 503 JSON on speech failures, 200 + silence ("degraded") on preview failures.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;

use common::{
    FakeStore, FakeSynth, HOST, TestResponse, capture_logs, eventually, get, get_with, post_json,
    post_json_with, send, state, state_with, state_with_budgets, test_app,
};
use slc_local_tts::api::speak::PREVIEW_TEXT;
use slc_local_tts::engine::{Prepared, SynthError};
use slc_local_tts::models::LoadRecovery;

const SILENCE_BYTES: usize = 8_864;

fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 1e-4,
        "expected {expected}, got {actual}"
    );
}

fn speech_body(text: &str) -> serde_json::Value {
    json!({
        "text": text,
        "model_id": "eleven_multilingual_v2",
        "voice_settings": {"stability": 0.5, "speed": 1.2}
    })
}

fn assert_wav(resp: &TestResponse) {
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(resp.headers["content-type"], "audio/wav");
    assert!(resp.body.starts_with(b"RIFF"));
}

fn assert_silent_ok(resp: &TestResponse) {
    assert_wav(resp);
    assert_eq!(resp.error_header(), None);
    assert_eq!(resp.body.len(), SILENCE_BYTES);
}

/// A 503 with the given `detail.status` and no error header.
fn assert_503(resp: &TestResponse, code: &str) {
    assert_eq!(resp.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(resp.headers["content-type"], "application/json");
    assert_eq!(resp.error_header(), None);
    assert_eq!(resp.json()["detail"]["status"], code);
}

fn assert_400(resp: &TestResponse) {
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    assert_eq!(resp.json()["detail"]["status"], "invalid_request");
}

/// A degraded preview: 200 + silence + the error code in the header.
fn assert_degraded(resp: &TestResponse, code: &str) {
    assert_wav(resp);
    assert_eq!(resp.body.len(), SILENCE_BYTES);
    assert_eq!(resp.error_header(), Some(code));
}

// ---------------------------------------------------------------- speech

#[tokio::test]
async fn speak_returns_wav_from_synth() {
    let st = state(200);
    let app = test_app(st.clone());
    let resp = post_json(
        &app,
        "/v1/text-to-speech/en_US-joe-medium?output_format=mp3_22050_32",
        json!({"text":"Welcome aboard","model_id":"eleven_multilingual_v2","voice_settings":{"speed":1.2}}),
    )
    .await;
    assert_wav(&resp);
    assert_eq!(resp.error_header(), None);
    let calls = st.synth.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].voice_id, "en_US-joe-medium");
    assert_close(calls[0].speed, 1.2);
}

#[tokio::test]
async fn stream_route_is_identical() {
    let app = test_app(state(200));
    let plain = post_json(&app, "/v1/text-to-speech/M1", speech_body("Welcome aboard")).await;
    let stream = post_json(
        &app,
        "/v1/text-to-speech/M1/stream?output_format=mp3_22050_32",
        speech_body("Welcome aboard"),
    )
    .await;
    assert_wav(&stream);
    assert_eq!(stream.body, plain.body);
}

#[tokio::test]
async fn supertonic_speed_uses_base() {
    let st = state(200);
    let app = test_app(st.clone());
    let resp = post_json(&app, "/v1/text-to-speech/F3/stream", speech_body("Hello")).await;
    assert_wav(&resp);
    assert_close(st.synth.calls()[0].speed, 1.26);
}

#[tokio::test]
async fn speak_cleans_slc_text_before_synthesis() {
    let st = state(200);
    let app = test_app(st.clone());
    let resp = post_json(
        &app,
        "/v1/text-to-speech/F3/stream",
        speech_body("Cruising at FL350.\r\n"),
    )
    .await;
    assert_wav(&resp);
    assert_eq!(
        st.synth.calls()[0].prepared,
        Prepared::Text("Cruising at flight level three five zero.".to_string())
    );
}

#[tokio::test]
async fn whitespace_only_is_silent_200() {
    let st = state(200);
    let app = test_app(st.clone());
    let resp = post_json(&app, "/v1/text-to-speech/M1/stream", speech_body("  \r\n")).await;
    assert_silent_ok(&resp);
    assert!(st.synth.calls().is_empty());
}

#[tokio::test]
async fn dots_are_silent_200() {
    let st = state(200);
    let app = test_app(st.clone());
    let resp = post_json(&app, "/v1/text-to-speech/M1/stream", speech_body("...")).await;
    assert_silent_ok(&resp);
    assert!(st.synth.calls().is_empty());
}

#[tokio::test]
async fn nothing_to_say_from_prepare_is_silent_200() {
    let synth = FakeSynth {
        prepare_result: Some(Err(SynthError::NothingToSay)),
        ..Default::default()
    };
    let st = state_with(FakeStore::default(), synth, 200);
    let app = test_app(st.clone());
    let resp = post_json(
        &app,
        "/v1/text-to-speech/en_US-joe-medium/stream",
        speech_body("Hello"),
    )
    .await;
    assert_silent_ok(&resp);
    assert!(st.synth.calls().is_empty());
}

/// SLC's speech-rate slider may send 0 or less at its slow end. That plays
/// at the engine's slowest speed instead of failing the line.
#[tokio::test]
async fn zero_or_negative_speed_plays_at_the_slowest_speed() {
    for (voice, slowest) in [("en_US-joe-medium", 0.5), ("M1", 0.7)] {
        for speed in ["0", "-1", "-10"] {
            let st = state(200);
            let app = test_app(st.clone());
            let body = format!(r#"{{"text":"Hello","voice_settings":{{"speed":{speed}}}}}"#);
            let uri = format!("/v1/text-to-speech/{voice}/stream");
            let resp = post_json(&app, &uri, body).await;
            assert_wav(&resp);
            assert_close(st.synth.calls()[0].speed, slowest);
        }
    }
}

/// A number too big for JSON's f64 is malformed; SLC never sends one.
#[tokio::test]
async fn speed_too_big_to_parse_is_400() {
    let app = test_app(state(200));
    let body = r#"{"text":"Hello","voice_settings":{"speed":1e999}}"#;
    let resp = post_json(&app, "/v1/text-to-speech/M1/stream", body).await;
    assert_400(&resp);
}

#[tokio::test]
async fn too_long_text_is_400() {
    let app = test_app(state(200));
    let too_long = "a".repeat(5_001);
    let resp = post_json(&app, "/v1/text-to-speech/M1/stream", speech_body(&too_long)).await;
    assert_400(&resp);

    let at_limit = "a".repeat(5_000);
    let resp = post_json(&app, "/v1/text-to-speech/M1/stream", speech_body(&at_limit)).await;
    assert_wav(&resp);
}

#[tokio::test]
async fn bad_json_and_missing_text_are_400() {
    let app = test_app(state(200));
    let resp = post_json(&app, "/v1/text-to-speech/M1/stream", "{not json").await;
    assert_400(&resp);
    let resp = post_json(
        &app,
        "/v1/text-to-speech/M1/stream",
        json!({"model_id": "eleven_multilingual_v2"}),
    )
    .await;
    assert_400(&resp);
}

#[tokio::test]
async fn unknown_voice_is_503() {
    let app = test_app(state(200));
    let resp = post_json(&app, "/v1/text-to-speech/nobody/stream", speech_body("Hi")).await;
    assert_503(&resp, "voice_not_found");
}

#[tokio::test]
async fn download_failure_is_503() {
    let store = FakeStore {
        fail_with: Some("no internet".into()),
        ..Default::default()
    };
    let app = test_app(state_with(store, FakeSynth::default(), 200));
    let resp = post_json(&app, "/v1/text-to-speech/M1/stream", speech_body("Hi")).await;
    assert_503(&resp, "model_download_failed");
}

#[tokio::test]
async fn slow_download_is_503_budget_exceeded() {
    let store = FakeStore {
        delay: Duration::from_secs(1),
        ..Default::default()
    };
    let app = test_app(state_with(store, FakeSynth::default(), 200));
    let started = Instant::now();
    let resp = post_json(&app, "/v1/text-to-speech/M1/stream", speech_body("Hi")).await;
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_503(&resp, "budget_exceeded");
    let message = resp.json()["detail"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("still downloading"), "{message}");
}

#[tokio::test]
async fn slow_synthesis_is_503_and_cancels() {
    let synth = FakeSynth {
        delay: Duration::from_secs(1),
        ..Default::default()
    };
    let st = state_with(FakeStore::default(), synth, 200);
    let app = test_app(st.clone());
    let started = Instant::now();
    let resp = post_json(&app, "/v1/text-to-speech/M1/stream", speech_body("Hi")).await;
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_503(&resp, "budget_exceeded");
    let message = resp.json()["detail"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("too long"), "{message}");

    // FakeSynth records the call only once its sleep ends.
    assert!(eventually(|| st.synth.calls().len() == 1).await);
    assert!(st.synth.calls()[0].cancel.is_cancelled());
}

#[tokio::test]
async fn synth_failure_is_503() {
    let synth = FakeSynth {
        synth_result: Err(SynthError::Failed("boom".into())),
        ..Default::default()
    };
    let app = test_app(state_with(FakeStore::default(), synth, 200));
    let resp = post_json(&app, "/v1/text-to-speech/M1/stream", speech_body("Hi")).await;
    assert_503(&resp, "synthesis_failed");
}

#[tokio::test]
async fn synth_panic_is_503() {
    let synth = FakeSynth {
        panic_times: AtomicUsize::new(1),
        ..Default::default()
    };
    let app = test_app(state_with(FakeStore::default(), synth, 200));
    let resp = post_json(&app, "/v1/text-to-speech/M1/stream", speech_body("Hi")).await;
    assert_eq!(resp.status, StatusCode::SERVICE_UNAVAILABLE);
    let code = resp.json()["detail"]["status"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        code == "synthesis_failed" || code == "internal_error",
        "{code}"
    );

    let next = post_json(&app, "/v1/text-to-speech/M1/stream", speech_body("Hi")).await;
    assert_wav(&next);
    assert_eq!(next.error_header(), None);
}

#[tokio::test]
async fn load_failure_reports_to_store() {
    let synth = FakeSynth {
        synth_result: Err(SynthError::LoadFailed("onnx: bad file".into())),
        ..Default::default()
    };
    let st = state_with(FakeStore::default(), synth, 200);
    let app = test_app(st.clone());
    let resp = post_json(&app, "/v1/text-to-speech/M1/stream", speech_body("Hi")).await;
    assert_503(&resp, "model_load_failed");
    // The report runs on a blocking thread and may finish after the response.
    assert!(eventually(|| st.store.load_failure_reports.load(Ordering::SeqCst) == 1).await);
}

#[tokio::test]
async fn load_failure_is_not_retried_for_five_minutes() {
    let synth = FakeSynth {
        synth_result: Err(SynthError::LoadFailed("onnx: bad file".into())),
        ..Default::default()
    };
    let st = state_with(FakeStore::default(), synth, 200);
    let app = test_app(st.clone());
    let first = post_json(&app, "/v1/text-to-speech/M1/stream", speech_body("Hi")).await;
    assert_503(&first, "model_load_failed");
    let second = post_json(&app, "/v1/text-to-speech/M1/stream", speech_body("Hi")).await;
    assert_503(&second, "model_load_failed");

    assert_eq!(
        st.synth.calls().len(),
        1,
        "second request reached synthesize"
    );
    assert!(eventually(|| st.store.load_failure_reports.load(Ordering::SeqCst) >= 1).await);
    // Give a stray second report time to show up before checking the count.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(st.store.load_failure_reports.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn queued_request_respects_cooldown() {
    // The first request holds the synthesis lock for 200 ms and then fails to
    // load; the second has passed the first cooldown check and is waiting
    // for the lock by then.
    let synth = FakeSynth {
        synth_result: Err(SynthError::LoadFailed("onnx: bad file".into())),
        delay: Duration::from_millis(200),
        ..Default::default()
    };
    let st = state_with(FakeStore::default(), synth, 2_000);
    let app = test_app(st.clone());
    let uri = "/v1/text-to-speech/M1/stream";
    let (a, b) = tokio::join!(
        post_json(&app, uri, speech_body("One")),
        post_json(&app, uri, speech_body("Two")),
    );
    assert_503(&a, "model_load_failed");
    assert_503(&b, "model_load_failed");
    assert_eq!(
        st.synth.calls().len(),
        1,
        "the queued request loaded the model"
    );
    assert!(eventually(|| st.store.load_failure_reports.load(Ordering::SeqCst) >= 1).await);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(st.store.load_failure_reports.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn quarantine_clears_cooldown() {
    let store = FakeStore {
        recovery: Some(LoadRecovery::Quarantined),
        ..Default::default()
    };
    let synth = FakeSynth {
        synth_result: Err(SynthError::LoadFailed("onnx: bad file".into())),
        ..Default::default()
    };
    let st = state_with(store, synth, 200);
    let app = test_app(st.clone());
    let uri = "/v1/text-to-speech/M1/stream";
    let first = post_json(&app, uri, speech_body("Hi")).await;
    assert_503(&first, "model_load_failed");
    assert!(eventually(|| st.store.load_failure_reports.load(Ordering::SeqCst) == 1).await);
    // The cooldown is cleared just after the report returns, so retry for
    // up to 2 s until a request gets through to `synthesize` again.
    let deadline = Instant::now() + Duration::from_secs(2);
    while st.synth.calls().len() < 2 && Instant::now() < deadline {
        post_json(&app, uri, speech_body("Hi")).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        st.synth.calls().len(),
        2,
        "the retry after quarantine was blocked"
    );
}

#[tokio::test]
async fn handler_panic_on_speech_is_503_internal_error() {
    let store = FakeStore {
        panic_times: AtomicUsize::new(1),
        ..Default::default()
    };
    let app = test_app(state_with(store, FakeSynth::default(), 200));
    let resp = post_json(&app, "/v1/text-to-speech/M1/stream", speech_body("Hi")).await;
    assert_503(&resp, "internal_error");

    let next = post_json(&app, "/v1/text-to-speech/M1/stream", speech_body("Hi")).await;
    assert_wav(&next);
}

#[tokio::test]
async fn handler_panic_on_preview_is_degraded() {
    let store = FakeStore {
        panic_times: AtomicUsize::new(1),
        ..Default::default()
    };
    let app = test_app(state_with(store, FakeSynth::default(), 200));
    let resp = get(&app, "/v1/voices/M1/preview", HOST).await;
    assert_degraded(&resp, "internal_error");
}

#[tokio::test]
async fn foreign_host_speech_is_403() {
    let st = state(200);
    let app = test_app(st.clone());
    let request = Request::post("/v1/text-to-speech/M1/stream")
        .header("host", "evil.example")
        .header("content-type", "application/json; charset=utf-8")
        .body(Body::from(speech_body("Hi").to_string()))
        .unwrap();
    let resp = send(&app, request).await;
    assert_eq!(resp.status, StatusCode::FORBIDDEN);
    assert_eq!(resp.json()["detail"]["status"], "forbidden_host");
    assert_eq!(st.store.ensure_calls.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------- preview

#[tokio::test]
async fn preview_uses_base_speed_and_fixed_text() {
    let st = state(200);
    let app = test_app(st.clone());
    // `get` sends no `xi-api-key`, like SLC's preview button.
    let resp = get(&app, "/v1/voices/M3/preview", HOST).await;
    assert_wav(&resp);
    assert_eq!(resp.error_header(), None);
    let calls = st.synth.calls();
    assert_close(calls[0].speed, 1.05);
    assert_eq!(calls[0].prepared, Prepared::Text(PREVIEW_TEXT.to_string()));
}

#[tokio::test]
async fn preview_of_unknown_voice_is_degraded() {
    let app = test_app(state(200));
    let resp = get(&app, "/v1/voices/nobody/preview", HOST).await;
    assert_degraded(&resp, "voice_not_found");
}

#[tokio::test]
async fn preview_download_failure_is_degraded() {
    let store = FakeStore {
        fail_with: Some("no internet".into()),
        ..Default::default()
    };
    let app = test_app(state_with(store, FakeSynth::default(), 200));
    let resp = get(&app, "/v1/voices/M1/preview", HOST).await;
    assert_degraded(&resp, "model_download_failed");
}

#[tokio::test]
async fn preview_waits_longer_than_speech() {
    let store = FakeStore {
        delay: Duration::from_millis(600),
        ..Default::default()
    };
    let app = test_app(state_with_budgets(store, FakeSynth::default(), 200, 2_000));
    let speech = post_json(&app, "/v1/text-to-speech/M1/stream", speech_body("Hi")).await;
    assert_503(&speech, "budget_exceeded");

    let preview = get(&app, "/v1/voices/M1/preview", HOST).await;
    assert_wav(&preview);
    assert_eq!(preview.error_header(), None);
}

// ---------------------------------------------------------------- concurrency

#[tokio::test]
async fn one_synthesis_at_a_time() {
    let synth = FakeSynth {
        delay: Duration::from_millis(100),
        ..Default::default()
    };
    let st = state_with(FakeStore::default(), synth, 2_000);
    let app = test_app(st.clone());
    let uri = "/v1/text-to-speech/M1/stream";
    let (a, b, c) = tokio::join!(
        post_json(&app, uri, speech_body("One")),
        post_json(&app, uri, speech_body("Two")),
        post_json(&app, uri, speech_body("Three")),
    );
    for resp in [&a, &b, &c] {
        assert_wav(resp);
    }
    assert_eq!(st.synth.calls().len(), 3);
    assert_eq!(st.synth.max_concurrent.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dropped_request_keeps_lock_until_native_work_ends() {
    let synth = FakeSynth {
        delay: Duration::from_millis(300),
        ..Default::default()
    };
    let st = state_with(FakeStore::default(), synth, 2_000);
    let app = test_app(st.clone());
    let uri = "/v1/text-to-speech/M1/stream";

    // Request A gives up after 50 ms: `timeout` drops its future, as hyper
    // does when the client disconnects.
    let a = tokio::time::timeout(
        Duration::from_millis(50),
        post_json(&app, uri, speech_body("First")),
    )
    .await;
    assert!(a.is_err(), "request A should still have been running");

    tokio::time::sleep(Duration::from_millis(10)).await;
    let b = post_json(&app, uri, speech_body("Second")).await;
    assert_wav(&b);

    let mut calls = st.synth.calls();
    assert_eq!(calls.len(), 2);
    calls.sort_by_key(|call| call.started);
    let (first, second) = (&calls[0], &calls[1]);
    assert_eq!(first.prepared, Prepared::Text("First".to_string()));
    assert!(
        second.started >= first.ended,
        "B's synthesis began before A's ended"
    );
    assert!(first.cancel.is_cancelled());
}

// ---------------------------------------------------------------- logging

#[tokio::test(flavor = "current_thread")]
async fn api_key_is_never_logged() {
    let (logs, _capture) = capture_logs();
    let app = test_app(state(200));
    let resp = post_json_with(
        &app,
        "/v1/text-to-speech/M1/stream",
        speech_body("Welcome aboard"),
        &[("xi-api-key", "sk-test-secret-123")],
    )
    .await;
    assert_wav(&resp);
    let preview = get_with(
        &app,
        "/v1/voices/M1/preview",
        HOST,
        &[("xi-api-key", "sk-test-secret-123")],
    )
    .await;
    assert_wav(&preview);

    let output = logs.contents();
    // The capture works: the per-request line was recorded.
    assert!(output.contains("outcome"), "{output}");
    assert!(!output.contains("sk-test-secret-123"), "{output}");
}

/// SLC's speech-rate slider (-10 to 10) becomes `voice_settings.speed`; the
/// per-request line shows the value exactly as SLC sent it.
#[tokio::test(flavor = "current_thread")]
async fn speech_log_line_shows_slcs_speed() {
    let (logs, _capture) = capture_logs();
    let app = test_app(state(200));
    let body = json!({"text": "Welcome aboard", "voice_settings": {"speed": 0.85}});
    let resp = post_json(&app, "/v1/text-to-speech/M1/stream", body).await;
    assert_wav(&resp);

    let output = logs.contents();
    let line = output
        .lines()
        .find(|line| line.contains("outcome"))
        .unwrap_or_else(|| panic!("no per-request line in:\n{output}"));
    assert!(line.contains("speed=0.85"), "{line}");
}

/// A request the server rejects is logged with its reason, which names the
/// bad value SLC sent, but never the line's text.
#[tokio::test(flavor = "current_thread")]
async fn rejected_request_is_logged_with_its_reason() {
    let (logs, _capture) = capture_logs();
    let app = test_app(state(200));
    let body = r#"{"text":"Hello there","voice_settings":{"speed":"fast"}}"#;
    let resp = post_json(&app, "/v1/text-to-speech/M1/stream", body).await;
    assert_400(&resp);

    let output = logs.contents();
    assert!(output.contains("Voice M1"), "{output}");
    assert!(output.contains("fast"), "{output}");
    assert!(!output.contains("Hello there"), "{output}");
}
