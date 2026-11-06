//! Compiled-in, curated English voice catalog.
//!
//! The voice table itself lives in `data.rs`, which `scripts/gen-catalog.ps1`
//! generates from `scripts/catalog-piper.csv`. This file holds the types and
//! the lookup functions.

mod data;

/// Which speech engine synthesizes a voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Engine {
    Piper,
    Kokoro,
    Supertonic,
}

impl Engine {
    /// Lowercase name used in logs, labels and `config.toml`.
    pub fn as_str(self) -> &'static str {
        match self {
            Engine::Piper => "piper",
            Engine::Kokoro => "kokoro",
            Engine::Supertonic => "supertonic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gender {
    Female,
    Male,
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Accent {
    American,
    British,
}

/// A downloadable model archive from the sherpa-onnx `tts-models` release.
/// Several voices can share one archive (all Kokoro voices do), so voices
/// hold a `&'static Archive` instead of a copy.
#[derive(Debug, PartialEq, Eq)]
pub struct Archive {
    /// File name without `.tar.bz2`; also the folder name once extracted.
    pub stem: &'static str,
    pub url: &'static str,
    /// Lowercase hex sha256 of the archive file.
    pub sha256: &'static str,
    /// Archive size in bytes.
    pub size: u64,
}

/// One selectable voice. Everything is `'static` because the catalog is
/// compiled into the binary.
#[derive(Debug, PartialEq)]
pub struct Voice {
    /// The ID SLC sends in request paths, e.g. `en_US-joe-medium` or `M1`.
    pub id: &'static str,
    /// Older IDs that should still resolve to this voice.
    pub aliases: &'static [&'static str],
    pub engine: Engine,
    /// Display name shown in SLC's voice picker.
    pub name: &'static str,
    pub gender: Option<Gender>,
    pub accent: Option<Accent>,
    pub description: &'static str,
    pub license: &'static str,
    pub archive: &'static Archive,
    /// sherpa-onnx speaker ID within the model.
    pub sid: i32,
    /// espeak-ng voice for phonemizing (Piper and Kokoro); never `en-gb`.
    pub espeak_voice: Option<&'static str>,
    /// True for voices that are slow on most CPUs (Kokoro).
    pub slow: bool,
}

impl Voice {
    /// True if `id` names this voice, by ID or alias, ignoring ASCII case.
    fn matches(&self, id: &str) -> bool {
        self.id.eq_ignore_ascii_case(id) || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(id))
    }
}

/// Every voice in the catalog, in display order.
pub fn voices() -> &'static [Voice] {
    data::VOICES
}

/// Looks up a catalog voice by ID or alias, ignoring ASCII case.
pub fn find(id: &str) -> Option<&'static Voice> {
    find_in(voices(), id)
}

/// Looks up a voice in `voices` by ID or alias, ignoring ASCII case.
/// Separate from `find` so tests can search their own table.
pub fn find_in(voices: &'static [Voice], id: &str) -> Option<&'static Voice> {
    voices.iter().find(|v| v.matches(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const MODEL_URL_BASE: &str =
        "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/";

    #[test]
    fn ids_and_aliases_are_unique_case_insensitively() {
        let mut seen = HashSet::new();
        for voice in voices() {
            for name in std::iter::once(&voice.id).chain(voice.aliases.iter()) {
                assert!(
                    seen.insert(name.to_ascii_lowercase()),
                    "duplicate id or alias: {name}"
                );
            }
        }
    }

    #[test]
    fn voice_count_is_65() {
        let all = voices();
        assert_eq!(all.len(), 65);
        let count = |engine| all.iter().filter(|v| v.engine == engine).count();
        assert_eq!(count(Engine::Piper), 27);
        assert_eq!(count(Engine::Kokoro), 28);
        assert_eq!(count(Engine::Supertonic), 10);
    }

    #[test]
    fn every_archive_is_well_formed() {
        for voice in voices() {
            let a = voice.archive;
            assert!(a.url.starts_with(MODEL_URL_BASE), "{}: {}", voice.id, a.url);
            assert!(
                a.url.ends_with(&format!("{}.tar.bz2", a.stem)),
                "{}: {}",
                voice.id,
                a.url
            );
            assert_eq!(
                a.url,
                format!("{MODEL_URL_BASE}{}.tar.bz2", a.stem),
                "{}",
                voice.id
            );
            assert_eq!(a.sha256.len(), 64, "{}", voice.id);
            assert!(
                a.sha256
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
                "{}: {}",
                voice.id,
                a.sha256
            );
            assert!(a.size > 0, "{}", voice.id);
        }
    }

    #[test]
    fn espeak_voice_is_never_en_gb() {
        const ALLOWED: [&str; 3] = ["en-us", "en", "en-gb-x-rp"];
        for voice in voices() {
            match voice.engine {
                Engine::Piper | Engine::Kokoro => {
                    let ev = voice
                        .espeak_voice
                        .unwrap_or_else(|| panic!("{} has no espeak voice", voice.id));
                    assert!(ALLOWED.contains(&ev), "{}: {ev}", voice.id);
                }
                Engine::Supertonic => assert_eq!(voice.espeak_voice, None, "{}", voice.id),
            }
        }
    }

    #[test]
    fn legacy_slc_ids_resolve() {
        let ids = [
            "M1",
            "M3",
            "F2",
            "F3",
            "F4",
            "en_US-joe-medium",
            "en_US-hfc_male-medium",
            "en_US-ljspeech-medium",
            "en_US-libritts_r-medium",
            "en_GB-northern_english_male-medium",
            "m1",
            "EN_US-JOE-MEDIUM",
        ];
        for id in ids {
            assert!(find(id).is_some(), "{id} should resolve");
        }
        assert_eq!(find("EN_US-JOE-MEDIUM").unwrap().id, "en_US-joe-medium");
    }

    #[test]
    fn supertonic_mapping() {
        assert_eq!(find("F1").unwrap().sid, 0);
        assert_eq!(find("M1").unwrap().sid, 5);
        assert_eq!(find("M5").unwrap().sid, 9);
        assert_eq!(find("M1").unwrap().engine, Engine::Supertonic);
    }

    #[test]
    fn kokoro_voices_share_one_archive_and_are_slow() {
        let kokoro: Vec<&Voice> = voices()
            .iter()
            .filter(|v| v.engine == Engine::Kokoro)
            .collect();
        assert!(!kokoro.is_empty());
        let first = kokoro[0].archive;
        for v in &kokoro {
            // Same `static` archive, not just an equal copy.
            assert!(std::ptr::eq(v.archive, first), "{}", v.id);
            assert!(v.slow, "{}", v.id);
        }
        // Only Kokoro is marked slow.
        assert!(
            voices()
                .iter()
                .filter(|v| v.engine != Engine::Kokoro)
                .all(|v| !v.slow)
        );
    }

    static TEST_ARCHIVE: Archive = Archive {
        stem: "test",
        url: "https://example.invalid/test.tar.bz2",
        sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        size: 1,
    };

    static TEST_VOICES: &[Voice] = &[Voice {
        id: "new-id",
        aliases: &["old-id"],
        engine: Engine::Piper,
        name: "Test",
        gender: None,
        accent: None,
        description: "Test voice",
        license: "CC0-1.0",
        archive: &TEST_ARCHIVE,
        sid: 0,
        espeak_voice: Some("en-us"),
        slow: false,
    }];

    #[test]
    fn find_checks_aliases() {
        assert_eq!(find_in(TEST_VOICES, "old-id").unwrap().id, "new-id");
        assert_eq!(find_in(TEST_VOICES, "OLD-ID").unwrap().id, "new-id");
        assert_eq!(find_in(TEST_VOICES, "new-id").unwrap().id, "new-id");
    }

    #[test]
    fn unknown_id_is_none() {
        assert!(find("no-such-voice").is_none());
        assert!(find("").is_none());
        assert!(find_in(TEST_VOICES, "other").is_none());
    }

    #[test]
    fn engine_as_str() {
        assert_eq!(Engine::Piper.as_str(), "piper");
        assert_eq!(Engine::Kokoro.as_str(), "kokoro");
        assert_eq!(Engine::Supertonic.as_str(), "supertonic");
    }
}
