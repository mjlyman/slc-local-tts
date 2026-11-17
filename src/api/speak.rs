//! `POST /v1/text-to-speech/{id}[/stream]` and `GET /v1/voices/{id}/preview`:
//! the request pipeline from SLC's text to a WAV reply.
//!
//! The two routes share [`speak_pipeline`] and differ only in their budget
//! and in how they answer a failure: the speech route sends a 503
//! ([`speech_failure`]), the preview route 200 + silence ([`degraded`]).

use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio::time::{Instant, timeout_at};

use super::AppState;
use super::cooldown::on_load_failure;
use super::error::{AppError, BudgetPhase, degraded, silent_ok, speech_failure};
use super::slc::{CLIENT_HEADER, CLIENT_VALUE};
use crate::audio::{encode_wav, level};
use crate::catalog::{self, Voice};
use crate::engine::{CancelFlag, Prepared, SynthError, SynthJob, Synthesizer, engine_speed};
use crate::frontend::text::{self, MAX_TEXT_CHARS};
use crate::models::ModelStore;
use crate::status::records::{RequestRecord, Route};
use crate::status::unix_ms;

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
    /// The encoded WAV and how many seconds of audio it holds.
    Wav { wav: Vec<u8>, seconds: f32 },
    /// Nothing speakable in the text: 0.2 s of silence, no error header.
    Silent,
}

impl Spoken {
    fn outcome(&self) -> &'static str {
        match self {
            Spoken::Wav { .. } => "spoken",
            Spoken::Silent => "silent",
        }
    }

    fn audio_seconds(&self) -> Option<f32> {
        match self {
            Spoken::Wav { seconds, .. } => Some(*seconds),
            Spoken::Silent => None,
        }
    }
}

impl IntoResponse for Spoken {
    fn into_response(self) -> Response {
        match self {
            Spoken::Wav { wav, .. } => {
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
            let request = Pipeline {
                route: Route::Speech,
                from_slc: true,
                voice_id: &voice_id,
                text: &text,
                slc_speed: speed,
                budget,
            };
            speak_pipeline(&state, request).await
        }
        Err(err) => Err(err),
    };
    result.unwrap_or_else(|err| speech_failure(err, &voice_id))
}

/// The preview handler: fixed text at the engine's base speed.
///
/// The settings window's Play button uses this route too, with the window's
/// client header. Its previews are recorded like any other, but they aren't
/// SLC's: they don't set the last request time or the connected marker.
pub async fn preview<S: ModelStore, Y: Synthesizer>(
    State(state): State<Arc<AppState<S, Y>>>,
    Path(voice_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let from_window = headers
        .get(CLIENT_HEADER)
        .is_some_and(|value| value.as_bytes() == CLIENT_VALUE.as_bytes());
    let request = Pipeline {
        route: Route::Preview,
        from_slc: !from_window,
        voice_id: &voice_id,
        text: PREVIEW_TEXT,
        slc_speed: None,
        budget: state.settings.preview_budget,
    };
    speak_pipeline(&state, request)
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

/// One request for [`speak_pipeline`]. The `'a` lifetime says the struct
/// only borrows its strings from the handler that builds it.
struct Pipeline<'a> {
    route: Route,
    /// False for the settings window's own previews.
    from_slc: bool,
    voice_id: &'a str,
    text: &'a str,
    /// Stays `f64` up to the log line, so the log shows SLC's value exactly
    /// (`0.85`, not the `f32` rounding `0.8500000238418579`).
    slc_speed: Option<f64>,
    budget: Duration,
}

/// Text in, WAV out, within `budget` of the request's arrival.
///
/// Every request that gets here ends in one log line and one request
/// record. Requests rejected before the pipeline (malformed bodies) get
/// neither a record nor a "last request" time.
async fn speak_pipeline<S: ModelStore, Y: Synthesizer>(
    state: &Arc<AppState<S, Y>>,
    request: Pipeline<'_>,
) -> Result<Response, AppError> {
    let Pipeline {
        route,
        from_slc,
        voice_id,
        text,
        slc_speed,
        budget,
    } = request;
    let arrived = Instant::now();
    // The wall-clock time too: `Instant` measures durations but can't be
    // shown to the user as a time of day.
    let arrived_at = SystemTime::now();
    if from_slc {
        state.live.request_arrived(arrived_at);
    }
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

    let outcome = match &result {
        Ok(spoken) => spoken.outcome(),
        Err(err) => err.code(),
    };
    let ms = arrived.elapsed().as_millis() as u64;
    tracing::info!(
        voice = voice_id,
        chars = text.chars().count(),
        engine = voice.map_or("?", |voice| voice.engine.as_str()),
        // SLC's speed as it sent it. An `Option` field is left out of the
        // line when it's `None`: previews, and SLC requests without a speed.
        speed = slc_speed,
        outcome,
        ms,
    );
    let error = result.as_ref().err();
    state.live.record(RequestRecord {
        // `Live::record` assigns the real number.
        seq: 0,
        at_ms: unix_ms(arrived_at),
        route,
        voice_id: voice_id.to_string(),
        text: text::clean(text),
        outcome: outcome.to_string(),
        phase: match error {
            Some(AppError::BudgetExceeded(phase)) => Some(*phase),
            _ => None,
        },
        reason: error.and_then(AppError::reason).map(str::to_string),
        message: error.map(AppError::message),
        audio_seconds: result.as_ref().ok().and_then(Spoken::audio_seconds),
        ms,
    });
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

    let mut model_dir = wait_for_model(state, voice, deadline, cancel).await?;
    let speed = engine_speed(voice.engine, &state.settings.base_speed, slc_speed);
    // Counted now: `text` moves into the blocking task that prepares it.
    let chars = text.chars().count();
    let mut prepared = match timeout_at(deadline, prepare(state, voice, text)).await {
        Err(_elapsed) => {
            cancel.cancel();
            return Err(AppError::BudgetExceeded(BudgetPhase::Speaking));
        }
        Ok(Ok(Some(prepared))) => prepared,
        Ok(Ok(None)) => return Ok(Spoken::Silent),
        Ok(Err(err)) => return Err(err),
    };

    let mut downloaded_again = false;
    loop {
        let work = synthesize(state, voice, model_dir, prepared, speed, chars, cancel);
        match timeout_at(deadline, work).await {
            Err(_elapsed) => {
                cancel.cancel();
                return Err(AppError::BudgetExceeded(BudgetPhase::Speaking));
            }
            Ok(Ok(Synthesized::Spoken(spoken))) => return Ok(spoken),
            Ok(Err(err)) => return Err(err),
            // The voice was deleted while this request waited for the lock.
            // Fetch it again once, within the same budget. A second delete
            // in that time is too unlikely to be worth another round.
            Ok(Ok(Synthesized::FilesGone(_))) if downloaded_again => {
                return Err(AppError::BudgetExceeded(BudgetPhase::Downloading));
            }
            Ok(Ok(Synthesized::FilesGone(back))) => {
                downloaded_again = true;
                prepared = back;
                model_dir = wait_for_model(state, voice, deadline, cancel).await?;
            }
        }
    }
}

/// Waits (within the budget) for `voice`'s model folder, downloading it if
/// needed, and refuses a model that recently failed to load.
async fn wait_for_model<S: ModelStore, Y: Synthesizer>(
    state: &Arc<AppState<S, Y>>,
    voice: &'static Voice,
    deadline: Instant,
    cancel: &CancelFlag,
) -> Result<PathBuf, AppError> {
    // Two timeouts against one deadline tell us which phase ran out of
    // time: this one covers only the download, the later ones the rest.
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
    Ok(model_dir)
}

/// Gets the text ready for the engine on a blocking thread. `None` means
/// there is nothing to say.
async fn prepare<S: ModelStore, Y: Synthesizer>(
    state: &Arc<AppState<S, Y>>,
    voice: &'static Voice,
    text: String,
) -> Result<Option<Prepared>, AppError> {
    // `spawn_blocking` needs owned (`'static`) data, so the closure gets
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
    match prepared {
        Ok(Ok(prepared)) => Ok(Some(prepared)),
        Ok(Err(SynthError::NothingToSay)) => Ok(None),
        Ok(Err(err)) => Err(synth_error(err)),
        Err(join_error) => Err(crashed(join_error)),
    }
}

/// What [`synthesize`] did.
enum Synthesized {
    Spoken(Spoken),
    /// The model's files were deleted while the request waited for the
    /// synthesis lock. Hands the prepared input back for the next try.
    FilesGone(Prepared),
}

/// Synthesizes the prepared input, one synthesis at a time.
async fn synthesize<S: ModelStore, Y: Synthesizer>(
    state: &Arc<AppState<S, Y>>,
    voice: &'static Voice,
    model_dir: PathBuf,
    prepared: Prepared,
    speed: f32,
    chars: usize,
    cancel: &CancelFlag,
) -> Result<Synthesized, AppError> {
    // `lock_owned` gives a guard that owns an `Arc` of the mutex instead of
    // borrowing it, so the guard can be moved into the blocking task below.
    let guard = Arc::clone(&state.synth_lock).lock_owned().await;
    // Checked again now that this request holds the lock: while it waited,
    // the request ahead of it may have failed to load this very model.
    if state.load_cooldown.is_cooling(voice.archive) {
        drop(guard);
        return Err(load_failed_error());
    }
    // And the voice may have been deleted from the settings window while
    // this request waited. Loading files that are gone would fail and start
    // the 5-minute load cooldown, so the caller downloads it again instead.
    if !state.store.is_installed(voice.archive) {
        drop(guard);
        return Ok(Synthesized::FilesGone(prepared));
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
        // Held next to the lock, so the status shows "speaking" for exactly
        // as long as the native work runs, even after a hang-up. Locals drop
        // in reverse order, so "speaking" clears just before the lock frees.
        let _speaking = task_state.live.start_speaking(voice.id, chars);
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
        Ok(Err(SynthError::NothingToSay)) => return Ok(Synthesized::Spoken(Spoken::Silent)),
        Ok(Err(err)) => return Err(synth_error(err)),
        Err(join_error) => return Err(crashed(join_error)),
    };
    let volume = state.volume_percent.load(Ordering::Relaxed);
    let leveled = level(&mut audio.samples, audio.sample_rate, volume);
    tracing::debug!(
        voice = voice.id,
        volume,
        lufs = leveled.measured_lufs,
        gain_db = leveled.gain_db,
        "levelled"
    );
    Ok(Synthesized::Spoken(Spoken::Wav {
        wav: encode_wav(&audio),
        seconds: audio.samples.len() as f32 / audio.sample_rate as f32,
    }))
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
        "the files may be damaged; if this keeps happening, delete the voice on the Voices \
         page (or its folder under models\\) so it downloads again"
            .to_string(),
    )
}

/// A blocking task that panicked (or was cancelled at shutdown).
fn crashed(join_error: tokio::task::JoinError) -> AppError {
    tracing::error!(error = %join_error, "the speech engine crashed");
    AppError::SynthesisFailed("the speech engine crashed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_failure_reason_reads_once() {
        // The log line and the window's Activity page both put this reason
        // after their own "couldn't be opened"/"could not be loaded", so it
        // mustn't say that again.
        assert_eq!(
            load_failed_error().message(),
            "could not be loaded: the files may be damaged; if this keeps happening, delete \
             the voice on the Voices page (or its folder under models\\) so it downloads again."
        );
    }
}
