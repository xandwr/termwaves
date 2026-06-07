//! Log-spaced frequency spectrum, the second view the TUI renders from.
//!
//! [`Spectrum`] pulls a window of recent samples out of a [`WaveScope`] channel,
//! runs a real FFT over a Hann-windowed copy, and folds the (linearly spaced)
//! FFT bins into a fixed set of log-spaced frequency bands. The result is a
//! per-band magnitude in dB: the shape a bar-graph analyzer wants.
//!
//! Two logarithms live here, and they're independent:
//!   * **frequency (horizontal):** band edges grow geometrically, so each band
//!     spans a roughly constant *ratio* of frequency (≈ musical pitch).
//!   * **amplitude (vertical):** band energy is converted to dB before display.
//!
//! The FFT itself is always linear (`rate/N` Hz per bin); the log spacing is a
//! pure display choice applied when bins are folded into bands. At low
//! frequencies a band may cover less than one bin: see [`Spectrum::compute`].

use std::f32::consts::PI;

use crate::scope::WaveScope;

/// FFT size, in samples. Must be a power of two for the radix-2 transform.
/// 2048 @ 48k gives ~23 Hz bins and ~43 ms of latency: a reasonable balance
/// between low-end resolution and responsiveness for a visualizer.
const FFT_SIZE: usize = 2048;

/// Floor for dB conversion: magnitudes at or below this map to 0.0 in the
/// normalized output. -90 dB is below the noise of any real playback path.
const DB_FLOOR: f32 = -90.0;

/// One frequency band: its center frequency (for labeling/debug) and its
/// current magnitude, normalized to `0.0..=1.0` where 1.0 is full-scale (0 dB).
#[derive(Clone, Copy, Debug, Default)]
pub struct Band {
    pub center_hz: f32,
    /// `0.0..=1.0`, already log-scaled (dB) and normalized against [`DB_FLOOR`].
    pub magnitude: f32,
}

/// Computes a log-spaced magnitude spectrum from a [`WaveScope`] channel.
///
/// All scratch is owned and reused, so [`Spectrum::compute`] never allocates
/// after construction. Build once with the desired band count; call `compute`
/// each frame.
pub struct Spectrum {
    /// Hann window coefficients, precomputed for `FFT_SIZE`.
    window: Vec<f32>,
    /// Windowed real input copied from the scope each frame.
    samples: Vec<f32>,
    /// FFT working buffers (real / imaginary), length `FFT_SIZE`.
    re: Vec<f32>,
    im: Vec<f32>,
    /// Bit-reversal permutation indices for `FFT_SIZE` (precomputed).
    rev: Vec<usize>,
    /// Inclusive FFT-bin range `[lo, hi]` feeding each output band.
    band_bins: Vec<(usize, usize)>,
    /// Output bands, reused across frames.
    bands: Vec<Band>,
}

impl Spectrum {
    /// Build a spectrum with `n_bands` log-spaced bands spanning roughly
    /// `min_hz..=max_hz`, given the capture `sample_rate` (Hz).
    ///
    /// `sample_rate` only fixes the bin→frequency mapping; pass the scope's
    /// negotiated rate. `min_hz` is clamped up to the first usable bin and
    /// `max_hz` down to Nyquist.
    pub fn new(sample_rate: u32, n_bands: usize, min_hz: f32, max_hz: f32) -> Self {
        let n = FFT_SIZE;
        let window = (0..n)
            .map(|i| {
                // Periodic Hann: 0.5 - 0.5*cos(2πi/N). Tapers the window edges to
                // suppress spectral leakage from the implicit rectangular cut.
                0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos()
            })
            .collect();

        let rev = bit_reversal(n);

        // Map the requested frequency range onto FFT bins, then space the band
        // *edges* geometrically across that range. Folding linear bins into
        // these log-spaced edges is where the horizontal log axis comes from.
        let bin_hz = sample_rate as f32 / n as f32;
        let nyquist_bin = n / 2; // bins 0..=N/2 are the unique (real-input) ones
        let lo_hz = min_hz.max(bin_hz).min(max_hz);
        let hi_hz = max_hz.min(nyquist_bin as f32 * bin_hz).max(lo_hz);

        let mut band_bins = Vec::with_capacity(n_bands);
        let mut bands = Vec::with_capacity(n_bands);
        let ratio = (hi_hz / lo_hz).powf(1.0 / n_bands as f32);
        for b in 0..n_bands {
            // Geometric edges: edge(b) = lo * ratio^b. Constant ratio per band.
            let f_lo = lo_hz * ratio.powi(b as i32);
            let f_hi = lo_hz * ratio.powi(b as i32 + 1);
            let center_hz = (f_lo * f_hi).sqrt(); // geometric mean
            // Bins covering [f_lo, f_hi). Clamp into the usable range and ensure
            // each band claims at least one bin so low bands never go empty.
            let mut lo = (f_lo / bin_hz).floor() as usize;
            let mut hi = (f_hi / bin_hz).ceil() as usize;
            lo = lo.clamp(1, nyquist_bin); // skip DC (bin 0)
            hi = hi.clamp(lo, nyquist_bin);
            band_bins.push((lo, hi));
            bands.push(Band {
                center_hz,
                magnitude: 0.0,
            });
        }

        Self {
            window,
            samples: vec![0.0; n],
            re: vec![0.0; n],
            im: vec![0.0; n],
            rev,
            band_bins,
            bands,
        }
    }

    /// Recompute the spectrum from the most recent audio on `channel`.
    /// Returns the band slice (also accessible via [`Spectrum::bands`]).
    pub fn compute(&mut self, scope: &WaveScope, channel: usize) -> &[Band] {
        let n = self.samples.len();

        // Pull the freshest FFT_SIZE samples (zero-padded if not yet filled),
        // applying the Hann window as we copy.
        scope.samples_into(channel, &mut self.samples);
        for i in 0..n {
            self.re[i] = self.samples[i] * self.window[i];
            self.im[i] = 0.0;
        }

        fft_in_place(&mut self.re, &mut self.im, &self.rev);

        // Fold linear bins into log bands. Each band takes the *mean* power of
        // its bins (mean, not sum, so wide high bands aren't unfairly louder
        // than narrow low ones), then converts to dB.
        //
        // Note the asymmetry log spacing forces: a low band like (lo=1, hi=2)
        // averages a single bin, while a high band may average hundreds. That's
        // inherent: the linear FFT gives the least resolution exactly where the
        // log axis wants the most. Raising FFT_SIZE is the only real fix.
        for (band, &(lo, hi)) in self.bands.iter_mut().zip(&self.band_bins) {
            let mut power = 0.0f32;
            for k in lo..hi {
                // Power = re² + im². Magnitude normalized so a full-scale tone
                // bin reads ~1.0 before windowing loss (the window costs ~6 dB,
                // absorbed into DB_FLOOR's headroom: fine for a visualizer).
                let p = self.re[k] * self.re[k] + self.im[k] * self.im[k];
                power += p;
            }
            let count = (hi - lo).max(1) as f32;
            let mag = (power / count).sqrt() / (n as f32 / 2.0);
            band.magnitude = to_db_normalized(mag);
        }

        &self.bands
    }

    /// The most recently computed bands (low frequency first).
    pub fn bands(&self) -> &[Band] {
        &self.bands
    }
}

/// Convert a linear magnitude (`0.0..`) to `0.0..=1.0`, where 1.0 is 0 dBFS and
/// 0.0 is [`DB_FLOOR`] or quieter.
fn to_db_normalized(mag: f32) -> f32 {
    if mag <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * mag.log10();
    ((db - DB_FLOOR) / -DB_FLOOR).clamp(0.0, 1.0)
}

/// Precompute the bit-reversal permutation for a length-`n` (power-of-two) FFT.
fn bit_reversal(n: usize) -> Vec<usize> {
    let bits = n.trailing_zeros();
    (0..n).map(|i| (i as u32).reverse_bits() >> (32 - bits)).map(|x| x as usize).collect()
}

/// In-place iterative radix-2 Cooley–Tukey FFT.
///
/// `re`/`im` hold the complex input and are overwritten with the transform;
/// `rev` is the precomputed bit-reversal permutation (see [`bit_reversal`]).
/// Length must be a power of two and match `rev.len()`.
///
/// Hand-rolled to avoid a dependency. If profiling shows the FFT dominating a
/// frame, swap this for `rustfft` (plan once, reuse): the call site only needs
/// `re`/`im` filled, so the rest of `compute` is unaffected.
fn fft_in_place(re: &mut [f32], im: &mut [f32], rev: &[usize]) {
    let n = re.len();

    // Reorder into bit-reversed index order (decimation in time).
    for i in 0..n {
        let j = rev[i];
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    // Butterflies over successively doubling sub-transform lengths.
    let mut len = 2;
    while len <= n {
        let half = len / 2;
        // Principal twiddle for this stage: e^{-2πi/len}.
        let ang = -2.0 * PI / len as f32;
        let (wstep_re, wstep_im) = (ang.cos(), ang.sin());
        let mut start = 0;
        while start < n {
            // Walk the twiddle around the unit circle by complex multiply,
            // rather than calling sin/cos per butterfly.
            let (mut w_re, mut w_im) = (1.0f32, 0.0f32);
            for k in 0..half {
                let a = start + k;
                let b = a + half;
                let t_re = w_re * re[b] - w_im * im[b];
                let t_im = w_re * im[b] + w_im * re[b];
                re[b] = re[a] - t_re;
                im[b] = im[a] - t_im;
                re[a] += t_re;
                im[a] += t_im;
                // w *= wstep
                let nw_re = w_re * wstep_re - w_im * wstep_im;
                let nw_im = w_re * wstep_im + w_im * wstep_re;
                w_re = nw_re;
                w_im = nw_im;
            }
            start += len;
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pure sine at a known frequency must light up the band containing it and
    /// leave distant bands near silence: verifies FFT + bin→band mapping.
    #[test]
    fn pure_tone_lands_in_its_band() {
        let rate = 48_000u32;
        let mut s = Spectrum::new(rate, 24, 30.0, 16_000.0);
        // Synthesize a 1 kHz sine directly into the FFT buffers, bypassing the
        // scope (we only exercise the transform + folding here).
        let freq = 1000.0f32;
        let n = s.samples.len();
        for i in 0..n {
            let v = (2.0 * PI * freq * i as f32 / rate as f32).sin();
            s.re[i] = v * s.window[i];
            s.im[i] = 0.0;
        }
        fft_in_place(&mut s.re, &mut s.im, &s.rev);
        for (band, &(lo, hi)) in s.bands.iter_mut().zip(&s.band_bins) {
            let mut power = 0.0f32;
            for k in lo..hi {
                power += s.re[k] * s.re[k] + s.im[k] * s.im[k];
            }
            let mag = (power / (hi - lo).max(1) as f32).sqrt() / (n as f32 / 2.0);
            band.magnitude = to_db_normalized(mag);
        }

        // Loudest band must contain 1 kHz.
        let loudest = s
            .bands
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.magnitude.partial_cmp(&b.1.magnitude).unwrap())
            .unwrap()
            .0;
        let (lo, hi) = s.band_bins[loudest];
        let bin_hz = rate as f32 / n as f32;
        assert!(
            (lo as f32 * bin_hz..=hi as f32 * bin_hz).contains(&freq),
            "loudest band {loudest} spans {:.0}..{:.0} Hz, expected to contain {freq} Hz",
            lo as f32 * bin_hz,
            hi as f32 * bin_hz,
        );
    }
}
