//! The commit list, with its graph resolved once.
//!
//! What [`Doc`](crate::rows::Doc) is to a diff. The topology, the branch
//! colours and the shape of every row all come from
//! [`gitten_core::graph`](gitten_core::graph) — the same three calls the window
//! and the terminal make — and the only thing this adds is *holding* them, which
//! is the point: a scroll is a request, and a request that re-walked 82,000
//! commits to answer "rows 400 to 800" would be 13 ms of work per page for an
//! answer that cannot change.

use gitten_core::graph::{lane_count, plan, Draw};
use gitten_core::{assign_lanes, Commit};

pub struct Log {
    pub commits: Vec<Commit>,
    /// One per commit, in the same order.
    pub plan: Vec<Draw>,
    /// Widest gutter any row needs, capped.
    ///
    /// The whole list gets this one width, unlike the GPUI client which gives
    /// each row its own. Not a disagreement about taste — a disagreement about
    /// what a row *is*: the window can scroll a container wider than itself, so
    /// a wide merge row pushes only its own subject across. A browser row is a
    /// fixed-width line, so a per-row gutter starts the subject in a different
    /// column on every line and the eye has nothing to scan down. The terminal
    /// reaches the same answer for the same reason.
    pub lanes: usize,
    /// How many lanes the history actually uses, uncapped — the number to
    /// *report*, against the number drawn.
    pub concurrent: usize,
}

impl Log {
    pub fn build(commits: Vec<Commit>) -> Self {
        let rows = assign_lanes(&commits);
        let concurrent = lane_count(&rows);
        let plan = plan(&commits, &rows);
        let lanes = plan.iter().map(|d| d.lanes).max().unwrap_or(1);
        Self {
            commits,
            plan,
            lanes,
            concurrent,
        }
    }

    pub fn len(&self) -> usize {
        self.commits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitten_core::graph::MAX_LANES;
    use gitten_core::parse_log;

    const LOG: &str = "\
m\x1fmmmmmmm\x1fa b\x1fAda Lovelace\x1f1700000400\x1fMerge branch\x1e\
a\x1faaaaaaa\x1fr\x1fAda Lovelace\x1f1700000300\x1fOn the trunk\x1e\
b\x1fbbbbbbb\x1fr\x1fGrace Hopper\x1f1700000200\x1fOn a branch\x1e\
r\x1frrrrrrr\x1f\x1fAda Lovelace\x1f1700000100\x1fRoot\x1e";

    #[test]
    fn a_log_carries_a_plan_per_commit() {
        let log = Log::build(parse_log(LOG));
        assert_eq!(log.len(), 4);
        assert_eq!(log.plan.len(), 4);
        assert!(log.plan[0].merge, "the merge did not say so");
        assert_eq!(log.lanes, 2);
        assert_eq!(log.concurrent, 2);
    }

    #[test]
    fn the_gutter_is_capped_and_the_report_is_not() {
        // git/git runs 280 concurrent lanes. The client draws twelve and says
        // how many there really were.
        let mut raw = String::from("h\x1fh\x1f");
        let parents: Vec<String> = (0..40).map(|i| format!("p{i}")).collect();
        raw.push_str(&parents.join(" "));
        raw.push_str("\x1fA\x1f1\x1foctopus\x1e");
        for p in &parents {
            raw.push_str(&format!("{p}\x1f{p}\x1f\x1fA\x1f1\x1fparent\x1e"));
        }
        let log = Log::build(parse_log(&raw));
        assert_eq!(log.lanes, MAX_LANES);
        assert!(log.concurrent > MAX_LANES, "{}", log.concurrent);
        assert!(log.plan[0].capped, "the row hiding lanes did not say so");
    }

    #[test]
    fn an_empty_log_is_one_lane_wide_and_not_zero() {
        // A gutter of zero columns makes the subject start at the left edge on
        // an empty list and jump when the first row arrives.
        let log = Log::build(Vec::new());
        assert!(log.is_empty());
        assert_eq!(log.lanes, 1);
        assert_eq!(log.concurrent, 1);
    }
}
