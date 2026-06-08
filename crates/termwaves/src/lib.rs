//! termwaves — middleman the OS audio output and forward the samples downstream.
//!
//! This crate is the signal path and nothing else: it captures the system audio
//! output off PipeWire into a ring buffer ([`start`] → [`CaptureHandle`]), keeps a
//! rolling per-channel history you can summarize as envelopes ([`WaveScope`]), and
//! turns a channel into a log-spaced, A-weighted magnitude [`Spectrum`]. It pulls
//! in no terminal UI — rendering is a client's job. The `termwaves-client` crate
//! in this workspace (`client/`) is one such client.

mod audio;
mod fft;
mod scope;
mod spectrum;

pub use audio::{CaptureHandle, start};
pub use fft::Fft;
pub use scope::{Envelope, WaveScope};
pub use spectrum::{Band, Spectrum};
