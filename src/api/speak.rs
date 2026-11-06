//! `POST /v1/text-to-speech/{id}[/stream]` and `GET /v1/voices/{id}/preview`:
//! the request pipeline from SLC's text to a WAV reply.
//!
//! The two routes share [`speak_pipeline`] and differ only in their budget
//! and in how they answer a failure: the speech route sends a 503
//! ([`speech_failure`]), the preview route 200 + silence ([`degraded`]).

use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio::time::{Instant, timeout_at};

use super::AppState;
use super::cooldown::on_load_failure;
use super::error::{AppError, BudgetPhase, degraded, silent_ok, speech_failure};
use crate::audio::{apply_gain_and_limit, encode_wav, engine_gain};
use crate::catalog::{self, Voice};
use crate::engine::{CancelFlag, SynthError, SynthJob, Synthesizer, engine_speed};
use crate::frontend::text::{self, MAX_TEXT_CHARS};
use crate::models::ModelStore;

/// What every preview says.
pub const PREVIEW_TEXT: &str =
    "Good afternoon, ladies and gentlemen. This is your captain speaking.";

/// Longest piece of the text written to the debug log.
const LOGGED_TEXT_CHARS: usize = 160;

/// The fields of SLC's speech body this server reads; serde ignores the rest
/// (`model_id`, `stability`, ...).
#[derive(Debug, Deserialize)]
pub struct SpeakRequest {
    pub text: String,
    pub voice_settings: Option<VoiceSettings>,
}

#[derive(Debug, Deserialize)]
pub struct VoiceSettings {
    pub speed: Option<f64>,
}

/// Sets the cancel flag when dropped. The pipeline holds one for its whole
/// run, so if hyper drops the request future (the client hung up), a
/// synthesis still running on its blocking thread is told to stop. After a
/// normal finish the flag is set too, but nothing reads it any more.
struct CancelOnDrop(CancelFlag);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// What a successful pipeline produced.
enum Spoken {
    Wav(Vec<u8>),
    /// Nothing speakable in the text: 0.2 s of silence, no error header.
    Silent,
}

impl Spoken {
    fn outcome(&self) -> &'static str {
        match self {
            Spoken::Wav(_) => "spoken",
            Spoken::Silent => "silent",
        }
    }
}

impl IntoResponse for Spoken {
    fn into_response(self) -> Response {
        match self {
            Spoken::Wav(wav) => {
                let mut resp = (StatusCode::OK, wav).into_response();
                resp.headers_mut()
                    .insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/wav"));
                resp
            }
            Spoken::Silent => silent_ok(),
        }
    }
}

/// The speech handler. The query (`output_format=mp3_...`) is ignored: SLC
/// plays WAV fine.
pub async fn speak<S: ModelStore, Y: Synthesizer>(
    State(state): State<Arc<AppState<S, Y>>>,
    Path(voice_id): Path<String>,
    body: Result<Json<SpeakRequest>, JsonRejection>,
) -> Response {
    let result = match validate(body) {
        Ok((text, speed)) => {
            let budget = state.settings.speech_budget;
            speak_pipeline(&state, &voice_id, &text, speed, budget).await
        }
        Err(err) => Err(err),
    };
    result.unwrap_or_else(|err| speech_failure(err, &voice_id))
}

/// The preview handler: fixed text at the engine's base speed.
pub async fn preview<S: ModelStore, Y: Synthesizer>(
    State(state): State<Arc<AppState<S, Y>>>,
    Path(voice_id): Path<String>,
) -> Response {
    let budget = state.settings.preview_budget;
    speak_pipeline(&state, &voice_id, PREVIEW_TEXT, None, budget)
        .await
        .unwrap_or_else(|err| degraded(&err, &voice_id))
}

/// `CatchPanicLayer` response for the speech routes.
pub fn speech_panic(payload: Box<dyn Any + Send + 'static>) -> Response {
    log_panic(payload.as_ref());
    speech_failure(AppError::Internal(PANIC_REASON.to_string()), "?")
}

/// `CatchPanicLayer` response for the preview route.
pub fn preview_panic(payload: Box<dyn Any + Send + 'static>) -> Response {
    log_panic(payload.as_ref());
    degraded(&AppError::Internal(PANIC_REASON.to_string()), "?")
}

const PANIC_REASON: &str = "the server hit a bug; if this keeps happening, restart SLC Local TTS";

fn log_panic(payload: &(dyn Any + Send)) {
    let detail = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("(no message)");
    tracing::error!(detail, "a request handler panicked");
}

/// Checks the parts of the body SLC never gets wrong; returns the text and
/// SLC's speed (SLC sets it from its speech-rate slider).
fn validate(
    body: Result<Json<SpeakRequest>, JsonRejection>,
) -> Result<(String, Option<f64>), AppError> {
    let Json(request) =
        body.map_err(|rejection| AppError::InvalidRequest(rejection.body_text()))?;
    if request.text.chars().count() > MAX_TEXT_CHARS {
        return Err(AppError::InvalidRequest(format!(
            "the text is longer than {MAX_TEXT_CHARS} characters"
        )));
    }
    let speed = request.voice_settings.and_then(|settings| settings.speed);
    // 0 or less is allowed: SLC's slider may send it at its slow end, and
    // `engine_speed` turns it into the engine's slowest speed. JSON can't
    // carry infinity or NaN, so this check is only a safety net.
    if let Some(speed) = speed
        && !speed.is_finite()
    {
        return Err(AppError::InvalidRequest(format!(
            "voice_settings.speed must be a finite number, but SLC sent {speed}"
        )));
    }
    Ok((request.text, speed))
}

/// Text in, WAV out, within `budget` of the request's arrival. `slc_speed`
/// stays `f64` up to the log line, so the log shows SLC's value exactly
/// (`0.85`, not the `f32` rounding `0.8500000238418579`).
async fn speak_pipeline<S: ModelStore, Y: Synthesizer>(
    state: &Arc<AppState<S, Y>>,
    voice_id: &str,
    text: &str,
    slc_speed: Option<f64>,
    budget: Duration,
) -> Result<Response, AppError> {
    let arrived = Instant::now();
    let cancel = CancelFlag::default();
    // Lives until this function returns or its future is dropped.
    let _cancel_on_drop = CancelOnDrop(cancel.clone());

    let truncated: String = text.chars().take(LOGGED_TEXT_CHARS).collect();
    tracing::debug!(voice = voice_id, text = %truncated, "speech request");

    let voice = catalog::find(voice_id);
    // `engine_speed` clamps, so a huge value that overflows f32 to infinity
    // still ends up at the engine's maximum.
    let engine_input = slc_speed.map(|speed| speed as f32);
    let result = match voice {
        None => Err(AppError::VoiceNotFound(voice_id.to_string())),
        Some(voice) => {
            speak_voice(state, voice, text, engine_input, arrived + budget, &cancel).await
        }
    };

    tracing::info!(
        voice = voice_id,
        chars = text.chars().count(),
        engine = voice.map_or("?", |voice| voice.engine.as_str()),
        // SLC's speed as it sent it. An `Option` field is left out of the
        // line when it's `None`: previews, and SLC requests without a speed.
        speed = slc_speed,
        outcome = match &result {
            Ok(spoken) => spoken.outcome(),
            Err(err) => err.code(),
        },
        ms = arrived.elapsed().as_millis() as u64,
    );
    result.map(Spoken::into_response)
}

/// The pipeline for a known voice.
async fn speak_voice<S: ModelStore, Y: Synthesizer>(
    state: &Arc<AppState<S, Y>>,
    voice: &'static Voice,
    text: &str,
    slc_speed: Option<f32>,
    deadline: Instant,
    cancel: &CancelFlag,
) -> Result<Spoken, AppError> {
    let Some(text) = text::prepare(text) else {
        return Ok(Spoken::Silent);
    };

    // Two timeouts against one deadline tell us which phase ran out of
    // time: the first covers only the download, the second everything after.
    let model_dir = match timeout_at(deadline, state.store.ensure_installed(voice.archive)).await {
        Err(_elapsed) => {
            cancel.cancel();
            return Err(AppError::BudgetExceeded(BudgetPhase::Downloading));
        }
        Ok(Err(err)) => return Err(AppError::ModelDownloadFailed(err.reason)),
        Ok(Ok(dir)) => dir,
    };

    // Checked after `ensure_installed`, so a re-download of quarantined
    // files can start; only loading the model is held back.
    if state.load_cooldown.is_cooling(voice.archive) {
        return Err(load_failed_error());
    }

    let speed = engine_speed(voice.engine, &state.settings.base_speed, slc_speed);
    let work = synthesize(state, voice, model_dir, text, speed, cancel);
    match timeout_at(deadline, work).await {
        Err(_elapsed) => {
            cancel.cancel();
            Err(AppError::BudgetExceeded(BudgetPhase::Speaking))
        }
        Ok(result) => result,
    }
}

/// Prepares the text and synthesizes it, one synthesis at a time.
async fn synthesize<S: ModelStore, Y: Synthesizer>(
    state: &Arc<AppState<S, Y>>,
    voice: &'static Voice,
    model_dir: PathBuf,
    text: String,
    speed: f32,
    cancel: &CancelFlag,
) -> Result<Spoken, AppError> {
    // `spawn_blocking` needs owned (`'static`) data, so each closure gets
    // its own `Arc` handle to the shared state.
    let task_state = Arc::clone(state);
    let prepared = tokio::task::spawn_blocking(move || {
        let result = task_state.synth.prepare(voice, &text);
        if let Err(SynthError::LoadFailed(detail)) = &result {
            on_load_failure(&task_state, voice, detail);
        }
        result
    })
    .await;
    let prepared = match prepared {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(SynthError::NothingToSay)) => return Ok(Spoken::Silent),
        Ok(Err(err)) => return Err(synth_error(err)),
        Err(join_error) => return Err(crashed(join_error)),
    };

    // `lock_owned` gives a guard that owns an `Arc` of the mutex instead of
    // borrowing it, so the guard can be moved into the blocking task below.
    let guard = Arc::clone(&state.synth_lock).lock_owned().await;
    // Checked again now that this request holds the lock: while it waited,
    // the request ahead of it may have failed to load this very model.
    if state.load_cooldown.is_cooling(voice.archive) {
        drop(guard);
        return Err(load_failed_error());
    }
    let task_state = Arc::clone(state);
    let job = SynthJob {
        voice,
        model_dir,
        prepared,
        speed,
    };
    let task_cancel = cancel.clone();
    // The guard lives inside the blocking task, not in this future. If the
    // request is dropped (timeout or hang-up), this future goes away but the
    // native code keeps running until it notices the cancel flag; the lock
    // is released only when that work really ends.
    let result = tokio::task::spawn_blocking(move || {
        let _guard = guard;
        let result = task_state.synth.synthesize(job, task_cancel);
        // Still under the lock: a request queued behind this one checks the
        // cooldown once it gets the lock, so it must already be recorded.
        // Done here rather than in the request future, which may be gone.
        if let Err(SynthError::LoadFailed(detail)) = &result {
            on_load_failure(&task_state, voice, detail);
        }
        result
    })
    .await;

    let mut audio = match result {
        Ok(Ok(audio)) => audio,
        Ok(Err(SynthError::NothingToSay)) => return Ok(Spoken::Silent),
        Ok(Err(err)) => return Err(synth_error(err)),
        Err(join_error) => return Err(crashed(join_error)),
    };
    apply_gain_and_limit(&mut audio.samples, engine_gain(voice.engine));
    Ok(Spoken::Wav(encode_wav(&audio)))
}

/// Maps a synthesizer error to the HTTP error. Load failures have already
/// been handled by [`on_load_failure`] on the blocking thread.
fn synth_error(err: SynthError) -> AppError {
    match err {
        SynthError::LoadFailed(_) => load_failed_error(),
        SynthError::Failed(detail) => AppError::SynthesisFailed(detail),
        SynthError::Cancelled => AppError::BudgetExceeded(BudgetPhase::Speaking),
        // Never reached: both callers turn `NothingToSay` into silence before
        // calling this. The arm exists because the match must be exhaustive.
        SynthError::NothingToSay => AppError::SynthesisFailed("there was nothing to say".into()),
    }
}

fn load_failed_error() -> AppError {
    AppError::ModelLoadFailed(
        "the voice files could not be opened; if this keeps happening, delete the voice's \
         folder under models\\ and it will download again"
            .to_string(),
    )
}

/// A blocking task that panicked (or was cancelled at shutdown).
fn crashed(join_error: tokio::task::JoinError) -> AppError {
    tracing::error!(error = %join_error, "the speech engine crashed");
    AppError::SynthesisFailed("the speech engine crashed".to_string())
}
