//! The settings window's HTTP client against the real router, served on a
//! real socket.

mod common;

use std::sync::Arc;

use axum::ServiceExt;
use axum::extract::Request;

use slc_local_tts::ui::api::{ClientError, HttpApi, ServerApi};

/// Serves the test app on `127.0.0.1` on a free port, on a runtime of its
/// own, and returns the port. The runtime is returned too: dropping it
/// stops the server.
fn serve() -> (tokio::runtime::Runtime, u16) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let port = listener.local_addr().unwrap().port();

    // The host guard accepts only the port the server is bound to, so the
    // state must say which one it got.
    let Ok(mut state) = Arc::try_unwrap(common::state(2_000)) else {
        panic!("a fresh state has one owner");
    };
    state.settings.port = port;
    let app = common::test_app(Arc::new(state));
    let service = ServiceExt::<Request>::into_make_service(app);
    runtime.spawn(async move { axum::serve(listener, service).await });
    (runtime, port)
}

// The HTTP client blocks, so this is a plain test, not a tokio one: the
// server runs on its own runtime's threads.
#[test]
fn http_api_reads_status_from_real_router() {
    let (_runtime, port) = serve();
    let api = HttpApi::new(port).unwrap();

    let status = api.status().unwrap();
    assert_eq!(status.port, port);
    assert_eq!(status.app, "SLC Local TTS");
    assert!(!api.voices().unwrap().is_empty());
    let requests = api.requests_after(0).unwrap();
    assert!(requests.records.is_empty());

    // The same request without the client header is refused, so the header
    // is why the client's got through.
    let url = format!("http://127.0.0.1:{port}/slc/status");
    let plain = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(url)
        .send()
        .unwrap();
    assert_eq!(plain.status().as_u16(), 403);
}

#[test]
fn absent_server_is_reported_quickly() {
    // A port nothing listens on: bind one, then let it go.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let api = HttpApi::new(port).unwrap();
    let started = std::time::Instant::now();
    assert!(matches!(api.status(), Err(ClientError::Unreachable(_))));
    let took = started.elapsed();
    // Windows itself takes about 2 s to report a refused connection.
    assert!(took < std::time::Duration::from_millis(1_200), "{took:?}");
}

#[test]
fn http_api_reports_refusals_and_absence() {
    let (runtime, port) = serve();
    let api = HttpApi::new(port).unwrap();
    match api.download("not-a-model") {
        Err(ClientError::Refused { status, message }) => {
            assert_eq!(status, 404);
            assert!(!message.is_empty());
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    drop(runtime);
    assert!(matches!(api.status(), Err(ClientError::Unreachable(_))));
}
