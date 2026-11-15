//! A fake backend, a harness that draws the settings window with no
//! screen, and snapshots of the server in typical states, shared by the
//! window's tests.
//!
//! Every file under `tests/` is its own crate and compiles this module
//! separately, so a helper only some of them use would warn as dead code in
//! the others.
#![allow(dead_code)]

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui;
use egui_kittest::Harness;

use slc_local_tts::api::error::BudgetPhase;
use slc_local_tts::api::slc::dto::{ActivityDto, DownloadDto, ModelDto, StatusDto, VoiceDto};
use slc_local_tts::catalog::{self, Archive};
use slc_local_tts::config::Config;
use slc_local_tts::models::{FailureKind, ModelState};
use slc_local_tts::status::records::{RequestRecord, Route};
use slc_local_tts::ui::app::WindowApp;
use slc_local_tts::ui::backend::{Action, Backend, Connection, Snapshot};
use slc_local_tts::ui::{fonts, theme};
use slc_local_tts::update::UpdateState;

/// "Now" in every fixture, in Unix milliseconds.
pub const NOW_MS: u64 = 1_800_000_000_000;

/// [`NOW_MS`] as a time: the clock the window reads in every test, so
/// "2 minutes ago" and the last hour's trace don't depend on today's date.
pub fn fixed_now() -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(NOW_MS)
}

// ---------------------------------------------------------------- FakeBackend

/// A backend that shows a fixed snapshot and records every action.
#[derive(Default)]
pub struct FakeBackend {
    pub snapshot: Mutex<Arc<Snapshot>>,
    pub actions: Mutex<Vec<Action>>,
}

impl FakeBackend {
    pub fn new(snapshot: Snapshot) -> Arc<FakeBackend> {
        Arc::new(FakeBackend {
            snapshot: Mutex::new(Arc::new(snapshot)),
            actions: Mutex::default(),
        })
    }

    /// Every action so far, oldest first.
    pub fn actions(&self) -> Vec<Action> {
        self.actions.lock().unwrap().clone()
    }

    /// Shows `snapshot` from the next frame on.
    pub fn set_snapshot(&self, snapshot: Snapshot) {
        *self.snapshot.lock().unwrap() = Arc::new(snapshot);
    }
}

impl Backend for FakeBackend {
    fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&self.snapshot.lock().unwrap())
    }

    fn act(&self, action: Action) {
        self.actions.lock().unwrap().push(action);
    }
}

thread_local! {
    /// The fake behind the latest [`harness`] on this thread. The harness
    /// owns the window, which keeps its backend private, so the tests reach
    /// the fake through here instead. Each test runs on a thread of its
    /// own, so tests never see each other's fakes.
    static BACKEND: RefCell<Option<Arc<FakeBackend>>> = const { RefCell::new(None) };
}

/// The fake backend of this thread's latest [`harness`].
pub fn backend() -> Arc<FakeBackend> {
    BACKEND.with(|slot| slot.borrow().clone().expect("call harness() first"))
}

/// Every action the window has sent since this thread's latest
/// [`harness`] started.
pub fn actions() -> Vec<Action> {
    backend().actions()
}

/// The settings window over a [`FakeBackend`] showing `snapshot`, drawn
/// with no screen. It uses the real styles and Windows fonts, as the
/// window does.
pub fn harness(snapshot: Snapshot, first_run: bool) -> Harness<'static, WindowApp> {
    sized_harness(snapshot, first_run, None)
}

/// [`harness`] in a screen `width` points wide (and the window's minimum
/// height). egui_kittest puts an 8-point margin around the app, so the
/// window itself gets `width - 16` points.
pub fn narrow_harness(snapshot: Snapshot, width: f32) -> Harness<'static, WindowApp> {
    sized_harness(snapshot, false, Some(egui::vec2(width, 480.0)))
}

/// [`harness`] in a screen `size` points big, or egui_kittest's default
/// 800 × 600 for `None`.
pub fn sized_harness(
    snapshot: Snapshot,
    first_run: bool,
    size: Option<egui::Vec2>,
) -> Harness<'static, WindowApp> {
    let backend = FakeBackend::new(snapshot);
    BACKEND.with(|slot| *slot.borrow_mut() = Some(Arc::clone(&backend)));
    let mut app = WindowApp::new(backend, first_run);
    app.clock = fixed_now;
    // The real styles and fonts, installed before the first frame as the
    // window does, so a style or page naming a font family the fonts don't
    // define fails here rather than only in the real window.
    let mut builder = Harness::builder().with_context_setup(|ctx| {
        theme::install(ctx);
        fonts::install(ctx, fonts::definitions(&fonts::windows_fonts_dir()));
    });
    if let Some(size) = size {
        builder = builder.with_size(size);
    }
    builder.build_ui_state(|ui, app: &mut WindowApp| app.draw(ui), app)
}

// ---------------------------------------------------------------- fixtures

fn archive_of(voice_id: &str) -> &'static Archive {
    catalog::find(voice_id).unwrap().archive
}

/// The Kokoro voices' archive.
pub fn kokoro() -> &'static Archive {
    archive_of("af_alloy")
}

/// The Supertonic voices' archive.
pub fn supertonic() -> &'static Archive {
    archive_of("M1")
}

/// Joe's archive (a Piper voice).
pub fn joe() -> &'static Archive {
    archive_of("en_US-joe-medium")
}

/// The status of an idle server that SLC last used two minutes ago.
pub fn idle_status() -> StatusDto {
    StatusDto {
        app: "SLC Local TTS".to_string(),
        version: "0.3.0".to_string(),
        port: 8765,
        state: ActivityDto::Idle,
        last_request_ms: Some(NOW_MS - 120_000),
        ever_connected: true,
        recent_failures: 0,
        failed_downloads: 0,
        downloads: Vec::new(),
        latest_seq: 0,
        update: UpdateState::UpToDate,
        can_restart_to_update: true,
        updated_to: None,
        config: Config::default(),
    }
}

/// A server that answers and is idle.
pub fn ready() -> Snapshot {
    Snapshot {
        connection: Connection::Connected,
        status: Some(idle_status()),
        ..Snapshot::default()
    }
}

/// Speaking an 87-character line with Joe.
pub fn speaking_joe() -> Snapshot {
    let mut status = idle_status();
    status.state = ActivityDto::Speaking {
        voice_id: "en_US-joe-medium".to_string(),
        chars: 87,
        started_ms: NOW_MS - 300,
    };
    Snapshot {
        status: Some(status),
        ..ready()
    }
}

/// Downloading the Kokoro voices, `done` bytes so far.
pub fn downloading_kokoro(done: u64) -> Snapshot {
    let mut status = idle_status();
    let total = kokoro().size;
    status.state = ActivityDto::Downloading {
        stem: kokoro().stem.to_string(),
        done,
        total,
    };
    status.downloads = vec![DownloadDto {
        stem: kokoro().stem.to_string(),
        done,
        total,
    }];
    Snapshot {
        status: Some(status),
        ..ready()
    }
}

/// A server that answers, but SLC has never sent it a request.
pub fn never_connected() -> Snapshot {
    let mut status = idle_status();
    status.ever_connected = false;
    status.last_request_ms = None;
    Snapshot {
        status: Some(status),
        ..ready()
    }
}

/// Idle, with `failures` announcements failed in the last hour and the
/// [`records_mixed`] records.
pub fn with_failures(failures: u32) -> Snapshot {
    let mut status = idle_status();
    status.recent_failures = failures;
    Snapshot {
        status: Some(status),
        records: records_mixed(),
        ..ready()
    }
}

/// The server stopped answering while the window was open. The last status
/// it sent is kept.
pub fn stopped() -> Snapshot {
    Snapshot {
        connection: Connection::Lost,
        ..ready()
    }
}

/// An update to `version` downloaded and ready to install.
pub fn update_ready(version: &str) -> Snapshot {
    let mut status = idle_status();
    status.update = UpdateState::Ready {
        version: version.to_string(),
    };
    Snapshot {
        status: Some(status),
        ..ready()
    }
}

/// `archive`'s row with every catalog voice it holds.
fn model(archive: &'static Archive, state: ModelState) -> ModelDto {
    let voices = catalog::voices()
        .iter()
        .filter(|voice| voice.archive.stem == archive.stem)
        .map(|voice| VoiceDto {
            id: voice.id.to_string(),
            name: voice.name.to_string(),
            gender: None,
            accent: None,
            slow: voice.slow,
            license: voice.license.to_string(),
        })
        .collect();
    let engine = catalog::voices()
        .iter()
        .find(|voice| voice.archive.stem == archive.stem)
        .map_or("piper", |voice| match voice.engine {
            catalog::Engine::Piper => "piper",
            catalog::Engine::Kokoro => "kokoro",
            catalog::Engine::Supertonic => "supertonic",
        });
    ModelDto {
        stem: archive.stem.to_string(),
        engine: engine.to_string(),
        size: archive.size,
        state,
        voices,
    }
}

/// Supertonic installed, Kokoro 161 MB into its download, Joe not
/// installed, and another Piper voice whose download failed for want of a
/// connection.
pub fn voices_mixed() -> Vec<ModelDto> {
    vec![
        model(supertonic(), ModelState::Installed),
        model(
            kokoro(),
            ModelState::Downloading {
                done: 161_000_000,
                total: kokoro().size,
            },
        ),
        model(joe(), ModelState::NotInstalled),
        model(
            archive_of("en_US-ljspeech-medium"),
            ModelState::Failed {
                reason: "no internet connection".to_string(),
                kind: FailureKind::Network,
            },
        ),
    ]
}

fn record(seq: u64, route: Route, voice_id: &str, text: &str, outcome: &str) -> RequestRecord {
    RequestRecord {
        seq,
        at_ms: NOW_MS - 60_000 * (10 - seq),
        route,
        voice_id: voice_id.to_string(),
        text: text.to_string(),
        outcome: outcome.to_string(),
        phase: None,
        reason: None,
        message: None,
        audio_seconds: None,
        ms: 400,
    }
}

/// A spoken line, a line that missed SLC's time limit while its voice
/// downloaded, a preview that played as silence, and an unspeakable line;
/// oldest first.
pub fn records_mixed() -> Vec<RequestRecord> {
    let mut spoken = record(
        1,
        Route::Speech,
        "en_US-joe-medium",
        "Ladies and gentlemen, welcome aboard.",
        "spoken",
    );
    spoken.audio_seconds = Some(6.1);

    let mut late = record(
        2,
        Route::Speech,
        "af_alloy",
        "Cabin crew, please take your seats for departure.",
        "budget_exceeded",
    );
    late.phase = Some(BudgetPhase::Downloading);
    late.message = Some("The voice is still downloading.".to_string());
    late.ms = 9_000;

    let mut preview = record(
        3,
        Route::Preview,
        "en_US-ljspeech-medium",
        "Hello",
        "model_download_failed",
    );
    preview.reason = Some("no internet connection".to_string());
    preview.message = Some("The voice couldn't be downloaded.".to_string());

    let silent = record(4, Route::Speech, "M1", "...", "silent");

    vec![spoken, late, preview, silent]
}

/// Every download in the catalog, none installed: a list far longer than
/// the window.
pub fn voices_all() -> Vec<ModelDto> {
    let mut stems: Vec<&'static Archive> = Vec::new();
    for voice in catalog::voices() {
        if !stems
            .iter()
            .any(|archive| archive.stem == voice.archive.stem)
        {
            stems.push(voice.archive);
        }
    }
    stems
        .into_iter()
        .map(|archive| model(archive, ModelState::NotInstalled))
        .collect()
}

/// `count` spoken lines, oldest first, each saying "Line number {seq}.".
pub fn numbered_records(count: u64) -> Vec<RequestRecord> {
    (1..=count)
        .map(|seq| RequestRecord {
            seq,
            at_ms: NOW_MS - 1_000 * (count - seq),
            text: format!("Line number {seq}."),
            audio_seconds: Some(2.0),
            // `record` works out its time from a seq of 10 or less, so
            // only its other fields are used.
            ..record(1, Route::Speech, "M1", "", "spoken")
        })
        .collect()
}
