//! Speech synthesis behind the `Synthesizer` trait.
//!
//! The HTTP layer only sees this trait, so its tests can use a fake. The real
//! implementation, [`sherpa::SherpaSynthesizer`], wraps sherpa-onnx.

pub mod cache;
pub mod sherpa;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::audio::Audio;
use crate::catalog::{Engine, Voice};
use crate::config::BaseSpeeds;

/// Lowest and highest speed sent to Piper and Kokoro.
const SPEED_RANGE: (f32, f32) = (0.5, 2.0);
/// Supertonic sounds broken below 0.7, so its floor is higher.
const SUPERTONIC_SPEED_RANGE: (f32, f32) = (0.7, 2.0);

/// Turns text into speech. Both methods block, so callers run them on a
/// blocking thread (`spawn_blocking`).
pub trait Synthesizer: Send + Sync + 'static {
    /// Gets text ready for `voice`'s engine: Piper and Kokoro text is
    /// phonemized with espeak-ng; Supertonic text passes through.
    /// `NothingToSay` means there is legitimately nothing to speak (the API
    /// answers with silence); `Failed` means espeak-ng itself failed (the
    /// API answers with an error).
    fn prepare(&self, voice: &'static Voice, text: &str) -> Result<Prepared, SynthError>;

    /// Synthesizes one job. Call only while holding the global synthesis
    /// lock: one synthesis at a time keeps CPU use low next to the flight sim.
    fn synthesize(&self, job: SynthJob, cancel: CancelFlag) -> Result<Audio, SynthError>;

    /// Unloads models unused for at least `idle_for`; returns how many.
    fn unload_idle(&self, idle_for: Duration) -> usize;
}

/// Engine-ready input produced by [`Synthesizer::prepare`].
#[derive(Debug, Clone, PartialEq)]
pub enum Prepared {
    /// espeak-ng phoneme codepoints, one inner vector per sentence.
    Phonemes(Vec<Vec<i32>>),
    /// Raw text, for engines that read text themselves (Supertonic).
    Text(String),
}

/// Everything [`Synthesizer::synthesize`] needs for one request.
#[derive(Debug)]
pub struct SynthJob {
    pub voice: &'static Voice,
    /// The extracted model folder for `voice.archive`.
    pub model_dir: PathBuf,
    pub prepared: Prepared,
    /// Final engine speed, from [`engine_speed`].
    pub speed: f32,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SynthError {
    #[error("nothing to say")]
    NothingToSay,
    #[error("model load failed: {0}")]
    LoadFailed(String),
    #[error("synthesis failed: {0}")]
    Failed(String),
    #[error("synthesis cancelled")]
    Cancelled,
}

/// A shared "stop now" switch. Cloning shares the same flag (like a Java
/// `AtomicBoolean` passed by reference), so the request handler keeps one
/// clone and the synthesis callback another.
#[derive(Debug, Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// The speed sent to the engine: the engine's base speed times SLC's speed
/// (1.0 if SLC sent none), clamped to what the engine handles well. A speed
/// of 0 or less from SLC lands on the engine's slowest speed.
pub fn engine_speed(engine: Engine, base: &BaseSpeeds, slc_speed: Option<f32>) -> f32 {
    let (base_speed, (min, max)) = match engine {
        Engine::Piper => (base.piper, SPEED_RANGE),
        Engine::Kokoro => (base.kokoro, SPEED_RANGE),
        Engine::Supertonic => (base.supertonic, SUPERTONIC_SPEED_RANGE),
    };
    (base_speed * slc_speed.unwrap_or(1.0)).clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Engine;
    use crate::config::BaseSpeeds;

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-4,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn speed_mapping() {
        let base = BaseSpeeds::default();
        assert_close(engine_speed(Engine::Piper, &base, Some(1.2)), 1.2);
        assert_close(engine_speed(Engine::Supertonic, &base, Some(1.2)), 1.26);
        assert_close(engine_speed(Engine::Supertonic, &base, Some(0.5)), 0.7);
        assert_close(engine_speed(Engine::Kokoro, &base, Some(3.0)), 2.0);
        assert_close(engine_speed(Engine::Piper, &base, None), 1.0);
        assert_close(engine_speed(Engine::Kokoro, &base, Some(0.1)), 0.5);
    }
}
