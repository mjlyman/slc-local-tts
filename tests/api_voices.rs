//! Routing, the host guard and the ElevenLabs-shaped voice list.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;

use common::{FakeStore, FakeSynth, HOST, get, send, state, state_with, test_app};
use slc_local_tts::catalog;

#[tokio::test]
async fn v2_voices_snapshot() {
    let app = test_app(state(200));
    let resp = get(&app, "/v2/voices", HOST).await;
    assert_eq!(resp.status, StatusCode::OK);
    insta::assert_json_snapshot!(resp.json(), {".voices[].preview_url" => "[url]"});
}

#[tokio::test]
async fn v1_and_v2_lists_match_and_ignore_query() {
    let app = test_app(state(200));
    let plain = get(&app, "/v2/voices", HOST).await;
    let v1 = get(&app, "/v1/voices?showlegacy=true", HOST).await;
    let v2 = get(
        &app,
        "/v2/voices?showlegacy=true&voice_type=non-default",
        HOST,
    )
    .await;
    assert_eq!(plain.status, StatusCode::OK);
    assert_eq!(v1.json(), plain.json());
    assert_eq!(v2.json(), plain.json());
}

#[tokio::test]
async fn list_wrapper_fields() {
    let app = test_app(state(200));
    let body = get(&app, "/v1/voices", HOST).await.json();
    assert_eq!(body["has_more"], false);
    assert_eq!(body["total_count"], catalog::voices().len());
    assert_eq!(body["next_page_token"], serde_json::Value::Null);
    assert_eq!(
        body["voices"].as_array().unwrap().len(),
        catalog::voices().len()
    );
}

fn voice_in<'a>(list: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    list["voices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["voice_id"] == id)
        .unwrap_or_else(|| panic!("{id} not in list"))
}

#[tokio::test]
async fn joe_object_shape() {
    let app = test_app(state(200));
    let list = get(&app, "/v1/voices", HOST).await.json();
    let joe = voice_in(&list, "en_US-joe-medium");
    assert_eq!(joe["category"], "premade");
    assert_eq!(joe["name"], "Joe (US, medium, Piper)");
    assert_eq!(
        joe["labels"],
        json!({"engine": "piper", "gender": "male", "accent": "american", "status": "downloadable"})
    );
    assert_eq!(
        joe["preview_url"],
        "http://127.0.0.1:8765/v1/voices/en_US-joe-medium/preview"
    );
    assert_eq!(joe["settings"]["speed"], 1.0);
}

#[tokio::test]
async fn supertonic_labels_omit_accent() {
    let app = test_app(state(200));
    let list = get(&app, "/v1/voices", HOST).await.json();
    let labels = &voice_in(&list, "M3")["labels"];
    assert!(labels.get("accent").is_none(), "{labels}");
    assert_eq!(labels["gender"], "male");
    assert_eq!(labels["engine"], "supertonic");
}

#[tokio::test]
async fn installed_status_follows_store() {
    let joe = catalog::find("en_US-joe-medium").unwrap();
    let app = test_app(state_with(
        FakeStore::with_installed(joe),
        FakeSynth::default(),
        200,
    ));
    let list = get(&app, "/v1/voices", HOST).await.json();
    assert_eq!(
        voice_in(&list, "en_US-joe-medium")["labels"]["status"],
        "installed"
    );
    assert_eq!(voice_in(&list, "M3")["labels"]["status"], "downloadable");
}

#[tokio::test]
async fn single_voice_and_unknown_voice() {
    let app = test_app(state(200));
    let m1 = get(&app, "/v1/voices/m1", HOST).await;
    assert_eq!(m1.status, StatusCode::OK);
    assert_eq!(m1.json()["voice_id"], "M1");

    let nope = get(&app, "/v1/voices/nope", HOST).await;
    assert_eq!(nope.status, StatusCode::NOT_FOUND);
    assert_eq!(nope.json()["detail"]["status"], "voice_not_found");
}

#[tokio::test]
async fn unknown_route_is_json_404() {
    let app = test_app(state(200));
    let resp = get(&app, "/v1/models", HOST).await;
    assert_eq!(resp.status, StatusCode::NOT_FOUND);
    assert_eq!(resp.json()["detail"]["status"], "not_found");
}

#[tokio::test]
async fn wrong_method_is_json_405() {
    let app = test_app(state(200));
    let request = Request::delete("/v1/voices")
        .header("host", HOST)
        .body(Body::empty())
        .unwrap();
    let resp = send(&app, request).await;
    assert_eq!(resp.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(resp.json()["detail"]["status"], "method_not_allowed");
}

#[tokio::test]
async fn foreign_host_is_403() {
    let app = test_app(state(200));
    let resp = get(&app, "/v1/voices", "evil.example:8765").await;
    assert_eq!(resp.status, StatusCode::FORBIDDEN);
    assert_eq!(resp.json()["detail"]["status"], "forbidden_host");
}

#[tokio::test]
async fn host_guard_edge_cases() {
    let app = test_app(state(200));
    // Wrong or empty port (another local service), or an empty Host.
    for host in [
        "127.0.0.1:9999",
        "localhost:80",
        "[::1]:1",
        "127.0.0.1:",
        "",
    ] {
        let resp = get(&app, "/health", host).await;
        assert_eq!(resp.status, StatusCode::FORBIDDEN, "{host:?}");
    }
    // Lookalikes that merely start or end with an allowed name.
    for host in ["localhost.evil.example:8765", "127.0.0.1.evil:8765"] {
        let resp = get(&app, "/health", host).await;
        assert_eq!(resp.status, StatusCode::FORBIDDEN, "{host:?}");
    }
    // No Host header at all.
    let request = Request::get("/health").body(Body::empty()).unwrap();
    assert_eq!(send(&app, request).await.status, StatusCode::FORBIDDEN);
    // The guard also covers unknown routes.
    let resp = get(&app, "/nowhere", "evil.example").await;
    assert_eq!(resp.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn localhost_host_allowed() {
    let app = test_app(state(200));
    for host in [
        "localhost:8765",
        "LocalHost:8765",
        "[::1]:8765",
        "127.0.0.1",
    ] {
        let resp = get(&app, "/health", host).await;
        assert_eq!(resp.status, StatusCode::OK, "{host}");
    }
}

#[tokio::test]
async fn preview_url_uses_the_host_as_sent() {
    let app = test_app(state(200));
    let m1 = get(&app, "/v1/voices/M1", "localhost:8765").await.json();
    assert_eq!(
        m1["preview_url"],
        "http://localhost:8765/v1/voices/M1/preview"
    );
    // No port in the Host header: the bound port is added.
    let m1 = get(&app, "/v1/voices/M1", "localhost").await.json();
    assert_eq!(
        m1["preview_url"],
        "http://localhost:8765/v1/voices/M1/preview"
    );
    let m1 = get(&app, "/v1/voices/M1", "[::1]").await.json();
    assert_eq!(m1["preview_url"], "http://[::1]:8765/v1/voices/M1/preview");
}

#[tokio::test]
async fn trailing_slash_accepted() {
    let app = test_app(state(200));
    let resp = get(&app, "/v1/voices/", HOST).await;
    assert_eq!(resp.status, StatusCode::OK);
    let resp = get(&app, "/health/", HOST).await;
    assert_eq!(resp.status, StatusCode::OK);
}

#[tokio::test]
async fn health_ok() {
    let app = test_app(state(200));
    for uri in ["/", "/health"] {
        let resp = get(&app, uri, HOST).await;
        assert_eq!(resp.status, StatusCode::OK, "{uri}");
        assert_eq!(resp.json(), json!({"status": "ok"}), "{uri}");
    }
}
