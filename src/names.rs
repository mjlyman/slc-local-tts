//! Names shown to people: a model's row title, a voice's name, and sizes in
//! MB. The tray and the settings window both use these, so they always agree.

use crate::catalog::{self, Engine};

/// The title of a downloadable model: "Kokoro voices" and "Supertonic
/// voices" for the two models that hold many voices, the voice's catalog
/// name for a Piper model (which holds one), or `stem` itself when the
/// catalog doesn't know it.
pub fn row_title(stem: &str) -> String {
    let first = catalog::voices().iter().find(|v| v.archive.stem == stem);
    match first {
        Some(voice) => match voice.engine {
            Engine::Kokoro => "Kokoro voices".to_string(),
            Engine::Supertonic => "Supertonic voices".to_string(),
            // Every Piper model holds exactly one voice.
            Engine::Piper => voice.name.to_string(),
        },
        None => stem.to_string(),
    }
}

/// `bytes` in decimal megabytes, rounded to the nearest one.
pub fn format_mb(bytes: u64) -> u64 {
    // `saturating_add` stops at u64::MAX instead of overflowing.
    bytes.saturating_add(500_000) / 1_000_000
}

/// A voice's catalog name, or `id` itself when the catalog doesn't know it.
pub fn voice_name(id: &str) -> String {
    catalog::find(id).map_or_else(|| id.to_string(), |voice| voice.name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(row_title("kokoro-multi-lang-v1_0"), "Kokoro voices");
        assert_eq!(
            row_title("sherpa-onnx-supertonic-3-tts-int8-2026-05-11"),
            "Supertonic voices"
        );
        assert_eq!(
            row_title("vits-piper-en_US-joe-medium"),
            "Joe (US, medium, Piper)"
        );
        assert_eq!(row_title("not-a-model"), "not-a-model");

        assert_eq!(format_mb(349_886_476), 350);
        assert_eq!(format_mb(128_774_318), 129);
        assert_eq!(format_mb(0), 0);
        assert_eq!(format_mb(u64::MAX), u64::MAX / 1_000_000);

        assert_eq!(voice_name("en_US-joe-medium"), "Joe (US, medium, Piper)");
        assert_eq!(voice_name("no-such-voice"), "no-such-voice");
    }

    #[test]
    fn every_engine_has_a_title() {
        // Kokoro and Supertonic hold several voices each, so their titles
        // must not depend on which voice the catalog lists first.
        for voice in catalog::voices() {
            let title = row_title(voice.archive.stem);
            match voice.engine {
                Engine::Kokoro => assert_eq!(title, "Kokoro voices"),
                Engine::Supertonic => assert_eq!(title, "Supertonic voices"),
                Engine::Piper => assert_eq!(title, voice.name),
            }
        }
    }
}
