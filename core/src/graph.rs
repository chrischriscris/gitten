//! What the commit graph looks like, minus the drawing.
//!
//! [`assign_lanes`](crate::assign_lanes) decides the topology: which column each
//! commit sits in and which lanes pass through, converge and fork. What is left
//! before anything can be painted is two questions, and neither of them is about
//! pixels or cells:
//!
//! - **What colour is this branch?** [`Hues`], and the answer is not "the lane's
//!   index" — see below.
//! - **How many lanes are there really?** [`lane_count`], uncapped, which is the
//!   number to *report*; [`MAX_LANES`] is the number to *draw*.
//!
//! - **What is drawn on this row?** [`plan`], which answers it as *halves*: a
//!   lane passing through has an upper half and a lower half, and a lane
//!   changing columns is a curve that crosses the row boundary. No pixels, no
//!   cells — a client multiplies by whatever a lane is worth to it.
//!
//! All three were written in the GPUI shell first and all three are pure
//! functions of the topology, so a canvas drawing Bézier curves, a terminal
//! drawing box characters and a browser drawing SVG paths agree about which
//! branch is amber, where the overflow starts and which halves exist.
//!
//! # Halves, and why
//!
//! A branch changing lanes is an S spanning a *whole* row. Drawn as one shape it
//! belongs to neither row, and a virtualized list that builds only what is
//! visible then has nothing to hang it on. So each row draws its own half: the
//! two meet on the row boundary, at the midpoint between the two lanes, sharing
//! a tangent — one long curve to read, two independent rows to build.
//!
//! That is why [`Curve`] has a `partner` and a direction rather than a start and
//! an end. Where the half actually goes is the client's arithmetic.

use crate::{Commit, GraphRow};

/// Hard cap on drawn lanes.
///
/// git/git reaches 280 concurrent lanes. At any sane lane width that is a gutter
/// that pushes the commit subject clean off the screen, and no human reads past
/// a dozen lanes anyway — git's own `--graph` is unreadable well before that. A
/// frontend collapses everything past the cap onto the last column and dims it,
/// so the overflow is *visible* rather than silently misdrawn.
///
/// Here rather than in a frontend because two frontends drawing the same
/// repository at different caps disagree about the shape of history, which is
/// not a rendering preference.
pub const MAX_LANES: usize = 12;

/// How many hues the wheel hands out.
///
/// Not the same thing as how many colours a theme ships: this is the size of the
/// "which branch holds which slot" ledger, and
/// [`Theme::lane`](crate::theme::Theme::lane) decides what a slot looks like.
/// Six is about the number of live branches an eye can tell apart at a glance.
pub const LANE_HUES: usize = 6;

/// How many lanes the topology actually uses — the honest number, uncapped.
///
/// Off the rows and not off anything a frontend built, because those have
/// already been collapsed onto [`MAX_LANES`]. This is what a status line reports
/// so that "280 lanes" is visible even when twelve are drawn.
pub fn lane_count(rows: &[GraphRow]) -> usize {
    rows.iter()
        .map(|r| {
            let widest = r.through.iter().chain(&r.merges).chain(&r.forks).max().copied();
            widest.unwrap_or(r.lane).max(r.lane) + 1
        })
        .max()
        .unwrap_or(1)
}

/// Hands out a colour per branch and keeps it until that branch ends.
///
/// Colouring by lane index is the obvious thing and it is wrong: lane 1 is
/// recycled the moment a branch merges, so branch after unrelated branch comes
/// out the same blue and the eye reads them as one long-running thing. So walk
/// the history instead and hand each *new* lane the next colour on the wheel,
/// skipping any colour a concurrently live lane already holds. Consecutive
/// branches therefore differ even when they share a column, and neighbours never
/// collide while [`LANE_HUES`] or fewer lanes are live.
///
/// # The order of the walk is part of the answer
///
/// A caller walks the rows newest-first and, **per row**, in this order:
///
/// 1. [`Hues::claim`] the row's own lane, and every lane converging on it.
/// 2. [`Hues::claim`] every lane passing through.
/// 3. [`Hues::release`] every converging lane, and the row's own lane if the
///    commit is a root.
/// 4. [`Hues::claim`] every lane forked out of it.
///
/// Releasing before the forks claim is what lets a merged branch's colour be
/// reused immediately below, which is the whole point of a wheel. Releasing
/// *after* them wastes a slot per merge and a busy repository runs out.
pub struct Hues {
    /// Per lane slot, mirroring [`assign_lanes`](crate::assign_lanes)' own
    /// bookkeeping.
    of: Vec<Option<u16>>,
    live: [u16; LANE_HUES],
    next: u16,
}

impl Default for Hues {
    fn default() -> Self {
        Self::new()
    }
}

impl Hues {
    pub fn new() -> Self {
        Self {
            of: Vec::new(),
            live: [0; LANE_HUES],
            // So the first lane claimed — the trunk — comes out as hue 0, which
            // is the theme's first lane colour.
            next: LANE_HUES as u16 - 1,
        }
    }

    /// This lane's colour, taking a fresh one off the wheel if the lane is new.
    ///
    /// Every read goes through here, so a lane can never come out blank.
    pub fn claim(&mut self, lane: usize) -> u16 {
        if self.of.len() <= lane {
            self.of.resize(lane + 1, None);
        }
        if let Some(hue) = self.of[lane] {
            return hue;
        }
        let n = LANE_HUES as u16;
        // The first free colour from here round the wheel; if all of them are
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
    pub fn release(&mut self, lane: usize) {
        if let Some(hue) = self.of.get_mut(lane).and_then(Option::take) {
            self.live[hue as usize] = self.live[hue as usize].saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{assign_lanes, parse_log};

    #[test]
    fn the_trunk_gets_the_first_colour() {
        let mut h = Hues::new();
        assert_eq!(h.claim(0), 0);
        assert_eq!(h.claim(0), 0, "a claim is not a rotation");
    }

    #[test]
    fn concurrent_lanes_never_share_a_colour() {
        let mut h = Hues::new();
        let hues: Vec<u16> = (0..LANE_HUES).map(|l| h.claim(l)).collect();
        let mut sorted = hues.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), LANE_HUES, "{hues:?}");
    }

    #[test]
    fn more_live_lanes_than_colours_repeats_rather_than_blanking() {
        let mut h = Hues::new();
        for l in 0..LANE_HUES + 3 {
            let hue = h.claim(l);
            assert!((hue as usize) < LANE_HUES, "lane {l} got hue {hue}");
        }
    }

    #[test]
    fn a_recycled_lane_gets_a_new_colour_and_not_its_predecessors() {
        // The bug this type exists for: lane 1 is reused the moment a branch
        // merges, so colouring by index makes unrelated branches read as one.
        let mut h = Hues::new();
        h.claim(0);
        let first = h.claim(1);
        h.release(1);
        let second = h.claim(1);
        assert_ne!(first, second, "the same column came out the same colour");
    }

    #[test]
    fn a_released_colour_becomes_available_again() {
        let mut h = Hues::new();
        let taken: Vec<u16> = (0..LANE_HUES).map(|l| h.claim(l)).collect();
        for l in 0..LANE_HUES {
            h.release(l);
        }
        // Every slot is free, so the wheel can hand out a full set again.
        let again: Vec<u16> = (0..LANE_HUES).map(|l| h.claim(l + 100)).collect();
        let mut sorted = again.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), LANE_HUES, "{taken:?} then {again:?}");
    }

    #[test]
    fn releasing_a_lane_that_never_claimed_one_is_not_a_panic() {
        let mut h = Hues::new();
        h.release(999);
        h.release(0);
        assert_eq!(h.claim(0), 0);
    }

    /// The merge in `LOG`, as a picture:
    ///
    /// ```text
    ///   m   merge of a and b
    ///   a   on the trunk        b forks off m and rejoins at r
    ///   b   on a branch
    ///   r   root
    /// ```
    const LOG: &str = "\
m\x1fm\x1fa b\x1fA\x1f1\x1fmerge\x1e\
a\x1fa\x1fr\x1fA\x1f1\x1ftrunk\x1e\
b\x1fb\x1fr\x1fA\x1f1\x1fbranch\x1e\
r\x1fr\x1f\x1fA\x1f1\x1froot\x1e";

    fn planned(log: &str) -> (Vec<Commit>, Vec<Draw>) {
        let commits = parse_log(log);
        let rows = assign_lanes(&commits);
        let plan = plan(&commits, &rows);
        (commits, plan)
    }

    #[test]
    fn the_newest_row_has_no_history_above_it_and_a_root_none_below() {
        let (_, p) = planned(LOG);
        let own = |d: &Draw| *d.lines.iter().find(|l| l.lane == d.lane).expect("its own lane");
        assert!(!own(&p[0]).up, "drew a line above the newest commit");
        assert!(own(&p[0]).down, "the trunk stops at the newest commit");
        // `r` is a root: its lane ends at the dot.
        assert!(!own(&p[3]).down, "drew history below a root");
        assert!(own(&p[3]).up);
    }

    #[test]
    fn a_fork_and_the_merge_it_rejoins_are_curve_halves_that_meet() {
        let (_, p) = planned(LOG);
        // `m` forks lane 1 downward for its second parent.
        let out = p[0].curves.iter().find(|c| c.down).expect("a fork out of the merge");
        assert_eq!((out.lane, out.partner), (0, 1));
        // `b` sits in lane 1 and its lane converges on the root below, so `r`
        // carries the other half — upward, from its own lane toward lane 1.
        let back = p[3].curves.iter().find(|c| !c.down).expect("a merge into the root");
        assert_eq!((back.lane, back.partner), (0, 1));
    }

    #[test]
    fn a_curve_carries_the_branchs_colour_and_not_the_trunks() {
        // The whole reason `Curve` has its own `hue`: a branch leaving the
        // trunk is the *branch*, and colouring it by the lane it starts in
        // makes every fork amber.
        let (_, p) = planned(LOG);
        let trunk = p[0].hue;
        let fork = p[0].curves.iter().find(|c| c.down).unwrap();
        assert_ne!(fork.hue, trunk, "the fork took the trunk's colour");
    }

    #[test]
    fn a_merge_says_so_and_an_ordinary_commit_does_not() {
        let (_, p) = planned(LOG);
        assert!(p[0].merge);
        assert!(!p[1].merge && !p[3].merge);
    }

    #[test]
    fn a_rows_width_is_its_own_lanes_and_not_the_repositorys() {
        // The property that gives a commit alone on the trunk the whole window
        // for its subject.
        let (_, p) = planned(LOG);
        assert_eq!(p[1].lanes, 2, "the trunk row has lane 1 passing through it");
        // A long straight history: every row is one lane wide.
        let straight: String = (0..5)
            .map(|i| format!("{i}\x1f{i}\x1f{}\x1fA\x1f1\x1fc{i}\x1e", i + 1))
            .collect();
        let (_, q) = planned(&straight);
        assert!(q.iter().all(|d| d.lanes == 1), "{:?}", q.iter().map(|d| d.lanes).collect::<Vec<_>>());
    }

    #[test]
    fn everything_past_the_cap_shares_one_column_and_one_line() {
        // git/git runs 280 concurrent lanes. Without the collapse this queues
        // 280 identical shapes per row for a gutter nobody can read.
        let mut log = String::from("h\x1fh\x1f");
        let parents: Vec<String> = (0..40).map(|i| format!("p{i}")).collect();
        log.push_str(&parents.join(" "));
        log.push_str("\x1fA\x1f1\x1foctopus\x1e");
        for p in &parents {
            log.push_str(&format!("{p}\x1f{p}\x1f\x1fA\x1f1\x1fparent\x1e"));
        }
        let (_, p) = planned(&log);
        assert!(p[0].capped, "the octopus row hides lanes and did not say so");
        for d in &p {
            assert!(d.lanes <= MAX_LANES, "{} columns", d.lanes);
            assert!(d.lines.iter().all(|l| l.lane as usize <= MAX_LANES));
            assert!(d.curves.iter().all(|c| c.lane as usize <= MAX_LANES));
            // One line per column, however many lanes collapsed onto the last.
            let mut lanes: Vec<u16> = d.lines.iter().map(|l| l.lane).collect();
            let before = lanes.len();
            lanes.sort_unstable();
            lanes.dedup();
            assert_eq!(lanes.len(), before, "two lines share a column");
        }
    }

    #[test]
    fn exactly_the_cap_hides_nothing_and_says_so() {
        // The plausible wrong answer is `lanes == MAX_LANES`. Twelve lanes with
        // a dimmed last column claims there is more history over there.
        let mut log = String::from("h\x1fh\x1f");
        let parents: Vec<String> = (0..MAX_LANES).map(|i| format!("p{i}")).collect();
        log.push_str(&parents.join(" "));
        log.push_str("\x1fA\x1f1\x1foctopus\x1e");
        for p in &parents {
            log.push_str(&format!("{p}\x1f{p}\x1f\x1fA\x1f1\x1fparent\x1e"));
        }
        let (_, p) = planned(&log);
        // Not the octopus row itself: its curves only reach the *midpoint*
        // toward each parent, so it is half as wide as the row below it, where
        // those twelve lanes are full lines. That is the point of measuring
        // each row on its own.
        assert_eq!(p.iter().map(|d| d.lanes).max(), Some(MAX_LANES));
        assert!(p[0].lanes < MAX_LANES, "a row of half-curves measured full width");
        assert!(p.iter().all(|d| !d.capped), "nothing was hidden");
    }

    #[test]
    fn an_empty_history_plans_nothing() {
        assert!(plan(&[], &[]).is_empty());
    }


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

    fn draws(cs: &[Commit]) -> Vec<Draw> {
        plan(cs, &assign_lanes(cs))
    }

    /// The two halves of every curve live in different rows, so the pair of
    /// lanes they aim at has to agree — otherwise they cross the boundary at
    /// different places and the branch visibly tears in half.
    ///
    /// The invariant the whole halves design rests on, and the reason it is
    /// checked here rather than in a client: every client draws the halves it is
    /// given, so if they do not match, all three tear identically.
    fn halves_meet(ds: &[Draw]) {
        for (i, d) in ds.iter().enumerate() {
            for c in &d.curves {
                let (row, want_down) =
                    if c.down { (i + 1, false) } else { (i.wrapping_sub(1), true) };
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
        // and dies without ever getting a column of its own — which is why the
        // far end of the curve is the *other row's dot* and not the lane.
        let cs = [
            commit("a", &["b", "c"]),
            commit("b", &["c"]),
            commit("c", &["d"]),
            commit("d", &[]),
        ];
        halves_meet(&draws(&cs));
    }

    #[test]
    fn consecutive_branches_in_one_lane_get_different_colours() {
        //   a (merge of b, c) … e (merge of f, g): two branches, both of which
        //   live in lane 1, one after the other.
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

    #[test]
    fn every_curve_in_a_real_repository_has_its_other_half() {
        // The synthetic cases above are three commits wide. This one is the
        // shape a merge-heavy history actually has.
        let log: String = (0..60)
            .map(|i| match i % 5 {
                0 => format!("c{i}\x1fc{i}\x1fc{}  c{}\x1fA\x1f1\x1fmerge\x1e", i + 1, i + 3),
                _ => format!("c{i}\x1fc{i}\x1fc{}\x1fA\x1f1\x1fone\x1e", i + 1),
            })
            .collect();
        let commits = parse_log(&log);
        halves_meet(&plan(&commits, &assign_lanes(&commits)));
    }

    #[test]
    fn the_lane_count_is_the_uncapped_truth() {
        // A merge whose second parent sits in a far column: the count is where
        // the topology actually reaches, not where a frontend stops drawing.
        let log = "\
a\x1fa\x1fb c\x1fA\x1f1\x1fmerge\x1e\
b\x1fb\x1fd\x1fA\x1f1\x1fone\x1e\
c\x1fc\x1fd\x1fA\x1f1\x1ftwo\x1e\
d\x1fd\x1f\x1fA\x1f1\x1froot\x1e";
        let commits = parse_log(log);
        let rows = assign_lanes(&commits);
        assert_eq!(lane_count(&rows), 2);
        assert!(lane_count(&[]) >= 1, "an empty log still has a column");
    }
}

// -------------------------------------------------------------- the row plan

/// A straight lane, in halves.
///
/// `up` runs from the row's top edge to the dot line, `down` from the dot line
/// to the bottom edge. A half is missing when a curve has taken it over, or when
/// there is simply no history that way — nothing exists above the newest row,
/// and a root commit's lane stops at its dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    pub lane: u16,
    pub hue: u16,
    pub up: bool,
    pub down: bool,
}

/// Half an S: it touches `lane` on the dot line and crosses the row boundary
/// halfway to `partner`, where the neighbouring row picks it up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Curve {
    pub lane: u16,
    pub partner: u16,
    /// Whose colour it carries: always the branch, never the trunk it leaves or
    /// joins. For a lane collapsing onto a dot that is the lane's own hue; for
    /// one born out of a dot it is the far end's.
    pub hue: u16,
    /// Leaving the dot line downward, or reaching up out of it.
    pub down: bool,
}

/// One row, flattened to what drawing needs.
///
/// Computed once at load, so nothing on a render path touches the commit list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draw {
    pub lane: u16,
    pub hue: u16,
    /// Two or more parents. Clients draw it heavier, so a join is findable while
    /// scrolling.
    pub merge: bool,
    pub lines: Vec<Line>,
    pub curves: Vec<Curve>,
    /// How many lane columns *this row* needs.
    ///
    /// Per row and not per repository: a commit alone on the trunk gets nearly
    /// the whole window for its subject, and only rows where the graph really is
    /// wider push their text across. Whole lanes, so the text steps on the lane
    /// grid — ragged by a column reads as "the graph is wider here", ragged by
    /// three pixels just reads as broken.
    ///
    /// A client that cannot scroll sideways — a terminal — takes the maximum
    /// over every row instead, and says so.
    pub lanes: usize,
    /// Whether this row actually *has* lanes past [`MAX_LANES`].
    ///
    /// Its own flag rather than `lanes == MAX_LANES`, which is the plausible
    /// wrong answer: a repository with exactly twelve lanes hides nothing, and
    /// dimming its last column would say there is more history over there when
    /// there is not.
    pub capped: bool,
}

/// A lane index every client can draw: everything past the cap shares the last
/// column.
///
/// Clamped to `MAX_LANES - 1` and not to `MAX_LANES`, so an index out of this is
/// always a column that exists. Which rows are *hiding* something is
/// [`Draw::capped`], because a sentinel index and a fact about the row are two
/// different things and using one as the other means twelve lanes look like
/// thirteen.
fn cap(lane: usize) -> u16 {
    lane.min(MAX_LANES - 1) as u16
}

/// What to draw for every row, given the topology and nothing else.
///
/// Walks the history once, in `git log` order. The order of the hue claims and
/// releases is the one [`Hues`] documents, and getting it wrong wastes a colour
/// per merge until a busy repository runs out.
///
/// It reads the rows either side of each one, because half of a curve lives next
/// door: a lane born at the fork above arrives *on a curve* and so has no top
/// half, and one ending at the merge below leaves on a curve and so has no
/// bottom half.
pub fn plan(commits: &[Commit], rows: &[GraphRow]) -> Vec<Draw> {
    let mut hues = Hues::new();
    let mut out = Vec::with_capacity(rows.len());

    for (i, (c, r)) in commits.iter().zip(rows).enumerate() {
        let above = i.checked_sub(1).map(|j| &rows[j]);
        let below = rows.get(i + 1);
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
            curves.push(Curve {
                lane: cap(r.lane),
                partner: cap(end),
                hue: hues.claim(m),
                down: false,
            });
        }

        for &lane in r.through.iter().chain(std::iter::once(&r.lane)) {
            let own = lane == r.lane;
            let (up, down) = (arrives(lane), departs(lane));
            let line = Line {
                lane: cap(lane),
                hue: hues.claim(lane),
                up: up.is_none() && !(own && i == 0),
                down: down.is_none() && !(own && c.parents.is_empty()),
            };
            // Everything past the cap shares a column: share the line too, or
            // git/git would queue 280 identical shapes per row.
            match lines.last_mut().filter(|l| l.lane == line.lane) {
                Some(prev) => {
                    prev.up |= line.up;
                    prev.down |= line.down;
                }
                None => lines.push(line),
            }
            for (end, down) in [(up, false), (down, true)] {
                if let Some(partner) = end {
                    curves.push(Curve { lane: cap(lane), partner: cap(partner), hue: line.hue, down });
                }
            }
        }

        // Branches that end here give their colour back, and a root gives up its
        // own lane, before the forks below claim theirs.
        for &m in &r.merges {
            hues.release(m);
        }
        if c.parents.is_empty() {
            hues.release(r.lane);
        }

        // Lanes born out of this dot: the head half of their curve.
        for &f in &r.forks {
            let end = departs(f).unwrap_or(f);
            curves.push(Curve {
                lane: cap(r.lane),
                partner: cap(end),
                hue: hues.claim(f),
                down: true,
            });
        }

        let lanes = width(cap(r.lane), &lines, &curves);
        let capped = r
            .through
            .iter()
            .chain(&r.merges)
            .chain(&r.forks)
            .chain(std::iter::once(&r.lane))
            .any(|l| *l >= MAX_LANES);
        out.push(Draw {
            lane: cap(r.lane),
            hue,
            merge: c.parents.len() > 1,
            lines,
            curves,
            lanes,
            capped,
        });
    }
    out
}

/// How many lane columns a row's shapes reach into.
///
/// A curve's half only travels to the *midpoint* between its two lanes, so it
/// often stops short of its partner's column entirely — and rounding that up to
/// the partner is what makes every merge row as wide as the widest merge in the
/// repository. Integer columns, because that is the grid the text lines up on.
fn width(lane: u16, lines: &[Line], curves: &[Curve]) -> usize {
    let mut last = lane as usize;
    for l in lines {
        last = last.max(l.lane as usize);
    }
    for c in curves {
        // `(a + b) / 2` rounded *up*: a half that ends mid-column still needs
        // that column.
        let mid = (c.lane as usize + c.partner as usize + 1) / 2;
        last = last.max(c.lane as usize).max(mid);
    }
    last + 1
}
