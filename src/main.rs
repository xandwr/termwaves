mod audio;
mod scope;
mod spectrum;

use std::io::Write;

use scope::WaveScope;
use spectrum::Spectrum;

/// Smoke test for the capture + scope layers until the TUI lands.
///
/// Renders channel 0's envelope as a single line of bars at ~60fps. Real
/// rendering will replace this; the point here is that [`WaveScope`] gives the
/// TUI everything it needs (readiness, format, per-channel envelopes) without
/// touching PipeWire.
fn main() {
    let handle = audio::start();
    let mut wave = WaveScope::new(handle);

    eprintln!("termwaves: waiting for audio…");
    let cols = 60usize;
    // ~0.1s of history across the columns at 48k — tighten/loosen to taste.
    let window = 4_800usize;

    // Spectrum is built lazily once the sample rate is known.
    let mut spectrum: Option<Spectrum> = None;
    let n_bands = 24usize;

    let mut announced = false;
    loop {
        wave.tick();

        if !wave.is_ready() {
            std::thread::sleep(std::time::Duration::from_millis(16));
            continue;
        }
        if !announced {
            eprintln!(
                "capturing: {} ch @ {} Hz",
                wave.channel_count(),
                wave.sample_rate()
            );
            spectrum = Some(Spectrum::new(wave.sample_rate(), n_bands, 30.0, 16_000.0));
            announced = true;
        }

        let env = wave.envelope(0, cols, window);
        let peak = env.iter().fold(0.0f32, |m, e| m.max(e.peak()));
        let bars = ((peak * cols as f32) as usize).min(cols);

        // Render the log-spaced spectrum as one char per band, height by level.
        let levels = b" .:-=+*#%@";
        let spec: String = spectrum
            .as_mut()
            .map(|s| {
                s.compute(&wave, 0)
                    .iter()
                    .map(|band| {
                        let idx = (band.magnitude * (levels.len() - 1) as f32) as usize;
                        levels[idx.min(levels.len() - 1)] as char
                    })
                    .collect()
            })
            .unwrap_or_default();

        print!("\r{:width$} | {}", "#".repeat(bars), spec, width = cols);
        std::io::stdout().flush().ok();

        std::thread::sleep(std::time::Duration::from_millis(16)); // ~60fps
    }
}
