//! The hand-written third-party notices stay in step with the code.
//!
//! `include_str!` reads each file at compile time, so a missing file fails the
//! build of this test, and a stale one fails an assertion.

use slc_local_tts::{catalog, update};

const VOICE_MODELS: &str = include_str!("../licenses/voice-models.txt");
const NATIVE: &str = include_str!("../licenses/native.txt");
const RELEASE_INFO: &str = include_str!("../scripts/release-info.ps1");

/// The lines that list a voice: each starts with `- `.
fn voice_lines() -> impl Iterator<Item = &'static str> {
    VOICE_MODELS.lines().filter(|line| line.starts_with("- "))
}

#[test]
fn voice_notice_covers_every_catalog_voice() {
    for v in catalog::voices() {
        let prefix = format!("- {}: {}.", v.id, v.license);
        assert!(
            voice_lines().any(|line| line.starts_with(&prefix)),
            "licenses/voice-models.txt has no line starting {prefix:?}"
        );
    }
}

#[test]
fn voice_notice_has_no_unknown_voices() {
    for line in voice_lines() {
        // "- en_US-joe-medium: CC0-1.0. ..." -> "en_US-joe-medium"
        let id = line[2..].split(':').next().unwrap_or_default();
        assert!(
            catalog::voices().iter().any(|v| v.id == id),
            "licenses/voice-models.txt lists {id:?}, which is not a catalog voice"
        );
    }
}

#[test]
fn native_notice_names_every_static_library() {
    for name in [
        "sherpa-onnx 2.0.1",
        "piper-phonemize 1.5.9",
        "espeak-ng",
        "onnxruntime",
        "kaldi-native-fbank",
        "kaldi-decoder",
        "kissfft",
        "OpenFst",
        "kaldifst",
        "simple-sentencepiece",
        "utf8proc",
    ] {
        assert!(
            NATIVE.contains(name),
            "licenses/native.txt doesn't mention {name:?}"
        );
    }
}

#[test]
fn release_info_matches_the_code() {
    let repo = format!("$RepoUrl = \"{}\"", update::REPO_URL);
    assert!(
        RELEASE_INFO.contains(&repo),
        "release-info.ps1 lacks {repo}"
    );
    assert!(
        RELEASE_INFO.contains("$PackId = \"SLC-Local-TTS\""),
        "release-info.ps1 lacks the pack ID"
    );
}
