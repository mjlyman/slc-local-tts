//! The window's HTTP client for the server's private `/slc/` routes.
//!
//! Every call blocks, so it runs on the backend's own threads, never on the
//! window's drawing thread. [`ServerApi`] is the seam the tests replace with
//! a scripted fake.

use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{HeaderMap, HeaderValue};

use crate::api::error::ERROR_HEADER;
use crate::api::slc::dto::{ModelDto, RequestsDto, StatusDto};
use crate::api::slc::{CLIENT_HEADER, CLIENT_VALUE};

/// How long an ordinary call may take. The server answers these at once,
/// so a slow answer means it's gone or stuck.
pub const TIMEOUT: Duration = Duration::from_secs(2);
/// How long a preview or a delete may take. A preview may first download
/// its voice, and a delete waits for any line being spoken to finish.
pub const LONG_TIMEOUT: Duration = Duration::from_secs(70);
/// How long making the connection may take. Windows takes about 2 s to
/// report that nothing listens on a local port, which would make a stopped
/// server look slow to notice. A running server's connections are accepted
/// by Windows itself, however busy the server is, so this only shortens
/// connects to a port nothing listens on.
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// Why a call to the server failed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ClientError {
    /// No answer: the server isn't running, or didn't answer in time.
    #[error("SLC Local TTS didn't answer ({0})")]
    Unreachable(String),
    /// The server answered with an error. `message` is its own text for
    /// the user.
    #[error("{message}")]
    Refused { status: u16, message: String },
    /// The server's answer couldn't be read.
    #[error("SLC Local TTS sent an answer the window couldn't read ({0})")]
    Bad(String),
}

/// A voice's preview: the WAV to play, and the server's error code when it
/// had to send silence instead.
#[derive(Debug, Clone, PartialEq)]
pub struct Preview {
    pub wav: Vec<u8>,
    /// The `X-SLC-Local-TTS-Error` header, if any.
    pub error: Option<String>,
}

/// The server, as the window uses it. Every call blocks until the server
/// answers or the call times out.
///
/// `Send + Sync + 'static` so it can be shared, in an `Arc`, by the
/// backend's threads for as long as they run.
pub trait ServerApi: Send + Sync + 'static {
    fn status(&self) -> Result<StatusDto, ClientError>;
    fn voices(&self) -> Result<Vec<ModelDto>, ClientError>;
    /// The request records newer than `seq`.
    fn requests_after(&self, seq: u64) -> Result<RequestsDto, ClientError>;
    /// Starts downloading the model `stem`, or joins its download.
    fn download(&self, stem: &str) -> Result<(), ClientError>;
    /// Deletes the model `stem`.
    fn delete(&self, stem: &str) -> Result<(), ClientError>;
    /// Asks the server to restart and install the downloaded update.
    fn apply_update(&self) -> Result<(), ClientError>;
    /// The voice's preview, the one SLC's preview button plays.
    fn preview_wav(&self, voice_id: &str) -> Result<Preview, ClientError>;
    /// The port the calls go to.
    fn port(&self) -> u16;
    /// Sends every later call to the server on `port` instead.
    fn set_port(&self, port: u16);
}

/// [`ServerApi`] over HTTP, to the server on `127.0.0.1`.
pub struct HttpApi {
    client: Client,
    /// The server's port on `127.0.0.1`. An atomic, so the poll thread can
    /// change it through the shared `Arc` while the action thread reads it,
    /// with no lock.
    port: AtomicU16,
}

impl HttpApi {
    /// A client for the server on `port`. It fails only if the HTTP
    /// library can't start, which no ordinary PC causes.
    pub fn new(port: u16) -> Result<HttpApi, reqwest::Error> {
        // The server refuses every `/slc/` request without this header, and
        // a preview with it isn't counted as a request from SLC.
        let mut headers = HeaderMap::new();
        headers.insert(CLIENT_HEADER, HeaderValue::from_static(CLIENT_VALUE));
        let client = Client::builder()
            .default_headers(headers)
            .timeout(TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            // The server is on this PC; a proxy set up for the internet
            // must never see these requests.
            .no_proxy()
            .build()?;
        Ok(HttpApi {
            client,
            port: AtomicU16::new(port),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port())
    }

    /// Reads a JSON answer from `GET path`.
    fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ClientError> {
        let body = send(self.client.get(self.url(path)))?
            .bytes()
            .map_err(|e| ClientError::Unreachable(e.to_string()))?;
        serde_json::from_slice(&body).map_err(|e| ClientError::Bad(e.to_string()))
    }
}

/// Sends `request` and turns any failure into a [`ClientError`]: no answer
/// is `Unreachable`, and an error status is `Refused` with the server's own
/// message.
fn send(request: RequestBuilder) -> Result<Response, ClientError> {
    let response = request
        .send()
        .map_err(|e| ClientError::Unreachable(e.to_string()))?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let message = response
        .bytes()
        .ok()
        .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).ok())
        .and_then(|body| body["detail"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| format!("SLC Local TTS answered with an error ({status})."));
    Err(ClientError::Refused {
        status: status.as_u16(),
        message,
    })
}

impl ServerApi for HttpApi {
    fn status(&self) -> Result<StatusDto, ClientError> {
        self.get_json("/slc/status")
    }

    fn voices(&self) -> Result<Vec<ModelDto>, ClientError> {
        self.get_json("/slc/voices")
    }

    fn requests_after(&self, seq: u64) -> Result<RequestsDto, ClientError> {
        self.get_json(&format!("/slc/requests?after={seq}"))
    }

    fn download(&self, stem: &str) -> Result<(), ClientError> {
        let path = format!("/slc/models/{stem}/download");
        send(self.client.post(self.url(&path))).map(drop)
    }

    fn delete(&self, stem: &str) -> Result<(), ClientError> {
        let path = format!("/slc/models/{stem}");
        send(self.client.delete(self.url(&path)).timeout(LONG_TIMEOUT)).map(drop)
    }

    fn apply_update(&self) -> Result<(), ClientError> {
        send(self.client.post(self.url("/slc/update/apply"))).map(drop)
    }

    fn preview_wav(&self, voice_id: &str) -> Result<Preview, ClientError> {
        let path = format!("/v1/voices/{voice_id}/preview");
        let response = send(self.client.get(self.url(&path)).timeout(LONG_TIMEOUT))?;
        let error = response
            .headers()
            .get(ERROR_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let wav = response
            .bytes()
            .map_err(|e| ClientError::Unreachable(e.to_string()))?
            .to_vec();
        Ok(Preview { wav, error })
    }

    fn port(&self) -> u16 {
        // Relaxed is enough: the port is a single value that orders nothing
        // else.
        self.port.load(Ordering::Relaxed)
    }

    fn set_port(&self, port: u16) {
        self.port.store(port, Ordering::Relaxed);
    }
}
