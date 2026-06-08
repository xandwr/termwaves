# termwaves

Real-time audio visualization in the terminal, driven by [PipeWire](https://pipewire.org/).

`termwaves` captures system audio via PipeWire and turns it into live oscilloscope
and spectrum data. It ships as two crates:

- **`termwaves`** — a library providing PipeWire capture, an FFT, a waveform scope,
  and a spectrum analyzer with no rendering dependencies of its own.
- **`termwaves-client`** — a [ratatui](https://ratatui.rs/) TUI that renders the
  visualizers, installed as the `termwaves` command.

> **Platform:** Linux with PipeWire. The library links against the system PipeWire
> libraries (`libpipewire-0.3`), so those development headers must be installed.

## Install the client

```sh
cargo install termwaves-client
termwaves
```

### Keybindings

| Key            | Action                         |
| -------------- | ------------------------------ |
| `q` / `Esc`    | Quit                           |
| `+` / `-`      | Zoom the time window in / out  |
| `Tab` / `c`    | Cycle channel                  |
| `F9`           | Toggle help overlay            |
| `F10`          | Toggle settings overlay        |
| `F1`–`F8`      | Select view                    |

## Use the library

```toml
[dependencies]
termwaves = "0.1"
```

```rust
use termwaves::{Spectrum, WaveScope, start};

// Begin capturing system audio from PipeWire.
let capture = start()?;

// Build a spectrum analyzer over 32 logarithmic bands from 24 Hz to 20 kHz.
let mut spectrum = Spectrum::new(48_000, 32, 24.0, 20_000.0);
let mut scope = WaveScope::new();
```

Public types: `start` / `CaptureHandle` (capture), `Fft`, `WaveScope` / `Envelope`
(waveform), and `Spectrum` / `Band` (frequency analysis).

## Building from source

```sh
git clone https://github.com/xandwr/termwaves
cd termwaves
cargo run            # runs the client (default member)
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
