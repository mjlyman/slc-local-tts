//! A look-ahead peak limiter: it turns down only the moments that would pass
//! the ceiling, and eases them down and back up so the change isn't heard as
//! a click.

use std::collections::VecDeque;

/// How far ahead the limiter looks, and how long it takes to turn down.
const LOOK_AHEAD_SECONDS: f32 = 0.005;
/// How slowly the gain comes back up after a peak.
const RELEASE_SECONDS: f32 = 0.080;

/// Turns down every sample that would go past `ceiling`, smoothly.
///
/// Works on the whole line at once (it runs after synthesis, not live), in
/// linear time. Samples under the ceiling far from any peak are
/// left exactly as they were. NaN samples are left as NaN and don't affect
/// their neighbours.
pub fn limit(samples: &mut [f32], sample_rate: u32, ceiling: f32) {
    debug_assert!(ceiling > 0.0, "the ceiling must be positive");
    if samples.is_empty() {
        return;
    }
    let fs = sample_rate as f32;
    let look = ((LOOK_AHEAD_SECONDS * fs).round() as usize).max(1);
    let release = 1.0 - (-1.0 / (RELEASE_SECONDS * fs)).exp();

    // need[n]: the gain sample n itself needs to stay under the ceiling.
    let need: Vec<f32> = samples.iter().map(|&x| needed_gain(x, ceiling)).collect();

    // rel[n]: the gain envelope. It drops at once to the smallest `need` in
    // the next `look` samples (the "hold"), and recovers towards 1 slowly.
    let mut rel = Vec::with_capacity(need.len());
    // Sliding minimum of need[n ..= n + look] with a monotonic deque: it holds
    // indices whose `need` values increase from front to back, so the front
    // is always the window's minimum. Each index is pushed and popped at most
    // once, so the whole loop is O(n) rather than O(n * look).
    let mut window: VecDeque<usize> = VecDeque::new();
    let mut next_in = 0;
    let mut previous = 1.0f32;
    for n in 0..need.len() {
        let window_end = (n + look).min(need.len() - 1);
        while next_in <= window_end {
            // A newer, smaller value makes older, larger ones useless: they
            // leave the window first and can never be its minimum again.
            while window.back().is_some_and(|&b| need[b] >= need[next_in]) {
                window.pop_back();
            }
            window.push_back(next_in);
            next_in += 1;
        }
        while window.front().is_some_and(|&f| f < n) {
            window.pop_front();
        }
        // The window always holds index n at least, so it isn't empty.
        let hold = need[window[0]];
        let r = hold.min(previous + (1.0 - previous) * release);
        rel.push(r);
        previous = r;
    }

    // gain[n]: the mean of rel[n - look + 1 ..= n], kept as a running sum.
    // This smooths the sudden drops in `rel` into a ramp `look` samples long.
    //
    // Why gain[n] <= need[n], so the peak is caught without the clamp below:
    // every rel[m] in the window has n - look < m <= n, so need[n] lies in
    // the hold window need[m ..= m + look], and rel[m] <= hold[m] <= need[n].
    // A mean of values that are all <= need[n] is itself <= need[n].
    // Before the start, rel counts as rel[0], which is the minimum of
    // need[0 ..= look], so the same holds for the first samples.
    let before_start = rel[0];
    let mut sum = before_start * look as f32;
    for n in 0..samples.len() {
        let leaving = if n >= look {
            rel[n - look]
        } else {
            before_start
        };
        sum += rel[n] - leaving;
        // `min(1.0)` keeps rounding in the running sum from ever turning
        // audio up.
        let gain = (sum / look as f32).min(1.0);
        // The clamp is a safety net for rounding; `clamp` leaves NaN as NaN.
        samples[n] = (samples[n] * gain).clamp(-ceiling, ceiling);
    }
}

/// The gain one sample needs to stay at or under `ceiling`. Silence, NaN
/// and infinite samples need none (1.0); the final clamp catches infinities.
fn needed_gain(x: f32, ceiling: f32) -> f32 {
    let magnitude = x.abs();
    if magnitude.is_finite() && magnitude > ceiling {
        ceiling / magnitude
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 22_050;
    const CEILING: f32 = 0.891;

    fn sine(amplitude: f32, seconds: f32, rate: u32) -> Vec<f32> {
        let n = (seconds * rate as f32) as usize;
        (0..n)
            .map(|i| {
                amplitude * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate as f32).sin()
            })
            .collect()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |p, s| p.max(s.abs()))
    }

    #[test]
    fn audio_under_the_ceiling_is_untouched() {
        let input = sine(0.5, 1.0, RATE);
        let mut output = input.clone();
        limit(&mut output, RATE, CEILING);
        assert_eq!(output, input);
    }

    #[test]
    fn a_spike_is_held_to_the_ceiling() {
        let spike_at = 22_050;
        let mut input = sine(0.1, 2.0, RATE);
        input[spike_at] = 2.0;
        let mut output = input.clone();
        limit(&mut output, RATE, CEILING);

        assert!(peak(&output) <= CEILING + 1e-6, "peak {}", peak(&output));

        let look = (0.005 * RATE as f32).round() as usize;
        let untouched_until = spike_at - 2 * look;
        assert_eq!(output[..untouched_until], input[..untouched_until]);

        let recovered_from = spike_at + RATE as usize;
        for i in recovered_from..input.len() {
            assert!(
                (output[i] - input[i]).abs() < 1e-4,
                "sample {i}: {} vs {}",
                output[i],
                input[i]
            );
        }
    }

    /// How much the limiter scaled sample `i` (output over input).
    fn ratio(output: &[f32], input: &[f32], i: usize) -> f32 {
        output[i] / input[i]
    }

    #[test]
    fn a_spike_is_turned_down_ahead_of_time_not_clipped() {
        let spike_at = 22_050;
        let mut input = sine(0.1, 2.0, RATE);
        input[spike_at] = 2.0;
        let mut output = input.clone();
        limit(&mut output, RATE, CEILING);

        // The gain is already well down just before the spike arrives...
        let before = ratio(&output, &input, spike_at - 1);
        assert!(before < 0.6, "gain just before the spike: {before}");
        // ...and at the spike it is exactly what the spike needs, so the
        // spike is scaled down, not cut off by the final clamp.
        let need = CEILING / 2.0;
        let at = ratio(&output, &input, spike_at);
        assert!(
            (at - need).abs() < 1e-4,
            "gain at the spike: {at}, need {need}"
        );
    }

    #[test]
    fn a_spike_at_the_very_start_is_turned_down_not_clipped() {
        let spike_at = 10;
        let mut input = sine(0.1, 0.5, RATE);
        input[spike_at] = 2.0;
        let mut output = input.clone();
        limit(&mut output, RATE, CEILING);

        // The samples around the spike get (close to) the spike's own gain,
        // rather than full level next to a clipped spike.
        let need = CEILING / 2.0;
        for i in [spike_at - 1, spike_at, spike_at + 1] {
            let r = ratio(&output, &input, i);
            assert!(
                (r - need).abs() < 1e-3,
                "gain at sample {i}: {r}, need {need}"
            );
        }
    }

    #[test]
    fn nan_does_not_spread() {
        let mut samples = sine(1.5, 0.5, RATE);
        samples[1000] = f32::NAN;
        limit(&mut samples, RATE, CEILING);
        for (i, s) in samples.iter().enumerate() {
            if i == 1000 {
                continue;
            }
            assert!(s.is_finite(), "sample {i} is {s}");
            assert!(s.abs() <= CEILING, "sample {i} is {s}");
        }
    }

    #[test]
    fn empty_and_zeros_are_fine() {
        let mut empty: Vec<f32> = vec![];
        limit(&mut empty, RATE, CEILING);
        assert!(empty.is_empty());

        let mut zeros = vec![0.0f32; 1000];
        limit(&mut zeros, RATE, CEILING);
        assert!(zeros.iter().all(|&x| x == 0.0));
    }
}
