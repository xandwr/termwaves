//! 3D spectral terrain: the F1 view.

use ratatui::{
    prelude::*,
    widgets::canvas::{Canvas, Line as CanvasLine},
};

use crossterm::event::KeyCode;

use crate::color::heat_color;
use crate::view::{Ctx, View, framed, placeholder_text};

/// Default number of spectrum rows held in the rolling landscape. Keys 1-9
/// rebind this live; see [`Terrain::handle_key`].
const DEFAULT_DEPTH: usize = 1;
const PEAK_HEIGHT: f32 = 6.6;
const SPIKE_GAMMA: f32 = 2.4;
const CAM_HEIGHT: f32 = 2.5;
const CAM_SETBACK: f32 = 8.0;
const CAM_PITCH: f32 = 0.6;
const FOCAL: f32 = 1.2;
/// Half the terrain's world-space span. The band index maps to
/// `[-WORLD_HALF_WIDTH, +WORLD_HALF_WIDTH]`, so raising this widens the
/// landscape physically without moving the camera.
const WORLD_HALF_WIDTH: f32 = 1.5;
/// Vertical NDC offset that recenters the downward-pitched view so the ground
/// plane lands inside the frame instead of clipping off the bottom.
const HORIZON_LIFT: f32 = 1.2;

/// A rolling 3D height-field built from successive spectrum rows.
pub struct Terrain {
    rows: Vec<f32>,
    width: usize,
    depth: usize,
    head: usize,
    primed: bool,
}

impl Terrain {
    /// Build an empty terrain holding rows of `width` bands.
    pub fn new(width: usize) -> Self {
        Self {
            rows: vec![0.0; DEFAULT_DEPTH * width.max(1)],
            width: width.max(1),
            depth: DEFAULT_DEPTH,
            head: 0,
            primed: false,
        }
    }

    /// Resize the landscape to `depth` rows, clearing it. The next pushes
    /// refill the (smaller or larger) ring from scratch.
    fn set_depth(&mut self, depth: usize) {
        if depth == self.depth {
            return;
        }
        self.depth = depth;
        self.rows = vec![0.0; depth * self.width];
        self.head = 0;
        self.primed = false;
    }

    /// Push the latest spectrum row, scrolling the landscape toward the camera.
    fn push(&mut self, magnitudes: &[f32]) {
        let base = self.head * self.width;
        let slot = &mut self.rows[base..base + self.width];
        for (dst, src) in slot.iter_mut().zip(magnitudes.iter()) {
            *dst = *src;
        }
        for dst in slot.iter_mut().skip(magnitudes.len()) {
            *dst = 0.0;
        }
        self.head = (self.head + 1) % self.depth;
        self.primed = true;
    }

    fn height(&self, r: usize, x: usize) -> f32 {
        let ring = (self.head + self.depth - 1 - r) % self.depth;
        self.rows[ring * self.width + x]
    }

    /// Render the terrain wireframe into `area`.
    fn render_wireframe(&self, f: &mut Frame, area: Rect) {
        if !self.primed || self.width < 2 {
            return;
        }

        let sx = (area.width as f64 * 2.0).max(1.0);
        let sy = (area.height as f64 * 4.0).max(1.0);

        const CELL_ASPECT: f64 = 2.0;
        // Cancels the unequal sx/sy pixel scaling applied at the ndc->px step
        // (and the 2:1 terminal-cell aspect) so a world-space square stays square.
        let aspect = (sy / sx) * (CELL_ASPECT * CELL_ASPECT);

        let width = self.width;
        let depth = self.depth;
        let project = |x: usize, r: usize| -> Option<(f64, f64)> {
            let wx = (x as f32 / (width - 1) as f32 - 0.5) * 2.0 * WORLD_HALF_WIDTH;
            let wy = self.height(r, x).powf(SPIKE_GAMMA) * PEAK_HEIGHT;
            let wz = r as f32;

            let ex = wx;
            let ty = wy - CAM_HEIGHT;
            let tz = wz + CAM_SETBACK;

            let (sp, cp) = (CAM_PITCH.sin(), CAM_PITCH.cos());
            let ey = ty * cp - tz * sp;
            let ez = ty * sp + tz * cp;
            if ez <= 0.05 {
                return None;
            }

            let ndc_x = (FOCAL * ex / ez) as f64 * aspect;
            let ndc_y = (FOCAL * ey / ez + HORIZON_LIFT) as f64;
            let px = (ndc_x * 0.5 + 0.5) * sx;
            let py = (ndc_y * 0.5 + 0.5) * sy;
            Some((px, py))
        };

        let canvas = Canvas::default()
            .x_bounds([0.0, sx])
            .y_bounds([0.0, sy])
            .paint(move |ctx| {
                for r in (0..depth).rev() {
                    for x in 0..width {
                        let Some((px, py)) = project(x, r) else {
                            continue;
                        };
                        let color = heat_color(self.height(r, x));

                        if x + 1 < width
                            && let Some((nx, ny)) = project(x + 1, r)
                        {
                            ctx.draw(&CanvasLine {
                                x1: px,
                                y1: py,
                                x2: nx,
                                y2: ny,
                                color,
                            });
                        }
                        if r > 0
                            && let Some((nx, ny)) = project(x, r - 1)
                        {
                            ctx.draw(&CanvasLine {
                                x1: px,
                                y1: py,
                                x2: nx,
                                y2: ny,
                                color,
                            });
                        }
                    }
                }
            });
        f.render_widget(canvas, area);
    }
}

impl View for Terrain {
    fn name(&self) -> &str {
        "3D terrain"
    }

    fn tick(&mut self, ctx: &Ctx) {
        if let Some(spectrum) = ctx.spectrum {
            let row: Vec<f32> = spectrum.bands().iter().map(|b| b.magnitude).collect();
            self.push(&row);
        }
    }

    /// Keys 1-9 set the landscape depth (number of rows) to that value.
    fn handle_key(&mut self, code: KeyCode) -> bool {
        if let KeyCode::Char(c @ '1'..='9') = code {
            self.set_depth(c as usize - '0' as usize);
            return true;
        }
        false
    }

    fn render(&self, f: &mut Frame, area: Rect, _ctx: &Ctx) {
        let inner = framed(f, area, "3D terrain");
        if self.primed {
            self.render_wireframe(f, inner);
        } else {
            placeholder_text(f, inner, "warming up…");
        }
    }
}
