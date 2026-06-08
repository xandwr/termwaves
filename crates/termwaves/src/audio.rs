//! PipeWire system-output capture.

use std::convert::TryInto;
use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use pipewire as pw;
use pw::{properties::properties, spa};
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::Pod;

use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Producer as _, Split};

const RING_CAPACITY: usize = 96_000;

type RingProducer = ringbuf::wrap::caching::Caching<Arc<HeapRb<f32>>, true, false>;
type RingConsumer = ringbuf::wrap::caching::Caching<Arc<HeapRb<f32>>, false, true>;

/// Negotiated stream format, shared from the capture thread to the consumer.
#[derive(Default)]
struct SharedFormat {
    channels: AtomicU32,
    rate: AtomicU32,
}

/// Handle the TUI uses to consume audio.
///
/// The capture thread is detached and runs until the process exits; dropping
/// this handle stops draining the ring but does not tear the thread down.
pub struct CaptureHandle {
    consumer: RingConsumer,
    format: Arc<SharedFormat>,
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

    /// Drain available interleaved samples into `out`, returning how many.
    pub fn read(&mut self, out: &mut [f32]) -> usize {
        self.consumer.pop_slice(out)
    }
}

struct CaptureState {
    info: spa::param::audio::AudioInfoRaw,
    format: Arc<SharedFormat>,
    producer: RingProducer,
    scratch: Vec<f32>,
}

/// Start capturing system output on a dedicated PipeWire thread.
pub fn start() -> CaptureHandle {
    let ring = Arc::new(HeapRb::<f32>::new(RING_CAPACITY));
    let (producer, consumer) = ring.split();

    let format = Arc::new(SharedFormat::default());

    let thread_format = format.clone();
    std::thread::Builder::new()
        .name("termwaves-capture".into())
        .spawn(move || {
            if let Err(e) = run_capture(producer, thread_format) {
                eprintln!("capture thread error: {e}");
            }
        })
        .expect("failed to spawn capture thread");

    CaptureHandle { consumer, format }
}

fn run_capture(
    producer: RingProducer,
    format: Arc<SharedFormat>,
) -> Result<(), Box<dyn std::error::Error>> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

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
            let n_bytes = data.chunk().size() as usize;
            let Some(bytes) = data.data() else { return };

            state.scratch.clear();
            state.scratch.extend(
                bytes[..n_bytes]
                    .chunks_exact(mem::size_of::<f32>())
                    .map(|b| f32::from_le_bytes(b.try_into().unwrap())),
            );
            let _ = state.producer.push_slice(&state.scratch);
        })
        .register()?;

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

    mainloop.run();
    Ok(())
}
