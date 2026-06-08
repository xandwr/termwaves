mod color;
mod combined;
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
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use termwaves::{Spectrum, WaveScope};

use combined::Combined;
use terrain::Terrain;
use view::{Ctx, Placeholder, View};

const SPEC_MIN_HZ: f32 = 24.0;
const SPEC_MAX_HZ: f32 = 20_000.0;
const N_BANDS: usize = 32;

const WINDOW_MIN: usize = 200;
const WINDOW_MAX: usize = 24_000;
const WINDOW_DEFAULT: usize = 4_800;

const FRAME: Duration = Duration::from_millis(16);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Overlay {
    None,
    Help,
    Settings,
}

struct App {
    wave: WaveScope,
    spectrum: Option<Spectrum>,
    window: usize,
    channel: usize,
    views: Vec<Box<dyn View>>,
    active: usize,
    overlay: Overlay,
}

impl App {
    fn new(wave: WaveScope) -> Self {
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
            overlay: Overlay::None,
        }
    }

    fn ctx(&self) -> Ctx<'_> {
        Ctx {
            wave: &self.wave,
            spectrum: self.spectrum.as_ref(),
            channel: self.channel,
            window: self.window,
        }
    }

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

    fn toggle_overlay(&mut self, overlay: Overlay) {
        self.overlay = if self.overlay == overlay {
            Overlay::None
        } else {
            overlay
        };
    }

    fn forward_key(&mut self, code: KeyCode) {
        self.views[self.active].handle_key(code);
    }
}

fn main() -> io::Result<()> {
    let handle = termwaves::start();
    let app = App::new(WaveScope::new(handle));

    let mut terminal = setup_terminal()?;
    let result = run(&mut terminal, app);
    restore_terminal(&mut terminal)?;
    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, mut app: App) -> io::Result<()> {
    loop {
        app.tick();
        terminal.draw(|f| ui(f, &app))?;

        if event::poll(FRAME)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Esc if app.overlay != Overlay::None => app.overlay = Overlay::None,
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::F(9) => app.toggle_overlay(Overlay::Help),
                KeyCode::F(10) => app.toggle_overlay(Overlay::Settings),
                KeyCode::Char('+') | KeyCode::Char('=') => app.zoom_in(),
                KeyCode::Char('-') | KeyCode::Char('_') => app.zoom_out(),
                KeyCode::Tab | KeyCode::Char('c') => app.next_channel(),
                KeyCode::F(n) => app.select_fkey(n),
                code => app.forward_key(code),
            }
        }
    }
}

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(f.area());

    render_status(f, chunks[0], app);
    app.views[app.active].render(f, chunks[1], &app.ctx());

    match app.overlay {
        Overlay::None => {}
        Overlay::Help => render_help(f, chunks[1]),
        Overlay::Settings => render_settings(f, chunks[1]),
    }
}

fn render_status(f: &mut Frame, area: Rect, app: &App) {
    let status = if app.wave.is_ready() {
        format!(
            " termwaves: {} · ch {}/{} @ {} Hz · window {} samp   [F9 help · F10 settings]",
            app.views[app.active].name(),
            app.channel,
            app.wave.channel_count(),
            app.wave.sample_rate(),
            app.window,
        )
    } else {
        " termwaves: waiting for audio…   [F9 help · q quit]".to_string()
    };
    f.render_widget(
        Line::from(status).style(Style::default().add_modifier(Modifier::DIM)),
        area,
    );
}

fn render_overlay(f: &mut Frame, area: Rect, title: &str, lines: Vec<Line>) {
    let content_w = lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(0)
        .max(title.len()) as u16;
    let w = (content_w + 4).min(area.width);
    let h = (lines.len() as u16 + 2).min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let panel = Rect::new(x, y, w, h);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string());
    let inner = block.inner(panel);
    f.render_widget(Clear, panel);
    f.render_widget(block, panel);
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_help(f: &mut Frame, area: Rect) {
    let key = |k: &str, desc: &str| {
        Line::from(vec![
            Span::styled(
                format!("{k:<10}"),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(desc.to_string()),
        ])
    };
    let section = |title: &str| {
        Line::from(Span::styled(
            title.to_string(),
            Style::default().add_modifier(Modifier::DIM),
        ))
    };
    let lines = vec![
        key("F1-F8", "switch view"),
        key("+ / -", "zoom window"),
        key("Tab / c", "next channel"),
        key("F9", "toggle this help"),
        key("F10", "settings"),
        key("Esc", "close overlay"),
        key("q", "quit"),
        Line::from(""),
        section("3D terrain (F1)"),
        key("1-9", "terrain depth"),
        key("r", "toggle rotary mode"),
        key("[ / ]", "rotary speed -/+"),
    ];
    render_overlay(f, area, " Help ", lines);
}

fn render_settings(f: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from("Settings coming soon.").style(Style::default().add_modifier(Modifier::DIM)),
        Line::from(""),
        Line::from("Press F10 or Esc to close."),
    ];
    render_overlay(f, area, " Settings ", lines);
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
