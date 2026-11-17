//! The private `/slc/` routes the settings window reads: the client guard,
//! the status, the voice models, downloads, deletes and the request records.

mod common;

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower_http::normalize_path::NormalizePath;

use common::{
    FakeStore, FakeSynth, HOST, TestResponse, capture_logs_at, get, get_with, post_json, send,
    slc_delete, slc_get, slc_post, slc_put_json, state, state_with, state_with_app_info, test_app,
    test_app_info,
};
use slc_local_tts::api::AppInfo;
use slc_local_tts::api::slc::{CLIENT_HEADER, CLIENT_VALUE};
use slc_local_tts::catalog::{self, Voice};
use slc_local_tts::models::{ModelState, StartDownload, StoreError};
use slc_local_tts::stop::{StopReason, StopSignal};
use slc_local_tts::update::UpdateState;

const SPEECH_URI: &str = "/v1/text-to-speech/M1/stream";

fn voice(id: &str) -> &'static Voice {
    catalog::find(id).unwrap()
}

/// A Kokoro voice; every Kokoro voice shares one archive.
fn kokoro() -> &'static Voice {
    voice("af_alloy")
}

/// A Supertonic voice.
fn supertonic() -> &'static Voice {
    voice("M1")
}

fn speech_body(text: &str) -> serde_json::Value {
    json!({
        "text": text,
        "model_id": "eleven_multilingual_v2",
        "voice_settings": {"speed": 1.0}
    })
}

fn assert_forbidden_client(resp: &TestResponse) {
    assert_eq!(resp.status, StatusCode::FORBIDDEN);
    assert_eq!(resp.json()["detail"]["status"], "forbidden_client");
}

/// A store whose Kokoro archive is 161 MB into its download.
fn kokoro_downloading() -> FakeStore {
    let store = FakeStore::default();
    store.states.lock().unwrap().insert(
        kokoro().archive.stem,
        ModelState::Downloading {
            done: 161_000_000,
            total: kokoro().archive.size,
        },
    );
    store
}

// ---------------------------------------------------------------- the guard

#[tokio::test]
async fn slc_routes_need_client_header() {
    let app = test_app(state(2_000));
    for uri in ["/slc/status", "/slc/voices", "/slc/requests?after=0"] {
        assert_forbidden_client(&get(&app, uri, HOST).await);
        let wrong = get_with(&app, uri, HOST, &[(CLIENT_HEADER, "browser")]).await;
        assert_forbidden_client(&wrong);
    }
}

#[tokio::test]
async fn volume_put_needs_client_header() {
    let app = test_app(state(2_000));
    let put = |client: Option<&str>| {
        let mut builder = Request::put("/slc/settings/volume")
            .header("host", HOST)
            .header("content-type", "application/json");
        if let Some(value) = client {
            builder = builder.header(CLIENT_HEADER, value);
        }
        builder
            .body(Body::from(json!({"percent": 150}).to_string()))
            .unwrap()
    };
    assert_forbidden_client(&send(&app, put(None)).await);
    assert_forbidden_client(&send(&app, put(Some("browser"))).await);
    let v = slc_get(&app, "/slc/status").await.json();
    assert_eq!(v["config"]["volume_percent"], 100, "{v}");
}

#[tokio::test]
async fn slc_routes_refuse_origin() {
    let app = test_app(state(2_000));
    let resp = get_with(
        &app,
        "/slc/status",
        HOST,
        &[
            (CLIENT_HEADER, CLIENT_VALUE),
            ("origin", "http://evil.example"),
        ],
    )
    .await;
    assert_forbidden_client(&resp);
}

#[tokio::test]
async fn slc_preflight_refused() {
    let app = test_app(state(2_000));
    let request = Request::options("/slc/status")
        .header("host", HOST)
        .header("origin", "http://evil.example")
        .header("access-control-request-method", "GET")
        .header("access-control-request-headers", CLIENT_HEADER)
        .body(Body::empty())
        .unwrap();
    let resp = send(&app, request).await;
    assert_forbidden_client(&resp);
    assert!(
        resp.headers.get("access-control-allow-origin").is_none(),
        "a preflight must never be approved"
    );
}

#[tokio::test]
async fn slc_wrong_method_is_405_and_unknown_path_404() {
    let app = test_app(state(2_000));
    let wrong_method = slc_delete(&app, "/slc/status").await;
    assert_eq!(wrong_method.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        wrong_method.json()["detail"]["status"],
        "method_not_allowed"
    );

    let unknown = slc_get(&app, "/slc/nothing-here").await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.json()["detail"]["status"], "not_found");
}

#[tokio::test]
async fn slc_foreign_host_403() {
    let app = test_app(state(2_000));
    let resp = get_with(
        &app,
        "/slc/status",
        "evil.example:8765",
        &[(CLIENT_HEADER, CLIENT_VALUE)],
    )
    .await;
    assert_eq!(resp.status, StatusCode::FORBIDDEN);
    assert_eq!(resp.json()["detail"]["status"], "forbidden_host");
}

// ---------------------------------------------------------------- status

#[tokio::test]
async fn status_shape() {
    let app = test_app(state(2_000));
    let resp = slc_get(&app, "/slc/status").await;
    assert_eq!(resp.status, StatusCode::OK);
    let v = resp.json();
    assert_eq!(v["app"], "SLC Local TTS");
    assert_eq!(v["version"], "0.0.0-test");
    assert_eq!(v["port"], 8765);
    assert_eq!(v["state"]["state"], "idle");
    assert!(v["last_request_ms"].is_null(), "{v}");
    assert_eq!(v["ever_connected"], false);
    assert_eq!(v["recent_failures"], 0);
    assert_eq!(v["failed_downloads"], 0);
    assert_eq!(v["downloads"], json!([]));
    assert_eq!(v["latest_seq"], 0);
    assert_eq!(v["update"]["state"], "not_installed");
    assert_eq!(v["can_restart_to_update"], false);
    assert!(v["updated_to"].is_null(), "{v}");
    assert_eq!(v["config"]["port"], 8765);
}

#[tokio::test]
async fn status_reports_speaking_then_downloading() {
    let synth = FakeSynth {
        delay: Duration::from_millis(400),
        ..Default::default()
    };
    let st = state_with(kokoro_downloading(), synth, 2_000);
    let app = test_app(st.clone());

    let speech_app = app.clone();
    let speech =
        tokio::spawn(async move { post_json(&speech_app, SPEECH_URI, speech_body("Hello")).await });
    assert!(common::eventually(|| st.live.snapshot().speaking.is_some()).await);

    // Speaking wins over the download.
    let v = slc_get(&app, "/slc/status").await.json();
    assert_eq!(v["state"]["state"], "speaking", "{v}");
    assert_eq!(v["state"]["voice_id"], "M1");
    assert_eq!(v["state"]["chars"], 5);
    assert!(v["state"]["started_ms"].as_u64().unwrap() > 0);
    assert!(v["last_request_ms"].as_u64().unwrap() > 0);
    assert_eq!(v["downloads"][0]["stem"], kokoro().archive.stem);

    assert_eq!(speech.await.unwrap().status, StatusCode::OK);

    let v = slc_get(&app, "/slc/status").await.json();
    assert_eq!(v["state"]["state"], "downloading", "{v}");
    assert_eq!(v["state"]["stem"], kokoro().archive.stem);
    assert_eq!(v["state"]["done"], 161_000_000);
    assert_eq!(v["state"]["total"], kokoro().archive.size);
    assert_eq!(v["ever_connected"], true);
}

#[tokio::test]
async fn status_counts_recent_failures() {
    let store = FakeStore {
        fail_with: Some("no internet".to_string()),
        ..Default::default()
    };
    let st = state_with(store, FakeSynth::default(), 2_000);
    let app = test_app(st);

    let speech = post_json(&app, SPEECH_URI, speech_body("Hello")).await;
    assert_eq!(speech.status, StatusCode::SERVICE_UNAVAILABLE);

    let v = slc_get(&app, "/slc/status").await.json();
    assert_eq!(v["recent_failures"], 1, "{v}");
    assert_eq!(v["latest_seq"], 1);
}

// ---------------------------------------------------------------- voices

#[tokio::test]
async fn voices_groups_by_archive_with_progress() {
    let store = kokoro_downloading();
    store
        .installed
        .lock()
        .unwrap()
        .insert(supertonic().archive.stem);
    let app = test_app(state_with(store, FakeSynth::default(), 2_000));

    let resp = slc_get(&app, "/slc/voices").await;
    assert_eq!(resp.status, StatusCode::OK);
    let v = resp.json();
    let entries = v.as_array().unwrap();

    let archives: HashSet<&str> = catalog::voices().iter().map(|v| v.archive.stem).collect();
    assert_eq!(entries.len(), archives.len());

    let entry = |stem: &str| {
        entries
            .iter()
            .find(|entry| entry["stem"] == stem)
            .unwrap_or_else(|| panic!("no entry for {stem}"))
            .clone()
    };
    let kokoro_entry = entry(kokoro().archive.stem);
    assert_eq!(kokoro_entry["engine"], "kokoro");
    assert_eq!(kokoro_entry["size"], kokoro().archive.size);
    assert_eq!(kokoro_entry["state"], "downloading");
    assert_eq!(kokoro_entry["done"], 161_000_000);
    assert_eq!(kokoro_entry["total"], kokoro().archive.size);
    assert_eq!(kokoro_entry["voices"].as_array().unwrap().len(), 28);
    let alloy = kokoro_entry["voices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|voice| voice["id"] == "af_alloy")
        .unwrap();
    assert_eq!(alloy["slow"], true);
    assert_eq!(alloy["name"], kokoro().name);
    assert_eq!(alloy["license"], kokoro().license);

    let supertonic_entry = entry(supertonic().archive.stem);
    assert_eq!(supertonic_entry["engine"], "supertonic");
    assert_eq!(supertonic_entry["state"], "installed");
    assert_eq!(supertonic_entry["voices"].as_array().unwrap().len(), 10);

    // Catalog order: the first entry is the first voice's archive.
    assert_eq!(entries[0]["stem"], catalog::voices()[0].archive.stem);
}

// ---------------------------------------------------------------- download

#[tokio::test]
async fn download_returns_202_and_starts() {
    let st = state(2_000);
    let app = test_app(st.clone());
    let uri = format!("/slc/models/{}/download", kokoro().archive.stem);

    let resp = slc_post(&app, &uri).await;

    assert_eq!(resp.status, StatusCode::ACCEPTED);
    assert_eq!(st.store.start_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn download_already_running_or_installed_is_202() {
    for result in [
        StartDownload::AlreadyDownloading,
        StartDownload::AlreadyInstalled,
    ] {
        let store = FakeStore {
            start_result: Some(result),
            ..Default::default()
        };
        let app = test_app(state_with(store, FakeSynth::default(), 2_000));
        let uri = format!("/slc/models/{}/download", kokoro().archive.stem);
        assert_eq!(slc_post(&app, &uri).await.status, StatusCode::ACCEPTED);
    }
}

#[tokio::test]
async fn download_unknown_stem_is_404() {
    let st = state(2_000);
    let app = test_app(st.clone());

    let resp = slc_post(&app, "/slc/models/no-such-model/download").await;

    assert_eq!(resp.status, StatusCode::NOT_FOUND);
    let v = resp.json();
    assert_eq!(v["detail"]["status"], "model_not_found");
    assert_eq!(
        v["detail"]["message"],
        "There's no voice download called \"no-such-model\"."
    );
    assert_eq!(st.store.start_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn download_refused_is_409() {
    let store = FakeStore {
        start_result: Some(StartDownload::Refused(StoreError {
            reason: "the file on the server has changed".to_string(),
        })),
        ..Default::default()
    };
    let app = test_app(state_with(store, FakeSynth::default(), 2_000));
    let uri = format!("/slc/models/{}/download", kokoro().archive.stem);

    let resp = slc_post(&app, &uri).await;

    assert_eq!(resp.status, StatusCode::CONFLICT);
    let v = resp.json();
    assert_eq!(v["detail"]["status"], "download_refused");
    assert_eq!(
        v["detail"]["message"],
        "This voice can't be downloaded: the file on the server has changed."
    );
}

// ---------------------------------------------------------------- delete

#[tokio::test]
async fn delete_returns_204() {
    let st = state_with(
        FakeStore::with_installed(supertonic()),
        FakeSynth::default(),
        2_000,
    );
    let app = test_app(st.clone());
    let uri = format!("/slc/models/{}", supertonic().archive.stem);

    let resp = slc_delete(&app, &uri).await;

    assert_eq!(resp.status, StatusCode::NO_CONTENT);
    assert_eq!(st.store.delete_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *st.synth.unloads.lock().unwrap(),
        [supertonic().archive.stem]
    );

    // Deleting what isn't installed is fine too.
    assert_eq!(slc_delete(&app, &uri).await.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_while_downloading_is_409() {
    let st = state_with(kokoro_downloading(), FakeSynth::default(), 2_000);
    let app = test_app(st.clone());
    let uri = format!("/slc/models/{}", kokoro().archive.stem);

    let resp = slc_delete(&app, &uri).await;

    assert_eq!(resp.status, StatusCode::CONFLICT);
    let v = resp.json();
    assert_eq!(v["detail"]["status"], "model_busy");
    assert_eq!(
        v["detail"]["message"],
        "This voice is downloading. Wait for it to finish, then delete it."
    );
    assert_eq!(st.store.delete_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn delete_unknown_stem_is_404() {
    let app = test_app(state(2_000));
    let resp = slc_delete(&app, "/slc/models/no-such-model").await;
    assert_eq!(resp.status, StatusCode::NOT_FOUND);
    assert_eq!(resp.json()["detail"]["status"], "model_not_found");
}

#[tokio::test]
async fn delete_io_error_is_500_with_its_text() {
    let store = FakeStore {
        delete_error: Some(slc_local_tts::models::DeleteError::Io(
            "access denied".to_string(),
        )),
        ..FakeStore::with_installed(supertonic())
    };
    let app = test_app(state_with(store, FakeSynth::default(), 2_000));
    let uri = format!("/slc/models/{}", supertonic().archive.stem);

    let resp = slc_delete(&app, &uri).await;

    assert_eq!(resp.status, StatusCode::INTERNAL_SERVER_ERROR);
    let v = resp.json();
    assert_eq!(v["detail"]["status"], "internal_error");
    assert_eq!(
        v["detail"]["message"],
        "The voice's files couldn't be deleted: access denied. Close any program using its \
         folder and try again."
    );
}

// ---------------------------------------------------------------- requests

#[tokio::test]
async fn requests_after_seq() {
    let app = test_app(state(2_000));
    for text in ["One", "Two"] {
        let resp = post_json(&app, SPEECH_URI, speech_body(text)).await;
        assert_eq!(resp.status, StatusCode::OK);
    }

    let all = slc_get(&app, "/slc/requests?after=0").await.json();
    assert_eq!(all["records"].as_array().unwrap().len(), 2, "{all}");
    assert_eq!(all["latest_seq"], 2);
    assert_eq!(all["records"][0]["text"], "One");
    assert_eq!(all["records"][0]["route"], "speech");

    let newer = slc_get(&app, "/slc/requests?after=1").await.json();
    assert_eq!(newer["records"].as_array().unwrap().len(), 1);
    assert_eq!(newer["records"][0]["seq"], 2);
    assert_eq!(newer["latest_seq"], 2);

    let none = slc_get(&app, "/slc/requests?after=2").await.json();
    assert_eq!(none["records"], json!([]));
    assert_eq!(none["latest_seq"], 2);
}

#[tokio::test]
async fn window_preview_is_recorded_but_not_slc() {
    let st = state(2_000);
    let app = test_app(st.clone());

    let preview = slc_get(&app, "/v1/voices/M1/preview").await;
    assert_eq!(preview.status, StatusCode::OK);

    let records = slc_get(&app, "/slc/requests?after=0").await.json();
    assert_eq!(records["records"].as_array().unwrap().len(), 1);
    assert_eq!(records["records"][0]["route"], "preview");
    let v = slc_get(&app, "/slc/status").await.json();
    assert!(v["last_request_ms"].is_null(), "{v}");
    assert_eq!(v["ever_connected"], false);

    // The same preview from SLC (no client header) counts as SLC's.
    let from_slc = get(&app, "/v1/voices/M1/preview", HOST).await;
    assert_eq!(from_slc.status, StatusCode::OK);
    let v = slc_get(&app, "/slc/status").await.json();
    assert!(v["last_request_ms"].as_u64().is_some(), "{v}");
}

// ---------------------------------------------------------------- logging

#[tokio::test]
async fn polling_does_not_log_at_info() {
    let app = test_app(state(2_000));
    let (logs, _capture) = capture_logs_at(tracing::Level::INFO);

    for _ in 0..3 {
        assert_eq!(slc_get(&app, "/slc/status").await.status, StatusCode::OK);
    }
    let polled = logs.contents();
    assert!(!polled.contains("/slc/status"), "{polled}");

    // A refused request is still worth a line.
    assert_eq!(
        get(&app, "/slc/status", HOST).await.status,
        StatusCode::FORBIDDEN
    );
    let refused = logs.contents();
    let line = refused
        .lines()
        .find(|line| line.contains("/slc/status"))
        .unwrap_or_else(|| panic!("no line for the refused request: {refused}"));
    assert!(line.contains("WARN"), "{line}");
    assert!(line.contains("403"), "{line}");
}

// ---------------------------------------------------------------- volume

const VOLUME_URI: &str = "/slc/settings/volume";

/// The `config.volume_percent` the status route reports.
async fn status_volume(app: &NormalizePath<axum::Router>) -> serde_json::Value {
    slc_get(app, "/slc/status").await.json()["config"]["volume_percent"].clone()
}

#[tokio::test]
async fn volume_put_changes_live_volume() {
    let app = test_app(state(2_000));
    let resp = slc_put_json(&app, VOLUME_URI, json!({"percent": 150})).await;
    assert_eq!(resp.status, StatusCode::NO_CONTENT);
    assert_eq!(status_volume(&app).await, 150);
}

#[tokio::test]
async fn volume_put_out_of_range_is_400() {
    let app = test_app(state(2_000));
    for percent in [49, 201] {
        let resp = slc_put_json(&app, VOLUME_URI, json!({"percent": percent})).await;
        assert_eq!(resp.status, StatusCode::BAD_REQUEST, "{percent}");
    }
    assert_eq!(status_volume(&app).await, 100);
}

#[tokio::test(flavor = "current_thread")]
async fn volume_put_logs_at_info() {
    let app = test_app(state(2_000));
    let (logs, _capture) = capture_logs_at(tracing::Level::INFO);
    let resp = slc_put_json(&app, VOLUME_URI, json!({"percent": 150})).await;
    assert_eq!(resp.status, StatusCode::NO_CONTENT);
    let contents = logs.contents();
    let lines: Vec<&str> = contents
        .lines()
        .filter(|line| line.contains("INFO") && line.contains("150"))
        .collect();
    assert_eq!(lines.len(), 1, "{contents}");
}

// ---------------------------------------------------------------- update

/// The app info of a server whose update check is at `update`, in a
/// program that can (or can't) restart to install it, stopping through
/// `stop`.
fn update_info(update: UpdateState, can_restart: bool, stop: &StopSignal) -> AppInfo {
    AppInfo {
        updates: tokio::sync::watch::channel(update).1,
        can_restart_to_update: can_restart,
        stop: stop.clone(),
        ..test_app_info()
    }
}

fn ready_140() -> UpdateState {
    UpdateState::Ready {
        version: "1.4.0".to_string(),
    }
}

fn assert_no_update(resp: &TestResponse) {
    assert_eq!(resp.status, StatusCode::CONFLICT);
    let v = resp.json();
    assert_eq!(v["detail"]["status"], "no_update");
    assert_eq!(v["detail"]["message"], "No update is ready to install.");
}

#[tokio::test]
async fn apply_route_requests_restart_when_ready() {
    let stop = StopSignal::new();
    let app = test_app(state_with_app_info(update_info(ready_140(), true, &stop)));

    let resp = slc_post(&app, "/slc/update/apply").await;

    assert_eq!(resp.status, StatusCode::ACCEPTED);
    assert_eq!(stop.requested(), Some(StopReason::RestartToUpdate));
}

#[tokio::test]
async fn apply_route_409_without_update() {
    let stop = StopSignal::new();
    for update in [
        UpdateState::NotInstalled,
        UpdateState::Off,
        UpdateState::UpToDate,
        UpdateState::Downloading,
        UpdateState::Failed {
            reason: "the update server couldn't be reached (timed out)".to_string(),
        },
    ] {
        let app = test_app(state_with_app_info(update_info(update, true, &stop)));
        assert_no_update(&slc_post(&app, "/slc/update/apply").await);
    }
    assert_eq!(stop.requested(), None);
}

#[tokio::test]
async fn apply_route_409_under_cli() {
    let stop = StopSignal::new();
    let app = test_app(state_with_app_info(update_info(ready_140(), false, &stop)));

    assert_no_update(&slc_post(&app, "/slc/update/apply").await);
    assert_eq!(stop.requested(), None);
}

#[tokio::test]
async fn apply_route_needs_client_header() {
    let stop = StopSignal::new();
    let app = test_app(state_with_app_info(update_info(ready_140(), true, &stop)));

    let resp = post_json(&app, "/slc/update/apply", json!({})).await;

    assert_forbidden_client(&resp);
    assert_eq!(stop.requested(), None);
}
