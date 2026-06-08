use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders},
};

use termwaves::{Spectrum, WaveScope};

pub struct Ctx<'a> {
    pub wave: &'a WaveScope,
    pub spectrum: Option<&'a Spectrum>,
    pub channel: usize,
    pub window: usize,
}

pub trait View {
    fn name(&self) -> &str;

    fn tick(&mut self, _ctx: &Ctx) {}

    fn handle_key(&mut self, _code: KeyCode) -> bool {
        false
    }

    fn render(&self, f: &mut Frame, area: Rect, ctx: &Ctx);
}

pub fn framed(f: &mut Frame, area: Rect, title: &str) -> Rect {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string());
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

pub fn placeholder_text(f: &mut Frame, area: Rect, text: &str) {
    f.render_widget(
        Line::from(text)
            .style(Style::default().add_modifier(Modifier::DIM))
            .centered(),
        area,
    );
}

pub struct Placeholder {
    name: String,
    body: String,
}

impl Placeholder {
    pub fn new(fkey: u8) -> Self {
        Self {
            name: format!("view F{fkey}"),
            body: format!("view F{fkey}: not implemented yet"),
        }
    }
}

impl View for Placeholder {
    fn name(&self) -> &str {
        &self.name
    }

    fn render(&self, f: &mut Frame, area: Rect, _ctx: &Ctx) {
        let inner = framed(f, area, &self.name);
        placeholder_text(f, inner, &self.body);
    }
}
