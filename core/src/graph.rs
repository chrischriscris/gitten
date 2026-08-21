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
//! Both were written in the GPUI shell first and both are pure functions of the
//! topology, so a terminal gutter drawn in box-drawing characters and a canvas
//! drawn in Bézier curves agree about which branch is amber.

use crate::GraphRow;

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
