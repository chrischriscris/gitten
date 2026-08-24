//! The commit graph, drawn.
//!
//! One canvas per row, and every stroke stays inside it. A branch changing
//! lanes is an S spanning a *whole* row, so each row paints one half: the
//! halves meet on the row boundary, at the midpoint between the two lanes,
//! sharing a tangent — so they read as one long curve while each still
//! virtualizes with the list for free. Twice the run of a half-row corner,
//! half the steepness, no clipping at the boundary.
//!
//! Geometry comes entirely from `gitten_core::GraphRow` — this file decides how
//! it looks, never what the topology is. It reads two things off the rows
//! either side: which lanes are mid-curve, because half of a curve lives next
//! door, and where a branch begins and ends, because that is what colour
//! follows (see [`gitten_core::graph::Hues`]) rather than the column it happens to occupy.

use gitten_core::graph::MAX_LANES;
use gitten_core::host::Host;
use gitten_core::theme::Theme;
use gpui::*;
use std::rc::Rc;

pub const ROW_H: f32 = 22.0;
const LANE_W: f32 = 14.0;

/// The whole plan — which halves exist, which curve pairs with which, which
/// branch is which colour, how many columns a row needs, how many lanes there
/// really are — comes from `core`, because every part of it is a pure function
/// of the topology. A terminal gutter in box characters, a browser drawing SVG
/// and this canvas therefore agree about all of it, and what is left in this
/// file is geometry. See `gitten_core::graph`.
pub use gitten_core::graph::{lane_count, plan as row_draws, Draw};

/// A lane is 2px, not the 1.5px a dense list first suggests. Thinner reads as
/// a hairline sketch rather than something you could grab, and 2px straddles
/// no pixel boundary: a lane centre lands on 7, so the edges land on 6 and 8
/// and every vertical is crisp at any scale factor.
const STROKE: f32 = 2.0;
/// Node radius, and a fatter one for merges so a join is findable while
/// scrolling. The band is a shade under half the radius, which keeps it at
/// roughly STROKE — a node looks drawn with the same pen as the lines feeding
/// it — while the hole opens up as the radius grows.
///
/// Whole pixels, for the reason [`STROKE`] is 2: a lane centre lands on 7, so a
/// radius of 4.5 put the node's edges on 2.5 and 11.5 and the one deliberately
/// crisp thing in the gutter had a soft dot sitting on it.
const DOT_R: f32 = 4.0;
const MERGE_R: f32 = 5.0;
const RING: f32 = 0.45;

/// Curve halves butt into each other on the row boundary. Two antialiased butt
/// caps meeting exactly leaves a faint crease, so each half runs a hair past
/// the boundary along its own tangent — collinear, so it cannot kink, and they
/// overlap instead of abutting.
const OVERSHOOT: f32 = 0.5;

/// Colour belongs to the *branch*, not to the column it happens to sit in —
/// see [`gitten_core::graph::Hues`]. Overflow is the exception: past the cap every lane shares one
/// column, so they share one grey and stop pretending to be individuals.
fn color(theme: &Theme, overflow: bool, hue: u16) -> Rgba {
    if overflow {
        return rgb(theme.lane_overflow);
    }
    rgb(theme.lane(hue as usize))
}

/// Whether this lane is the collapsed column of a row that is hiding lanes.
///
/// Two conditions and not one: a repository with exactly [`MAX_LANES`] lanes
/// hides nothing, and dimming its last column would claim there is more history
/// over there. `Draw::capped` is what says there is.
fn overflowed(d: &Draw, lane: u16) -> bool {
    d.capped && lane as usize == MAX_LANES - 1
}

/// The breath between the last stroke of the graph and the first letter of the
/// subject — about one character. The whole point of measuring each row on its
/// own is that the two sit together, so this is small on purpose.
const GAP: f32 = 6.0;

/// Where lane `lane` is centred, clamped so overflow lanes stack on the final
/// column.
fn lane_x(lane: u16) -> f32 {
    (lane as usize).min(MAX_LANES - 1) as f32 * LANE_W + LANE_W / 2.0
}

/// Pixels for a row's own lane count, plus the breath before the subject. The
/// one thing this file adds to `core`'s plan besides the drawing itself.
pub fn row_width(d: &Draw) -> f32 {
    d.lanes as f32 * LANE_W + GAP
}

pub fn row_canvas(d: Draw, host: Rc<Host>) -> impl IntoElement {
    let w = row_width(&d);
    canvas(
        move |_bounds, _window, _cx| d,
        move |bounds, d: Draw, window, _cx| paint_row(bounds, &d, window, &host.theme),
    )
    .flex_none()
    .w(px(w))
    .h(px(ROW_H))
}

fn paint_row(bounds: Bounds<Pixels>, d: &Draw, window: &mut Window, theme: &Theme) {
    let ox = f32::from(bounds.origin.x);
    let x = |lane: u16| ox + lane_x(lane);
    let top = f32::from(bounds.origin.y);
    let mid = top + ROW_H / 2.0;
    let bot = top + ROW_H;

    // Straight halves first, as quads. A vertical line is a rectangle, and a
    // quad costs a fraction of a tessellated stroke path — with a 12-lane cap
    // these dominate every frame.
    for l in &d.lines {
        let (y0, y1) = (if l.up { top } else { mid }, if l.down { bot } else { mid });
        if y0 == y1 {
            continue; // a lane that is curve at both ends
        }
        let lx = x(l.lane) - STROKE / 2.0;
        window.paint_quad(fill(
            Bounds::from_corners(point(px(lx), px(y0)), point(px(lx + STROKE), px(y1))),
            color(theme, overflowed(d, l.lane), l.hue),
        ));
    }

    for c in &d.curves {
        // Either end in the collapsed column makes the whole thing overflow —
        // otherwise the half anchored on a visible lane comes out in the
        // branch's colour and the half in the collapsed column comes out grey,
        // and one curve changes colour halfway across the gutter.
        let over = overflowed(d, c.lane.max(c.partner));
        half_s(
            window,
            x(c.lane),
            x(c.partner),
            mid,
            c.down,
            color(theme, over, c.hue),
        );
    }

    // The node last: it is opaque, so it punches through whatever runs under
    // it and the lines read as passing behind. GPUI orders overlapping
    // primitives by insertion, so this is enough — no z-index needed.
    let r = if d.merge { MERGE_R } else { DOT_R };
    dot(
        window,
        x(d.lane),
        mid,
        r,
        color(theme, overflowed(d, d.lane), d.hue),
        theme.chrome.bg,
    );
}

/// One row's half of an S, from the dot line at `(x, y)` out through the row
/// boundary, ending halfway to `partner_x`.
///
/// The whole curve is the symmetric cubic from `(x, y)` to `(partner_x, y ±
/// ROW_H)` whose handles are half a row long — vertical where it leaves a lane
/// and vertical where it lands on one, steepest in between. Splitting that at
/// t=0.5 lands exactly on the row boundary, midway between the lanes; these
/// are the de Casteljau control points of that split, so the neighbour's half
/// continues this one to the pixel.
fn half_s(window: &mut Window, x: f32, partner_x: f32, y: f32, down: bool, color: Rgba) {
    let dx = (partner_x - x) / 2.0;
    let dy = if down { ROW_H / 2.0 } else { -ROW_H / 2.0 };

    let mut p = PathBuilder::stroke(px(STROKE));
    p.move_to(point(px(x), px(y)));
    p.cubic_bezier_to(
        point(px(x + dx), px(y + dy)),
        point(px(x), px(y + dy * 0.5)),
        point(px(x + dx * 0.5), px(y + dy * 0.75)),
    );

    // Tangent at the boundary — carry on along it for half a pixel.
    let (tx, ty) = (dx * 0.5, dy * 0.25);
    let len = (tx * tx + ty * ty).sqrt();
    if len > 0.0 {
        let s = OVERSHOOT / len;
        p.line_to(point(px(x + dx + tx * s), px(y + dy + ty * s)));
    }

    if let Ok(path) = p.build() {
        window.paint_path(path, color);
    }
}

/// A ring with the background punched out of the middle — one quad, since a
/// quad with corner radii at half its size *is* a circle, and the shader
/// antialiases it better than tessellation would.
fn dot(window: &mut Window, x: f32, y: f32, r: f32, color: Rgba, bg: gitten_core::theme::Rgb) {
    window.paint_quad(quad(
        Bounds::from_corners(point(px(x - r), px(y - r)), point(px(x + r), px(y + r))),
        Corners::all(px(r)),
        rgb(bg),
        Edges::all(px(r * RING)),
        color,
        BorderStyle::Solid,
    ));
}
