use ratatui::{
    prelude::*,
    style::Color,
    widgets::canvas::{Canvas, Context, Line as CanvasLine},
};

use crossterm::event::KeyCode;

use crate::color::{ocean_color, surface_color};
use crate::view::{Ctx, View, framed, placeholder_text};

const DEFAULT_DEPTH: usize = 9;
const PEAK_HEIGHT: f32 = 4.0;
const SPIKE_GAMMA: f32 = 2.4;
const CAM_HEIGHT: f32 = 2.0;
const CAM_SETBACK: f32 = 10.0;
const CAM_PITCH: f32 = 0.66;
const FOCAL: f32 = 2.4;
const WORLD_HALF_WIDTH: f32 = 1.4;
const HORIZON_LIFT: f32 = 2.0;
const SMOOTHING: f32 = 0.33;
const ADAPT_ALPHA: f32 = 0.01;
const TILT_STRENGTH: f32 = 1.0;
const PEAK_FLOOR: f32 = 0.001;
const LIGHT_DIR: (f32, f32, f32) = (-0.5, 0.8, 0.5);
const AMBIENT: f32 = 0.2;
const ISLAND_MIN_DEPTH: usize = 6;

const YAW_SPEED_DEFAULT: f32 = 0.03;
const YAW_SPEED_STEP: f32 = 0.01;
const YAW_SPEED_MAX: f32 = 0.3;
const FIELD_DECAY: f32 = 0.94;
const FIELD_STAMP: f32 = 0.6;

/// Faces whose height spread stays within this fraction of `PEAK_HEIGHT` are
/// treated as coplanar and greedily merged into one outlined plane.
const MERGE_TOLERANCE: f32 = 0.07;

/// A merged plane counts as "ocean" (flat sea) when its highest point sits
/// within this fraction of `PEAK_HEIGHT` of the baseline. These get filled blue.
const SEA_LEVEL: f32 = 0.04;

/// A greedily-merged rectangle of faces spanning cells
/// `[x0, x1] x [r0, r1]` (inclusive), all roughly coplanar.
struct MeshRect {
    x0: usize,
    x1: usize,
    r0: usize,
    r1: usize,
    /// Highest surface_y among the rect's corners (for sea-level test).
    hi: f32,
}

pub struct Terrain {
    rows: Vec<f32>,
    width: usize,
    depth: usize,
    head: usize,
    primed: bool,
    centroid: f32,
    peak: f32,
    rotary: bool,
    yaw: f32,
    yaw_speed: f32,
    field: Vec<f32>,
    field_peak: f32,
    greedy: bool,
}

impl Terrain {
    pub fn new(width: usize) -> Self {
        let width = width.max(1);
        Self {
            rows: vec![0.0; DEFAULT_DEPTH * width],
            width,
            depth: DEFAULT_DEPTH,
            head: 0,
            primed: false,
            centroid: 0.5,
            peak: 1.0,
            rotary: false,
            yaw: 0.0,
            yaw_speed: YAW_SPEED_DEFAULT,
            field: vec![0.0; DEFAULT_DEPTH * width],
            field_peak: 1.0,
            greedy: true,
        }
    }

    fn set_depth(&mut self, depth: usize) {
        if depth == self.depth {
            return;
        }
        self.depth = depth;
        self.rows = vec![0.0; depth * self.width];
        self.field = vec![0.0; depth * self.width];
        self.field_peak = 1.0;
        self.head = 0;
        self.primed = false;
        self.centroid = 0.5;
        self.peak = 1.0;
    }

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

    fn stamp_field(&mut self, magnitudes: &[f32]) {
        let (s, c) = (self.yaw.sin(), self.yaw.cos());
        const RIDGE: f32 = 0.18;
        let last_x = (self.width - 1).max(1) as f32;
        let last_z = (self.depth - 1).max(1) as f32;
        let mut peak = 0.0f32;
        for z in 0..self.depth {
            let v = z as f32 / last_z - 0.5;
            for x in 0..self.width {
                let u = x as f32 / last_x - 0.5;
                let along = u * c + v * s;
                let across = -u * s + v * c;
                let band_pos = (across + 0.5).clamp(0.0, 1.0);
                let band = (band_pos * last_x).round() as usize;
                let mag = magnitudes.get(band).copied().unwrap_or(0.0);
                let weight = (-(along * along) / (RIDGE * RIDGE)).exp();
                let cell = z * self.width + x;
                let deposit = mag * weight * FIELD_STAMP;
                let val = (self.field[cell] * FIELD_DECAY + deposit).min(4.0);
                self.field[cell] = val;
                peak = peak.max(val);
            }
        }
        self.field_peak += ADAPT_ALPHA * (peak - self.field_peak);
    }

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

    fn emphasis(&self, x: usize) -> f32 {
        let pos = x as f32 / (self.width - 1).max(1) as f32;
        let alignment = (1.0 - (pos - self.centroid).abs()).max(0.0);
        1.0 + (TILT_STRENGTH - 1.0) * alignment
    }

    fn height(&self, r: usize, x: usize) -> f32 {
        if self.rotary {
            let raw = self.field[r * self.width + x];
            return (raw / self.field_peak.max(PEAK_FLOOR)).min(1.0);
        }
        let norm = self.peak.max(PEAK_FLOOR);
        let ring = (self.head + self.depth - 1 - r) % self.depth;
        let raw = self.rows[ring * self.width + x];
        (raw * self.emphasis(x) / norm).min(1.0)
    }

    fn island_factor(&self, r: usize) -> f32 {
        if self.rotary || self.depth < ISLAND_MIN_DEPTH {
            return 1.0;
        }
        let t = r as f32 / (self.depth - 1) as f32;
        (t * std::f32::consts::PI).sin()
    }

    fn surface_y(&self, x: usize, r: usize) -> f32 {
        self.height(r, x).powf(SPIKE_GAMMA) * PEAK_HEIGHT * self.island_factor(r)
    }

    fn world(&self, x: usize, r: usize) -> (f32, f32, f32) {
        let wx = (x as f32 / (self.width - 1) as f32 - 0.5) * 2.0 * WORLD_HALF_WIDTH;
        let wy = self.surface_y(x, r);
        let wz = r as f32;
        (wx, wy, wz)
    }

    fn face_shade(&self, x: usize, r: usize) -> Option<f32> {
        if x + 1 >= self.width || r + 1 >= self.depth {
            return None;
        }
        let a = self.world(x, r);
        let b = self.world(x + 1, r);
        let c = self.world(x, r + 1);
        let e1 = (b.0 - a.0, b.1 - a.1, b.2 - a.2);
        let e2 = (c.0 - a.0, c.1 - a.1, c.2 - a.2);
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

    fn edge_shade(&self, x: usize, r: usize, along_x: bool) -> f32 {
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

    /// Height range spanned by the four corners of face `(x, r)` (the quad
    /// between cells `x..x+1` and `r..r+1`). Returns `(min, max)` surface_y.
    fn face_extent(&self, x: usize, r: usize) -> (f32, f32) {
        let corners = [
            self.surface_y(x, r),
            self.surface_y(x + 1, r),
            self.surface_y(x, r + 1),
            self.surface_y(x + 1, r + 1),
        ];
        let mut lo = corners[0];
        let mut hi = corners[0];
        for &c in &corners[1..] {
            lo = lo.min(c);
            hi = hi.max(c);
        }
        (lo, hi)
    }

    /// Greedily merge coplanar faces into axis-aligned rectangles. Flat (quiet)
    /// stretches of the island collapse into large planes; spiky (loud) bands
    /// stay finely subdivided, so the mesh re-tessellates live with the signal.
    fn greedy_mesh(&self) -> Vec<MeshRect> {
        let fw = self.width - 1; // face grid width
        let fd = self.depth - 1; // face grid depth
        if fw == 0 || fd == 0 {
            return Vec::new();
        }
        let tol = MERGE_TOLERANCE * PEAK_HEIGHT;
        let mut used = vec![false; fw * fd];
        let mut rects = Vec::new();

        for r in 0..fd {
            for x in 0..fw {
                if used[r * fw + x] {
                    continue;
                }
                let (mut lo, mut hi) = self.face_extent(x, r);

                // Grow along x while the running height spread stays in tol.
                let mut x1 = x;
                while x1 + 1 < fw && !used[r * fw + x1 + 1] {
                    let (l, h) = self.face_extent(x1 + 1, r);
                    let (nlo, nhi) = (lo.min(l), hi.max(h));
                    if nhi - nlo > tol {
                        break;
                    }
                    lo = nlo;
                    hi = nhi;
                    x1 += 1;
                }

                // Grow along r, accepting a whole row only if every face in the
                // strip [x, x1] keeps the merged extent within tolerance.
                let mut r1 = r;
                'rows: while r1 + 1 < fd {
                    let mut row_lo = lo;
                    let mut row_hi = hi;
                    for cx in x..=x1 {
                        if used[(r1 + 1) * fw + cx] {
                            break 'rows;
                        }
                        let (l, h) = self.face_extent(cx, r1 + 1);
                        row_lo = row_lo.min(l);
                        row_hi = row_hi.max(h);
                        if row_hi - row_lo > tol {
                            break 'rows;
                        }
                    }
                    lo = row_lo;
                    hi = row_hi;
                    r1 += 1;
                }

                for rr in r..=r1 {
                    for cx in x..=x1 {
                        used[rr * fw + cx] = true;
                    }
                }
                rects.push(MeshRect {
                    x0: x,
                    x1: x1 + 1,
                    r0: r,
                    r1: r1 + 1,
                    hi,
                });
            }
        }
        rects
    }

    fn render_greedy(&self, f: &mut Frame, area: Rect) {
        if !self.primed || self.width < 2 || self.depth < 2 {
            return;
        }

        let sx = (area.width as f64 * 2.0).max(1.0);
        let sy = (area.height as f64 * 4.0).max(1.0);

        const CELL_ASPECT: f64 = 2.0;
        let aspect = (sy / sx) * (CELL_ASPECT * CELL_ASPECT);

        let project = move |wx: f32, wy: f32, wz: f32| -> Option<(f64, f64)> {
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
        let vertex = move |x: usize, r: usize| {
            let (wx, wy, wz) = self.world(x, r);
            project(wx, wy, wz)
        };

        let rects = self.greedy_mesh();

        let canvas = Canvas::default()
            .x_bounds([0.0, sx])
            .y_bounds([0.0, sy])
            .paint(move |ctx| {
                let sea = SEA_LEVEL * PEAK_HEIGHT;
                // Painter's order: draw far (large r) rectangles first.
                let mut order: Vec<&MeshRect> = rects.iter().collect();
                order.sort_by_key(|rect| std::cmp::Reverse(rect.r1));

                for rect in order {
                    let is_ocean = rect.hi <= sea;
                    // Fade deep (far) -> shallow (near) by depth.
                    let ocean_t = rect.r0 as f32 / (self.depth - 1).max(1) as f32;

                    // Flat planes resting at sea level are ocean: fill them blue.
                    if is_ocean
                        && let (Some(a), Some(b), Some(c), Some(d)) = (
                            vertex(rect.x0, rect.r0),
                            vertex(rect.x1, rect.r0),
                            vertex(rect.x1, rect.r1),
                            vertex(rect.x0, rect.r1),
                        )
                    {
                        fill_quad(ctx, [a, b, c, d], ocean_color(ocean_t));
                    }

                    // Outline that follows the terrain surface: walk each edge
                    // of the rectangle cell-by-cell so the border tracks the
                    // real heightfield instead of a flat plane. Ocean borders
                    // take the water color so the sea reads as one flat sheet.
                    let color = if is_ocean {
                        ocean_color((ocean_t + 0.15).min(1.0))
                    } else {
                        let shade = self.edge_shade(rect.x0.min(self.width - 2), rect.r0, true);
                        surface_color(shade)
                    };
                    let mut seg = |ax: usize, ar: usize, bx: usize, br: usize| {
                        if let (Some((x1, y1)), Some((x2, y2))) = (vertex(ax, ar), vertex(bx, br)) {
                            ctx.draw(&CanvasLine {
                                x1,
                                y1,
                                x2,
                                y2,
                                color,
                            });
                        }
                    };

                    // Front and back edges (constant r), stepping along x.
                    for x in rect.x0..rect.x1 {
                        seg(x, rect.r0, x + 1, rect.r0);
                        seg(x, rect.r1, x + 1, rect.r1);
                    }
                    // Left and right edges (constant x), stepping along r.
                    for r in rect.r0..rect.r1 {
                        seg(rect.x0, r, rect.x0, r + 1);
                        seg(rect.x1, r, rect.x1, r + 1);
                    }
                }
            });
        f.render_widget(canvas, area);
    }

    fn render_wireframe(&self, f: &mut Frame, area: Rect) {
        if !self.primed || self.width < 2 {
            return;
        }

        let sx = (area.width as f64 * 2.0).max(1.0);
        let sy = (area.height as f64 * 4.0).max(1.0);

        const CELL_ASPECT: f64 = 2.0;
        let aspect = (sy / sx) * (CELL_ASPECT * CELL_ASPECT);

        let width = self.width;
        let depth = self.depth;
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
        if self.rotary {
            self.yaw = (self.yaw + self.yaw_speed).rem_euclid(std::f32::consts::TAU);
        }
        if let Some(spectrum) = ctx.spectrum {
            let row: Vec<f32> = spectrum.bands().iter().map(|b| b.magnitude).collect();
            self.push(&row);
            self.adapt();
            if self.rotary {
                self.stamp_field(&row);
            }
        }
    }

    fn handle_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char(c @ '1'..='9') => {
                self.set_depth((c as usize - '0' as usize) * 2);
                true
            }
            KeyCode::Char('g') => {
                self.greedy = !self.greedy;
                true
            }
            KeyCode::Char('r') => {
                self.rotary = !self.rotary;
                if self.rotary {
                    self.field.iter_mut().for_each(|c| *c = 0.0);
                    self.field_peak = 1.0;
                }
                true
            }
            KeyCode::Char(']') => {
                self.yaw_speed = (self.yaw_speed + YAW_SPEED_STEP).min(YAW_SPEED_MAX);
                true
            }
            KeyCode::Char('[') => {
                self.yaw_speed = (self.yaw_speed - YAW_SPEED_STEP).max(-YAW_SPEED_MAX);
                true
            }
            _ => false,
        }
    }

    fn render(&self, f: &mut Frame, area: Rect, _ctx: &Ctx) {
        let title = if self.greedy {
            "3D terrain (greedy)"
        } else {
            "3D terrain"
        };
        let inner = framed(f, area, title);
        if self.primed {
            if self.greedy {
                self.render_greedy(f, inner);
            } else {
                self.render_wireframe(f, inner);
            }
        } else {
            placeholder_text(f, inner, "warming up…");
        }
    }
}

/// Scanline-fill a convex quad (4 screen-space corners, in order) with `color`
/// by drawing closely-spaced horizontal lines. Used to paint flat ocean planes.
fn fill_quad(ctx: &mut Context, quad: [(f64, f64); 4], color: Color) {
    let (mut y_min, mut y_max) = (quad[0].1, quad[0].1);
    for &(_, y) in &quad[1..] {
        y_min = y_min.min(y);
        y_max = y_max.max(y);
    }
    if y_max - y_min <= f64::EPSILON {
        return;
    }

    // One braille sub-row in canvas units; half-steps avoid seams.
    const STEP: f64 = 0.5;
    let mut y = y_min;
    while y <= y_max {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for i in 0..4 {
            let (ax, ay) = quad[i];
            let (bx, by) = quad[(i + 1) % 4];
            // Does edge a->b straddle this scanline?
            if (ay <= y && by >= y) || (by <= y && ay >= y) {
                let dy = by - ay;
                let x = if dy.abs() < f64::EPSILON {
                    ax.min(bx)
                } else {
                    ax + (bx - ax) * (y - ay) / dy
                };
                lo = lo.min(x);
                hi = hi.max(x);
            }
        }
        if hi >= lo {
            ctx.draw(&CanvasLine {
                x1: lo,
                y1: y,
                x2: hi,
                y2: y,
                color,
            });
        }
        y += STEP;
    }
}
