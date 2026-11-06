//! End-to-end tests with the real models: real downloads (`DiskStore` +
//! `HttpInstaller`), real phonemizing and real synthesis
//! (`SherpaSynthesizer`), all through the HTTP app.
//!
//! Every test is `#[ignore]`: run them with `cargo test -- --ignored`. The
//! models go into `SLC_LOCAL_TTS_HOME` if it is set, else
//! `target/real-model-home`, so they download only once (about 600 MB the
//! first time).
//!
//! No port is bound: requests go straight into the app with `oneshot`.
//!
//! Windows only: the `redirect_temp` safety argument in `setup` relies on
//! Windows' thread-safe environment.
#![cfg(windows)]

mod common;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::StatusCode;
use serde_json::json;
use tower_http::normalize_path::NormalizePath;

use common::{PORT, TestResponse, post_json};
use slc_local_tts::api::cooldown::LoadCooldown;
use slc_local_tts::api::{ApiSettings, AppState, app};
use slc_local_tts::config::Config;
use slc_local_tts::engine::sherpa::SherpaSynthesizer;
use slc_local_tts::frontend::espeak::{self, CHECK_IPA, CHECK_TEXT};
use slc_local_tts::models::DiskStore;
use slc_local_tts::models::install::HttpInstaller;
use slc_local_tts::paths::DataDir;

/// Long enough for a first run's downloads, so a slow connection doesn't
/// turn into a 503. The real server uses `SPEECH_BUDGET` (9 s).
const BUDGET: Duration = Duration::from_secs(600);

/// One test at a time. Tests that share a voice would otherwise download
/// the same archive into the same folder at once from separate stores, and
/// parallel synthesis would make the timings meaningless. A tokio mutex,
/// because the guard is held across `.await`.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The data folder: `SLC_LOCAL_TTS_HOME` if set, else
/// `<crate>/target/real-model-home`.
fn data_dir() -> DataDir {
    if std::env::var_os("SLC_LOCAL_TTS_HOME").is_some_and(|v| !v.is_empty()) {
        DataDir::from_env().unwrap()
    } else {
        DataDir::at(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("real-model-home"),
        )
    }
}

/// Points `TMP`/`TEMP` at the data folder's private espeak dir and runs the
/// startup check, once per process, as `main` and `server::start` do. Every
/// test calls this first. Returns the check's outcome, so each test fails
/// with the reason instead of a poisoned `Once`.
fn setup() -> &'static Result<(), String> {
    static SETUP: OnceLock<Result<(), String>> = OnceLock::new();
    SETUP.get_or_init(|| {
        let dir = espeak::private_temp_dir(&data_dir().espeak_root());
        // SAFETY: `set_var` is `unsafe` because on some platforms another
        // thread reading the environment at the same time can see freed
        // memory. This binary is Windows only (`#![cfg(windows)]` at the top),
        // where std documents it as always safe, even with several threads,
        // because Windows' own environment functions are thread-safe.
        // Libtest's threads are running, but no native thread
        // (espeak or sherpa) exists yet. `OnceLock` plus `SERIAL` make sure
        // TMP is set before any test phonemizes. Like tests/espeak_real.rs,
        // this binary is its own process, so the change can't leak into
        // other tests. `_temp` puts TMP back when this closure ends, after
        // `verify_startup`, the process's first phonemize call, has returned.
        let _temp = unsafe { espeak::redirect_temp(&dir) };
        espeak::verify_startup(&dir, espeak::ipa).map_err(|e| e.to_string())
    })
}

/// The real app: real store, real installer, real synthesizer, default
/// config, but 600 s budgets.
fn real_app() -> NormalizePath<Router> {
    let data = data_dir();
    let config = Config::default();
    app(Arc::new(AppState {
        store: DiskStore::new(data.models(), HttpInstaller::new()),
        synth: Arc::new(SherpaSynthesizer::new(&config)),
        synth_lock: Arc::new(tokio::sync::Mutex::new(())),
        load_cooldown: LoadCooldown::default(),
        settings: ApiSettings {
            // Matches the `Host` that `post_json` sends; nothing is bound.
            port: PORT,
            speech_budget: BUDGET,
            preview_budget: BUDGET,
            base_speed: config.base_speed,
        },
    }))
}

/// The length of a 16-bit mono WAV, from its header: data bytes ÷ (2 × rate).
fn wav_seconds(wav: &[u8]) -> f64 {
    assert!(wav.len() >= 44 && wav.starts_with(b"RIFF"), "not a WAV");
    let le_u32 = |at: usize| u32::from_le_bytes(wav[at..at + 4].try_into().unwrap());
    let rate = le_u32(24);
    let data_len = le_u32(40);
    assert_eq!(data_len as usize, wav.len() - 44, "WAV data length");
    f64::from(data_len) / (2.0 * f64::from(rate))
}

/// Speaks `text` with `voice` the way SLC asks (speed 1.2), checks it is a
/// clean 200 WAV and returns its length in seconds. Prints the timing so
/// `--nocapture` shows it.
async fn speak(voice: &str, text: &str) -> f64 {
    assert_eq!(setup(), &Ok(()), "espeak startup check");
    let app = real_app();
    let body = json!({
        "text": text,
        "model_id": "eleven_multilingual_v2",
        "voice_settings": {"stability": 0.5, "similarity_boost": 0.75, "speed": 1.2}
    });
    let uri = format!("/v1/text-to-speech/{voice}/stream?output_format=mp3_22050_32");

    let started = Instant::now();
    let resp: TestResponse = post_json(&app, &uri, body).await;
    let elapsed = started.elapsed();

    assert_eq!(
        resp.status,
        StatusCode::OK,
        "{voice}: {}",
        String::from_utf8_lossy(&resp.body)
    );
    assert_eq!(resp.error_header(), None, "{voice}: error header");
    assert_eq!(resp.headers["content-type"], "audio/wav");
    let seconds = wav_seconds(&resp.body);
    println!(
        "{voice}: {} chars -> {seconds:.2} s of audio in {:.2} s",
        text.chars().count(),
        elapsed.as_secs_f64()
    );
    seconds
}

#[tokio::test]
#[ignore = "real espeak data; run with --ignored"]
async fn fort_lauderdale_phonemizes() {
    let _serial = SERIAL.lock().await;
    assert_eq!(setup(), &Ok(()), "espeak startup check");
    assert_eq!(espeak::ipa(CHECK_TEXT, "en-us").as_deref(), Some(CHECK_IPA));
    // The espeak data is in the private dir. Only a run on a fresh data
    // home proves this run unpacked it there; on later runs the folder (and
    // its stamp) may be left from an earlier run.
    let private = espeak::private_temp_dir(&data_dir().espeak_root());
    assert!(
        private
            .join("piper-phonemize-espeak-ng-data")
            .join("espeak-ng-data")
            .join("phontab")
            .exists(),
        "no espeak data in {}",
        private.display()
    );
}

#[tokio::test]
#[ignore = "downloads a real model; run with --ignored"]
async fn piper_joe_speaks() {
    let _serial = SERIAL.lock().await;
    let seconds = speak(
        "en_US-joe-medium",
        "Cabin crew, please prepare for departure.",
    )
    .await;
    assert!(seconds > 0.5, "only {seconds:.2} s of audio");
}

#[tokio::test]
#[ignore = "downloads a real model; run with --ignored"]
async fn kokoro_heart_speaks() {
    let _serial = SERIAL.lock().await;
    let seconds = speak("af_heart", "Cabin crew, please prepare for departure.").await;
    assert!(seconds > 0.5, "only {seconds:.2} s of audio");
}

#[tokio::test]
#[ignore = "downloads a real model; run with --ignored"]
async fn supertonic_m1_speaks() {
    let _serial = SERIAL.lock().await;
    let seconds = speak("M1", "Cabin crew, please prepare for departure.").await;
    assert!(seconds > 0.5, "only {seconds:.2} s of audio");
}

/// A degree sign, an em dash, an ampersand and SLC's trailing CRLF.
const QUIRKS_LINE: &str = "It's 31°C — ladies & gentlemen.\r\n";

#[tokio::test]
#[ignore = "downloads a real model; run with --ignored"]
async fn slc_quirks_speak() {
    let _serial = SERIAL.lock().await;
    let seconds = speak("en_US-joe-medium", QUIRKS_LINE).await;
    assert!(seconds > 1.0, "only {seconds:.2} s of audio");
}

#[tokio::test]
#[ignore = "downloads a real model; run with --ignored"]
async fn kokoro_slc_quirks_speak() {
    let _serial = SERIAL.lock().await;
    let seconds = speak("af_heart", QUIRKS_LINE).await;
    assert!(seconds > 1.0, "only {seconds:.2} s of audio");
}

#[tokio::test]
#[ignore = "downloads a real model; run with --ignored"]
async fn supertonic_slc_quirks_speak() {
    let _serial = SERIAL.lock().await;
    let seconds = speak("M1", QUIRKS_LINE).await;
    assert!(seconds > 1.0, "only {seconds:.2} s of audio");
}

/// One ~600-character sentence with no full stop until the end. Under 20 s
/// of audio would mean the engine cut it short.
const LONG_SENTENCE: &str = "Ladies and gentlemen, on behalf of the captain and the \
    entire crew we would like to welcome you aboard this afternoon's flight to \
    Fort Lauderdale, and while we finish loading the last of the baggage and \
    complete our final checks we ask that you take a moment to make sure that \
    your seat belt is fastened, that your seat back is fully upright, that your \
    tray table is stowed, that your window shade is open and that any larger \
    electronic devices are switched off and stored safely in the seat pocket in \
    front of you or in the overhead bins, so that we can push back from the gate \
    on time and get you to your destination as smoothly as possible.";

#[tokio::test]
#[ignore = "downloads a real model; run with --ignored"]
async fn long_run_on_sentence_speaks() {
    let _serial = SERIAL.lock().await;
    let chars = LONG_SENTENCE.chars().count();
    assert!((550..=650).contains(&chars), "sentence is {chars} chars");
    assert_eq!(LONG_SENTENCE.matches('.').count(), 1, "one sentence only");

    let seconds = speak("en_US-joe-medium", LONG_SENTENCE).await;
    assert!(
        seconds > 20.0,
        "only {seconds:.2} s of audio for {chars} chars: the engine truncated it"
    );
}

#[tokio::test]
#[ignore = "downloads a real model; run with --ignored"]
async fn kokoro_long_run_on_sentence_speaks() {
    let _serial = SERIAL.lock().await;
    let chars = LONG_SENTENCE.chars().count();
    assert!((550..=650).contains(&chars), "sentence is {chars} chars");
    assert_eq!(LONG_SENTENCE.matches('.').count(), 1, "one sentence only");

    let seconds = speak("af_heart", LONG_SENTENCE).await;
    assert!(
        seconds > 20.0,
        "only {seconds:.2} s of audio for {chars} chars: the engine truncated it"
    );
}

/// A line that cleans down to punctuation only must be a clean 200 with
/// SLC Local TTS's 0.2 s of silence, never a 503. `frontend::text` filters
/// it out before espeak-ng runs; if one ever slipped through, espeak-ng
/// itself still reports "nothing to say" for it, not a failure.
#[tokio::test]
#[ignore = "downloads a real model; run with --ignored"]
async fn punctuation_only_line_is_silence() {
    let _serial = SERIAL.lock().await;
    assert_eq!(setup(), &Ok(()), "espeak startup check");
    assert_eq!(espeak::phonemize("...", "en-us"), Ok(None));

    for voice in ["en_US-joe-medium", "af_heart"] {
        let seconds = speak(voice, "... —\r\n").await;
        assert!(
            (seconds - 0.2).abs() < 0.01,
            "{voice}: {seconds:.2} s, expected 0.2 s of silence"
        );
    }
}
