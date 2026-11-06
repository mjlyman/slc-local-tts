//! `GET /v1/voices`, `GET /v2/voices` and `GET /v1/voices/{id}`:
//! the catalog in the ElevenLabs voice-object shape SLC's voice picker reads.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::Serialize;

use super::error::AppError;
use super::{AppState, RequestHost};
use crate::catalog::{self, Accent, Gender, Voice};
use crate::engine::Synthesizer;
use crate::models::ModelStore;

/// The list wrapper. One page holds every voice.
#[derive(Debug, Serialize)]
pub struct VoiceList {
    pub voices: Vec<VoiceObject>,
    pub has_more: bool,
    pub total_count: usize,
    pub next_page_token: Option<String>,
}

/// One voice, in the shape (field order included) the old prototype proved
/// works with SLC. Most fields are fixed values SLC expects to exist.
#[derive(Debug, Serialize)]
pub struct VoiceObject {
    pub voice_id: &'static str,
    pub name: &'static str,
    /// Always empty.
    pub samples: Vec<serde_json::Value>,
    pub category: &'static str,
    pub fine_tuning: FineTuning,
    pub labels: Labels,
    pub description: &'static str,
    pub preview_url: String,
    /// Always empty.
    pub available_for_tiers: Vec<serde_json::Value>,
    pub settings: VoiceSettings,
    /// Always `null`.
    pub sharing: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct FineTuning {
    /// Always `{}`.
    pub state: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct Labels {
    pub engine: &'static str,
    // A voice with no known gender or accent has no such key at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gender: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<&'static str>,
    /// `installed` or `downloadable`.
    pub status: &'static str,
}

/// ElevenLabs' default voice settings; SLC sends its own speed per request.
#[derive(Debug, Serialize)]
pub struct VoiceSettings {
    pub stability: f32,
    pub similarity_boost: f32,
    pub style: f32,
    pub use_speaker_boost: bool,
    pub speed: f32,
}

/// `GET /v1/voices` and `GET /v2/voices`. Query parameters (`showlegacy`,
/// `voice_type`, ...) are ignored: SLC calls all four variants per refresh and
/// de-duplicates the answers.
pub async fn list_voices<S: ModelStore, Y: Synthesizer>(
    State(state): State<Arc<AppState<S, Y>>>,
    Extension(host): Extension<RequestHost>,
) -> Json<VoiceList> {
    let voices: Vec<VoiceObject> = catalog::voices()
        .iter()
        .map(|voice| voice_object(voice, state.store.is_installed(voice.archive), &host))
        .collect();
    Json(VoiceList {
        total_count: voices.len(),
        voices,
        has_more: false,
        next_page_token: None,
    })
}

/// `GET /v1/voices/{id}`: one voice, by ID or alias, ignoring case.
pub async fn get_voice<S: ModelStore, Y: Synthesizer>(
    State(state): State<Arc<AppState<S, Y>>>,
    Extension(host): Extension<RequestHost>,
    Path(id): Path<String>,
) -> Result<Json<VoiceObject>, AppError> {
    let voice = catalog::find(&id).ok_or(AppError::VoiceNotFound(id))?;
    let installed = state.store.is_installed(voice.archive);
    Ok(Json(voice_object(voice, installed, &host)))
}

fn voice_object(voice: &'static Voice, installed: bool, host: &RequestHost) -> VoiceObject {
    VoiceObject {
        voice_id: voice.id,
        name: voice.name,
        samples: Vec::new(),
        category: "premade",
        fine_tuning: FineTuning {
            state: serde_json::Map::new(),
        },
        labels: Labels {
            engine: voice.engine.as_str(),
            gender: voice.gender.map(gender_label),
            accent: voice.accent.map(accent_label),
            status: if installed {
                "installed"
            } else {
                "downloadable"
            },
        },
        description: voice.description,
        preview_url: format!("http://{}/v1/voices/{}/preview", host.0, voice.id),
        available_for_tiers: Vec::new(),
        settings: VoiceSettings {
            stability: 1.0,
            similarity_boost: 1.0,
            style: 0.0,
            use_speaker_boost: false,
            speed: 1.0,
        },
        sharing: None,
    }
}

fn gender_label(gender: Gender) -> &'static str {
    match gender {
        Gender::Female => "female",
        Gender::Male => "male",
        Gender::Neutral => "neutral",
    }
}

fn accent_label(accent: Accent) -> &'static str {
    match accent {
        Accent::American => "american",
        Accent::British => "british",
    }
}
