//! HTTP layer: shared state, the router, the host guard and the request log.
//! Handlers live in the submodules.
pub mod cooldown;
pub mod error;
pub mod speak;
pub mod voices;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{Method, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::normalize_path::NormalizePath;

use crate::config::BaseSpeeds;
use crate::engine::Synthesizer;
use crate::models::ModelStore;
use error::AppError;

/// Largest request body accepted. SLC's speech bodies are a few hundred bytes;
/// 5,000 characters of text fits comfortably.
const MAX_BODY_BYTES: usize = 65_536;

/// Host names a request may be addressed to. Compared ignoring ASCII case.
const ALLOWED_HOST_NAMES: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

/// How long a speech request may take before it fails with a 503. SLC gives
/// up at 10 s.
pub const SPEECH_BUDGET: Duration = Duration::from_secs(9);
/// How long a preview may take, downloads included, before it degrades. SLC
/// waits up to 100 s for a preview.
pub const PREVIEW_BUDGET: Duration = Duration::from_secs(60);

/// The settings the HTTP layer needs from `config.toml`.
#[derive(Debug, Clone)]
pub struct ApiSettings {
    /// The port the server is bound to; the host guard checks it.
    pub port: u16,
    /// How long a speech request may take before it fails.
    pub speech_budget: Duration,
    /// How long a preview may take before it degrades.
    pub preview_budget: Duration,
    pub base_speed: BaseSpeeds,
}

/// Everything the handlers share. Generic over the store and synthesizer so
/// tests can plug in fakes; production uses `DiskStore` and
/// `SherpaSynthesizer`.
pub struct AppState<S, Y> {
    pub store: Arc<S>,
    pub synth: Arc<Y>,
    /// Held for the whole of every `synthesize` call: one synthesis at a time.
    pub synth_lock: Arc<tokio::sync::Mutex<()>>,
    /// Archives whose model recently failed to load, so they aren't loaded
    /// again for a while.
    pub load_cooldown: cooldown::LoadCooldown,
    pub settings: ApiSettings,
}

/// The `Host` header of a request that passed the host guard, with the bound
/// port added if the client left it out. Handlers use it to build absolute
/// URLs (`preview_url`) that point back at the address the client used.
#[derive(Debug, Clone)]
pub struct RequestHost(pub String);

/// Builds the routes and middleware. Use [`app`] to serve it.
///
/// The handlers are generic functions (`list_voices::<S, Y>`), so the router
/// is monomorphized for the concrete store and synthesizer, like a C++
/// template instantiation: no trait objects, no dynamic dispatch.
pub fn router<S: ModelStore, Y: Synthesizer>(state: Arc<AppState<S, Y>>) -> Router {
    // A panic in a handler becomes the route's own failure response, so SLC
    // never sees a dropped connection. The speech routes share one handler.
    let speech = post(speak::speak::<S, Y>).layer(CatchPanicLayer::custom(speak::speech_panic));
    let preview = get(speak::preview::<S, Y>).layer(CatchPanicLayer::custom(speak::preview_panic));
    Router::new()
        .route("/", get(health))
        .route("/health", get(health))
        .route("/v1/voices", get(voices::list_voices::<S, Y>))
        .route("/v2/voices", get(voices::list_voices::<S, Y>))
        .route("/v1/voices/{id}", get(voices::get_voice::<S, Y>))
        .route("/v1/voices/{id}/preview", preview)
        .route("/v1/text-to-speech/{id}", speech.clone())
        .route("/v1/text-to-speech/{id}/stream", speech)
        .fallback(route_not_found)
        .method_not_allowed_fallback(method_not_allowed)
        // Layers wrap everything added above them, and a later `.layer` is
        // the outer one. So a request passes the request log first, then the
        // host guard, then the body limit, then reaches the handler. Logging
        // outermost means refused requests are logged too.
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), host_guard))
        .layer(middleware::from_fn(request_log))
        .with_state(state)
}

/// The router with trailing slashes trimmed (`/v1/voices/` → `/v1/voices`).
///
/// `NormalizePath` must wrap the router from the outside. A `Router` picks
/// its route before any of its own layers run, so as a `.layer` inside the
/// router it would rewrite the path too late to change which route matches.
pub fn app<S: ModelStore, Y: Synthesizer>(state: Arc<AppState<S, Y>>) -> NormalizePath<Router> {
    NormalizePath::trim_trailing_slash(router(state))
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

async fn route_not_found(method: Method, uri: Uri) -> AppError {
    // Warn level: SLC may call endpoints its logged traffic didn't show, and this
    // line is how we would find out. Path only: a query might carry secrets.
    tracing::warn!(%method, path = uri.path(), "Unknown route; answered 404.");
    AppError::RouteNotFound
}

async fn method_not_allowed() -> AppError {
    AppError::MethodNotAllowed
}

/// Refuses requests not addressed to this machine on the bound port.
///
/// This protects against DNS rebinding: a web page in the user's browser can
/// make its own domain resolve to 127.0.0.1 and then send requests here. The
/// browser still sends that domain as the `Host`, so checking `Host` stops it.
/// It matters because requests can start model downloads of hundreds of MB.
async fn host_guard<S: ModelStore, Y: Synthesizer>(
    State(state): State<Arc<AppState<S, Y>>>,
    mut request: Request,
    next: Next,
) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| validate_host(raw, state.settings.port));
    match host {
        Some(host) => {
            request.extensions_mut().insert(RequestHost(host));
            next.run(request).await
        }
        None => AppError::ForbiddenHost.into_response(),
    }
}

/// Checks a `Host` header value. Returns it with `:<port>` appended when it
/// had no port, or `None` if the name or port isn't allowed.
fn validate_host(raw: &str, port: u16) -> Option<String> {
    // Split "name:port". An IPv6 name is bracketed and contains colons
    // itself, so for it the port can only follow the closing bracket.
    let (name, sent_port) = if raw.starts_with('[') {
        let close = raw.find(']')?;
        let (name, rest) = raw.split_at(close + 1);
        match rest {
            "" => (name, None),
            _ => (name, Some(rest.strip_prefix(':')?)),
        }
    } else {
        match raw.split_once(':') {
            Some((name, sent_port)) => (name, Some(sent_port)),
            None => (raw, None),
        }
    };
    if !ALLOWED_HOST_NAMES
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(name))
    {
        return None;
    }
    match sent_port {
        None => Some(format!("{raw}:{port}")),
        Some(sent) if sent.parse::<u16>().ok() == Some(port) => Some(raw.to_string()),
        Some(_) => None,
    }
}

/// Logs one line per request: method, path, status and time taken. Never
/// headers (SLC sends its `xi-api-key` there) and never the query string.
async fn request_log(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let started = Instant::now();
    let response = next.run(request).await;
    tracing::info!(
        %method,
        path,
        status = response.status().as_u16(),
        ms = started.elapsed().as_millis() as u64,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::validate_host;

    #[test]
    fn validate_host_cases() {
        assert_eq!(
            validate_host("127.0.0.1:8765", 8765).as_deref(),
            Some("127.0.0.1:8765")
        );
        assert_eq!(
            validate_host("LOCALHOST", 8765).as_deref(),
            Some("LOCALHOST:8765")
        );
        assert_eq!(validate_host("[::1]", 8765).as_deref(), Some("[::1]:8765"));
        assert_eq!(
            validate_host("[::1]:8765", 8765).as_deref(),
            Some("[::1]:8765")
        );
        for bad in [
            "[::1]8765",
            "[::1",
            "[::2]:8765",
            "localhost:08765x",
            "localhost:8765:1",
            "127.0.0.1:99999",
        ] {
            assert_eq!(validate_host(bad, 8765), None, "{bad}");
        }
    }
}
