//! Test fakes and request helpers shared by the API integration tests.
//!
//! Every file under `tests/` is its own crate and compiles this module
//! separately, so a helper only one of them uses would warn as dead code in
//! the others.
#![allow(dead_code)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use tower::ServiceExt;
use tower_http::normalize_path::NormalizePath;

use slc_local_tts::api::cooldown::LoadCooldown;
use slc_local_tts::api::{ApiSettings, AppState, app};
use slc_local_tts::audio::Audio;
use slc_local_tts::catalog::{Archive, Voice};
use slc_local_tts::config::BaseSpeeds;
use slc_local_tts::engine::{CancelFlag, Prepared, SynthError, SynthJob, Synthesizer};
use slc_local_tts::models::{LoadRecovery, ModelStore, StoreError};

/// The port every test state pretends to be bound to.
pub const PORT: u16 = 8765;
/// The `Host` header SLC sends when pointed at the default URL.
pub const HOST: &str = "127.0.0.1:8765";

// ---------------------------------------------------------------- FakeStore

/// A `ModelStore` that never touches the disk or the network. Configure it
/// with a struct literal (`FakeStore { delay, ..Default::default() }`) before
/// it goes into an `Arc`; the counters and the installed set use interior
/// mutability so they still work afterwards.
#[derive(Default)]
pub struct FakeStore {
    /// Archive stems that count as installed.
    pub installed: Mutex<HashSet<&'static str>>,
    /// If set, `ensure_installed` fails with this reason.
    pub fail_with: Option<String>,
    /// How long `ensure_installed` takes (an async sleep, like a download).
    pub delay: Duration,
    pub ensure_calls: AtomicUsize,
    pub load_failure_reports: AtomicUsize,
    /// What `report_load_failure` returns; `None` means `FilesOk`.
    pub recovery: Option<LoadRecovery>,
    /// How many of the next `ensure_installed` calls panic (counts down).
    pub panic_times: AtomicUsize,
}

impl FakeStore {
    /// A store where `voice`'s archive is already installed.
    pub fn with_installed(voice: &'static Voice) -> Self {
        let store = FakeStore::default();
        store.installed.lock().unwrap().insert(voice.archive.stem);
        store
    }
}

impl ModelStore for FakeStore {
    fn is_installed(&self, archive: &'static Archive) -> bool {
        self.installed.lock().unwrap().contains(archive.stem)
    }

    async fn ensure_installed(&self, archive: &'static Archive) -> Result<PathBuf, StoreError> {
        self.ensure_calls.fetch_add(1, Ordering::SeqCst);
        if take_one(&self.panic_times) {
            panic!("FakeStore was told to panic");
        }
        tokio::time::sleep(self.delay).await;
        if let Some(reason) = &self.fail_with {
            return Err(StoreError {
                reason: reason.clone(),
            });
        }
        self.installed.lock().unwrap().insert(archive.stem);
        Ok(PathBuf::from("fake-models").join(archive.stem))
    }

    fn report_load_failure(&self, _archive: &'static Archive) -> LoadRecovery {
        self.load_failure_reports.fetch_add(1, Ordering::SeqCst);
        self.recovery.unwrap_or(LoadRecovery::FilesOk)
    }
}

/// Takes one off `counter` if it is above zero; true if it did.
fn take_one(counter: &AtomicUsize) -> bool {
    counter
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
}

// ---------------------------------------------------------------- FakeSynth

/// What one `synthesize` call was given, and when it ran.
#[derive(Debug, Clone)]
pub struct SynthCall {
    pub voice_id: &'static str,
    pub prepared: Prepared,
    pub speed: f32,
    pub cancel: CancelFlag,
    pub started: Instant,
    pub ended: Instant,
}

/// A `Synthesizer` that records its calls instead of running a model.
pub struct FakeSynth {
    /// `None`: `prepare` returns `Prepared::Text(text)` for every voice, so
    /// tests can assert the exact text the synthesizer saw.
    pub prepare_result: Option<Result<Prepared, SynthError>>,
    pub synth_result: Result<Audio, SynthError>,
    /// Blocks the calling thread this long inside `synthesize`, like native
    /// code would.
    pub delay: Duration,
    /// How many of the next `synthesize` calls panic. Counts down, so a
    /// test can show the server still works after a panic.
    pub panic_times: AtomicUsize,
    pub calls: Mutex<Vec<SynthCall>>,
    pub current: AtomicUsize,
    pub max_concurrent: AtomicUsize,
}

impl Default for FakeSynth {
    fn default() -> Self {
        FakeSynth {
            prepare_result: None,
            // 0.1 s of a quiet constant signal: non-empty and not silence.
            synth_result: Ok(Audio {
                samples: vec![0.25; 2_205],
                sample_rate: 22_050,
            }),
            delay: Duration::ZERO,
            panic_times: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
            current: AtomicUsize::new(0),
            max_concurrent: AtomicUsize::new(0),
        }
    }
}

impl FakeSynth {
    pub fn calls(&self) -> Vec<SynthCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl Synthesizer for FakeSynth {
    fn prepare(&self, _voice: &'static Voice, text: &str) -> Result<Prepared, SynthError> {
        match &self.prepare_result {
            Some(result) => result.clone(),
            None => Ok(Prepared::Text(text.to_string())),
        }
    }

    fn synthesize(&self, job: SynthJob, cancel: CancelFlag) -> Result<Audio, SynthError> {
        let started = Instant::now();
        let now_running = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_concurrent.fetch_max(now_running, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        self.current.fetch_sub(1, Ordering::SeqCst);
        if take_one(&self.panic_times) {
            panic!("FakeSynth was told to panic");
        }
        self.calls.lock().unwrap().push(SynthCall {
            voice_id: job.voice.id,
            prepared: job.prepared,
            speed: job.speed,
            cancel,
            started,
            ended: Instant::now(),
        });
        self.synth_result.clone()
    }

    fn unload_idle(&self, _idle_for: Duration) -> usize {
        0
    }
}

// ---------------------------------------------------------------- state

pub type TestState = Arc<AppState<FakeStore, FakeSynth>>;

/// Default fakes (nothing installed, a working synth); both the speech and
/// the preview budget are `budget_ms`.
pub fn state(budget_ms: u64) -> TestState {
    state_with(FakeStore::default(), FakeSynth::default(), budget_ms)
}

/// The given fakes; both budgets are `budget_ms`.
pub fn state_with(store: FakeStore, synth: FakeSynth, budget_ms: u64) -> TestState {
    state_with_budgets(store, synth, budget_ms, budget_ms)
}

/// The given fakes with separate speech and preview budgets.
pub fn state_with_budgets(
    store: FakeStore,
    synth: FakeSynth,
    speech_ms: u64,
    preview_ms: u64,
) -> TestState {
    Arc::new(AppState {
        store: Arc::new(store),
        synth: Arc::new(synth),
        synth_lock: Arc::new(tokio::sync::Mutex::new(())),
        load_cooldown: LoadCooldown::default(),
        settings: ApiSettings {
            port: PORT,
            speech_budget: Duration::from_millis(speech_ms),
            preview_budget: Duration::from_millis(preview_ms),
            base_speed: BaseSpeeds::default(),
        },
    })
}

/// The full app (router wrapped in trailing-slash normalization).
pub fn test_app(state: TestState) -> NormalizePath<Router> {
    app(state)
}

// ---------------------------------------------------------------- requests

/// A response with its body already read.
pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl TestResponse {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "body is not JSON ({e}): {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    /// The `X-SLC-Local-TTS-Error` header, if present.
    pub fn error_header(&self) -> Option<&str> {
        self.headers
            .get(slc_local_tts::api::error::ERROR_HEADER)
            .map(|v| v.to_str().unwrap())
    }
}

/// Sends `request` through a clone of `app` and reads the whole body.
pub async fn send(app: &NormalizePath<Router>, request: Request<Body>) -> TestResponse {
    let resp = app.clone().oneshot(request).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    TestResponse {
        status,
        headers,
        body,
    }
}

/// `GET uri` with the given `Host` header.
pub async fn get(app: &NormalizePath<Router>, uri: &str, host: &str) -> TestResponse {
    get_with(app, uri, host, &[]).await
}

/// `GET uri` with the given `Host` and extra headers (e.g. `xi-api-key`).
pub async fn get_with(
    app: &NormalizePath<Router>,
    uri: &str,
    host: &str,
    headers: &[(&str, &str)],
) -> TestResponse {
    let mut builder = Request::get(uri).header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    send(app, builder.body(Body::empty()).unwrap()).await
}

/// `POST uri` with `body` as the JSON body and SLC's usual headers.
/// `body` is anything printable: a `serde_json::Value` prints as JSON, and a
/// `&str` is sent as-is (for malformed-JSON tests).
pub async fn post_json(
    app: &NormalizePath<Router>,
    uri: &str,
    body: impl ToString,
) -> TestResponse {
    post_json_with(app, uri, body, &[]).await
}

/// `post_json` with extra headers.
pub async fn post_json_with(
    app: &NormalizePath<Router>,
    uri: &str,
    body: impl ToString,
    headers: &[(&str, &str)],
) -> TestResponse {
    let mut builder = Request::post(uri)
        .header("host", HOST)
        .header("content-type", "application/json; charset=utf-8");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    send(app, builder.body(Body::from(body.to_string())).unwrap()).await
}

// ---------------------------------------------------------------- logs

/// Everything a capturing subscriber has written so far.
#[derive(Clone, Default)]
pub struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl LogBuffer {
    pub fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Keeps a log capture installed; drop it to stop capturing.
pub struct LogCapture {
    _guard: tracing::subscriber::DefaultGuard,
    _second: tracing::Dispatch,
}

/// Installs a subscriber that records every event at `TRACE` and above, for
/// the current thread only, until the returned `LogCapture` is dropped. Use
/// it in a `current_thread` test so the handlers run on this thread.
pub fn capture_logs() -> (LogBuffer, LogCapture) {
    // Seen with tracing-core 0.1.36 (callsite.rs, `Dispatchers::rebuilder`);
    // if this capture flakes after an upgrade, look here first.
    // With exactly one live dispatcher, tracing-core computes the interest
    // of a newly hit log statement from the *calling thread's* subscriber.
    // Other tests run on threads with none, so a statement they reach first
    // would be cached as "never" and vanish from this capture too. A second
    // live dispatcher switches tracing-core to asking every dispatcher.
    let second = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let buffer = LogBuffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    (
        buffer,
        LogCapture {
            _guard: guard,
            _second: second,
        },
    )
}

/// Polls `condition` every 10 ms for up to 2 s; true if it became true.
pub async fn eventually(condition: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    condition()
}
