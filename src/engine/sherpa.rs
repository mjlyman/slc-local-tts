//! The real [`Synthesizer`]: sherpa-onnx running Piper, Kokoro and Supertonic.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig, OfflineTtsSupertonicModelConfig, OfflineTtsVitsModelConfig,
};

use super::cache::ModelCache;
use super::{CancelFlag, Prepared, SynthError, SynthJob, Synthesizer};
use crate::audio::Audio;
use crate::catalog::{Engine, Voice};
use crate::config::Config;
use crate::frontend::espeak;

/// Piper's VITS sampling values, read per voice from its `.onnx.json`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PiperInference {
    pub noise_scale: f32,
    pub length_scale: f32,
    pub noise_w: f32,
}

impl Default for PiperInference {
    /// The values Piper itself uses when a voice's JSON has none.
    fn default() -> Self {
        PiperInference {
            noise_scale: 0.667,
            length_scale: 1.0,
            noise_w: 0.8,
        }
    }
}

pub struct SherpaSynthesizer {
    num_threads: i32,
    supertonic_steps: i32,
    /// Keyed by `voice.archive.stem`, not the voice ID: one archive is one
    /// model, so every Kokoro voice (and every speaker of a multi-speaker
    /// Piper model) shares a single loaded copy. The RAM figures assume this.
    cache: ModelCache<OfflineTts>,
}

impl SherpaSynthesizer {
    pub fn new(config: &Config) -> Self {
        SherpaSynthesizer {
            num_threads: config.num_threads,
            supertonic_steps: config.supertonic.num_steps,
            cache: ModelCache::new(),
        }
    }
}

impl Synthesizer for SherpaSynthesizer {
    fn prepare(&self, voice: &'static Voice, text: &str) -> Result<Prepared, SynthError> {
        match voice.engine {
            Engine::Piper | Engine::Kokoro => {
                let espeak_voice = voice.espeak_voice.ok_or_else(|| {
                    SynthError::Failed(format!(
                        "catalog bug: voice {} has no espeak-ng voice",
                        voice.id
                    ))
                })?;
                prepared_from_phonemes(espeak::phonemize(text, espeak_voice))
            }
            Engine::Supertonic => {
                let text = text.trim();
                if text.is_empty() {
                    Err(SynthError::NothingToSay)
                } else {
                    Ok(Prepared::Text(text.to_string()))
                }
            }
        }
    }

    fn synthesize(&self, job: SynthJob, cancel: CancelFlag) -> Result<Audio, SynthError> {
        let voice = job.voice;
        let key = voice.archive.stem;
        let tts = self
            .cache
            .get_or_load(key, || {
                load_model(voice.engine, &job.model_dir, self.num_threads)
            })
            .map_err(SynthError::LoadFailed)?;

        let num_speakers = tts.num_speakers();
        if voice.sid >= num_speakers.max(1) {
            return Err(SynthError::LoadFailed(format!(
                "voice {} wants speaker {}, but model {key} has only {num_speakers}",
                voice.id, voice.sid
            )));
        }

        // With phonemes, sherpa-onnx uses the codepoints *instead of* the text
        // argument, so the text is left empty.
        let (text, phoneme_codepoints) = match job.prepared {
            Prepared::Phonemes(sentences) => (String::new(), Some(sentences)),
            Prepared::Text(text) => (text, None),
        };
        // The input length: phonemes for Piper/Kokoro, characters for
        // Supertonic. Never the text itself (no request text at info level).
        let chars: usize = match &phoneme_codepoints {
            Some(sentences) => sentences.iter().map(Vec::len).sum(),
            None => text.chars().count(),
        };
        tracing::info!("synth start voice={} chars={}", voice.id, chars);

        let mut config = GenerationConfig {
            sid: voice.sid,
            speed: job.speed,
            phoneme_codepoints,
            ..Default::default()
        };
        if voice.engine == Engine::Supertonic {
            config.num_steps = self.supertonic_steps;
            config.extra = Some(HashMap::from([(
                "lang".to_string(),
                serde_json::json!("en"),
            )]));
        }

        if cancel.is_cancelled() {
            return Err(SynthError::Cancelled);
        }
        // sherpa-onnx requires a `'static` callback, so it can't borrow
        // `cancel` from this stack frame. Moving in a clone works because a
        // clone shares the same underlying flag (it's an `Arc`).
        let flag = cancel.clone();
        let keep_going = move |_samples: &[f32], _progress: f32| !flag.is_cancelled();
        let generated = tts.generate_with_config(&text, &config, Some(keep_going));
        self.cache.mark_used(key, Instant::now());

        if cancel.is_cancelled() {
            return Err(SynthError::Cancelled);
        }
        let generated = generated
            .ok_or_else(|| SynthError::Failed("sherpa-onnx returned no result".to_string()))?;
        if generated.samples().is_empty() {
            return Err(SynthError::Failed("no audio".to_string()));
        }
        let sample_rate = u32::try_from(generated.sample_rate()).map_err(|_| {
            SynthError::Failed(format!("bad sample rate {}", generated.sample_rate()))
        })?;
        Ok(Audio {
            samples: generated.samples().to_vec(),
            sample_rate,
        })
    }

    fn unload_idle(&self, idle_for: Duration) -> usize {
        self.cache.unload_idle(Instant::now(), idle_for)
    }
}

/// Turns [`espeak::phonemize`]'s answer into `prepare`'s. Nothing to say
/// (`Ok(None)`) is `NothingToSay`, which the API answers with silence. An
/// espeak-ng failure is `Failed`, which the API answers with an error (a 503
/// on the speech route), never with silence that SLC would cache.
fn prepared_from_phonemes(
    phonemes: Result<Option<Vec<Vec<i32>>>, espeak::PhonemizeError>,
) -> Result<Prepared, SynthError> {
    match phonemes {
        Ok(Some(sentences)) => Ok(Prepared::Phonemes(sentences)),
        Ok(None) => Err(SynthError::NothingToSay),
        Err(e) => Err(SynthError::Failed(e.to_string())),
    }
}

/// Builds the sherpa-onnx config for `engine` from `dir` and loads the model.
fn load_model(engine: Engine, dir: &Path, num_threads: i32) -> Result<OfflineTts, String> {
    let config = build_config(engine, dir, num_threads)?;
    tracing::info!(engine = engine.as_str(), dir = %dir.display(), "loading voice model");
    let started = Instant::now();
    let tts = OfflineTts::create(&config).ok_or_else(|| {
        format!(
            "sherpa-onnx could not load the {} model in {}",
            engine.as_str(),
            dir.display()
        )
    })?;
    tracing::info!(
        engine = engine.as_str(),
        dir = %dir.display(),
        ms = started.elapsed().as_millis() as u64,
        "voice model loaded"
    );
    Ok(tts)
}

fn build_config(engine: Engine, dir: &Path, num_threads: i32) -> Result<OfflineTtsConfig, String> {
    let mut model = OfflineTtsModelConfig {
        num_threads,
        ..Default::default()
    };
    match engine {
        Engine::Piper => {
            let onnx = single_onnx(dir)?;
            let inference = read_piper_inference(&onnx);
            model.vits = OfflineTtsVitsModelConfig {
                model: Some(path_string(&onnx)),
                tokens: Some(existing(dir, "tokens.txt")?),
                noise_scale: inference.noise_scale,
                noise_scale_w: inference.noise_w,
                length_scale: inference.length_scale,
                ..Default::default()
            };
        }
        Engine::Kokoro => {
            let onnx = kokoro_model_file(dir);
            if !onnx.is_file() {
                return Err(format!("missing {}", onnx.display()));
            }
            model.kokoro = OfflineTtsKokoroModelConfig {
                model: Some(path_string(&onnx)),
                voices: Some(existing(dir, "voices.bin")?),
                tokens: Some(existing(dir, "tokens.txt")?),
                ..Default::default()
            };
        }
        Engine::Supertonic => {
            model.supertonic = OfflineTtsSupertonicModelConfig {
                duration_predictor: Some(existing(dir, "duration_predictor.int8.onnx")?),
                text_encoder: Some(existing(dir, "text_encoder.int8.onnx")?),
                vector_estimator: Some(existing(dir, "vector_estimator.int8.onnx")?),
                vocoder: Some(existing(dir, "vocoder.int8.onnx")?),
                tts_json: Some(existing(dir, "tts.json")?),
                unicode_indexer: Some(existing(dir, "unicode_indexer.bin")?),
                voice_style: Some(existing(dir, "voice.bin")?),
            };
        }
    }
    Ok(OfflineTtsConfig {
        model,
        ..Default::default()
    })
}

/// Kokoro's model file: `model.int8.onnx` if present, else `model.onnx`
/// (the fp32 build the catalog uses).
fn kokoro_model_file(dir: &Path) -> PathBuf {
    let int8 = dir.join("model.int8.onnx");
    if int8.is_file() {
        int8
    } else {
        dir.join("model.onnx")
    }
}

/// Piper archives hold exactly one `.onnx` file, named after the voice.
fn single_onnx(dir: &Path) -> Result<PathBuf, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut onnx: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "onnx"))
        .collect();
    if onnx.len() == 1 {
        Ok(onnx.remove(0))
    } else {
        Err(format!(
            "expected one .onnx file in {}, found {}",
            dir.display(),
            onnx.len()
        ))
    }
}

/// Reads the `inference` block from `<model>.onnx.json`, falling back to
/// Piper's defaults (with a warning) if the file can't be read.
fn read_piper_inference(onnx: &Path) -> PiperInference {
    let mut json_path = onnx.as_os_str().to_owned();
    json_path.push(".json");
    let json_path = PathBuf::from(json_path);
    match std::fs::read_to_string(&json_path) {
        Ok(json) => piper_inference(&json),
        Err(e) => {
            tracing::warn!(
                path = %json_path.display(),
                "can't read the Piper voice settings ({e}); using defaults"
            );
            PiperInference::default()
        }
    }
}

/// Parses a Piper `.onnx.json`. Missing or malformed values keep their defaults.
fn piper_inference(json: &str) -> PiperInference {
    let defaults = PiperInference::default();
    let value: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let inference = &value["inference"];
    // Indexing a `serde_json::Value` with a missing key gives `Null`, and
    // `as_f64` on `Null` gives `None`, so absent fields fall back cleanly.
    let read = |name: &str, default: f32| inference[name].as_f64().map_or(default, |v| v as f32);
    PiperInference {
        noise_scale: read("noise_scale", defaults.noise_scale),
        length_scale: read("length_scale", defaults.length_scale),
        noise_w: read("noise_w", defaults.noise_w),
    }
}

/// `dir/name` as a string, or an error naming the missing file.
fn existing(dir: &Path, name: &str) -> Result<String, String> {
    let path = dir.join(name);
    if path.is_file() {
        Ok(path_string(&path))
    } else {
        Err(format!("missing {}", path.display()))
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Archive, find};

    const VCTK_JSON: &str = r#"{
        "audio": {"sample_rate": 22050, "quality": "medium"},
        "espeak": {"voice": "en"},
        "inference": {"noise_scale": 0.333, "length_scale": 1.4, "noise_w": 0.333},
        "num_speakers": 109
    }"#;

    #[test]
    fn piper_inference_values_from_json() {
        let vctk = piper_inference(VCTK_JSON);
        assert_eq!(
            (vctk.noise_scale, vctk.length_scale, vctk.noise_w),
            (0.333, 1.4, 0.333)
        );
        let plain = piper_inference(r#"{"audio": {"sample_rate": 22050}}"#);
        assert_eq!(
            (plain.noise_scale, plain.length_scale, plain.noise_w),
            (0.667, 1.0, 0.8)
        );
    }

    #[test]
    fn kokoro_file_detection() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("model.onnx"), b"").unwrap();
        assert_eq!(kokoro_model_file(tmp.path()), tmp.path().join("model.onnx"));
        std::fs::write(tmp.path().join("model.int8.onnx"), b"").unwrap();
        assert_eq!(
            kokoro_model_file(tmp.path()),
            tmp.path().join("model.int8.onnx")
        );
    }

    #[test]
    fn prepare_supertonic_passes_text_through() {
        let synth = SherpaSynthesizer::new(&Config::default());
        let prepared = synth.prepare(find("M1").unwrap(), "Hi").unwrap();
        assert_eq!(prepared, Prepared::Text("Hi".to_string()));
    }

    #[test]
    fn prepare_supertonic_blank_text_is_nothing_to_say() {
        let synth = SherpaSynthesizer::new(&Config::default());
        let prepared = synth.prepare(find("M1").unwrap(), " \r\n");
        assert_eq!(prepared, Err(SynthError::NothingToSay));
    }

    #[test]
    fn phonemes_become_prepared_phonemes() {
        assert_eq!(
            prepared_from_phonemes(Ok(Some(vec![vec![1, 2]]))),
            Ok(Prepared::Phonemes(vec![vec![1, 2]]))
        );
    }

    #[test]
    fn no_phonemes_is_nothing_to_say() {
        assert_eq!(
            prepared_from_phonemes(Ok(None)),
            Err(SynthError::NothingToSay)
        );
    }

    #[test]
    fn an_espeak_failure_is_a_real_failure_not_silence() {
        let result = prepared_from_phonemes(Err(espeak::PhonemizeError));
        assert!(
            matches!(&result, Err(SynthError::Failed(msg)) if msg.contains("espeak-ng")),
            "got {result:?}"
        );
    }

    static TEST_ARCHIVE: Archive = Archive {
        stem: "test",
        url: "",
        sha256: "",
        size: 0,
    };

    /// A Piper voice missing its espeak-ng voice: a catalog bug.
    static NO_ESPEAK_VOICE: Voice = Voice {
        id: "broken",
        aliases: &[],
        engine: Engine::Piper,
        name: "Broken",
        gender: None,
        accent: None,
        description: "",
        license: "",
        archive: &TEST_ARCHIVE,
        sid: 0,
        espeak_voice: None,
        slow: false,
    };

    #[test]
    fn prepare_piper_without_espeak_voice_fails() {
        let synth = SherpaSynthesizer::new(&Config::default());
        let result = synth.prepare(&NO_ESPEAK_VOICE, "Hi");
        assert!(
            matches!(&result, Err(SynthError::Failed(msg)) if msg.contains("broken")),
            "got {result:?}"
        );
    }
}
