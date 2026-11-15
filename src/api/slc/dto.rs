//! The JSON the private `/slc/` routes send. The settings window reads it
//! back into these same types, so every type both serializes and
//! deserializes.

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::models::ModelState;
use crate::status::records::RequestRecord;
use crate::update::UpdateState;

/// `GET /slc/status`: the app, what it's doing, and its settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusDto {
    /// Always "SLC Local TTS".
    pub app: String,
    pub version: String,
    /// The port the server is listening on.
    pub port: u16,
    pub state: ActivityDto,
    /// When SLC last asked for something, in Unix milliseconds.
    pub last_request_ms: Option<u64>,
    /// True once SLC has ever sent a request, in this run or an earlier one.
    pub ever_connected: bool,
    /// Announcements that failed in the last hour.
    pub recent_failures: u32,
    /// Voice models whose last download failed.
    pub failed_downloads: u32,
    /// Every download running now, in catalog order.
    pub downloads: Vec<DownloadDto>,
    /// The `seq` of the newest request record (0 before the first).
    pub latest_seq: u64,
    pub update: UpdateState,
    pub can_restart_to_update: bool,
    /// The version this copy was just updated to, right after an update.
    pub updated_to: Option<String>,
    /// The settings the server started with.
    pub config: Config,
}

/// What the server is busy with. It serializes with a `state` tag, for
/// example `{"state":"speaking","voice_id":"M1","chars":42,"started_ms":..}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ActivityDto {
    Idle,
    Speaking {
        voice_id: String,
        chars: usize,
        /// When the line started, in Unix milliseconds.
        started_ms: u64,
    },
    /// The first download in catalog order, when several run.
    Downloading {
        stem: String,
        done: u64,
        total: u64,
    },
}

/// One download in progress: bytes done out of the archive's size.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DownloadDto {
    pub stem: String,
    pub done: u64,
    pub total: u64,
}

/// `GET /slc/voices`: one downloadable model and the voices it holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelDto {
    /// The archive's name, which the download and delete routes take.
    pub stem: String,
    pub engine: String,
    /// The download's size in bytes.
    pub size: u64,
    /// `flatten` puts the state's fields into this object rather than under
    /// a key of their own: `"state":"downloading","done":..,"total":..`.
    #[serde(flatten)]
    pub state: ModelState,
    pub voices: Vec<VoiceDto>,
}

/// One voice of a model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VoiceDto {
    pub id: String,
    pub name: String,
    pub gender: Option<String>,
    pub accent: Option<String>,
    /// True for voices that are slow on most computers (Kokoro).
    pub slow: bool,
    pub license: String,
}

/// `GET /slc/requests?after={seq}`: the records newer than `seq`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequestsDto {
    /// Oldest first.
    pub records: Vec<RequestRecord>,
    /// The `seq` of the newest record the server holds.
    pub latest_seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::FailureKind;

    fn model(state: ModelState) -> ModelDto {
        ModelDto {
            stem: "kokoro-multi-lang-v1_0".to_string(),
            engine: "kokoro".to_string(),
            size: 350_000_000,
            state,
            voices: vec![VoiceDto {
                id: "af_alloy".to_string(),
                name: "Alloy".to_string(),
                gender: Some("female".to_string()),
                accent: None,
                slow: true,
                license: "Apache-2.0".to_string(),
            }],
        }
    }

    #[test]
    fn model_dto_round_trips_every_state() {
        let states = [
            ModelState::NotInstalled,
            ModelState::Installed,
            ModelState::Downloading {
                done: 161_000_000,
                total: 350_000_000,
            },
            ModelState::Failed {
                reason: "no internet".to_string(),
                kind: FailureKind::Network,
            },
            ModelState::Failed {
                reason: "the file changed".to_string(),
                kind: FailureKind::Permanent,
            },
            ModelState::Failed {
                reason: "the disk is full".to_string(),
                kind: FailureKind::Other,
            },
        ];
        for state in states {
            let dto = model(state);
            let json = serde_json::to_string(&dto).unwrap();
            let back: ModelDto = serde_json::from_str(&json).unwrap();
            assert_eq!(back, dto, "{json}");
        }
    }

    #[test]
    fn model_dto_flattens_its_state() {
        let downloading =
            serde_json::to_value(model(ModelState::Downloading { done: 1, total: 2 })).unwrap();
        assert_eq!(downloading["state"], "downloading");
        assert_eq!(downloading["done"], 1);
        assert_eq!(downloading["total"], 2);

        let failed = serde_json::to_value(model(ModelState::Failed {
            reason: "no internet".to_string(),
            kind: FailureKind::Network,
        }))
        .unwrap();
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["reason"], "no internet");
        assert_eq!(failed["kind"], "network");
    }

    /// The status the window reads back: a line being spoken, a download,
    /// an update ready, and settings other than the defaults.
    fn busy_status() -> StatusDto {
        let config = Config {
            port: 9100,
            num_threads: 3,
            check_for_updates: false,
            ..Config::default()
        };
        StatusDto {
            app: "SLC Local TTS".to_string(),
            version: "0.3.0".to_string(),
            port: 9100,
            state: ActivityDto::Speaking {
                voice_id: "en_US-joe-medium".to_string(),
                chars: 87,
                started_ms: 1_800_000_000_000,
            },
            last_request_ms: Some(1_799_999_880_000),
            ever_connected: true,
            recent_failures: 1,
            failed_downloads: 1,
            downloads: vec![DownloadDto {
                stem: "kokoro-multi-lang-v1_0".to_string(),
                done: 161_000_000,
                total: 350_000_000,
            }],
            latest_seq: 42,
            update: UpdateState::Ready {
                version: "1.4.0".to_string(),
            },
            can_restart_to_update: true,
            updated_to: Some("0.3.0".to_string()),
            config,
        }
    }

    #[test]
    fn status_dto_round_trips() {
        let mut statuses = vec![busy_status()];
        for update in [
            UpdateState::NotInstalled,
            UpdateState::Off,
            UpdateState::UpToDate,
            UpdateState::Downloading,
            UpdateState::Failed {
                reason: "no internet".to_string(),
            },
        ] {
            let mut status = busy_status();
            status.update = update;
            status.state = ActivityDto::Idle;
            status.updated_to = None;
            statuses.push(status);
        }
        for status in statuses {
            let json = serde_json::to_string(&status).unwrap();
            let back: StatusDto = serde_json::from_str(&json).unwrap();
            assert_eq!(back, status, "{json}");
        }
    }

    #[test]
    fn requests_dto_round_trips() {
        use crate::api::error::BudgetPhase;
        use crate::status::records::Route;

        let spoken = RequestRecord {
            seq: 41,
            at_ms: 1_800_000_000_000,
            route: Route::Speech,
            voice_id: "M1".to_string(),
            text: "Welcome aboard, Jürgen.".to_string(),
            outcome: "spoken".to_string(),
            phase: None,
            reason: None,
            message: None,
            audio_seconds: Some(6.1),
            ms: 420,
        };
        let failed = RequestRecord {
            seq: 42,
            route: Route::Preview,
            outcome: "budget_exceeded".to_string(),
            phase: Some(BudgetPhase::Downloading),
            reason: Some("still downloading".to_string()),
            message: Some("The voice wasn't installed yet.".to_string()),
            audio_seconds: None,
            ms: 60_000,
            ..spoken.clone()
        };
        let dto = RequestsDto {
            records: vec![spoken, failed],
            latest_seq: 42,
        };
        let json = serde_json::to_string(&dto).unwrap();
        let back: RequestsDto = serde_json::from_str(&json).unwrap();
        assert_eq!(back, dto, "{json}");
    }

    #[test]
    fn activity_is_tagged() {
        let speaking = ActivityDto::Speaking {
            voice_id: "M1".to_string(),
            chars: 42,
            started_ms: 7,
        };
        let json = serde_json::to_value(&speaking).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"state": "speaking", "voice_id": "M1", "chars": 42, "started_ms": 7})
        );
        assert_eq!(
            serde_json::to_value(ActivityDto::Idle).unwrap(),
            serde_json::json!({"state": "idle"})
        );
        let back: ActivityDto = serde_json::from_value(json).unwrap();
        assert_eq!(back, speaking);
    }
}
