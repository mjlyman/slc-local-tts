//! The private `/slc/` routes the settings window reads and drives: the
//! status, the voice models, downloads, deletes, the recent requests and
//! "Restart and update now".
//!
//! Only the window may use them. Every request must carry
//! [`CLIENT_HEADER`]`: `[`CLIENT_VALUE`], and any request with an `Origin`
//! header is refused. A web page can't add a custom header to a request to
//! another site without first asking the server's permission (a CORS
//! "preflight" `OPTIONS` request, which always carries `Origin`), and this
//! server never gives it, so pages in the user's browser can't reach these
//! routes. The host guard applies to them too.

pub mod dto;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::SystemTime;

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;

use super::AppState;
use super::error::{AppError, error_response};
use super::manage::delete_model;
use super::voices::{accent_label, gender_label};
use crate::catalog::{self, Archive};
use crate::engine::Synthesizer;
use crate::models::{DeleteError, ModelState, ModelStore, StartDownload};
use crate::status::unix_ms;
use crate::stop::StopReason;
use crate::update::UpdateState;
use dto::{ActivityDto, DownloadDto, ModelDto, RequestsDto, StatusDto, VoiceDto};

/// The header every request to these routes must carry...
pub const CLIENT_HEADER: &str = "X-SLC-Local-TTS-Client";
/// ...with this value.
pub const CLIENT_VALUE: &str = "window";

/// Every private route's path starts with this.
pub const PREFIX: &str = "/slc/";

/// The app's name, as the status route reports it.
const APP_NAME: &str = "SLC Local TTS";

/// The answer when the delete's own task fails, which only a bug causes.
const DELETE_STOPPED: &str = "An internal error stopped the delete. Try again; if it keeps happening, restart SLC Local TTS.";

/// The private routes, each behind the client guard. `api::router` merges
/// them in before its own layers, so the host guard and the request log
/// wrap them too.
///
/// The return type is `Router<Arc<AppState<..>>>`: a router that still needs
/// its state, which `api::router` supplies with `with_state` after merging.
pub fn routes<S: ModelStore, Y: Synthesizer>() -> Router<Arc<AppState<S, Y>>> {
    Router::new()
        .route("/slc/status", get(status::<S, Y>))
        .route("/slc/voices", get(voices::<S, Y>))
        .route("/slc/models/{stem}/download", post(download::<S, Y>))
        .route("/slc/models/{stem}", delete(remove::<S, Y>))
        .route("/slc/requests", get(requests::<S, Y>))
        .route("/slc/update/apply", post(apply_update::<S, Y>))
        // The 405 answer for a known path asked with another method (such
        // as a browser's `OPTIONS` preflight), set here so the guard below
        // wraps it. Without it, `api::router`'s own 405 handler would be
        // put in after the guard, outside it.
        .method_not_allowed_fallback(super::method_not_allowed)
        // `route_layer` runs the guard only for requests that match one of
        // the paths above (any method), so an unknown `/slc/...` path still
        // gets the ordinary 404.
        .route_layer(middleware::from_fn(client_guard))
}

/// Refuses every request that isn't from the settings window.
async fn client_guard(request: Request, next: Next) -> Response {
    let headers = request.headers();
    // Checked first, so a browser's preflight (`OPTIONS` with an `Origin`)
    // is refused before anything else looks at it.
    if headers.contains_key(header::ORIGIN) {
        return AppError::ForbiddenClient.into_response();
    }
    let from_window = headers
        .get(CLIENT_HEADER)
        .is_some_and(|value| value.as_bytes() == CLIENT_VALUE.as_bytes());
    if !from_window {
        return AppError::ForbiddenClient.into_response();
    }
    next.run(request).await
}

/// What `GET /slc/status` answers, as of `now`. The tray reads it too.
pub fn status_snapshot<S: ModelStore, Y: Synthesizer>(
    state: &AppState<S, Y>,
    now: SystemTime,
) -> StatusDto {
    let live = state.live.snapshot();
    let mut downloads = Vec::new();
    let mut failed_downloads = 0;
    for archive in archives() {
        match state.store.state(archive) {
            ModelState::Downloading { done, total } => downloads.push(DownloadDto {
                stem: archive.stem.to_string(),
                done,
                total,
            }),
            ModelState::Failed { .. } => failed_downloads += 1,
            ModelState::NotInstalled | ModelState::Installed => {}
        }
    }
    // A line being spoken matters more than a download in the background.
    let activity = match (&live.speaking, downloads.first()) {
        (Some(speaking), _) => ActivityDto::Speaking {
            voice_id: speaking.voice_id.to_string(),
            chars: speaking.chars,
            started_ms: unix_ms(speaking.started),
        },
        (None, Some(download)) => ActivityDto::Downloading {
            stem: download.stem.clone(),
            done: download.done,
            total: download.total,
        },
        (None, None) => ActivityDto::Idle,
    };
    let info = &state.app_info;
    StatusDto {
        app: APP_NAME.to_string(),
        version: info.version.to_string(),
        port: state.settings.port,
        state: activity,
        last_request_ms: live.last_request.map(unix_ms),
        ever_connected: live.ever_connected,
        recent_failures: state.live.recent_failures(now),
        failed_downloads,
        downloads,
        latest_seq: live.latest_seq,
        // `borrow` reads the receiver's current value; the clone ends the
        // borrow at once, so the update check is never kept waiting.
        update: info.updates.borrow().clone(),
        can_restart_to_update: info.can_restart_to_update,
        updated_to: info.updated_to.clone(),
        config: info.config.clone(),
    }
}

/// `GET /slc/status`.
async fn status<S: ModelStore, Y: Synthesizer>(
    State(state): State<Arc<AppState<S, Y>>>,
) -> Json<StatusDto> {
    Json(status_snapshot(&state, SystemTime::now()))
}

/// `GET /slc/voices`: every model, in catalog order, with its voices.
async fn voices<S: ModelStore, Y: Synthesizer>(
    State(state): State<Arc<AppState<S, Y>>>,
) -> Json<Vec<ModelDto>> {
    let models = archives()
        .into_iter()
        .map(|archive| {
            let voices: Vec<VoiceDto> = catalog::voices()
                .iter()
                .filter(|voice| voice.archive.stem == archive.stem)
                .map(|voice| VoiceDto {
                    id: voice.id.to_string(),
                    name: voice.name.to_string(),
                    gender: voice.gender.map(|g| gender_label(g).to_string()),
                    accent: voice.accent.map(|a| accent_label(a).to_string()),
                    slow: voice.slow,
                    license: voice.license.to_string(),
                })
                .collect();
            ModelDto {
                stem: archive.stem.to_string(),
                // Every voice of an archive has the same engine.
                engine: engine_of(archive).to_string(),
                size: archive.size,
                state: state.store.state(archive),
                voices,
            }
        })
        .collect();
    Json(models)
}

/// `POST /slc/models/{stem}/download`: starts the download, or joins the
/// one running, and answers at once.
async fn download<S: ModelStore, Y: Synthesizer>(
    State(state): State<Arc<AppState<S, Y>>>,
    Path(stem): Path<String>,
) -> Result<StatusCode, AppError> {
    let archive = find_archive(&stem).ok_or(AppError::ModelNotFound(stem))?;
    match state.store.start_download(archive) {
        StartDownload::Started
        | StartDownload::AlreadyDownloading
        | StartDownload::AlreadyInstalled => Ok(StatusCode::ACCEPTED),
        StartDownload::Refused(error) => Err(AppError::DownloadRefused(error.reason)),
    }
}

/// `DELETE /slc/models/{stem}`: 204 once the model is gone, including when
/// it wasn't installed.
async fn remove<S: ModelStore, Y: Synthesizer>(
    State(state): State<Arc<AppState<S, Y>>>,
    Path(stem): Path<String>,
) -> Response {
    let Some(archive) = find_archive(&stem) else {
        return AppError::ModelNotFound(stem).into_response();
    };
    // hyper drops a handler's future when the client hangs up, which would
    // abandon a delete still waiting for the synthesis lock. A spawned task
    // belongs to the runtime instead, so the delete always runs to the end;
    // this handler only waits for its result. The task owns its own `Arc`
    // of the state, because it may outlive this handler.
    let task_state = Arc::clone(&state);
    let joined = tokio::spawn(async move { delete_model(&task_state, archive).await }).await;
    match joined {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(DeleteError::Downloading)) => AppError::ModelBusy.into_response(),
        // Its text already says what happened and what to do.
        Ok(Err(error @ DeleteError::Io(_))) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            &error.to_string(),
        ),
        // The task panicked: a bug. The detail is for the log only.
        Err(error) => {
            tracing::error!(stem = archive.stem, error = %error, "the delete task failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                DELETE_STOPPED,
            )
        }
    }
}

/// `POST /slc/update/apply`: "Restart and update now". With an update
/// ready, in a program that can restart itself, it asks the server to stop
/// and answers 202 at once; the program installs the update once the server
/// has stopped. Otherwise 409.
async fn apply_update<S: ModelStore, Y: Synthesizer>(
    State(state): State<Arc<AppState<S, Y>>>,
) -> Result<StatusCode, AppError> {
    let info = &state.app_info;
    // `matches!` reads the value inside the borrow, which ends with the
    // statement, so the update check is never kept waiting.
    let ready = matches!(*info.updates.borrow(), UpdateState::Ready { .. });
    if !ready || !info.can_restart_to_update {
        return Err(AppError::NoUpdate);
    }
    tracing::info!("Restart and update now chosen in the settings window");
    // The server finishes the requests in progress, this answer included,
    // before it stops.
    info.stop.request(StopReason::RestartToUpdate);
    Ok(StatusCode::ACCEPTED)
}

/// The query of `GET /slc/requests`; a missing `after` means 0 (all).
#[derive(Debug, Deserialize)]
struct After {
    #[serde(default)]
    after: u64,
}

/// `GET /slc/requests?after={seq}`: the records newer than `seq`.
async fn requests<S: ModelStore, Y: Synthesizer>(
    State(state): State<Arc<AppState<S, Y>>>,
    query: Result<Query<After>, QueryRejection>,
) -> Result<Json<RequestsDto>, AppError> {
    let Query(After { after }) =
        query.map_err(|rejection| AppError::InvalidRequest(rejection.body_text()))?;
    // `latest_seq` is read before the records, so a record that arrives in
    // between is in `records` and the newest `seq` covers it. The window
    // asks for `after = latest_seq` next time and never misses one.
    let latest_seq = state.live.snapshot().latest_seq;
    let records = state.live.records_after(after);
    let latest_seq = records
        .last()
        .map_or(latest_seq, |record| record.seq.max(latest_seq));
    Ok(Json(RequestsDto {
        records,
        latest_seq,
    }))
}

/// Every archive in the catalog, once each, in the order its first voice
/// appears. Several voices share one archive (all Kokoro voices do).
fn archives() -> Vec<&'static Archive> {
    let mut seen = HashSet::new();
    catalog::voices()
        .iter()
        .map(|voice| voice.archive)
        // `insert` returns false for a stem already seen.
        .filter(|archive| seen.insert(archive.stem))
        .collect()
}

/// The catalog archive called `stem`, if there is one.
fn find_archive(stem: &str) -> Option<&'static Archive> {
    catalog::voices()
        .iter()
        .map(|voice| voice.archive)
        .find(|archive| archive.stem == stem)
}

/// The engine of `archive`'s voices.
fn engine_of(archive: &'static Archive) -> &'static str {
    catalog::voices()
        .iter()
        .find(|voice| voice.archive.stem == archive.stem)
        .map_or("?", |voice| voice.engine.as_str())
}
