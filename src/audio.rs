//! Audio buffers, the peak limiter and WAV encoding.
//!
//! Every speech reply is a 16-bit mono PCM WAV at the model's native rate.

use crate::catalog::Engine;

/// Length of the "degraded" reply (unspeakable text, real failures).
pub const SILENCE_SECONDS: f32 = 0.2;
/// Sample rate of the silent reply.
pub const SILENCE_RATE: u32 = 22_050;
/// Loudest allowed absolute sample after limiting (leaves headroom below 1.0).
pub const PEAK_LIMIT: f32 = 0.95;

/// Mono audio as floating-point samples in roughly -1.0..=1.0.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Per-engine loudness multiplier. All 1.0 for now; tune here later.
pub fn engine_gain(engine: Engine) -> f32 {
    match engine {
        Engine::Piper => 1.0,
        Engine::Kokoro => 1.0,
        Engine::Supertonic => 1.0,
    }
}

/// Multiplies every sample by `gain`, then, if the loudest sample exceeds
/// `PEAK_LIMIT`, scales the whole buffer so that peak equals `PEAK_LIMIT`.
/// Scaling everything by one factor keeps the relative loudness (no distortion).
pub fn apply_gain_and_limit(samples: &mut [f32], gain: f32) {
    for s in samples.iter_mut() {
        *s *= gain;
    }
    // `f32::max` returns the other operand when one is NaN, so a NaN sample
    // cannot poison the peak. An empty or all-zero buffer gives peak 0.0,
    // which is never above the limit, so there is no divide-by-zero.
    let peak = samples.iter().fold(0.0f32, |p, s| p.max(s.abs()));
    if peak > PEAK_LIMIT {
        let scale = PEAK_LIMIT / peak;
        for s in samples.iter_mut() {
            *s *= scale;
        }
    }
}

/// The 0.2 s silent reply used when text is unspeakable or synthesis fails.
pub fn silence() -> Audio {
    let count = (SILENCE_SECONDS * SILENCE_RATE as f32).round() as usize;
    Audio {
        samples: vec![0.0; count],
        sample_rate: SILENCE_RATE,
    }
}

/// Encodes mono audio as a 16-bit PCM WAV (44-byte header, then samples).
pub fn encode_wav(audio: &Audio) -> Vec<u8> {
    let data_len = audio.samples.len() * 2;
    // WAV stores sizes as u32. Speech output is far below 4 GB, so this can
    // only fail on a bug; in release builds the cast would wrap, not panic.
    debug_assert!(data_len <= (u32::MAX - 36) as usize, "WAV too large");
    let data_len = data_len as u32;

    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&audio.sample_rate.to_le_bytes());
    wav.extend_from_slice(&(audio.sample_rate * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for s in &audio.samples {
        // `clamp` passes NaN through, and `NaN as i16` is 0 in Rust (casts
        // saturate), so a NaN sample becomes silence instead of panicking.
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        wav.extend_from_slice(&v.to_le_bytes());
    }
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le_i16(bytes: &[u8], at: usize) -> i16 {
        i16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    #[test]
    fn wav_header_bytes() {
        let wav = encode_wav(&Audio {
            samples: vec![0.0, 1.0],
            sample_rate: 22050,
        });
        let mut expected: Vec<u8> = Vec::new();
        expected.extend_from_slice(b"RIFF");
        expected.extend_from_slice(&40u32.to_le_bytes());
        expected.extend_from_slice(b"WAVE");
        expected.extend_from_slice(b"fmt ");
        expected.extend_from_slice(&16u32.to_le_bytes());
        expected.extend_from_slice(&1u16.to_le_bytes());
        expected.extend_from_slice(&1u16.to_le_bytes());
        expected.extend_from_slice(&22050u32.to_le_bytes());
        expected.extend_from_slice(&44100u32.to_le_bytes());
        expected.extend_from_slice(&2u16.to_le_bytes());
        expected.extend_from_slice(&16u16.to_le_bytes());
        expected.extend_from_slice(b"data");
        expected.extend_from_slice(&4u32.to_le_bytes());
        expected.extend_from_slice(&0i16.to_le_bytes());
        expected.extend_from_slice(&32767i16.to_le_bytes());
        assert_eq!(wav.len(), 48);
        assert_eq!(wav, expected);
    }

    #[test]
    fn clipping_is_clamped() {
        let wav = encode_wav(&Audio {
            samples: vec![2.0, -2.0],
            sample_rate: 22050,
        });
        assert_eq!(le_i16(&wav, 44), 32767);
        assert_eq!(le_i16(&wav, 46), -32767);
    }

    #[test]
    fn silence_is_point_two_seconds() {
        let s = silence();
        assert_eq!(s.samples.len(), 4410);
        assert!(s.samples.iter().all(|&x| x == 0.0));
        assert_eq!(s.sample_rate, SILENCE_RATE);
        assert_eq!(encode_wav(&s).len(), 8864);
    }

    #[test]
    fn limiter_caps_loud_audio() {
        let mut s = vec![0.5, -1.5];
        apply_gain_and_limit(&mut s, 1.0);
        assert!((s[1].abs() - PEAK_LIMIT).abs() < 1e-6);
        assert!((s[0] - 0.5 * PEAK_LIMIT / 1.5).abs() < 1e-6);
        assert!((s[0] - 0.3167).abs() < 1e-3);
    }

    #[test]
    fn limiter_leaves_quiet_audio() {
        let mut s = vec![0.2, -0.3];
        apply_gain_and_limit(&mut s, 1.0);
        assert_eq!(s, vec![0.2, -0.3]);
    }

    #[test]
    fn gain_is_applied_before_limiting() {
        let mut s = vec![0.2, -0.3];
        apply_gain_and_limit(&mut s, 2.0);
        assert!((s[0] - 0.4).abs() < 1e-6);
        assert!((s[1] + 0.6).abs() < 1e-6);
    }

    #[test]
    fn empty_samples_encode_to_bare_header() {
        let wav = encode_wav(&Audio {
            samples: vec![],
            sample_rate: 24000,
        });
        assert_eq!(wav.len(), 44);
        assert_eq!(&wav[40..44], &0u32.to_le_bytes());
        assert_eq!(&wav[4..8], &36u32.to_le_bytes());
    }

    #[test]
    fn limiter_handles_empty_and_all_zero() {
        let mut empty: Vec<f32> = vec![];
        apply_gain_and_limit(&mut empty, 1.0);
        let mut zeros = vec![0.0f32; 8];
        apply_gain_and_limit(&mut zeros, 1.0);
        assert!(zeros.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn nan_does_not_poison_limiter_or_encoder() {
        let mut s = vec![f32::NAN, 0.5, -1.9];
        apply_gain_and_limit(&mut s, 1.0);
        assert!((s[2].abs() - PEAK_LIMIT).abs() < 1e-6);
        assert!(s[1].is_finite());
        let wav = encode_wav(&Audio {
            samples: vec![f32::NAN],
            sample_rate: 22050,
        });
        assert_eq!(le_i16(&wav, 44), 0);
    }
}
