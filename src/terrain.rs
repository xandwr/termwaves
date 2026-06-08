//! 3D spectral terrain: the F1 view.

use ratatui::{
    prelude::*,
    widgets::canvas::{Canvas, Circle, Line as CanvasLine},
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
/// Above this depth the terrain becomes an "island": the nearest and furthest
/// rows are pinned to y=0 and a smooth envelope tugs the interior rows down so
/// only the middle of the landscape rises, like land surrounded by water.
const ISLAND_MIN_DEPTH: usize = 6;
/// How many balls roll around on the landscape.
const BALL_COUNT: usize = 5;
/// How hard the surface slope pushes a ball each tick (grid units / tick²).
const BALL_GRAVITY: f32 = 0.12;
/// Velocity retained each tick (the rest is lost to friction), so balls settle
/// in valleys instead of oscillating forever.
const BALL_FRICTION: f32 = 0.82;
/// Speed clamp so a steep plane can't fling a ball across the whole grid in one
/// tick (grid units / tick).
const BALL_MAX_SPEED: f32 = 0.6;
/// Radius of the drawn ball marker, in screen pixels.
const BALL_RADIUS: f64 = 2.5;
/// Ticks a ball spends airborne in its lift-and-spiral animation before it
/// drops back onto the surface near the center.
const BALL_FLOAT_TICKS: u32 = 90;
/// Peak extra world-height a floating ball reaches at the top of its arc, on top
/// of the surface height. Sets how high the "wind" lifts it.
const BALL_FLOAT_LIFT: f32 = 3.0;
/// Number of full turns a ball spins through during its float, as the spiral
/// winds inward toward the center.
const BALL_FLOAT_TURNS: f32 = 2.5;
/// Per-tick chance (out of `u32::MAX`) that a silent, rolling ball starts saying
/// something. ~0.4% gives an occasional pipe-up without constant chatter.
const BALL_SPEAK_CHANCE: u32 = (u32::MAX as f64 * 0.004) as u32;
/// Ticks a speech bubble stays up once a ball starts talking.
const BALL_SPEAK_TICKS: u32 = 70;

/// The things the little people say, picked at random.
const PHRASES: &[&str] = &[
    "wheee!",
    "i'm flying!",
    "where am i?",
    "nice hill",
    "wooo",
    "help",
    "again!",
    "so windy",
    "hi mom",
    "is this the cloud?",
    "5 stars",
    "whoa",
    "not again",
    "tell my wife i love her",
    "yeet",
    "weather's nice up here",
];

/// What a ball is doing this tick. Balls roll on the surface until they reach an
/// edge, then float up in an inward spiral before dropping back near the center.
#[derive(Clone, Copy)]
enum BallState {
    /// Marble rolling on the height-field, pushed downhill by the planes.
    Rolling { vx: f32, vr: f32 },
    /// Lifted by the "wind": spiraling up and inward toward the center. `phase`
    /// runs `0.0..1.0` over `BALL_FLOAT_TICKS`; `from_*` is the lift-off point.
    Floating {
        phase: f32,
        from_x: f32,
        from_r: f32,
    },
}

/// A ball living on the terrain. Position is in continuous grid coordinates
/// (`gx` across bands, `gr` receding), so it can sit between grid vertices; the
/// surface height is sampled there each tick.
#[derive(Clone, Copy)]
struct Ball {
    gx: f32,
    gr: f32,
    /// Extra world-height above the surface, nonzero only while floating.
    lift: f32,
    state: BallState,
    /// Current speech: `(phrase index, ticks remaining)`, or `None` if quiet.
    speech: Option<(usize, u32)>,
}

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
    /// Marbles rolling on the surface, pushed downhill by the planes.
    balls: Vec<Ball>,
    /// Xorshift state driving the random speech timing. Seeded to a fixed
    /// nonzero constant; it just needs to look unpredictable, not be secure.
    rng: u32,
}

impl Terrain {
    /// Build an empty terrain holding rows of `width` bands.
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
            balls: Self::spawn_balls(width, DEFAULT_DEPTH),
            rng: 0x9E3779B9,
        }
    }

    /// Advance the xorshift PRNG and return the next pseudo-random `u32`.
    fn next_rand(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }

    /// Scatter `BALL_COUNT` balls across the interior of a `width`×`depth` grid,
    /// spread evenly along the depth axis and staggered across the bands so they
    /// don't start in a single line. Deterministic (no RNG) so resizes are stable.
    fn spawn_balls(width: usize, depth: usize) -> Vec<Ball> {
        let max_x = (width - 1) as f32;
        let max_r = (depth - 1) as f32;
        (0..BALL_COUNT)
            .map(|i| {
                let t = (i as f32 + 1.0) / (BALL_COUNT as f32 + 1.0);
                // Stagger across the bands using a coprime-ish stride for spread.
                let xf = ((i as f32 * 0.37 + 0.2) % 1.0).clamp(0.05, 0.95);
                Ball {
                    gx: xf * max_x,
                    gr: t * max_r,
                    lift: 0.0,
                    state: BallState::Rolling { vx: 0.0, vr: 0.0 },
                    speech: None,
                }
            })
            .collect()
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
        self.balls = Self::spawn_balls(self.width, depth);
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

    /// Depth-wise height envelope. For deep landscapes (`depth >= ISLAND_MIN_DEPTH`)
    /// the nearest (`r=0`) and furthest (`r=depth-1`) rows are pinned to 0 and the
    /// interior is pulled up by a smooth hump, so the middle rises like an island
    /// in water. Shallow landscapes are left untouched (factor `1.0`).
    fn island_factor(&self, r: usize) -> f32 {
        if self.depth < ISLAND_MIN_DEPTH {
            return 1.0;
        }
        // Normalize row to 0..=1 across the depth, then a sine hump that is 0 at
        // both edges and 1 in the middle.
        let t = r as f32 / (self.depth - 1) as f32;
        (t * std::f32::consts::PI).sin()
    }

    /// Rendered surface height (the `wy` of [`world`]) at grid vertex `(x, r)`.
    fn surface_y(&self, x: usize, r: usize) -> f32 {
        self.height(r, x).powf(SPIKE_GAMMA) * PEAK_HEIGHT * self.island_factor(r)
    }

    /// Surface height at *continuous* grid coordinates `(gx, gr)`, bilinearly
    /// interpolated between the four surrounding vertices. Coordinates are
    /// clamped to the grid so balls near the edge still sample a valid cell.
    fn surface_y_at(&self, gx: f32, gr: f32) -> f32 {
        let gx = gx.clamp(0.0, (self.width - 1) as f32);
        let gr = gr.clamp(0.0, (self.depth - 1) as f32);
        let x0 = gx.floor() as usize;
        let r0 = gr.floor() as usize;
        let x1 = (x0 + 1).min(self.width - 1);
        let r1 = (r0 + 1).min(self.depth - 1);
        let fx = gx - x0 as f32;
        let fr = gr - r0 as f32;
        let h00 = self.surface_y(x0, r0);
        let h10 = self.surface_y(x1, r0);
        let h01 = self.surface_y(x0, r1);
        let h11 = self.surface_y(x1, r1);
        let top = h00 + (h10 - h00) * fx;
        let bot = h01 + (h11 - h01) * fx;
        top + (bot - top) * fr
    }

    /// World-space position of grid vertex `(x, r)`: `x` across the bands, the
    /// rendered height up, and `r` receding from the camera. The projection in
    /// `render_wireframe` consumes these, and face normals are built from them.
    fn world(&self, x: usize, r: usize) -> (f32, f32, f32) {
        let wx = (x as f32 / (self.width - 1) as f32 - 0.5) * 2.0 * WORLD_HALF_WIDTH;
        let wy = self.surface_y(x, r);
        let wz = r as f32;
        (wx, wy, wz)
    }

    /// Advance every ball one tick. Rolling balls are pushed downhill by the
    /// surface slope (the planes do the pushing); a ball that rolls into an edge
    /// is caught by the "wind" and switches to a floating spiral that lifts it up
    /// and inward, dropping it back onto the surface near the center.
    fn step_balls(&mut self) {
        if !self.primed || self.width < 2 || self.depth < 2 {
            return;
        }
        // Move the balls out so the gradient sampling can borrow `self` immutably.
        let mut balls = std::mem::take(&mut self.balls);
        for ball in &mut balls {
            match ball.state {
                BallState::Rolling { vx, vr } => self.step_rolling(ball, vx, vr),
                BallState::Floating {
                    phase,
                    from_x,
                    from_r,
                } => self.step_floating(ball, phase, from_x, from_r),
            }
            // Tick down any active speech; otherwise occasionally pipe up.
            match ball.speech {
                Some((_, 1)) | None => {
                    ball.speech = if self.next_rand() < BALL_SPEAK_CHANCE {
                        let idx = self.next_rand() as usize % PHRASES.len();
                        Some((idx, BALL_SPEAK_TICKS))
                    } else {
                        None
                    };
                }
                Some((idx, ticks)) => ball.speech = Some((idx, ticks - 1)),
            }
        }
        self.balls = balls;
    }

    /// One tick of a rolling ball: accelerate downhill along the surface gradient,
    /// apply friction, clamp speed, and advance. Reaching an edge lifts it into
    /// the floating state instead of wrapping.
    fn step_rolling(&self, ball: &mut Ball, mut vx: f32, mut vr: f32) {
        let max_x = (self.width - 1) as f32;
        let max_r = (self.depth - 1) as f32;
        // Finite-difference step for the gradient, small relative to a cell.
        const EPS: f32 = 0.25;
        // Downhill direction = negative gradient of the surface height.
        let dy_dx =
            self.surface_y_at(ball.gx + EPS, ball.gr) - self.surface_y_at(ball.gx - EPS, ball.gr);
        let dy_dr =
            self.surface_y_at(ball.gx, ball.gr + EPS) - self.surface_y_at(ball.gx, ball.gr - EPS);
        vx += -dy_dx * BALL_GRAVITY;
        vr += -dy_dr * BALL_GRAVITY;
        vx *= BALL_FRICTION;
        vr *= BALL_FRICTION;
        let speed = (vx * vx + vr * vr).sqrt();
        if speed > BALL_MAX_SPEED {
            let s = BALL_MAX_SPEED / speed;
            vx *= s;
            vr *= s;
        }
        ball.gx += vx;
        ball.gr += vr;

        // Did it reach an edge? If so, the wind lifts it into a spiral.
        if ball.gx <= 0.0 || ball.gx >= max_x || ball.gr <= 0.0 || ball.gr >= max_r {
            ball.gx = ball.gx.clamp(0.0, max_x);
            ball.gr = ball.gr.clamp(0.0, max_r);
            ball.state = BallState::Floating {
                phase: 0.0,
                from_x: ball.gx,
                from_r: ball.gr,
            };
        } else {
            ball.state = BallState::Rolling { vx, vr };
        }
    }

    /// One tick of a floating ball: advance the spiral. The base position lerps
    /// from the lift-off point toward the center while a shrinking rotating offset
    /// winds it inward, and `lift` traces a rise-and-fall arc. When the phase
    /// completes the ball lands at the center and resumes rolling.
    fn step_floating(&self, ball: &mut Ball, phase: f32, from_x: f32, from_r: f32) {
        let max_x = (self.width - 1) as f32;
        let max_r = (self.depth - 1) as f32;
        let (cx, cr) = (max_x * 0.5, max_r * 0.5);

        let phase = phase + 1.0 / BALL_FLOAT_TICKS as f32;
        if phase >= 1.0 {
            // Land at the center and roll again.
            ball.gx = cx;
            ball.gr = cr;
            ball.lift = 0.0;
            ball.state = BallState::Rolling { vx: 0.0, vr: 0.0 };
            return;
        }

        // Base path: straight lerp from lift-off point to center.
        let base_x = from_x + (cx - from_x) * phase;
        let base_r = from_r + (cr - from_r) * phase;

        // Spiral offset: rotate around the base path, radius shrinking to 0 so the
        // ball corkscrews inward as it climbs.
        let angle = phase * BALL_FLOAT_TURNS * std::f32::consts::TAU;
        let start_radius = ((from_x - cx).powi(2) + (from_r - cr).powi(2)).sqrt();
        let radius = start_radius * (1.0 - phase);
        ball.gx = (base_x + angle.cos() * radius).clamp(0.0, max_x);
        ball.gr = (base_r + angle.sin() * radius).clamp(0.0, max_r);

        // Height arc: rise then fall, peaking mid-float.
        ball.lift = (phase * std::f32::consts::PI).sin() * BALL_FLOAT_LIFT;
        ball.state = BallState::Floating {
            phase,
            from_x,
            from_r,
        };
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

        // Project each ball's surface position up front; the canvas closure only
        // needs the resulting screen point and its current line (if any), not a
        // borrow of `self`.
        let ball_pts: Vec<(f64, f64, Option<&'static str>)> = self
            .balls
            .iter()
            .filter_map(|b| {
                let wx = (b.gx / (self.width - 1) as f32 - 0.5) * 2.0 * WORLD_HALF_WIDTH;
                let wy = self.surface_y_at(b.gx, b.gr) + b.lift;
                let (px, py) = project(wx, wy, b.gr)?;
                let phrase = b.speech.map(|(idx, _)| PHRASES[idx]);
                Some((px, py, phrase))
            })
            .collect();

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
                // Draw the balls last so they sit on top of the wireframe.
                ctx.layer();
                for &(px, py, phrase) in &ball_pts {
                    ctx.draw(&Circle {
                        x: px,
                        y: py,
                        radius: BALL_RADIUS,
                        color: Color::White,
                    });
                    if let Some(text) = phrase {
                        // Float the line just above the dot. Labels always render
                        // on top of the canvas regardless of layer.
                        ctx.print(
                            px,
                            py + BALL_RADIUS * 2.0,
                            Line::from(text).style(Style::default().fg(Color::Yellow)),
                        );
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
            self.step_balls();
        }
    }

    /// Keys 1-9 set the landscape depth (number of rows) to twice that value,
    /// so `5` gives a depth of 10, `9` gives 18, etc.
    fn handle_key(&mut self, code: KeyCode) -> bool {
        if let KeyCode::Char(c @ '1'..='9') = code {
            self.set_depth((c as usize - '0' as usize) * 2);
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
