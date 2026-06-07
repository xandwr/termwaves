//! 3D spectral terrain: the F1 view.
//!
//! Each audio frame contributes one row of band magnitudes (the same log-spaced
//! spectrum the F1 bar graph draws). We keep a ring of the most recent rows and
//! treat them as a height-field:
//!
//! * **X (width)** : frequency band, low on the left, high on the right.
//! * **Z (depth)** : time. The newest row sits right at the camera (z = 0) and
//!   older rows recede into the distance (z grows).
//! * **Y (height)**: band magnitude, `0.0..=1.0`, scaled by [`PEAK_HEIGHT`].
//!
//! Pushing a fresh row each frame scrolls the height-field toward the camera.
//! At the current shallow [`DEPTH`] this reads less as a deep scrolling
//! landscape and more as a single shaded crest whose facets shift with the
//! audio: the camera is fixed and the few time-slices stream past it. See
//! [`DEPTH`] for why three rows is the sweet spot for that gradient-plane look.
//!
//! Rendering is a hand-rolled pinhole projection onto a braille [`Canvas`]:
//! every grid vertex is projected to screen space, then we stroke the wireframe
//! (each vertex linked to its right and far neighbours). Wireframe rather than a
//! filled surface because a terminal has no depth buffer and lines read cleanly
//! where shaded polygons would smear. Far rows are drawn before near ones so
//! nearer geometry paints over them: a painter's-algorithm stand-in for depth
//! testing that's good enough at this resolution.

use ratatui::{
    prelude::*,
    widgets::canvas::{Canvas, Line as CanvasLine},
};

use crate::heat_color;

/// How many time-rows of history the terrain holds (depth resolution).
///
/// 3 is deliberate, not a placeholder, and is the *minimum* that works: two rows
/// span a single strip of quads, which can only ever read as a flat ridgeline:
/// one slope carries no curvature. Three rows give two adjacent strips, and the
/// difference between their slopes is the second-order information the eye reads
/// as a shaded, receding plane (the green facets and the cyan→blue gradient
/// slope). Drop to 2 and the surface collapses to a contour line; raise it well
/// past a handful and it goes back to the dense scrolling landscape. Keep at 3
/// for the gradient-plane look.
const DEPTH: usize = 3;

/// World-space height of a full-scale (magnitude 1.0) band
const PEAK_HEIGHT: f32 = 32.0;

/// Shaping exponent applied to each magnitude before it becomes a height
const SPIKE_GAMMA: f32 = 2.2;

/// Camera placement in world space
const CAM_HEIGHT: f32 = 2.0;
/// How far behind the newest row (z = 0) the eye sits
const CAM_SETBACK: f32 = 8.0;
/// Downward pitch of the eye, in radians
const CAM_PITCH: f32 = 0.5;
/// Focal length of the pinhole projection, in the same units as the grid
const FOCAL: f32 = 1.0;

/// A rolling 3D height-field built from successive spectrum rows.
///
/// Width (band count) is fixed at construction; depth is [`DEPTH`]. Newly pushed
/// rows overwrite the oldest, and a logical `head` marks where "now" is so we can
/// walk rows newest-first without shifting memory.
pub struct Terrain {
    /// `DEPTH` rows of `width` magnitudes each, flattened row-major.
    /// Row `r` (0 = newest) lives at ring index `(head + DEPTH - 1 - r) % DEPTH`.
    rows: Vec<f32>,
    /// Bands per row.
    width: usize,
    /// Ring write cursor: the next slot a pushed row will occupy.
    head: usize,
    /// Whether at least one row has been pushed (else there's nothing to draw).
    primed: bool,
}

impl Terrain {
    /// Build an empty terrain holding rows of `width` bands.
    pub fn new(width: usize) -> Self {
        Self {
            rows: vec![0.0; DEPTH * width.max(1)],
            width: width.max(1),
            head: 0,
            primed: false,
        }
    }

    /// Push the latest spectrum row (one magnitude per band, `0.0..=1.0`),
    /// scrolling the landscape one step toward the camera. A row whose length
    /// differs from `width` is truncated or zero-padded to fit.
    pub fn push(&mut self, magnitudes: &[f32]) {
        let base = self.head * self.width;
        let slot = &mut self.rows[base..base + self.width];
        for (dst, src) in slot.iter_mut().zip(magnitudes.iter()) {
            *dst = *src;
        }
        // Zero-pad if the incoming row was short (band count shouldn't change
        // mid-run, but stay defensive rather than carry stale data).
        for dst in slot.iter_mut().skip(magnitudes.len()) {
            *dst = 0.0;
        }
        self.head = (self.head + 1) % DEPTH;
        self.primed = true;
    }

    /// Magnitude at depth-row `r` (0 = newest) and band `x`. Rows that predate
    /// the first push read as 0.0, so a partially-filled ring draws as flat.
    fn height(&self, r: usize, x: usize) -> f32 {
        let ring = (self.head + DEPTH - 1 - r) % DEPTH;
        self.rows[ring * self.width + x]
    }

    /// Render the terrain wireframe into `area`.
    pub fn render(&self, f: &mut Frame, area: Rect) {
        if !self.primed || self.width < 2 {
            return;
        }

        // Canvas works in its own coordinate space; we project world → a
        // symmetric screen box and let the Canvas map that to braille cells.
        // 2× horizontal cells (braille) for smoother lines.
        let sx = (area.width as f64 * 2.0).max(1.0);
        let sy = (area.height as f64 * 4.0).max(1.0); // braille is 4 dots tall

        // Aspect-ratio correction so the terrain fills the pane width instead of
        // sitting square in the middle. Terminal cells are roughly twice as tall
        // as wide, which the 2×4 braille grid only partly offsets; fold both the
        // pane's pixel aspect and that ~2:1 cell shape into one horizontal gain.
        const CELL_ASPECT: f64 = 2.0; // cell height / width
        let aspect = (sx / sy) * CELL_ASPECT;

        let width = self.width;
        // Project a grid vertex (band x, depth r) to screen space, or None if it
        // falls behind the eye (non-positive depth after the camera transform).
        let project = |x: usize, r: usize| -> Option<(f64, f64)> {
            // World coordinates. Centre x on 0; map band index to [-1, 1] so the
            // FOV is independent of band count.
            let wx = (x as f32 / (width - 1) as f32 - 0.5) * 2.0;
            // Gamma-shape the magnitude so peaks spike and the floor stays low.
            let wy = self.height(r, x).powf(SPIKE_GAMMA) * PEAK_HEIGHT;
            let wz = r as f32; // depth into the screen

            // Camera at (0, CAM_HEIGHT, -CAM_SETBACK). Translate into eye space.
            let ex = wx;
            let ty = wy - CAM_HEIGHT;
            let tz = wz + CAM_SETBACK;

            // Pitch the gaze down by CAM_PITCH: rotate the (y, z) pair so the view
            // axis tilts toward the terrain. This is what creates the horizon: far
            // rows climb toward a vanishing line instead of crowding the bottom.
            let (sp, cp) = (CAM_PITCH.sin(), CAM_PITCH.cos());
            let ey = ty * cp - tz * sp;
            let ez = ty * sp + tz * cp;
            if ez <= 0.05 {
                return None; // behind / at the eye: skip to avoid divide blow-up
            }

            // Pinhole projection. Screen origin is centre; +ndc_y is up. Stretch
            // x by the pane aspect so the grid spans the full width on any size.
            let ndc_x = (FOCAL * ex / ez) as f64 * aspect;
            let ndc_y = (FOCAL * ey / ez) as f64;
            // Map NDC (~[-1,1]) to Canvas space [0, s). The Canvas itself already
            // flips y (its y_bounds run bottom→top), so keep +y = up here and let
            // it do the single flip: flipping again would render upside down.
            let px = (ndc_x * 0.5 + 0.5) * sx;
            let py = (ndc_y * 0.5 + 0.5) * sy;
            Some((px, py))
        };

        let canvas = Canvas::default()
            .x_bounds([0.0, sx])
            .y_bounds([0.0, sy])
            .paint(move |ctx| {
                // Painter's algorithm: far rows first so nearer geometry overdraws.
                for r in (0..DEPTH).rev() {
                    for x in 0..width {
                        let Some((px, py)) = project(x, r) else {
                            continue;
                        };
                        let color = heat_color(self.height(r, x));

                        // Edge to the right neighbour (constant depth).
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
                        // Edge to the nearer neighbour (constant band, r-1 is one
                        // step toward the camera). Drawing toward the near row
                        // ties successive time-slices into ribs.
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
