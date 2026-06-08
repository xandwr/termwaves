//! The `View` trait and the render context every view draws from.
//!
//! Views are pure renderers over the shared DSP engine ([`WaveScope`] +
//! [`Spectrum`]). Adding a view means writing one type and pushing it into the
//! list in `App::new` — no enum arms or `match` sites to update.

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders},
};

use crate::scope::WaveScope;
use crate::spectrum::Spectrum;

/// Everything a view may read while ticking or rendering: the live audio model,
/// the (possibly not-yet-ready) spectrum, and the current channel/zoom.
pub struct Ctx<'a> {
    pub wave: &'a WaveScope,
    pub spectrum: Option<&'a Spectrum>,
    pub channel: usize,
    pub window: usize,
}

/// A selectable top-level visualization.
pub trait View {
    /// Short name for the status line.
    fn name(&self) -> &str;

    /// Advance any view-owned state from the latest audio. Default: no-op for
    /// stateless views that read everything fresh at render time.
    fn tick(&mut self, _ctx: &Ctx) {}

    /// Handle a key press the global event loop didn't claim. Return `true` if
    /// the view consumed it. Default: ignore everything.
    fn handle_key(&mut self, _code: KeyCode) -> bool {
        false
    }

    /// Draw the view body into `area`.
    fn render(&self, f: &mut Frame, area: Rect, ctx: &Ctx);
}

/// Render a titled border around `area` and return the inner rect to draw into.
pub fn framed(f: &mut Frame, area: Rect, title: &str) -> Rect {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string());
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// Draw dimmed, centered placeholder text (e.g. "warming up…").
pub fn placeholder_text(f: &mut Frame, area: Rect, text: &str) {
    f.render_widget(
        Line::from(text)
            .style(Style::default().add_modifier(Modifier::DIM))
            .centered(),
        area,
    );
}

/// A not-yet-implemented view bound to a function key.
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
