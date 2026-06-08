//! The combined waveform + spectrum view.

use ratatui::{
    prelude::*,
    widgets::{
        Bar, BarChart, BarGroup,
        canvas::{Canvas, Line as CanvasLine},
    },
};

use crate::color::heat_color;
use crate::view::{Ctx, View, framed};

/// Stacked oscilloscope (top) and log-spaced spectrum bars (bottom).
pub struct Combined;

impl View for Combined {
    fn name(&self) -> &str {
        "combined"
    }

    fn render(&self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let chunks = Layout::vertical([Constraint::Min(6), Constraint::Min(6)]).split(area);
        render_waveform(f, chunks[0], ctx);
        render_spectrum(f, chunks[1], ctx);
    }
}

fn render_waveform(f: &mut Frame, area: Rect, ctx: &Ctx) {
    let inner = framed(f, area, "waveform");

    let cols = (inner.width as usize * 2).max(1);
    let env = ctx.wave.envelope(ctx.channel, cols, ctx.window);

    let canvas = Canvas::default()
        .x_bounds([0.0, cols as f64])
        .y_bounds([-1.0, 1.0])
        .paint(move |c| {
            for (i, e) in env.iter().enumerate() {
                let x = i as f64;
                c.draw(&CanvasLine {
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

fn render_spectrum(f: &mut Frame, area: Rect, ctx: &Ctx) {
    let inner = framed(f, area, "spectrum");

    let Some(spectrum) = ctx.spectrum else {
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
