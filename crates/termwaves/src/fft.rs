use std::f32::consts::PI;

pub struct Fft {
    window: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    rev: Vec<usize>,
}

impl Fft {
    pub fn new(n: usize) -> Self {
        debug_assert!(n.is_power_of_two(), "FFT size must be a power of two");
        let window = (0..n)
            .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos())
            .collect();
        Self {
            window,
            re: vec![0.0; n],
            im: vec![0.0; n],
            rev: bit_reversal(n),
        }
    }

    pub fn len(&self) -> usize {
        self.re.len()
    }

    pub fn transform(&mut self, samples: &[f32]) -> (&[f32], &[f32]) {
        debug_assert_eq!(samples.len(), self.re.len());
        for (re, (sample, w)) in self.re.iter_mut().zip(samples.iter().zip(&self.window)) {
            *re = sample * w;
        }
        self.im.fill(0.0);
        fft_in_place(&mut self.re, &mut self.im, &self.rev);
        (&self.re, &self.im)
    }
}

fn bit_reversal(n: usize) -> Vec<usize> {
    let bits = n.trailing_zeros();
    (0..n)
        .map(|i| ((i as u32).reverse_bits() >> (32 - bits)) as usize)
        .collect()
}

fn fft_in_place(re: &mut [f32], im: &mut [f32], rev: &[usize]) {
    let n = re.len();

    for (i, &j) in rev.iter().enumerate() {
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
    fn dc_input_lands_in_bin_zero() {
        let n = 64;
        let mut fft = Fft::new(n);
        let (re, im) = fft.transform(&vec![1.0; n]);
        let power = |k: usize| re[k] * re[k] + im[k] * im[k];
        let bin0 = power(0);
        let mid: f32 = (n / 4..n / 2).map(power).sum();
        assert!(bin0 > mid * 1000.0, "bin0={bin0}, mid-band sum={mid}");
    }

    #[test]
    fn pure_bin_concentrates() {
        let n = 256;
        let k = 8;
        let mut fft = Fft::new(n);
        let samples: Vec<f32> = (0..n)
            .map(|i| (2.0 * PI * k as f32 * i as f32 / n as f32).sin())
            .collect();
        let (re, im) = fft.transform(&samples);
        let power = |b: usize| re[b] * re[b] + im[b] * im[b];
        let peak = (1..n / 2)
            .max_by(|&a, &b| power(a).total_cmp(&power(b)))
            .unwrap();
        assert!(
            (peak as i32 - k as i32).abs() <= 1,
            "peak bin {peak}, expected ~{k}"
        );
    }
}
