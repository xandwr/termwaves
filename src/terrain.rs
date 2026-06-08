//! 3D spectral terrain: the F1 view.

use ratatui::{
    prelude::*,
    widgets::canvas::{Canvas, Line as CanvasLine},
};

use crossterm::event::KeyCode;

use crate::color::surface_color;
use crate::view::{Ctx, View, framed, placeholder_text};

/// Default number of spectrum rows held in the rolling landscape. Keys 1-9
/// rebind this live; see [`Terrain::handle_key`].
const DEFAULT_DEPTH: usize = 9;
const PEAK_HEIGHT: f32 = 4.0;
const SPIKE_GAMMA: f32 = 2.4;
const CAM_HEIGHT: f32 = 2.0;
const CAM_SETBACK: f32 = 10.0;
const CAM_PITCH: f32 = 0.66;
const FOCAL: f32 = 2.4;
/// Half the terrain's world-space span. The band index maps to
/// `[-WORLD_HALF_WIDTH, +WORLD_HALF_WIDTH]`, so raising this widens the
/// landscape physically without moving the camera.
const WORLD_HALF_WIDTH: f32 = 1.4;
/// Vertical NDC offset that recenters the downward-pitched view so the ground
/// plane lands inside the frame instead of clipping off the bottom.
const HORIZON_LIFT: f32 = 2.0;
/// EMA factor for per-band height smoothing: each tick a band eases this
/// fraction of the way toward the new magnitude. Higher = snappier, lower =
/// smoother. 1.0 disables smoothing (raw passthrough).
const SMOOTHING: f32 = 0.33;
/// EMA factor for the adaptive emphasis (centroid + peak). Low and slow so the
/// terrain breathes toward the current material instead of flickering on
/// transients.
const ADAPT_ALPHA: f32 = 0.01;
/// Strongest per-band height multiplier the energy-tilt can apply at the far
/// end of the spectrum. 1.0 = no tilt; higher leans harder toward wherever the
/// buffer's energy currently sits.
const TILT_STRENGTH: f32 = 1.0;
/// Floor for the peak-normalization divisor, so a near-silent buffer doesn't
/// blow tiny magnitudes up to full height (and divide-by-zero).
const PEAK_FLOOR: f32 = 0.001;
/// Direction the scene light comes from, in world space (x: across bands,
/// y: up, z: toward camera). Faces whose normal aligns with this are brightest.
/// Normalized at use.
const LIGHT_DIR: (f32, f32, f32) = (-0.5, 0.8, 0.5);
/// Ambient floor so faces turned away from the light stay visible instead of
/// going pure black.
const AMBIENT: f32 = 0.2;

/// A rolling 3D height-field built from successive spectrum rows.
pub struct Terrain {
    rows: Vec<f32>,
    width: usize,
    depth: usize,
    head: usize,
    primed: bool,
    /// Smoothed spectral centroid of the whole ring, `0.0` (all-low) ..= `1.0`
    /// (all-high). Drives the energy-tilt in [`Terrain::emphasis`].
    centroid: f32,
    /// Smoothed peak height across the ring, used to normalize quiet passages up
    /// to full frame height.
    peak: f32,
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
            centroid: 0.5,
            peak: 1.0,
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
        self.centroid = 0.5;
        self.peak = 1.0;
    }

    /// Push the latest spectrum row, scrolling the landscape toward the camera.
    ///
    /// The new front row is the previous front row's heights eased a `SMOOTHING`
    /// fraction toward `magnitudes`, so each band rises and falls smoothly
    /// instead of snapping to every frame.
    fn push(&mut self, magnitudes: &[f32]) {
        let prev = (self.head + self.depth - 1) % self.depth;
        let base = self.head * self.width;
        for x in 0..self.width {
            let target = magnitudes.get(x).copied().unwrap_or(0.0);
            let last = if self.primed {
                self.rows[prev * self.width + x]
            } else {
                target
            };
            self.rows[base + x] = last + SMOOTHING * (target - last);
        }
        self.head = (self.head + 1) % self.depth;
        self.primed = true;
    }

    /// Re-measure the adaptive emphasis from the whole ring buffer and ease the
    /// smoothed `centroid`/`peak` toward it. Called once per tick after `push`.
    ///
    /// `centroid` is the energy-weighted mean band position (0 = all-low,
    /// 1 = all-high); `peak` is the loudest stored height. Both are EMA'd by
    /// `ADAPT_ALPHA` so the terrain breathes toward the current material.
    fn adapt(&mut self) {
        if !self.primed || self.width < 2 {
            return;
        }
        let mut energy = 0.0f32;
        let mut weighted = 0.0f32;
        let mut peak = 0.0f32;
        let span = (self.width - 1) as f32;
        for ring in 0..self.depth {
            for x in 0..self.width {
                let m = self.rows[ring * self.width + x];
                // Square so loud bands dominate the centroid (perceived energy).
                let e = m * m;
                energy += e;
                weighted += e * (x as f32 / span);
                peak = peak.max(m);
            }
        }
        let frame_centroid = if energy > 0.0 { weighted / energy } else { 0.5 };
        self.centroid += ADAPT_ALPHA * (frame_centroid - self.centroid);
        self.peak += ADAPT_ALPHA * (peak - self.peak);
    }

    /// Per-band height multiplier: tilt energy toward where the buffer's energy
    /// sits (low `centroid` lifts low bands, high `centroid` lifts high bands).
    fn emphasis(&self, x: usize) -> f32 {
        let pos = x as f32 / (self.width - 1).max(1) as f32;
        // Peaks at the band where the buffer's energy sits, fading to neutral
        // one full spectrum-width away.
        let alignment = (1.0 - (pos - self.centroid).abs()).max(0.0);
        1.0 + (TILT_STRENGTH - 1.0) * alignment
    }

    /// Stored magnitude for row `r`, band `x`, after adaptive emphasis and
    /// peak-normalization. This is the value the wireframe and color see.
    fn height(&self, r: usize, x: usize) -> f32 {
        let ring = (self.head + self.depth - 1 - r) % self.depth;
        let raw = self.rows[ring * self.width + x];
        let norm = self.peak.max(PEAK_FLOOR);
        (raw * self.emphasis(x) / norm).min(1.0)
    }

    /// World-space position of grid vertex `(x, r)`: `x` across the bands, the
    /// rendered height up, and `r` receding from the camera. The projection in
    /// `render_wireframe` consumes these, and face normals are built from them.
    fn world(&self, x: usize, r: usize) -> (f32, f32, f32) {
        let wx = (x as f32 / (self.width - 1) as f32 - 0.5) * 2.0 * WORLD_HALF_WIDTH;
        let wy = self.height(r, x).powf(SPIKE_GAMMA) * PEAK_HEIGHT;
        let wz = r as f32;
        (wx, wy, wz)
    }

    /// Lambert brightness of the quad face whose near-left corner is `(x, r)`.
    /// The normal is the cross product of the face's two world-space edges; the
    /// shade is `max(AMBIENT, n·light)`. Returns `None` past the grid edge.
    fn face_shade(&self, x: usize, r: usize) -> Option<f32> {
        if x + 1 >= self.width || r + 1 >= self.depth {
            return None;
        }
        let a = self.world(x, r);
        let b = self.world(x + 1, r);
        let c = self.world(x, r + 1);
        // Two edges of the quad sharing corner `a`.
        let e1 = (b.0 - a.0, b.1 - a.1, b.2 - a.2);
        let e2 = (c.0 - a.0, c.1 - a.1, c.2 - a.2);
        // n = e1 × e2, oriented so the +y component faces up toward the light.
        let mut n = (
            e1.1 * e2.2 - e1.2 * e2.1,
            e1.2 * e2.0 - e1.0 * e2.2,
            e1.0 * e2.1 - e1.1 * e2.0,
        );
        if n.1 < 0.0 {
            n = (-n.0, -n.1, -n.2);
        }
        let nlen = (n.0 * n.0 + n.1 * n.1 + n.2 * n.2).sqrt().max(1e-6);
        let (lx, ly, lz) = LIGHT_DIR;
        let llen = (lx * lx + ly * ly + lz * lz).sqrt();
        let dot = (n.0 * lx + n.1 * ly + n.2 * lz) / (nlen * llen);
        Some(dot.max(0.0).mul_add(1.0 - AMBIENT, AMBIENT))
    }

    /// Shade for an edge, averaged over the (up to two) faces it borders. `(x, r)`
    /// is the edge's lower-left endpoint; `along_x` picks the band-wise edge
    /// (`true`) or the depth-wise edge (`false`). Falls back to a flat shade
    /// when no face exists (e.g. depth 1, or the grid border).
    fn edge_shade(&self, x: usize, r: usize, along_x: bool) -> f32 {
        // A band-wise edge is shared by the faces in front of and behind it;
        // a depth-wise edge by the faces to its left and right.
        let (a, b) = if along_x {
            (
                self.face_shade(x, r),
                r.checked_sub(1).and_then(|rp| self.face_shade(x, rp)),
            )
        } else {
            (
                self.face_shade(x, r),
                x.checked_sub(1).and_then(|xp| self.face_shade(xp, r)),
            )
        };
        match (a, b) {
            (Some(a), Some(b)) => 0.5 * (a + b),
            (Some(s), None) | (None, Some(s)) => s,
            (None, None) => AMBIENT + (1.0 - AMBIENT) * 0.5,
        }
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
        // Project a world-space point to screen pixels.
        let project = |wx: f32, wy: f32, wz: f32| -> Option<(f64, f64)> {
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
        let vertex = |x: usize, r: usize| {
            let (wx, wy, wz) = self.world(x, r);
            project(wx, wy, wz)
        };

        let canvas = Canvas::default()
            .x_bounds([0.0, sx])
            .y_bounds([0.0, sy])
            .paint(move |ctx| {
                for r in (0..depth).rev() {
                    for x in 0..width {
                        let Some((px, py)) = vertex(x, r) else {
                            continue;
                        };

                        if x + 1 < width
                            && let Some((nx, ny)) = vertex(x + 1, r)
                        {
                            ctx.draw(&CanvasLine {
                                x1: px,
                                y1: py,
                                x2: nx,
                                y2: ny,
                                color: surface_color(self.edge_shade(x, r, true)),
                            });
                        }
                        if r > 0
                            && let Some((nx, ny)) = vertex(x, r - 1)
                        {
                            ctx.draw(&CanvasLine {
                                x1: px,
                                y1: py,
                                x2: nx,
                                y2: ny,
                                color: surface_color(self.edge_shade(x, r - 1, false)),
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
            self.adapt();
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
