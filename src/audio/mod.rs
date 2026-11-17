//! Audio buffers, loudness levelling and WAV encoding.
//!
//! Every speech reply is a 16-bit mono PCM WAV at the model's native rate.

pub mod limiter;
pub mod loudness;

/// Length of the "degraded" reply (unspeakable text, real failures).
pub const SILENCE_SECONDS: f32 = 0.2;
/// Sample rate of the silent reply.
pub const SILENCE_RATE: u32 = 22_050;
/// How loud every line is made at 100% volume, in LUFS.
pub const TARGET_LUFS: f32 = -16.0;
/// Loudest allowed absolute sample after levelling: -1 dBFS, which leaves
/// headroom below full scale.
pub const CEILING: f32 = 0.891_251;
/// The volume used until the user changes it, in percent.
pub const DEFAULT_VOLUME_PERCENT: u16 = 100;
/// The most a line is turned up, in dB.
const MAX_BOOST_DB: f32 = 20.0;
/// The most a line is turned down, in dB.
const MAX_CUT_DB: f32 = 10.0;

/// What [`level`] measured and did, for the log.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Leveled {
    /// The line's loudness before levelling; `None` for silence.
    pub measured_lufs: Option<f32>,
    /// The total gain applied before limiting, in dB: the capped levelling
    /// gain plus the volume's shift.
    pub gain_db: f32,
}

/// Mono audio as floating-point samples in roughly -1.0..=1.0.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Turns a line up or down to [`TARGET_LUFS`] (shifted by `volume_percent`),
/// so every voice and every line sounds equally loud, then limits its peaks
/// to [`CEILING`].
///
/// The levelling gain is capped at +20 dB and -10 dB, so a near-silent line
/// (a breath, a click) isn't blown up into loud noise. The volume is added
/// after the cap, so the slider moves every line, even a capped one.
/// Silence is left as it is.
pub fn level(samples: &mut [f32], sample_rate: u32, volume_percent: u16) -> Leveled {
    let Some(measured) = loudness::integrated_lufs(samples, sample_rate) else {
        return Leveled {
            measured_lufs: None,
            gain_db: 0.0,
        };
    };
    let auto_db = (TARGET_LUFS - measured).clamp(-MAX_CUT_DB, MAX_BOOST_DB);
    // Twice the volume is +6 dB, half is -6 dB. The volume is checked to
    // 50..=200 wherever it comes in; `max(1)` only keeps a 0 from giving
    // -inf dB (and NaN samples), treating it as 1% (-40 dB) instead.
    let volume_db = 20.0 * (f32::from(volume_percent.max(1)) / 100.0).log10();
    let gain_db = auto_db + volume_db;
    let gain = 10f32.powf(gain_db / 20.0);
    for s in samples.iter_mut() {
        *s *= gain;
    }
    limiter::limit(samples, sample_rate, CEILING);
    Leveled {
        measured_lufs: Some(measured),
        gain_db,
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
    fn nan_encodes_as_silence() {
        let wav = encode_wav(&Audio {
            samples: vec![f32::NAN],
            sample_rate: 22050,
        });
        assert_eq!(le_i16(&wav, 44), 0);
    }

    fn sine(amplitude: f32, seconds: f32, rate: u32) -> Vec<f32> {
        let n = (seconds * rate as f32) as usize;
        (0..n)
            .map(|i| {
                amplitude * (2.0 * std::f32::consts::PI * 997.0 * i as f32 / rate as f32).sin()
            })
            .collect()
    }

    fn lufs(samples: &[f32], rate: u32) -> f32 {
        loudness::integrated_lufs(samples, rate).unwrap()
    }

    #[test]
    fn quiet_line_is_raised_to_target() {
        let mut s = sine(0.05, 2.0, 22_050);
        let leveled = level(&mut s, 22_050, 100);
        let after = lufs(&s, 22_050);
        assert!((after + 16.0).abs() < 0.1, "{after}");
        let measured = leveled.measured_lufs.unwrap();
        assert!((measured + 29.03).abs() < 0.2, "{measured}");
    }

    #[test]
    fn volume_shifts_the_target() {
        let mut louder = sine(0.05, 2.0, 22_050);
        level(&mut louder, 22_050, 200);
        let l = lufs(&louder, 22_050);
        assert!((l + 9.98).abs() < 0.1, "200%: {l}");

        let mut quieter = sine(0.05, 2.0, 22_050);
        level(&mut quieter, 22_050, 50);
        let l = lufs(&quieter, 22_050);
        assert!((l + 22.02).abs() < 0.1, "50%: {l}");
    }

    #[test]
    fn boost_is_capped_at_20_db() {
        let mut s = sine(0.001, 2.0, 22_050);
        assert_eq!(level(&mut s, 22_050, 100).gain_db, 20.0);
    }

    #[test]
    fn cut_is_capped_at_10_db() {
        let mut s = sine(0.9, 2.0, 22_050);
        assert_eq!(level(&mut s, 22_050, 100).gain_db, -10.0);
    }

    #[test]
    fn volume_still_applies_past_the_boost_cap() {
        // A 0.001 sine is about -63 LUFS, so its levelling gain is capped at
        // +20 dB; the volume then moves it by another 6 dB either way.
        let mut louder = sine(0.001, 2.0, 22_050);
        let gain = level(&mut louder, 22_050, 200).gain_db;
        assert!((gain - 26.02).abs() < 0.01, "200%: {gain}");

        let mut quieter = sine(0.001, 2.0, 22_050);
        let gain = level(&mut quieter, 22_050, 50).gain_db;
        assert!((gain - 13.98).abs() < 0.01, "50%: {gain}");
    }

    #[test]
    fn volume_zero_gives_finite_samples() {
        let mut s = sine(0.05, 2.0, 22_050);
        let leveled = level(&mut s, 22_050, 0);
        assert!(leveled.gain_db.is_finite(), "{}", leveled.gain_db);
        assert!(s.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn silence_is_left_alone() {
        let mut s = vec![0.0f32; 22_050];
        let leveled = level(&mut s, 22_050, 100);
        assert!(s.iter().all(|&x| x == 0.0));
        assert_eq!(leveled.measured_lufs, None);
        assert_eq!(leveled.gain_db, 0.0);
    }

    #[test]
    fn peaks_never_pass_the_ceiling() {
        let mut s = sine(0.05, 2.0, 22_050);
        for at in [5_000, 15_000, 30_000, 40_000] {
            s[at] = 0.6;
        }
        level(&mut s, 22_050, 200);
        let peak = s.iter().fold(0.0f32, |p, x| p.max(x.abs()));
        assert!(peak <= CEILING, "{peak}");
    }

    #[test]
    #[ignore = "timing; run in a release build"]
    fn ten_seconds_level_quickly() {
        // White-ish noise from a simple linear congruential generator.
        let mut state: u32 = 12_345;
        let mut s: Vec<f32> = (0..441_000)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect();
        let started = std::time::Instant::now();
        level(&mut s, 44_100, 100);
        let took = started.elapsed();
        println!("levelled 10 s in {took:?}");
        assert!(took.as_millis() < 20, "{took:?}");
    }
}
