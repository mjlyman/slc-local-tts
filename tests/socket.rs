//! SLC sends `Expect: 100-continue` on every speech request. This test talks
//! to the real server over a real TCP socket to prove the server answers
//! `100 Continue` before the body arrives, then serves the request.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::ServiceExt;
use axum::extract::Request;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use common::{FakeStore, FakeSynth};
use slc_local_tts::api::cooldown::LoadCooldown;
use slc_local_tts::api::{ApiSettings, AppState, app};
use slc_local_tts::config::BaseSpeeds;
use slc_local_tts::server;

/// How long any one read may wait before the test fails instead of hanging.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Reads from `stream` until `buffer` contains `\r\n\r\n` (the end of a
/// response's headers) and returns everything read up to and including it.
/// Bytes after it stay in `buffer` for the next call.
async fn read_head(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> String {
    loop {
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            let head: Vec<u8> = buffer.drain(..end + 4).collect();
            return String::from_utf8(head).unwrap();
        }
        let mut chunk = [0u8; 4096];
        let n = tokio::time::timeout(READ_TIMEOUT, stream.read(&mut chunk))
            .await
            .expect("timed out waiting for the server's response")
            .unwrap();
        assert!(
            n > 0,
            "connection closed before the headers ended; got: {}",
            String::from_utf8_lossy(buffer)
        );
        buffer.extend_from_slice(&chunk[..n]);
    }
}

#[tokio::test]
async fn expect_100_continue_over_real_socket() {
    // Port 0: the OS picks a free port, so this never touches 8765.
    let listener = server::bind(0, Path::new("config.toml")).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = Arc::new(AppState {
        store: Arc::new(FakeStore::default()),
        synth: Arc::new(FakeSynth::default()),
        synth_lock: Arc::new(tokio::sync::Mutex::new(())),
        load_cooldown: LoadCooldown::default(),
        settings: ApiSettings {
            // The host guard only accepts requests addressed to this port.
            port,
            speech_budget: Duration::from_secs(5),
            preview_budget: Duration::from_secs(5),
            base_speed: BaseSpeeds::default(),
        },
    });
    // See `Started::serve` in src/server.rs for why the turbofish is needed.
    let service = ServiceExt::<Request>::into_make_service(app(state));
    let server = tokio::spawn(async move { axum::serve(listener, service).await });

    let body = r#"{"text":"Hi"}"#;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let head = format!(
        "POST /v1/text-to-speech/en_US-joe-medium/stream?output_format=mp3_22050_32 HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Content-Type: application/json; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Expect: 100-continue\r\n\
         \r\n",
        body.len()
    );
    // The headers only: a client that sent `Expect` waits for `100 Continue`
    // before it sends the body.
    stream.write_all(head.as_bytes()).await.unwrap();

    let mut buffer = Vec::new();
    let interim = read_head(&mut stream, &mut buffer).await;
    assert!(
        interim.starts_with("HTTP/1.1 100 Continue"),
        "expected 100 Continue first, got: {interim}"
    );

    stream.write_all(body.as_bytes()).await.unwrap();
    let response = read_head(&mut stream, &mut buffer).await;
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "expected 200 OK, got: {response}"
    );
    assert!(
        response
            .to_ascii_lowercase()
            .contains("content-type: audio/wav"),
        "expected a WAV, got: {response}"
    );

    // The body: exactly `Content-Length` bytes, some of which may already
    // be in `buffer` from the last read.
    let length: usize = response
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap_or_else(|| panic!("no content-length in: {response}"));
    while buffer.len() < length {
        let mut chunk = [0u8; 4096];
        let n = tokio::time::timeout(READ_TIMEOUT, stream.read(&mut chunk))
            .await
            .expect("timed out waiting for the response body")
            .unwrap();
        assert!(
            n > 0,
            "connection closed after {} of {length} body bytes",
            buffer.len()
        );
        buffer.extend_from_slice(&chunk[..n]);
    }
    assert_eq!(buffer.len(), length, "more bytes than Content-Length");
    assert!(buffer.starts_with(b"RIFF"), "body is not a WAV");

    server.abort();
}
