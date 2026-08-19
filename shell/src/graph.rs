//! The commit graph, drawn.
//!
//! One canvas per row. Every curve this file draws is contained within a
//! single row (a fork runs mid->bottom, a merge runs top->mid), so nothing is
//! clipped at the boundary and the graph virtualizes with the list for free.
//! Geometry comes entirely from `plait_core::GraphRow` — this file decides how
//! it looks, never what the topology is.

use gpui::*;
use plait_core::{Commit, GraphRow};

pub const ROW_H: f32 = 22.0;
const LANE_W: f32 = 14.0;

/// Hard cap on drawn lanes. git/git reaches 280 concurrently, which is a
/// 3,920px gutter — it pushes the commit text off the screen entirely and no
/// human reads past a dozen lanes anyway. Everything beyond the cap collapses
/// onto the last column, dimmed, so it reads as "there is more over here"
/// rather than silently lying about the topology.
const MAX_LANES: usize = 12;
const OVERFLOW_COLOR: u32 = 0x453f39;
const DOT_R: f32 = 3.5;
const STROKE: f32 = 1.5;

/// Warm amber first so the checked-out lane reads as primary; the rest are
/// spaced around the wheel at roughly matched lightness so no lane shouts.
const LANE_COLORS: [u32; 6] = [0xdfa851, 0x6f9ecf, 0xa983c9, 0x5fa8a0, 0xc97d6f, 0x8fb35e];

fn lane_color(lane: usize) -> Rgba {
    if lane >= MAX_LANES {
        return rgb(OVERFLOW_COLOR);
    }
    rgb(LANE_COLORS[lane % LANE_COLORS.len()])
}

/// Total gutter width, capped. Read by the view so the text columns line up.
pub fn gutter_width(lanes: usize) -> f32 {
    lanes.min(MAX_LANES).max(1) as f32 * LANE_W
}

/// A row flattened to just what painting needs, precomputed once at load so the
/// paint callback never touches the commit list.
#[derive(Clone)]
pub struct RowDraw {
    lane: usize,
    through: Vec<usize>,
    merges: Vec<usize>,
    forks: Vec<usize>,
    is_merge: bool,
}

pub fn row_draws(commits: &[Commit], rows: &[GraphRow]) -> Vec<RowDraw> {
    commits
        .iter()
        .zip(rows)
        .map(|(c, r)| RowDraw {
            lane: r.lane,
            through: r.through.clone(),
            merges: r.merges.clone(),
            forks: r.forks.clone(),
            is_merge: c.parents.len() > 1,
        })
        .collect()
}

pub fn lane_count(draws: &[RowDraw]) -> usize {
    draws
        .iter()
        .map(|d| {
            let widest = d.through.iter().chain(&d.merges).chain(&d.forks).max().copied();
            widest.unwrap_or(d.lane).max(d.lane) + 1
        })
        .max()
        .unwrap_or(1)
}

pub fn row_canvas(d: RowDraw, lanes: usize) -> impl IntoElement {
    canvas(
        move |_bounds, _window, _cx| d,
        move |bounds, d: RowDraw, window, _cx| paint_row(bounds, &d, window),
    )
    .flex_none()
    .w(px(gutter_width(lanes)))
    .h(px(ROW_H))
}

fn paint_row(bounds: Bounds<Pixels>, d: &RowDraw, window: &mut Window) {
    // Clamp past the cap so overflow lanes stack on the final column.
    let lane_x = |lane: usize| {
        let col = lane.min(MAX_LANES - 1) as f32;
        bounds.origin.x + px(col * LANE_W + LANE_W / 2.0)
    };
    let top = bounds.origin.y;
    let mid = top + px(ROW_H / 2.0);
    let bot = top + px(ROW_H);

    // Lanes that simply continue past this row, plus our own. These are
    // straight verticals — i.e. rectangles. Painting them as quads instead of
    // tessellated stroke paths is the single biggest win in this function:
    // twelve quads cost a fraction of twelve paths, and with a 12-lane cap
    // these dominate every frame.
    for &lane in std::iter::once(&d.lane).chain(&d.through) {
        let x = lane_x(lane) - px(STROKE / 2.0);
        window.paint_quad(fill(
            Bounds::from_corners(point(x, top), point(x + px(STROKE), bot)),
            lane_color(lane),
        ));
    }

    // A merge commit forks a lane downward: out of the dot, then down.
    for &f in &d.forks {
        curve(window, point(lane_x(d.lane), mid), point(lane_x(f), bot), point(lane_x(f), mid), lane_color(f));
    }

    // A branch point collapses a lane inward: down from above, then into the dot.
    for &m in &d.merges {
        curve(window, point(lane_x(m), top), point(lane_x(d.lane), mid), point(lane_x(m), mid), lane_color(m));
    }

    dot(window, lane_x(d.lane), mid, d.is_merge, lane_color(d.lane));
}

fn curve(window: &mut Window, from: Point<Pixels>, to: Point<Pixels>, ctrl: Point<Pixels>, color: Rgba) {
    let mut p = PathBuilder::stroke(px(STROKE));
    p.move_to(from);
    p.curve_to(to, ctrl);
    if let Ok(path) = p.build() {
        window.paint_path(path, color);
    }
}

/// Four quadratic segments — indistinguishable from a circle at this radius.
fn dot(window: &mut Window, cx: Pixels, cy: Pixels, is_merge: bool, color: Rgba) {
    let r = px(if is_merge { DOT_R + 1.0 } else { DOT_R });
    let mut p = PathBuilder::fill();
    p.move_to(point(cx + r, cy));
    p.curve_to(point(cx, cy + r), point(cx + r, cy + r));
    p.curve_to(point(cx - r, cy), point(cx - r, cy + r));
    p.curve_to(point(cx, cy - r), point(cx - r, cy - r));
    p.curve_to(point(cx + r, cy), point(cx + r, cy - r));
    if let Ok(path) = p.build() {
        window.paint_path(path, color);
    }
}
