//! Loudness measurement: LUFS is how loud a sound seems to a listener
//! (ITU-R BS.1770 / EBU R128), so equal LUFS sounds equally loud.

use std::f64::consts::PI;

/// Block length and hop for gating: 400 ms blocks, 100 ms apart.
const BLOCK_SECONDS: f64 = 0.4;
const HOP_SECONDS: f64 = 0.1;
/// Blocks quieter than this are ignored entirely.
const ABSOLUTE_GATE_LUFS: f64 = -70.0;
/// Blocks this much quieter than the average of the rest are ignored.
const RELATIVE_GATE_LU: f64 = 10.0;

/// One biquad filter section (direct form I, coefficients already divided by a0).
struct Biquad {
    b: [f64; 3],
    a: [f64; 2], // a1, a2
}

impl Biquad {
    /// Runs the filter over `input`, returning a new buffer.
    fn process(&self, input: &[f64]) -> Vec<f64> {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        input
            .iter()
            .map(|&x| {
                let y = self.b[0] * x + self.b[1] * x1 + self.b[2] * x2
                    - self.a[0] * y1
                    - self.a[1] * y2;
                x2 = x1;
                x1 = x;
                y2 = y1;
                y1 = y;
                y
            })
            .collect()
    }
}

/// K-weighting stage 1: a high shelf that models the head's acoustics.
fn shelf(fs: f64) -> Biquad {
    let f0 = 1681.974450955533;
    let gain_db = 3.999843853973347;
    let q = 0.7071752369554196;
    let k = (PI * f0 / fs).tan();
    let vh = 10f64.powf(gain_db / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    }
}

/// K-weighting stage 2: a high-pass that ignores very low rumble.
fn high_pass(fs: f64) -> Biquad {
    let f0 = 38.13547087602444;
    let q = 0.5003270373238773;
    let k = (PI * f0 / fs).tan();
    let a0 = 1.0 + k / q + k * k;
    Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    }
}

fn lufs_of_energy(mean_square: f64) -> f64 {
    -0.691 + 10.0 * mean_square.log10()
}

/// Integrated loudness of mono `samples` in LUFS, or `None` when the audio is
/// empty or silent (nothing passes the gates).
pub fn integrated_lufs(samples: &[f32], sample_rate: u32) -> Option<f32> {
    if samples.is_empty() || sample_rate == 0 {
        return None;
    }
    let fs = f64::from(sample_rate);

    // NaN and infinite samples would poison every filter output after them.
    let clean: Vec<f64> = samples
        .iter()
        .map(|&s| if s.is_finite() { f64::from(s) } else { 0.0 })
        .collect();
    let weighted = high_pass(fs).process(&shelf(fs).process(&clean));

    // Mean square of each gating block.
    let block_len = (BLOCK_SECONDS * fs).round() as usize;
    let hop = (HOP_SECONDS * fs).round() as usize;
    let mean_square = |block: &[f64]| block.iter().map(|v| v * v).sum::<f64>() / block.len() as f64;
    let blocks: Vec<f64> = if weighted.len() < block_len {
        // A short line counts as one block over all its samples.
        vec![mean_square(&weighted)]
    } else {
        weighted
            .windows(block_len)
            .step_by(hop.max(1))
            .map(mean_square)
            .collect()
    };

    // Absolute gate, then relative gate based on what survived.
    let above_abs: Vec<f64> = blocks
        .into_iter()
        .filter(|&e| e > 0.0 && lufs_of_energy(e) > ABSOLUTE_GATE_LUFS)
        .collect();
    if above_abs.is_empty() {
        return None;
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let relative_gate = lufs_of_energy(mean(&above_abs)) - RELATIVE_GATE_LU;
    let kept: Vec<f64> = above_abs
        .into_iter()
        .filter(|&e| lufs_of_energy(e) > relative_gate)
        .collect();
    if kept.is_empty() {
        return None;
    }
    Some(lufs_of_energy(mean(&kept)) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amplitude: f32, freq_hz: f32, seconds: f32, rate: u32) -> Vec<f32> {
        let n = (seconds * rate as f32) as usize;
        (0..n)
            .map(|i| {
                amplitude * (2.0 * std::f32::consts::PI * freq_hz * i as f32 / rate as f32).sin()
            })
            .collect()
    }

    #[test]
    fn full_scale_997hz_sine_reads_minus_3_01() {
        for rate in [22_050, 24_000, 44_100, 48_000] {
            let l = integrated_lufs(&sine(1.0, 997.0, 3.0, rate), rate).unwrap();
            assert!((l + 3.01).abs() < 0.2, "{rate} Hz: {l}");
        }
    }

    #[test]
    fn amplitude_0_1_reads_minus_23_01() {
        let l = integrated_lufs(&sine(0.1, 997.0, 3.0, 48_000), 48_000).unwrap();
        assert!((l + 23.01).abs() < 0.2, "{l}");
    }

    #[test]
    fn silence_and_empty_are_none() {
        assert_eq!(integrated_lufs(&vec![0.0; 48_000], 48_000), None);
        assert_eq!(integrated_lufs(&[], 48_000), None);
    }

    #[test]
    fn line_shorter_than_a_block_is_measured() {
        let l = integrated_lufs(&sine(0.1, 997.0, 0.2, 22_050), 22_050).unwrap();
        assert!((l + 23.01).abs() < 1.0, "{l}");
    }

    #[test]
    fn nan_and_infinite_samples_count_as_silence() {
        let clean = sine(0.1, 997.0, 2.0, 24_000);
        let mut dirty = clean.clone();
        dirty[1000] = f32::NAN;
        dirty[2000] = f32::INFINITY;
        let reference = integrated_lufs(&clean, 24_000).unwrap();
        let l = integrated_lufs(&dirty, 24_000).unwrap();
        assert!(l.is_finite());
        assert!((l - reference).abs() < 0.2, "{l} vs {reference}");
    }
}
