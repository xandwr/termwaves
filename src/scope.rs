//! Waveform model the TUI renders from.
//!
//! [`WaveScope`] owns the consumer side of the capture: each tick it drains the
//! ring, deinterleaves into per-channel rolling histories, and can summarize any
//! channel as a column-sized min/max envelope: the shape a terminal waveform
//! wants.

use crate::audio::CaptureHandle;

/// Samples of history kept per channel. ~0.5s @ 48k: enough to fill a wide
/// terminal at typical zoom while staying cheap to scan each frame.
const HISTORY_PER_CHANNEL: usize = 24_000;

/// One column of a rendered waveform: the min and max sample over the slice of
/// audio that column covers. Drawing the span between them gives the familiar
/// filled-envelope look (rather than aliasing a single sample per column).
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

/// Per-channel circular sample history fed from the capture ring.
struct ChannelHistory {
    buf: Vec<f32>,
    /// Index of the next write: `buf[head]` is the oldest sample.
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

    /// Number of valid samples currently stored.
    fn len(&self) -> usize {
        if self.filled {
            self.buf.len()
        } else {
            self.head
        }
    }

    /// Read the `n` most recent samples in chronological order via `f`.
    /// Calls `f(i, sample)` for `i in 0..n.min(len())`.
    fn for_recent(&self, n: usize, mut f: impl FnMut(usize, f32)) {
        let len = self.len();
        let n = n.min(len);
        // Oldest-of-the-window index within the logical (chronological) stream.
        let start = len - n;
        for i in 0..n {
            let logical = start + i;
            // Map logical position to physical slot.
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
    /// Scratch for ring drains, reused across ticks to avoid per-frame allocs.
    drain: Vec<f32>,
    /// Channel count last seen; histories are (re)built when this changes.
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
    /// Call once per render frame before [`Self::envelope`].
    pub fn tick(&mut self) {
        let ch = self.handle.channels() as usize;
        if ch == 0 {
            return; // format not negotiated yet
        }
        if ch != self.n_channels {
            // (Re)allocate histories on first format or a layout change.
            self.channels = (0..ch).map(|_| ChannelHistory::new()).collect();
            self.n_channels = ch;
        }

        loop {
            let n = self.handle.read(&mut self.drain);
            if n == 0 {
                break;
            }
            // Deinterleave: sample i belongs to channel `i % ch`.
            for (i, &sample) in self.drain[..n].iter().enumerate() {
                self.channels[i % ch].push(sample);
            }
            if n < self.drain.len() {
                break; // ring drained
            }
        }
    }

    /// Copy the most recent `out.len()` samples of `channel` into `out`, oldest
    /// first. Returns how many were written; if fewer than `out.len()` samples
    /// exist, the leading slots are zero-filled so `out` always reads as a
    /// contiguous window ending at "now" (the shape an FFT wants).
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
    /// oldest column first. Returns an empty vec for an out-of-range channel or
    /// before any audio has arrived.
    ///
    /// `window` is how many of the most recent samples to span across the
    /// columns: larger means more history per screen (zoomed out).
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
        // Assign each windowed sample to a column by its position in the window.
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
        // Columns that received no samples (window < cols) collapse to silence.
        for e in &mut out {
            if !e.min.is_finite() {
                *e = Envelope::default();
            }
        }
        out
    }
}
