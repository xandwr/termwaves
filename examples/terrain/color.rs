//! Shared color mapping for the visualizations.

use ratatui::style::Color;

/// Color of fully-shadowed terrain faces (a deep purple) and fully-lit ones
/// (Light Cornflower Blue, `#93CCEA`). The surface ramps between these by the
/// surface-normal brightness. Swap these to recolor the whole landscape.
const SURFACE_SHADOW: (u8, u8, u8) = (80, 0, 40);
const SURFACE_HIGHLIGHT: (u8, u8, u8) = (0, 250, 255);

/// Shade a terrain face by a `0.0..=1.0` brightness (typically a Lambert `n·l`
/// term), interpolating from the purple shadow color to the cornflower-blue
/// highlight.
pub fn surface_color(brightness: f32) -> Color {
    let b = brightness.clamp(0.0, 1.0);
    let (sr, sg, sb) = SURFACE_SHADOW;
    let (hr, hg, hb) = SURFACE_HIGHLIGHT;
    let lerp = |lo: u8, hi: u8| (lo as f32 + (hi as f32 - lo as f32) * b).round() as u8;
    Color::Rgb(lerp(sr, hr), lerp(sg, hg), lerp(sb, hb))
}

/// Map a normalized intensity `0.0..=1.0` to a cold→hot heatmap color.
pub fn heat_color(t: f32) -> Color {
    const STOPS: [(u8, u8, u8); 5] = [
        (0, 0, 255),
        (0, 255, 255),
        (0, 255, 0),
        (255, 255, 0),
        (255, 0, 0),
    ];
    let t = t.clamp(0.0, 1.0);
    let segments = (STOPS.len() - 1) as f32;
    let scaled = t * segments;
    let i = (scaled.floor() as usize).min(STOPS.len() - 2);
    let frac = scaled - i as f32;
    let (r0, g0, b0) = STOPS[i];
    let (r1, g1, b1) = STOPS[i + 1];
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * frac).round() as u8;
    Color::Rgb(lerp(r0, r1), lerp(g0, g1), lerp(b0, b1))
}
