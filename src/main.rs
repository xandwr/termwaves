mod audio;
mod scope;
mod spectrum;
mod terrain;

use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    prelude::*,
    widgets::{
        Bar, BarChart, BarGroup, Block, Borders,
        canvas::{Canvas, Line as CanvasLine},
    },
};

use scope::WaveScope;
use spectrum::Spectrum;
use terrain::Terrain;

const SPEC_MIN_HZ: f32 = 30.0;
const SPEC_MAX_HZ: f32 = 16_000.0;
const N_BANDS: usize = 48;

const WINDOW_MIN: usize = 480;
const WINDOW_MAX: usize = 24_000;
const WINDOW_DEFAULT: usize = 4_800;

const FRAME: Duration = Duration::from_millis(16);

/// The active top-level view, selected via the function keys F1–F8.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Combined,
    Terrain,
    Stub(u8),
}

impl View {
    /// Map a function-key index (1..=8) to its view, if any.
    fn from_fkey(n: u8) -> Option<View> {
        match n {
            1 => Some(View::Terrain),
            2 => Some(View::Combined),
            3..=8 => Some(View::Stub(n)),
            _ => None,
        }
    }

    /// Short human-readable name for the status line.
    fn name(self) -> String {
        match self {
            View::Combined => "combined".to_string(),
            View::Terrain => "3D terrain".to_string(),
            View::Stub(n) => format!("view F{n}"),
        }
    }
}

/// Owns the render-side state the UI draws from each frame.
struct App {
    wave: WaveScope,
    spectrum: Option<Spectrum>,
    window: usize,
    channel: usize,
    view: View,
    terrain: Option<Terrain>,
}

impl App {
    fn new(wave: WaveScope) -> Self {
        Self {
            wave,
            spectrum: None,
            window: WINDOW_DEFAULT,
            channel: 0,
            view: View::Terrain,
            terrain: None,
        }
    }

    /// Pull fresh audio, lazily build the spectrum, and feed the terrain a row.
    fn tick(&mut self) {
        self.wave.tick();
        if self.spectrum.is_none() && self.wave.is_ready() {
            self.spectrum = Some(Spectrum::new(
                self.wave.sample_rate(),
                N_BANDS,
                SPEC_MIN_HZ,
                SPEC_MAX_HZ,
            ));
            self.terrain = Some(Terrain::new(N_BANDS));
        }

        if let (Some(spectrum), Some(terrain)) = (self.spectrum.as_mut(), self.terrain.as_mut()) {
            let bands = spectrum.compute(&self.wave, self.channel);
            let row: Vec<f32> = bands.iter().map(|b| b.magnitude).collect();
            terrain.push(&row);
        }
    }

    fn zoom_in(&mut self) {
        self.window = (self.window / 2).max(WINDOW_MIN);
    }

    fn zoom_out(&mut self) {
        self.window = (self.window * 2).min(WINDOW_MAX);
    }

    fn next_channel(&mut self) {
        let n = self.wave.channel_count();
        if n > 0 {
            self.channel = (self.channel + 1) % n;
        }
    }
}

fn main() -> io::Result<()> {
    let handle = audio::start();
    let app = App::new(WaveScope::new(handle));

    let mut terminal = setup_terminal()?;
    let result = run(&mut terminal, app);
    restore_terminal(&mut terminal)?;
    result
}

/// Event loop: tick + redraw on each frame timeout, handle keys as they arrive.
fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, mut app: App) -> io::Result<()> {
    loop {
        app.tick();
        terminal.draw(|f| ui(f, &mut app))?;

        if event::poll(FRAME)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Char('+') | KeyCode::Char('=') => app.zoom_in(),
                        KeyCode::Char('-') | KeyCode::Char('_') => app.zoom_out(),
                        KeyCode::Tab | KeyCode::Char('c') => app.next_channel(),
                        KeyCode::F(n) => {
                            if let Some(view) = View::from_fkey(n) {
                                app.view = view;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

/// Draw the full frame: a status row, then the active view's body below it.
fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(f.area());

    render_status(f, chunks[0], app);

    match app.view {
        View::Combined => render_combined(f, chunks[1], app),
        View::Terrain => render_terrain(f, chunks[1], app),
        View::Stub(n) => render_stub(f, chunks[1], n),
    }
}

fn render_terrain(f: &mut Frame, area: Rect, app: &mut App) {
    let block = Block::default().borders(Borders::ALL).title("3D terrain");
    let inner = block.inner(area);
    f.render_widget(block, area);

    match app.terrain.as_ref() {
        Some(terrain) => terrain.render(f, inner),
        None => f.render_widget(
            Line::from("warming up…")
                .style(Style::default().add_modifier(Modifier::DIM))
                .centered(),
            inner,
        ),
    }
}

fn render_combined(f: &mut Frame, area: Rect, app: &mut App) {
    let chunks = Layout::vertical([Constraint::Min(6), Constraint::Min(6)]).split(area);

    render_waveform(f, chunks[0], app);
    render_spectrum(f, chunks[1], app);
}

fn render_stub(f: &mut Frame, area: Rect, n: u8) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("F{n}"));
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Line::from(format!("view F{n}: not implemented yet"))
            .style(Style::default().add_modifier(Modifier::DIM))
            .centered(),
        inner,
    );
}

fn render_status(f: &mut Frame, area: Rect, app: &App) {
    let status = if app.wave.is_ready() {
        format!(
            " termwaves: {} · ch {}/{} @ {} Hz · window {} samp   [F1-F8 view · +/- zoom · Tab channel · q quit]",
            app.view.name(),
            app.channel,
            app.wave.channel_count(),
            app.wave.sample_rate(),
            app.window,
        )
    } else {
        " termwaves: waiting for audio…   [q quit]".to_string()
    };
    f.render_widget(
        Line::from(status).style(Style::default().add_modifier(Modifier::DIM)),
        area,
    );
}

fn render_waveform(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title("waveform");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = (inner.width as usize * 2).max(1);
    let env = app.wave.envelope(app.channel, cols, app.window);

    let canvas = Canvas::default()
        .x_bounds([0.0, cols as f64])
        .y_bounds([-1.0, 1.0])
        .paint(move |ctx| {
            for (i, e) in env.iter().enumerate() {
                let x = i as f64;
                ctx.draw(&CanvasLine {
                    x1: x,
                    y1: e.min as f64,
                    x2: x,
                    y2: e.max as f64,
                    color: Color::Cyan,
                });
            }
        });
    f.render_widget(canvas, inner);
}

fn render_spectrum(f: &mut Frame, area: Rect, app: &mut App) {
    let block = Block::default().borders(Borders::ALL).title("spectrum");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(spectrum) = app.spectrum.as_ref() else {
        return;
    };
    let bands = spectrum.bands();

    let height = inner.height.max(1);
    let bars: Vec<Bar> = bands
        .iter()
        .map(|b| {
            let value = (b.magnitude * height as f32).round() as u64;
            Bar::default()
                .value(value)
                .text_value(String::new())
                .style(Style::default().fg(heat_color(b.magnitude)))
        })
        .collect();

    let chart = BarChart::default()
        .data(BarGroup::default().bars(&bars))
        .bar_width(((inner.width as usize / bands.len().max(1)).max(1)) as u16)
        .bar_gap(0)
        .max(height as u64);
    f.render_widget(chart, inner);
}

/// Map a normalized intensity `0.0..=1.0` to a cold→hot heatmap color.
pub(crate) fn heat_color(t: f32) -> Color {
    const STOPS: [(u8, u8, u8); 5] = [
        (0, 0, 255),
        (0, 255, 255),
        (0, 255, 0),
        (255, 255, 0),
        (255, 0, 0),
    ];
    let t = t.clamp(0.0, 1.0);
    let segments = (STOPS.len() - 1) as f32;
    let scaled = t * segments;
    let i = (scaled.floor() as usize).min(STOPS.len() - 2);
    let frac = scaled - i as f32;
    let (r0, g0, b0) = STOPS[i];
    let (r1, g1, b1) = STOPS[i + 1];
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * frac).round() as u8;
    Color::Rgb(lerp(r0, r1), lerp(g0, g1), lerp(b0, b1))
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()
}
