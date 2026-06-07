//! Log-spaced frequency spectrum, the second view the TUI renders from.

use std::f32::consts::PI;

use crate::scope::WaveScope;

const FFT_SIZE: usize = 4096;
const DB_FLOOR: f32 = -90.0;
const LOUDNESS_EMA_ALPHA: f32 = 0.001;
const ADAPTIVE_GAIN_CLAMP_DB: f32 = 12.0;

/// One frequency band: center frequency and `0.0..=1.0` normalized magnitude.
#[derive(Clone, Copy, Debug, Default)]
pub struct Band {
    #[allow(dead_code)]
    pub center_hz: f32,
    pub magnitude: f32,
}

/// Computes a log-spaced magnitude spectrum from a [`WaveScope`] channel.
pub struct Spectrum {
    window: Vec<f32>,
    samples: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    rev: Vec<usize>,
    band_bins: Vec<(usize, usize)>,
    weight_db: Vec<f32>,
    bands: Vec<Band>,
    loudness_baseline_db: Option<f32>,
}

impl Spectrum {
    /// Build a spectrum with `n_bands` log-spaced bands over `min_hz..=max_hz`.
    pub fn new(sample_rate: u32, n_bands: usize, min_hz: f32, max_hz: f32) -> Self {
        let n = FFT_SIZE;
        let window = (0..n)
            .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos())
            .collect();

        let rev = bit_reversal(n);

        let bin_hz = sample_rate as f32 / n as f32;
        let nyquist_bin = n / 2;
        let lo_hz = min_hz.max(bin_hz).min(max_hz);
        let hi_hz = max_hz.min(nyquist_bin as f32 * bin_hz).max(lo_hz);

        let mut band_bins = Vec::with_capacity(n_bands);
        let mut weight_db = Vec::with_capacity(n_bands);
        let mut bands = Vec::with_capacity(n_bands);
        let ratio = (hi_hz / lo_hz).powf(1.0 / n_bands as f32);
        for b in 0..n_bands {
            let f_lo = lo_hz * ratio.powi(b as i32);
            let f_hi = lo_hz * ratio.powi(b as i32 + 1);
            let center_hz = (f_lo * f_hi).sqrt();
            let mut lo = (f_lo / bin_hz).floor() as usize;
            let mut hi = (f_hi / bin_hz).ceil() as usize;
            lo = lo.clamp(1, nyquist_bin);
            hi = hi.clamp(lo, nyquist_bin);
            band_bins.push((lo, hi));
            weight_db.push(a_weight_db(center_hz));
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
            weight_db,
            bands,
            loudness_baseline_db: None,
        }
    }

    /// Recompute the spectrum from the most recent audio on `channel`.
    pub fn compute(&mut self, scope: &WaveScope, channel: usize) -> &[Band] {
        let n = self.samples.len();

        scope.samples_into(channel, &mut self.samples);
        for i in 0..n {
            self.re[i] = self.samples[i] * self.window[i];
            self.im[i] = 0.0;
        }

        fft_in_place(&mut self.re, &mut self.im, &self.rev);

        let mut weighted_power = 0.0f32;
        for (i, (&(lo, hi), &w_db)) in self.band_bins.iter().zip(&self.weight_db).enumerate() {
            let mut power = 0.0f32;
            for k in lo..hi {
                let p = self.re[k] * self.re[k] + self.im[k] * self.im[k];
                power += p;
            }
            let count = (hi - lo).max(1) as f32;
            let mag = (power / count).sqrt() / (n as f32 / 2.0);
            let w_lin = 10.0f32.powf(w_db / 10.0);
            weighted_power += mag * mag * w_lin;
            self.samples[i] = mag;
        }

        let frame_loudness_db = if weighted_power > 0.0 {
            10.0 * weighted_power.log10()
        } else {
            DB_FLOOR
        };

        let baseline_db = match self.loudness_baseline_db {
            Some(prev) => {
                let next = prev + LOUDNESS_EMA_ALPHA * (frame_loudness_db - prev);
                self.loudness_baseline_db = Some(next);
                next
            }
            None => {
                self.loudness_baseline_db = Some(frame_loudness_db);
                frame_loudness_db
            }
        };

        let adaptive_gain_db = (frame_loudness_db - baseline_db)
            .clamp(-ADAPTIVE_GAIN_CLAMP_DB, ADAPTIVE_GAIN_CLAMP_DB);

        for ((band, &w_db), i) in self.bands.iter_mut().zip(&self.weight_db).zip(0..) {
            let mag = self.samples[i];
            band.magnitude = to_db_normalized(mag, w_db + adaptive_gain_db);
        }

        &self.bands
    }

    /// The most recently computed bands (low frequency first).
    pub fn bands(&self) -> &[Band] {
        &self.bands
    }
}

/// Convert a linear magnitude to `0.0..=1.0`, applying `gain_db` before normalizing.
fn to_db_normalized(mag: f32, gain_db: f32) -> f32 {
    if mag <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * mag.log10() + gain_db;
    ((db - DB_FLOOR) / -DB_FLOOR).clamp(0.0, 1.0)
}

/// A-weighting gain in dB at frequency `f` (Hz), the IEC 61672 curve.
fn a_weight_db(f: f32) -> f32 {
    let f2 = f * f;
    let num = 12194.0f32.powi(2) * f2 * f2;
    let den = (f2 + 20.6f32.powi(2))
        * ((f2 + 107.7f32.powi(2)) * (f2 + 737.9f32.powi(2))).sqrt()
        * (f2 + 12194.0f32.powi(2));
    let ra = num / den;
    20.0 * ra.log10() + 2.0
}

/// Precompute the bit-reversal permutation for a length-`n` (power-of-two) FFT.
fn bit_reversal(n: usize) -> Vec<usize> {
    let bits = n.trailing_zeros();
    (0..n)
        .map(|i| (i as u32).reverse_bits() >> (32 - bits))
        .map(|x| x as usize)
        .collect()
}

/// In-place iterative radix-2 Cooley–Tukey FFT.
fn fft_in_place(re: &mut [f32], im: &mut [f32], rev: &[usize]) {
    let n = re.len();

    for i in 0..n {
        let j = rev[i];
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    let mut len = 2;
    while len <= n {
        let half = len / 2;
        let ang = -2.0 * PI / len as f32;
        let (wstep_re, wstep_im) = (ang.cos(), ang.sin());
        let mut start = 0;
        while start < n {
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

    #[test]
    fn pure_tone_lands_in_its_band() {
        let rate = 48_000u32;
        let mut s = Spectrum::new(rate, 24, 30.0, 16_000.0);
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
            band.magnitude = to_db_normalized(mag, a_weight_db(band.center_hz));
        }

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
