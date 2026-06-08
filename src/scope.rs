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
    pub fn peak(&self) -> f32 {
        self.min.abs().max(self.max.abs())
    }
}

struct ChannelHistory {
    buf: Vec<f32>,
    head: usize,
    filled: bool,
    /// Min/max summary pyramid over the *logical* (oldest-first) history.
    /// `mips[0]` is one envelope per sample; each higher level halves the
    /// count by merging adjacent pairs. Rebuilt lazily when `dirty`.
    mips: Vec<Vec<Envelope>>,
    dirty: bool,
}

impl ChannelHistory {
    fn new() -> Self {
        Self {
            buf: vec![0.0; HISTORY_PER_CHANNEL],
            head: 0,
            filled: false,
            mips: Vec::new(),
            dirty: true,
        }
    }

    fn push(&mut self, sample: f32) {
        self.buf[self.head] = sample;
        self.head += 1;
        if self.head == self.buf.len() {
            self.head = 0;
            self.filled = true;
        }
        self.dirty = true;
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

    /// Rebuild the min/max summary pyramid from the current ring contents.
    /// O(len) total across all levels (geometric series). Called at most once
    /// per `tick`, regardless of how many frames are rendered in between.
    fn rebuild_mips(&mut self) {
        let len = self.len();
        // Level 0: one envelope per logical sample, oldest first.
        let mut level0 = Vec::with_capacity(len);
        self.for_recent(len, |_, s| {
            level0.push(Envelope { min: s, max: s });
        });

        let mut mips = Vec::new();
        mips.push(level0);
        while mips.last().map_or(0, |l| l.len()) > 1 {
            let prev = mips.last().unwrap();
            let mut next = Vec::with_capacity(prev.len().div_ceil(2));
            let mut i = 0;
            while i < prev.len() {
                let a = prev[i];
                let merged = if i + 1 < prev.len() {
                    let b = prev[i + 1];
                    Envelope {
                        min: a.min.min(b.min),
                        max: a.max.max(b.max),
                    }
                } else {
                    a
                };
                next.push(merged);
                i += 2;
            }
            mips.push(next);
        }

        self.mips = mips;
        self.dirty = false;
    }

    /// Summarize the most recent `window` samples as `cols` envelopes, oldest
    /// first. Each column owns sample range `[c*window/cols, (c+1)*window/cols)`;
    /// we read whichever mip level keeps that range a handful of entries, so the
    /// result is identical to a full O(window) scan but costs ~O(cols).
    fn envelope_from_mips(&self, cols: usize, window: usize, out: &mut [Envelope]) {
        debug_assert_eq!(out.len(), cols);
        let total = self.mips.first().map_or(0, |l| l.len());
        let window = window.min(total);
        if cols == 0 || window == 0 {
            out.fill(Envelope::default());
            return;
        }

        // Pick the coarsest level whose entries are no wider than one column, so
        // each column still aggregates >= 1 entry — guaranteeing we never widen
        // a column's true [lo,hi) sample span enough to miss or borrow a peak.
        let spc = window / cols; // samples per column (floor)
        let level = if spc <= 1 {
            0
        } else {
            // largest level with 2^level <= spc
            (usize::BITS - 1 - spc.leading_zeros()) as usize
        }
        .min(self.mips.len() - 1);

        let lvl = &self.mips[level];
        let scale = 1usize << level; // samples per entry at this level
        // Logical sample index of the window's first sample (oldest in view).
        let win_start = total - window;

        for (c, e_out) in out.iter_mut().enumerate() {
            // Sample range this column owns, in logical (oldest-first) coords.
            let s_lo = win_start + (c * window) / cols;
            let s_hi = win_start + ((c + 1) * window) / cols;
            if s_hi <= s_lo {
                // Zoomed past 1 sample/column: this column has no sample of its
                // own. Leave it empty, matching the pre-mip behavior.
                *e_out = Envelope::default();
                continue;
            }
            // Mip entries covering [s_lo, s_hi): floor(lo) .. ceil(hi).
            let e_lo = s_lo / scale;
            let e_hi = s_hi.div_ceil(scale).min(lvl.len());
            let mut acc = Envelope {
                min: f32::INFINITY,
                max: f32::NEG_INFINITY,
            };
            for entry in &lvl[e_lo..e_hi] {
                if entry.min < acc.min {
                    acc.min = entry.min;
                }
                if entry.max > acc.max {
                    acc.max = entry.max;
                }
            }
            *e_out = if acc.min.is_finite() {
                acc
            } else {
                Envelope::default()
            };
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

        // Refresh summary pyramids once per tick for channels that took new
        // samples. Per-frame `envelope()` then just reads them (O(cols)).
        for hist in &mut self.channels {
            if hist.dirty {
                hist.rebuild_mips();
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

        let mut out = vec![Envelope::default(); cols];
        hist.envelope_from_mips(cols, window, &mut out);
        out
    }
}
