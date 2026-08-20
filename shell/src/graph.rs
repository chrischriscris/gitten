//! The commit graph, drawn.
//!
//! One canvas per row, and every stroke stays inside it. A branch changing
//! lanes is an S spanning a *whole* row, so each row paints one half: the
//! halves meet on the row boundary, at the midpoint between the two lanes,
//! sharing a tangent — so they read as one long curve while each still
//! virtualizes with the list for free. Twice the run of a half-row corner,
//! half the steepness, no clipping at the boundary.
//!
//! Geometry comes entirely from `plait_core::GraphRow` — this file decides how
//! it looks, never what the topology is. It reads two things off the rows
//! either side: which lanes are mid-curve, because half of a curve lives next
//! door, and where a branch begins and ends, because that is what colour
//! follows (see [`Hues`]) rather than the column it happens to occupy.

use gpui::*;
use plait_core::host::Host;
use plait_core::theme::Theme;
use plait_core::{Commit, GraphRow};
use std::rc::Rc;

pub const ROW_H: f32 = 22.0;
const LANE_W: f32 = 14.0;

/// Hard cap on drawn lanes. git/git reaches 280 concurrently, which is a
/// 3,920px gutter — it pushes the commit text off the screen entirely and no
/// human reads past a dozen lanes anyway. Everything beyond the cap collapses
/// onto the last column, dimmed, so it reads as "there is more over here"
/// rather than silently lying about the topology.
const MAX_LANES: usize = 12;

/// A lane is 2px, not the 1.5px a dense list first suggests. Thinner reads as
/// a hairline sketch rather than something you could grab, and 2px straddles
/// no pixel boundary: a lane centre lands on 7, so the edges land on 6 and 8
/// and every vertical is crisp at any scale factor.
const STROKE: f32 = 2.0;
/// Node radius, and a fatter one for merges so a join is findable while
/// scrolling. The band is a shade under half the radius, which keeps it at
/// roughly STROKE — a node looks drawn with the same pen as the lines feeding
/// it — while the hole opens up as the radius grows.
const DOT_R: f32 = 4.5;
const MERGE_R: f32 = 5.5;
const RING: f32 = 0.45;

/// Curve halves butt into each other on the row boundary. Two antialiased butt
/// caps meeting exactly leaves a faint crease, so each half runs a hair past
/// the boundary along its own tangent — collinear, so it cannot kink, and they
/// overlap instead of abutting.
const OVERSHOOT: f32 = 0.5;

/// How many hues the wheel hands out. Not the same thing as how many colours a
/// theme ships: this is the size of the "which branch has which slot" ledger,
/// and the theme decides what a slot looks like. Six is the number of live
/// branches that can be told apart at a glance.
const LANE_HUES: usize = 6;

/// Colour belongs to the *branch*, not to the column it happens to sit in —
/// see [`Hues`]. Overflow is the exception: past the cap every lane shares one
/// column, so they share one grey and stop pretending to be individuals.
fn color(theme: &Theme, lane: u16, hue: u16) -> Rgba {
    if lane as usize >= MAX_LANES {
        return rgb(theme.lane_overflow);
    }
    rgb(theme.lane(hue as usize))
}

/// Hands out a colour per branch and keeps it until that branch ends.
///
/// Colouring by lane index is the obvious thing and it is wrong: lane 1 is
/// recycled the moment a branch merges, so branch after unrelated branch comes
/// out the same blue and the eye reads them as one long-running thing. So walk
/// the history instead and hand each *new* lane the next colour on the wheel,
/// skipping any colour a concurrently live lane already holds. Consecutive
/// branches therefore differ even when they share a column, and neighbours
/// never collide while six or fewer lanes are live.
struct Hues {
    /// Per lane slot, mirroring core's own bookkeeping.
    of: Vec<Option<u16>>,
    live: [u16; LANE_HUES],
    next: u16,
}

impl Hues {
    fn new() -> Self {
        Self {
            of: Vec::new(),
            live: [0; LANE_HUES],
            // So the first lane claimed — the trunk — comes out amber.
            next: LANE_HUES as u16 - 1,
        }
    }

    /// This lane's colour, taking a fresh one off the wheel if the lane is
    /// new. Every read goes through here, so a lane can never come out blank.
    fn claim(&mut self, lane: usize) -> u16 {
        if self.of.len() <= lane {
            self.of.resize(lane + 1, None);
        }
        if let Some(hue) = self.of[lane] {
            return hue;
        }
        let n = LANE_HUES as u16;
        // The first free colour from here round the wheel; if all six are
        // live, take the next one anyway — a repeat beats a blank.
        for _ in 0..n {
            self.next = (self.next + 1) % n;
            if self.live[self.next as usize] == 0 {
                break;
            }
        }
        self.live[self.next as usize] += 1;
        self.of[lane] = Some(self.next);
        self.next
    }

    /// The branch ended here; its colour goes back on the wheel.
    fn release(&mut self, lane: usize) {
        if let Some(hue) = self.of.get_mut(lane).and_then(Option::take) {
            self.live[hue as usize] = self.live[hue as usize].saturating_sub(1);
        }
    }
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

/// How many lanes the topology actually uses — the honest number, uncapped,
/// which is why it comes off the core rows and not off the draws (those are
/// already collapsed onto the cap).
pub fn lane_count(rows: &[GraphRow]) -> usize {
    rows.iter()
        .map(|r| {
            let widest = r.through.iter().chain(&r.merges).chain(&r.forks).max().copied();
            widest.unwrap_or(r.lane).max(r.lane) + 1
        })
        .max()
        .unwrap_or(1)
}

/// A straight lane, in halves: `up` runs from the row's top edge to the dot
/// line, `down` from the dot line to the bottom edge. A half is missing when a
/// curve has taken it over, or when there is simply no history that way.
#[derive(Clone, Copy)]
struct Line {
    lane: u16,
    hue: u16,
    up: bool,
    down: bool,
}

/// Half an S. It touches `lane` on the dot line and crosses the row boundary
/// halfway to `partner`, where the neighbouring row picks it up.
#[derive(Clone, Copy)]
struct Curve {
    lane: u16,
    partner: u16,
    /// Whose colour it carries: always the branch, never the trunk it leaves
    /// or joins. For a lane collapsing onto a dot that is the lane's own hue;
    /// for one born out of a dot it is the far end's.
    hue: u16,
    /// Leaving the dot line downward, or reaching up out of it.
    down: bool,
}

/// A row flattened to just what painting needs, precomputed once at load so
/// the paint callback never touches the commit list.
#[derive(Clone)]
pub struct RowDraw {
    lane: u16,
    hue: u16,
    is_merge: bool,
    lines: Vec<Line>,
    curves: Vec<Curve>,
    /// This row's gutter width, measured once here rather than per frame.
    width: f32,
}

impl RowDraw {
    /// How much room this row's graph needs — its own lanes, not the widest
    /// row in the repository. A commit sitting alone on the trunk gets nearly
    /// the whole window for its subject, and only rows where the graph really
    /// is wider push their text across. Measured in whole lanes so the text
    /// steps on the lane grid: ragged by a column reads as "the graph is wider
    /// here", ragged by three pixels just reads as broken.
    pub fn width(&self) -> f32 {
        self.width
    }
}

fn measure(lane: u16, lines: &[Line], curves: &[Curve]) -> f32 {
    let col = |x: f32| (x / LANE_W) as usize;
    let mut last = col(lane_x(lane));
    for l in lines {
        last = last.max(col(lane_x(l.lane)));
    }
    for c in curves {
        // A half only travels to the midpoint between the two lanes, so it
        // often stops short of its partner's column entirely.
        let reach = lane_x(c.lane).max((lane_x(c.lane) + lane_x(c.partner)) / 2.0) + STROKE / 2.0;
        last = last.max(col(reach));
    }
    (last + 1) as f32 * LANE_W + GAP
}

/// Lanes past the cap collapse onto one column, so they may as well collapse
/// in the data too: git/git would otherwise queue 280 identical quads per row.
fn cap(lane: usize) -> u16 {
    lane.min(MAX_LANES) as u16
}

pub fn row_draws(commits: &[Commit], rows: &[GraphRow]) -> Vec<RowDraw> {
    let mut hues = Hues::new();
    let mut draws = Vec::with_capacity(rows.len());

    for (i, (c, r)) in commits.iter().zip(rows).enumerate() {
        let above = i.checked_sub(1).map(|j| &rows[j]);
        let below = rows.get(i + 1);

        // A lane born at the fork above arrives on a curve and so has no top
        // half; one ending at the merge below leaves on a curve and so has no
        // bottom half. Either way the partner is that row's dot.
        let arrives = |lane| above.filter(|a| a.forks.contains(&lane)).map(|a| a.lane);
        let departs = |lane| below.filter(|b| b.merges.contains(&lane)).map(|b| b.lane);

        let mut lines: Vec<Line> = Vec::with_capacity(r.through.len().min(MAX_LANES) + 1);
        let mut curves = Vec::with_capacity(r.forks.len() + r.merges.len());

        // Our own lane may be a branch tip nothing has drawn yet.
        let hue = hues.claim(r.lane);

        // Lanes converging on this dot: the tail half of their curve, in their
        // own colour, before that colour goes back on the wheel below.
        for &m in &r.merges {
            // A lane forked one row up and merged away again immediately never
            // gets a column of its own, so the far end of the curve is that
            // row's dot — otherwise the two halves would aim at different
            // midpoints and tear apart at the boundary.
            let end = arrives(m).unwrap_or(m);
            curves.push(Curve { lane: cap(r.lane), partner: cap(end), hue: hues.claim(m), down: false });
        }

        for &lane in r.through.iter().chain(std::iter::once(&r.lane)) {
            let own = lane == r.lane;
            let (up, down) = (arrives(lane), departs(lane));
            let line = Line {
                lane: cap(lane),
                hue: hues.claim(lane),
                // Nothing exists above the newest row, and a root commit's
                // lane stops at its dot. Don't draw history that isn't there.
                up: up.is_none() && !(own && i == 0),
                down: down.is_none() && !(own && c.parents.is_empty()),
            };
            // Everything past the cap shares a column: share the line too,
            // or git/git would queue 280 identical quads per row.
            match lines.last_mut().filter(|l| l.lane == line.lane) {
                Some(prev) => {
                    prev.up |= line.up;
                    prev.down |= line.down;
                }
                None => lines.push(line),
            }
            for (end, down) in [(up, false), (down, true)] {
                if let Some(partner) = end {
                    curves.push(Curve {
                        lane: cap(lane),
                        partner: cap(partner),
                        hue: line.hue,
                        down,
                    });
                }
            }
        }

        // Branches that end here give their colour back, and a root gives up
        // its own lane, before the forks below claim theirs.
        for &m in &r.merges {
            hues.release(m);
        }
        if c.parents.is_empty() {
            hues.release(r.lane);
        }

        // Lanes born out of this dot: the head half of their curve.
        for &f in &r.forks {
            let end = departs(f).unwrap_or(f);
            curves.push(Curve { lane: cap(r.lane), partner: cap(end), hue: hues.claim(f), down: true });
        }

        draws.push(RowDraw {
            lane: cap(r.lane),
            hue,
            is_merge: c.parents.len() > 1,
            width: measure(cap(r.lane), &lines, &curves),
            lines,
            curves,
        });
    }
    draws
}

pub fn row_canvas(d: RowDraw, host: Rc<Host>) -> impl IntoElement {
    let w = d.width();
    canvas(
        move |_bounds, _window, _cx| d,
        move |bounds, d: RowDraw, window, _cx| paint_row(bounds, &d, window, &host.theme),
    )
    .flex_none()
    .w(px(w))
    .h(px(ROW_H))
}

fn paint_row(bounds: Bounds<Pixels>, d: &RowDraw, window: &mut Window, theme: &Theme) {
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
            color(theme, l.lane, l.hue),
        ));
    }

    for c in &d.curves {
        // Either end past the cap makes the whole thing overflow — otherwise
        // the half anchored on a visible lane comes out in the branch's colour
        // and the half in the collapsed column comes out grey, and one curve
        // changes colour halfway across the gutter.
        let side = c.lane.max(c.partner);
        half_s(window, x(c.lane), x(c.partner), mid, c.down, color(theme, side, c.hue));
    }

    // The node last: it is opaque, so it punches through whatever runs under
    // it and the lines read as passing behind. GPUI orders overlapping
    // primitives by insertion, so this is enough — no z-index needed.
    let r = if d.is_merge { MERGE_R } else { DOT_R };
    dot(window, x(d.lane), mid, r, color(theme, d.lane, d.hue), theme.chrome.bg);
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
fn dot(window: &mut Window, x: f32, y: f32, r: f32, color: Rgba, bg: plait_core::theme::Rgb) {
    window.paint_quad(quad(
        Bounds::from_corners(point(px(x - r), px(y - r)), point(px(x + r), px(y + r))),
        Corners::all(px(r)),
        rgb(bg),
        Edges::all(px(r * RING)),
        color,
        BorderStyle::Solid,
    ));
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: that pulls in gpui's glob, whose own
    // `test` attribute shadows the built-in one and blows the macro recursion
    // limit. Name what we need instead.
    use super::{row_draws, Curve, RowDraw};
    use plait_core::Commit;

    fn commit(sha: &str, parents: &[&str]) -> Commit {
        Commit {
            sha: sha.into(),
            short: sha.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            author: "Ada".into(),
            timestamp: 0,
            subject: sha.into(),
        }
    }

    fn draws(cs: &[Commit]) -> Vec<RowDraw> {
        row_draws(cs, &plait_core::assign_lanes(cs))
    }

    /// The two halves of every curve live in different rows, so the pair of
    /// lanes they aim at has to agree — otherwise they cross the boundary at
    /// different x and the branch visibly tears in half.
    fn halves_meet(ds: &[RowDraw]) {
        for (i, d) in ds.iter().enumerate() {
            for c in &d.curves {
                let (row, want_down) = if c.down { (i + 1, false) } else { (i.wrapping_sub(1), true) };
                let Some(other) = ds.get(row) else { continue };
                let pair = |c: &Curve| {
                    let (a, b) = (c.lane.min(c.partner), c.lane.max(c.partner));
                    (a, b, c.hue)
                };
                assert!(
                    other.curves.iter().any(|o| o.down == want_down && pair(o) == pair(c)),
                    "row {i} curve {:?} has no other half in row {row}: {:?}",
                    pair(c),
                    other.curves.iter().map(pair).collect::<Vec<_>>(),
                );
            }
        }
    }

    #[test]
    fn a_branch_and_its_merge_are_one_unbroken_curve() {
        //   a (merge of b, c)   fork out of a's dot, arriving on c's lane
        //   |\
        //   b c
        //   |/
        //   d                   and collapsing back into d's dot
        let cs = [
            commit("a", &["b", "c"]),
            commit("b", &["d"]),
            commit("c", &["d"]),
            commit("d", &[]),
        ];
        halves_meet(&draws(&cs));
    }

    #[test]
    fn a_branch_that_lasts_one_row_is_still_one_unbroken_curve() {
        // c is both a's second parent and b's only parent, so its lane is born
        // and dies without ever getting a column of its own.
        let cs = [
            commit("a", &["b", "c"]),
            commit("b", &["c"]),
            commit("c", &["d"]),
            commit("d", &[]),
        ];
        halves_meet(&draws(&cs));
    }

    #[test]
    fn history_stops_where_it_stops() {
        let cs = [commit("a", &["b"]), commit("b", &[])];
        let ds = draws(&cs);
        assert!(!ds[0].lines[0].up, "nothing exists above the newest commit");
        assert!(ds[0].lines[0].down);
        assert!(!ds[1].lines[0].down, "a root's lane ends at its dot");
    }

    #[test]
    fn consecutive_branches_in_one_lane_get_different_colours() {
        //   a (merge of b, c) ... e (merge of f, g): two branches, both of
        //   which live in lane 1, one after the other.
        let cs = [
            commit("a", &["b", "c"]),
            commit("b", &["e"]),
            commit("c", &["e"]),
            commit("e", &["f", "g"]),
            commit("f", &["h"]),
            commit("g", &["h"]),
            commit("h", &[]),
        ];
        let ds = draws(&cs);
        let hue = |row: usize| ds[row].hue;
        assert_eq!(ds[2].lane, 1);
        assert_eq!(ds[5].lane, 1);
        assert_ne!(hue(2), hue(5), "lane 1 recycled, colour must not be");
        assert_eq!(hue(0), hue(3), "the trunk keeps its colour throughout");
    }
}



