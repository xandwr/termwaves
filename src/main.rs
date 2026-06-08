mod audio;
mod color;
mod combined;
mod fft;
mod scope;
mod spectrum;
mod terrain;
mod view;

use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::prelude::*;

use combined::Combined;
use scope::WaveScope;
use spectrum::Spectrum;
use terrain::Terrain;
use view::{Ctx, Placeholder, View};

const SPEC_MIN_HZ: f32 = 24.0;
const SPEC_MAX_HZ: f32 = 20_000.0;
const N_BANDS: usize = 32;

const WINDOW_MIN: usize = 200;
const WINDOW_MAX: usize = 24_000;
const WINDOW_DEFAULT: usize = 4_800;

const FRAME: Duration = Duration::from_millis(16);

/// Owns the shared DSP engine and the list of selectable views.
struct App {
    wave: WaveScope,
    spectrum: Option<Spectrum>,
    window: usize,
    channel: usize,
    views: Vec<Box<dyn View>>,
    active: usize,
}

impl App {
    fn new(wave: WaveScope) -> Self {
        // Order here is the function-key order: F1, F2, F3…
        let views: Vec<Box<dyn View>> = vec![
            Box::new(Terrain::new(N_BANDS)),
            Box::new(Combined),
            Box::new(Placeholder::new(3)),
            Box::new(Placeholder::new(4)),
            Box::new(Placeholder::new(5)),
            Box::new(Placeholder::new(6)),
            Box::new(Placeholder::new(7)),
            Box::new(Placeholder::new(8)),
        ];
        Self {
            wave,
            spectrum: None,
            window: WINDOW_DEFAULT,
            channel: 0,
            views,
            active: 0,
        }
    }

    /// Borrow the current channel/zoom and DSP state as a render context.
    fn ctx(&self) -> Ctx<'_> {
        Ctx {
            wave: &self.wave,
            spectrum: self.spectrum.as_ref(),
            channel: self.channel,
            window: self.window,
        }
    }

    /// Pull fresh audio, lazily build the spectrum, and tick every view.
    fn tick(&mut self) {
        self.wave.tick();
        if self.spectrum.is_none() && self.wave.is_ready() {
            self.spectrum = Some(Spectrum::new(
                self.wave.sample_rate(),
                N_BANDS,
                SPEC_MIN_HZ,
                SPEC_MAX_HZ,
            ));
        }
        if let Some(spectrum) = self.spectrum.as_mut() {
            spectrum.compute(&self.wave, self.channel);
        }

        let ctx = Ctx {
            wave: &self.wave,
            spectrum: self.spectrum.as_ref(),
            channel: self.channel,
            window: self.window,
        };
        for v in &mut self.views {
            v.tick(&ctx);
        }
    }

    /// Select the view bound to function key `n` (1-based), if one exists.
    fn select_fkey(&mut self, n: u8) {
        let idx = (n as usize).wrapping_sub(1);
        if idx < self.views.len() {
            self.active = idx;
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
        terminal.draw(|f| ui(f, &app))?;

        if event::poll(FRAME)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('+') | KeyCode::Char('=') => app.zoom_in(),
                KeyCode::Char('-') | KeyCode::Char('_') => app.zoom_out(),
                KeyCode::Tab | KeyCode::Char('c') => app.next_channel(),
                KeyCode::F(n) => app.select_fkey(n),
                _ => {}
            }
        }
    }
}

/// Draw the full frame: a status row, then the active view's body below it.
fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(f.area());

    render_status(f, chunks[0], app);
    app.views[app.active].render(f, chunks[1], &app.ctx());
}

fn render_status(f: &mut Frame, area: Rect, app: &App) {
    let status = if app.wave.is_ready() {
        format!(
            " termwaves: {} · ch {}/{} @ {} Hz · window {} samp   [F1-F8 view · +/- zoom · Tab channel · q quit]",
            app.views[app.active].name(),
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
