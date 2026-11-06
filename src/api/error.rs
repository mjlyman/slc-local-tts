//! The error model and the "never send SLC an error" responses.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::audio::{encode_wav, silence};
use crate::catalog::{self, Engine};

/// Name of the response header that tells a human (or a log reader) which
/// error was hidden behind a silent 200.
pub const ERROR_HEADER: &str = "X-SLC-Local-TTS-Error";

/// Where a request was when its time budget ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetPhase {
    /// The voice's model was still downloading.
    Downloading,
    /// The model was installed; preparing or speaking the line took too long.
    Speaking,
}

/// Every error the HTTP layer can produce. Like a Java checked-exception
/// hierarchy collapsed into one enum: each variant knows its own status code,
/// machine-readable code and plain-English message.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("voice not found: {0}")]
    VoiceNotFound(String),
    #[error("forbidden host")]
    ForbiddenHost,
    #[error("route not found")]
    RouteNotFound,
    #[error("method not allowed")]
    MethodNotAllowed,
    #[error("request budget exceeded while {0:?}")]
    BudgetExceeded(BudgetPhase),
    #[error("model download failed: {0}")]
    ModelDownloadFailed(String),
    #[error("model load failed: {0}")]
    ModelLoadFailed(String),
    #[error("synthesis failed: {0}")]
    SynthesisFailed(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl AppError {
    /// The HTTP status this error uses when it is sent as a real error.
    pub fn status(&self) -> StatusCode {
        match self {
            AppError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            AppError::VoiceNotFound(_) | AppError::RouteNotFound => StatusCode::NOT_FOUND,
            AppError::ForbiddenHost => StatusCode::FORBIDDEN,
            AppError::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            AppError::BudgetExceeded(_) => StatusCode::GATEWAY_TIMEOUT,
            AppError::ModelDownloadFailed(_) => StatusCode::BAD_GATEWAY,
            AppError::ModelLoadFailed(_) | AppError::SynthesisFailed(_) | AppError::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    /// Stable snake_case identifier, used in the JSON body and the header.
    pub fn code(&self) -> &'static str {
        match self {
            AppError::InvalidRequest(_) => "invalid_request",
            AppError::VoiceNotFound(_) => "voice_not_found",
            AppError::ForbiddenHost => "forbidden_host",
            AppError::RouteNotFound => "not_found",
            AppError::MethodNotAllowed => "method_not_allowed",
            AppError::BudgetExceeded(_) => "budget_exceeded",
            AppError::ModelDownloadFailed(_) => "model_download_failed",
            AppError::ModelLoadFailed(_) => "model_load_failed",
            AppError::SynthesisFailed(_) => "synthesis_failed",
            AppError::Internal(_) => "internal_error",
        }
    }

    /// User-facing text. Written as a phrase that reads correctly after
    /// "Voice M1: " and ends with a full stop.
    pub fn message(&self) -> String {
        match self {
            AppError::InvalidRequest(reason) => format!("the request was not valid: {reason}."),
            AppError::VoiceNotFound(id) => format!(
                "is not a voice this server knows (\"{id}\"). Pick a different voice in SLC."
            ),
            AppError::ForbiddenHost => {
                "was refused because the request did not come from this computer.".to_string()
            }
            AppError::RouteNotFound => {
                "was asked for a page this server does not have.".to_string()
            }
            AppError::MethodNotAllowed => {
                "was asked in a way this server does not support.".to_string()
            }
            AppError::BudgetExceeded(BudgetPhase::Downloading) => {
                "is still downloading, so it was not ready in time.".to_string()
            }
            AppError::BudgetExceeded(BudgetPhase::Speaking) => {
                "took too long to speak this line.".to_string()
            }
            AppError::ModelDownloadFailed(reason) => {
                format!("could not be downloaded: {reason}.")
            }
            AppError::ModelLoadFailed(reason) => format!("could not be loaded: {reason}."),
            AppError::SynthesisFailed(reason) => format!("could not be spoken: {reason}."),
            AppError::Internal(reason) => format!("hit an unexpected problem: {reason}."),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({
            "detail": { "status": self.code(), "message": self.message() }
        });
        (self.status(), Json(body)).into_response()
    }
}

/// A 200 WAV response with the given body and an optional error code header.
fn wav_response(error_code: Option<&'static str>) -> Response {
    let body = encode_wav(&silence());
    let mut resp = (StatusCode::OK, body).into_response();
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/wav"));
    if let Some(code) = error_code {
        resp.headers_mut()
            .insert(ERROR_HEADER, HeaderValue::from_static(code));
    }
    resp
}

/// 0.2 s of silence with no error header: the answer for text that is
/// legitimately unspeakable.
pub fn silent_ok() -> Response {
    wav_response(None)
}

/// The preview route's failure response: 200 + silence + an error header +
/// a loud log line. SLC crashes on any error from the preview route, so
/// failures there look like success to it.
pub fn degraded(err: &AppError, voice_id: &str) -> Response {
    let advice = match err {
        AppError::BudgetExceeded(BudgetPhase::Downloading) => {
            " The download continues in the background; press preview again in a minute or two."
        }
        _ => "",
    };
    tracing::error!(
        "Voice {voice_id}: {} The preview played silence.{advice}",
        err.message()
    );
    wav_response(Some(err.code()))
}

/// The speech route's failure response. Malformed requests, which
/// SLC shouldn't send, keep their 400. Everything else is a 503: SLC plays a
/// short failure sound and asks for the line again next time, whereas a
/// silent 200 would be cached by SLC forever.
pub fn speech_failure(err: AppError, voice_id: &str) -> Response {
    if let AppError::InvalidRequest(_) = err {
        // Logged loudly with the reason, which names any bad value SLC sent
        // (for example a speed from its speech-rate slider). The reasons
        // never contain the request's text.
        tracing::error!(
            "Voice {voice_id}: {} The line was not spoken.",
            err.message()
        );
        return err.into_response();
    }
    tracing::error!(
        "Voice {voice_id}: {} SLC played its failure sound for this line and will ask again \
         next time.{}",
        err.message(),
        speech_advice(&err, voice_id)
    );
    let body = serde_json::json!({
        "detail": { "status": err.code(), "message": err.message() }
    });
    (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
}

/// What the user can do about a speech failure, as a sentence that starts
/// with a space, or "" when there is nothing useful to say.
fn speech_advice(err: &AppError, voice_id: &str) -> String {
    match err {
        AppError::BudgetExceeded(BudgetPhase::Downloading) => format!(
            " Install voices before flying: press the voice's preview button in SLC, \
             or run slc-local-tts download {voice_id}."
        ),
        AppError::BudgetExceeded(BudgetPhase::Speaking) => {
            match catalog::find(voice_id).map(|voice| voice.engine) {
                Some(Engine::Kokoro) => " Kokoro voices are slow on long lines: use a Piper or \
                    Supertonic voice for long announcements, or raise num_threads in config.toml."
                    .to_string(),
                _ => " If this keeps happening, raise num_threads in config.toml or close other \
                    heavy programs."
                    .to_string(),
            }
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn body_of(resp: Response) -> Vec<u8> {
        axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }

    fn all_variants() -> Vec<(AppError, u16, &'static str)> {
        vec![
            (AppError::InvalidRequest("x".into()), 400, "invalid_request"),
            (AppError::VoiceNotFound("v".into()), 404, "voice_not_found"),
            (AppError::ForbiddenHost, 403, "forbidden_host"),
            (AppError::RouteNotFound, 404, "not_found"),
            (AppError::MethodNotAllowed, 405, "method_not_allowed"),
            (
                AppError::BudgetExceeded(BudgetPhase::Downloading),
                504,
                "budget_exceeded",
            ),
            (
                AppError::BudgetExceeded(BudgetPhase::Speaking),
                504,
                "budget_exceeded",
            ),
            (
                AppError::ModelDownloadFailed("r".into()),
                502,
                "model_download_failed",
            ),
            (
                AppError::ModelLoadFailed("r".into()),
                500,
                "model_load_failed",
            ),
            (
                AppError::SynthesisFailed("r".into()),
                500,
                "synthesis_failed",
            ),
            (AppError::Internal("r".into()), 500, "internal_error"),
        ]
    }

    #[tokio::test]
    async fn json_shape_per_variant() {
        for (err, status, code) in all_variants() {
            assert_eq!(err.status().as_u16(), status, "{code}");
            assert_eq!(err.code(), code);
            assert!(!err.message().is_empty());
            let resp = err.into_response();
            assert_eq!(resp.status().as_u16(), status, "{code}");
            let v: serde_json::Value = serde_json::from_slice(&body_of(resp).await).unwrap();
            assert_eq!(v["detail"]["status"], code);
            assert!(!v["detail"]["message"].as_str().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn degraded_is_200_silence_with_header() {
        let resp = degraded(&AppError::BudgetExceeded(BudgetPhase::Downloading), "M1");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "audio/wav");
        assert_eq!(resp.headers()["x-slc-local-tts-error"], "budget_exceeded");
        assert_eq!(body_of(resp).await.len(), 8864);
    }

    #[tokio::test]
    async fn silent_ok_has_no_error_header() {
        let resp = silent_ok();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "audio/wav");
        assert!(resp.headers().get("x-slc-local-tts-error").is_none());
        assert_eq!(body_of(resp).await.len(), 8864);
    }

    /// Runs `f` with a `tracing` subscriber that writes into a buffer, and
    /// returns everything it logged.
    fn capture_logs(f: impl FnOnce()) -> String {
        #[derive(Clone, Default)]
        struct Buffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        // Seen with tracing-core 0.1.36 (callsite.rs, `Dispatchers::rebuilder`);
        // if this capture flakes after an upgrade, look here first.
        // With exactly one live dispatcher, tracing-core computes the
        // interest of a newly hit log statement from the calling thread's
        // subscriber, so a statement another test thread reaches first would
        // be cached as "never". A second live dispatcher avoids that.
        let _second = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap()
    }

    #[tokio::test]
    async fn speech_failure_is_503_json() {
        let resp = speech_failure(AppError::ModelDownloadFailed("no internet".into()), "M1");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(resp.headers()["content-type"], "application/json");
        assert!(resp.headers().get("x-slc-local-tts-error").is_none());
        let v: serde_json::Value = serde_json::from_slice(&body_of(resp).await).unwrap();
        assert_eq!(v["detail"]["status"], "model_download_failed");
        assert!(!v["detail"]["message"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn speech_failure_keeps_400_for_invalid_request() {
        let resp = speech_failure(AppError::InvalidRequest("bad json".into()), "M1");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(resp.headers().get("x-slc-local-tts-error").is_none());
        let v: serde_json::Value = serde_json::from_slice(&body_of(resp).await).unwrap();
        assert_eq!(v["detail"]["status"], "invalid_request");
    }

    #[test]
    fn budget_messages_name_the_phase() {
        let downloading = AppError::BudgetExceeded(BudgetPhase::Downloading);
        let speaking = AppError::BudgetExceeded(BudgetPhase::Speaking);
        assert!(downloading.message().contains("still downloading"));
        assert!(speaking.message().contains("too long"));
        assert_eq!(downloading.code(), "budget_exceeded");
        assert_eq!(speaking.code(), "budget_exceeded");
    }

    #[test]
    fn speech_failure_log_gives_advice() {
        let logs = capture_logs(|| {
            speech_failure(AppError::BudgetExceeded(BudgetPhase::Downloading), "M1");
        });
        assert!(logs.contains("failure sound"), "{logs}");
        assert!(logs.contains("slc-local-tts download M1"), "{logs}");

        // af_alloy is a Kokoro voice.
        let logs = capture_logs(|| {
            speech_failure(AppError::BudgetExceeded(BudgetPhase::Speaking), "af_alloy");
        });
        assert!(logs.contains("Piper or Supertonic"), "{logs}");

        // M1 is a Supertonic voice.
        let logs = capture_logs(|| {
            speech_failure(AppError::BudgetExceeded(BudgetPhase::Speaking), "M1");
        });
        assert!(logs.contains("num_threads"), "{logs}");
        assert!(!logs.contains("Kokoro"), "{logs}");
    }

    #[test]
    fn degraded_log_says_preview_played_silence() {
        let logs = capture_logs(|| {
            degraded(&AppError::BudgetExceeded(BudgetPhase::Downloading), "M1");
        });
        assert!(logs.contains("The preview played silence."), "{logs}");
        assert!(
            logs.contains("press preview again in a minute or two."),
            "{logs}"
        );
    }
}
