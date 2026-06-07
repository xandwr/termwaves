//! Waveform model the TUI renders from.

use crate::audio::CaptureHandle;

const HISTORY_PER_CHANNEL: usize = 24_000;

/// One column of a rendered waveform: min and max sample over its slice.
#[derive(Clone, Copy, Debug, Default)]
pub struct Envelope {
    pub min: f32,
    pub max: f32,
}

impl Envelope {
    /// Peak magnitude in this column, in `0.0..=1.0` for in-range audio.
    #[allow(dead_code)]
    pub fn peak(&self) -> f32 {
        self.min.abs().max(self.max.abs())
    }
}

struct ChannelHistory {
    buf: Vec<f32>,
    head: usize,
    filled: bool,
}

impl ChannelHistory {
    fn new() -> Self {
        Self {
            buf: vec![0.0; HISTORY_PER_CHANNEL],
            head: 0,
            filled: false,
        }
    }

    fn push(&mut self, sample: f32) {
        self.buf[self.head] = sample;
        self.head += 1;
        if self.head == self.buf.len() {
            self.head = 0;
            self.filled = true;
        }
    }

    fn len(&self) -> usize {
        if self.filled {
            self.buf.len()
        } else {
            self.head
        }
    }

    fn for_recent(&self, n: usize, mut f: impl FnMut(usize, f32)) {
        let len = self.len();
        let n = n.min(len);
        let start = len - n;
        for i in 0..n {
            let logical = start + i;
            let phys = if self.filled {
                (self.head + logical) % self.buf.len()
            } else {
                logical
            };
            f(i, self.buf[phys]);
        }
    }
}

/// Drains a [`CaptureHandle`] into per-channel histories and renders envelopes.
pub struct WaveScope {
    handle: CaptureHandle,
    channels: Vec<ChannelHistory>,
    drain: Vec<f32>,
    n_channels: usize,
}

impl WaveScope {
    pub fn new(handle: CaptureHandle) -> Self {
        Self {
            handle,
            channels: Vec::new(),
            drain: vec![0.0; 8192],
            n_channels: 0,
        }
    }

    /// True once capture has negotiated a format and samples are flowing.
    pub fn is_ready(&self) -> bool {
        self.handle.is_ready()
    }

    pub fn sample_rate(&self) -> u32 {
        self.handle.sample_rate()
    }

    pub fn channel_count(&self) -> usize {
        self.n_channels
    }

    /// Pull all currently-available audio into the per-channel histories.
    pub fn tick(&mut self) {
        let ch = self.handle.channels() as usize;
        if ch == 0 {
            return;
        }
        if ch != self.n_channels {
            self.channels = (0..ch).map(|_| ChannelHistory::new()).collect();
            self.n_channels = ch;
        }

        loop {
            let n = self.handle.read(&mut self.drain);
            if n == 0 {
                break;
            }
            for (i, &sample) in self.drain[..n].iter().enumerate() {
                self.channels[i % ch].push(sample);
            }
            if n < self.drain.len() {
                break;
            }
        }
    }

    /// Copy the most recent `out.len()` samples of `channel` into `out`, oldest
    /// first, zero-padding the lead. Returns how many real samples were written.
    pub fn samples_into(&self, channel: usize, out: &mut [f32]) -> usize {
        let Some(hist) = self.channels.get(channel) else {
            out.fill(0.0);
            return 0;
        };
        let n = out.len().min(hist.len());
        let pad = out.len() - n;
        out[..pad].fill(0.0);
        hist.for_recent(n, |i, sample| out[pad + i] = sample);
        n
    }

    /// Summarize the most recent audio on `channel` as `cols` min/max envelopes,
    /// oldest first. `window` is how many recent samples to span (zoom).
    pub fn envelope(&self, channel: usize, cols: usize, window: usize) -> Vec<Envelope> {
        let Some(hist) = self.channels.get(channel) else {
            return Vec::new();
        };
        if cols == 0 {
            return Vec::new();
        }
        let window = window.min(hist.len());
        if window == 0 {
            return Vec::new();
        }

        let mut out = vec![
            Envelope {
                min: f32::INFINITY,
                max: f32::NEG_INFINITY,
            };
            cols
        ];
        hist.for_recent(window, |i, sample| {
            let col = (i * cols) / window;
            let col = col.min(cols - 1);
            let e = &mut out[col];
            if sample < e.min {
                e.min = sample;
            }
            if sample > e.max {
                e.max = sample;
            }
        });
        for e in &mut out {
            if !e.min.is_finite() {
                *e = Envelope::default();
            }
        }
        out
    }
}
