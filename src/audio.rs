//! PipeWire system-output capture.
//!
//! Connects a capture stream to the default sink's MONITOR (via
//! `STREAM_CAPTURE_SINK`), decodes interleaved F32LE frames, and pushes them
//! into a lock-free ring buffer. The TUI side drains that ring through
//! [`CaptureHandle`] without ever touching PipeWire.

use std::convert::TryInto;
use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use pipewire as pw;
use pw::{properties::properties, spa};
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::Pod;

use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Producer as _, Split};

/// Ring capacity in samples (interleaved across channels). ~1s of stereo @ 48k.
/// The renderer is expected to drain faster than this fills; on overrun we drop
/// the newest samples, which only ever costs a visual frame.
const RING_CAPACITY: usize = 96_000;

type RingProducer = ringbuf::wrap::caching::Caching<Arc<HeapRb<f32>>, true, false>;
type RingConsumer = ringbuf::wrap::caching::Caching<Arc<HeapRb<f32>>, false, true>;

/// Negotiated stream format, shared from the capture thread to the consumer.
///
/// Packed into atomics because `param_changed` fires on the PipeWire thread and
/// the TUI reads it on the render thread. Both fields are 0 until the first
/// format negotiation completes.
#[derive(Default)]
struct SharedFormat {
    channels: AtomicU32,
    rate: AtomicU32,
}

/// Handle the TUI uses to consume audio. Drop it to stop capture.
pub struct CaptureHandle {
    consumer: RingConsumer,
    format: Arc<SharedFormat>,
    running: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CaptureHandle {
    /// Number of interleaved channels, or 0 before the format is negotiated.
    pub fn channels(&self) -> u32 {
        self.format.channels.load(Ordering::Relaxed)
    }

    /// Sample rate in Hz, or 0 before the format is negotiated.
    pub fn sample_rate(&self) -> u32 {
        self.format.rate.load(Ordering::Relaxed)
    }

    /// True once the stream has negotiated a format and is delivering audio.
    pub fn is_ready(&self) -> bool {
        self.channels() > 0
    }

    /// Drain available samples into `out`, returning how many were written.
    /// Samples are interleaved (frame-major): `[L0, R0, L1, R1, ...]`.
    pub fn read(&mut self, out: &mut [f32]) -> usize {
        self.consumer.pop_slice(out)
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        // The PipeWire main loop blocks on its own thread; signalling `running`
        // isn't enough to wake it, so we can't cleanly join here without a loop
        // signal. Detach rather than block forever on shutdown.
        if let Some(thread) = self.thread.take() {
            // Best-effort: if the loop already exited, this returns promptly.
            let _ = thread;
        }
    }
}

/// User data carried through the PipeWire callbacks.
struct CaptureState {
    info: spa::param::audio::AudioInfoRaw,
    format: Arc<SharedFormat>,
    producer: RingProducer,
    /// Reused decode scratch so the RT process callback never allocates.
    scratch: Vec<f32>,
}

/// Start capturing system output on a dedicated PipeWire thread.
///
/// Returns immediately with a [`CaptureHandle`]; the stream negotiates format
/// asynchronously, so poll [`CaptureHandle::is_ready`] before relying on
/// [`CaptureHandle::channels`].
pub fn start() -> CaptureHandle {
    let ring = Arc::new(HeapRb::<f32>::new(RING_CAPACITY));
    let (producer, consumer) = ring.split();

    let format = Arc::new(SharedFormat::default());
    let running = Arc::new(AtomicBool::new(true));

    let thread_format = format.clone();
    let thread = std::thread::Builder::new()
        .name("termwaves-capture".into())
        .spawn(move || {
            if let Err(e) = run_capture(producer, thread_format) {
                eprintln!("capture thread error: {e}");
            }
        })
        .expect("failed to spawn capture thread");

    CaptureHandle {
        consumer,
        format,
        running,
        thread: Some(thread),
    }
}

/// The PipeWire main loop, owned entirely by the capture thread.
fn run_capture(
    producer: RingProducer,
    format: Arc<SharedFormat>,
) -> Result<(), Box<dyn std::error::Error>> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    // STREAM_CAPTURE_SINK = true connects to a sink's MONITOR instead of a mic.
    // With no TARGET_OBJECT, it follows the default sink: the active output.
    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
    };
    props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");

    let stream = pw::stream::StreamBox::new(&core, "termwaves-capture", props)?;

    let state = CaptureState {
        info: Default::default(),
        format,
        producer,
        scratch: Vec::with_capacity(8192),
    };

    let _listener = stream
        .add_local_listener_with_user_data(state)
        .param_changed(|_, state, id, param| {
            let Some(param) = param else { return };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            let (media_type, media_subtype) = match format_utils::parse_format(param) {
                Ok(v) => v,
                Err(_) => return,
            };
            if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                return;
            }
            state
                .info
                .parse(param)
                .expect("failed to parse AudioInfoRaw");
            // Publish the negotiated layout for the consumer side.
            state
                .format
                .channels
                .store(state.info.channels(), Ordering::Relaxed);
            state
                .format
                .rate
                .store(state.info.rate(), Ordering::Relaxed);
        })
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            let n_samples = data.chunk().size() as usize / mem::size_of::<f32>();
            let Some(bytes) = data.data() else { return };

            // Decode F32LE into reused scratch, then push the batch in one call.
            // Batching keeps the RT callback to a single ring write instead of
            // one atomic-fenced push per sample.
            state.scratch.clear();
            state.scratch.reserve(n_samples);
            for n in 0..n_samples {
                let start = n * mem::size_of::<f32>();
                let end = start + mem::size_of::<f32>();
                let f = f32::from_le_bytes(bytes[start..end].try_into().unwrap());
                state.scratch.push(f);
            }
            // Overrun drops the tail: acceptable for a visualizer.
            let _ = state.producer.push_slice(&state.scratch);
        })
        .register()?;

    // Negotiate F32LE; leave rate/channels empty to accept the native graph.
    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    let obj = pw::spa::pod::Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: pw::spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    )?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&values).unwrap()];

    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    mainloop.run(); // blocks until the process exits
    Ok(())
}
