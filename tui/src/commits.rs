//! The commit list, and the graph gutter drawn in box-drawing characters.
//!
//! Topology is [`plait_core::assign_lanes`] and colour is
//! [`plait_core::graph::Hues`], both untouched — this file decides only what a
//! lane *looks like* in a cell grid, which is the same division
//! `shell/src/graph.rs` has with its Bézier curves. Both frontends therefore
//! agree about which branch is amber and where the overflow begins.
//!
//! # One row per commit, and what that costs
//!
//! The window has real curves: a branch changing lanes is an S spanning a whole
//! row, painted in halves that meet on the boundary. A cell grid has no halves —
//! a row is one row of characters — so a merge and a fork are drawn *on the
//! commit's own row* as a horizontal run with a corner at the far end, which is
//! what `git log --graph` and lazygit both do.
//!
//! The honest cost: a merge and a fork in the same row of the same lane collapse
//! into one glyph, and a lane that both arrives and departs on a curve is drawn
//! as a crossing rather than as a swerve. Both are readable and neither is a lie
//! about what merged into what.
//!
//! # Which way is down
//!
//! `git log` is newest-first, so a row below is *older*. A lane converging on a
//! commit ([`GraphRow::merges`]) came from above, and a lane forked out of it
//! ([`GraphRow::forks`]) continues below. That is why a merge's corner points up
//! and a fork's points down, and getting it backwards draws a history where
//! branches merge into their own children.
//!
//! # Two columns per lane
//!
//! One for the glyph, one for the connector between it and the next lane. `git
//! --graph`'s spacing, and the reason a diagonal is expressible at all: with one
//! column per lane there is nowhere to put the `─` that says two lanes are
//! joined.

use crate::screen::{Ink, Pen, Screen};
use plait_core::graph::{lane_count, Hues, MAX_LANES};
use plait_core::host::Host;
use plait_core::theme::{Rgb, Theme};
use plait_core::{assign_lanes, initials, Commit, GraphRow};

/// Columns per lane: the glyph, then the gap a connector runs through.
const LANE_W: usize = 2;

/// The sha column, and a space after it. Fixed, unlike the graph: the eye scans
/// it vertically, so it has to be a column and not a ragged edge.
///
/// Clipped from the *right*, unlike a line number: an abbreviated sha is a
/// prefix and git resolves a prefix, so the last characters are the ones to
/// lose. `put_right` here would produce something that looks like a sha and
/// names no commit.
const SHA_W: usize = 8;
/// Two letters and a space.
const WHO_W: usize = 3;

/// What a lane looks like in one cell.
///
/// Named rather than inlined because every one of them is a decision the eye
/// reads, and because the terminal set has to survive a font without box
/// drawing: an extension replacing this struct is how a plain-ASCII fallback
/// happens, which is the only way it can happen without a second `render`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyphs {
    /// A lane passing straight through.
    pub through: char,
    /// An ordinary commit, and one with two or more parents. The merge is
    /// heavier so a join is findable while scrolling.
    pub dot: char,
    pub merge_dot: char,
    /// A lane crossing a horizontal connector.
    pub cross: char,
    /// The horizontal connector itself.
    pub run: char,
    /// A lane arriving from above and turning toward the dot: to its left, to
    /// its right.
    pub up_left: char,
    pub up_right: char,
    /// A lane leaving downward, likewise.
    pub down_left: char,
    pub down_right: char,
}

impl Default for Glyphs {
    fn default() -> Self {
        Self::rounded()
    }
}

impl Glyphs {
    /// The shipped set. Rounded corners, because a square `┘` beside a `│` reads
    /// as a table and the graph is not one.
    pub fn rounded() -> Self {
        Self {
            through: '│',
            dot: '●',
            merge_dot: '◉',
            cross: '┼',
            run: '─',
            up_left: '╰',
            up_right: '╯',
            down_left: '╭',
            down_right: '╮',
        }
    }

    /// Nothing outside ASCII, for a terminal or a font that cannot draw the
    /// rest. `git log --graph`'s own alphabet.
    pub fn ascii() -> Self {
        Self {
            through: '|',
            dot: '*',
            merge_dot: '@',
            cross: '+',
            run: '-',
            up_left: '\\',
            up_right: '/',
            down_left: '/',
            down_right: '\\',
        }
    }
}

/// One commit, flattened to what drawing needs: resolved once at load, never per
/// frame.
struct Draw {
    lane: u16,
    hue: u16,
    merge: bool,
    /// `(lane, hue)` for every lane passing straight through this row.
    through: Vec<(u16, u16)>,
    /// `(lane, hue)` for lanes converging on this commit from above.
    merges: Vec<(u16, u16)>,
    /// `(lane, hue)` for lanes forked out of it, continuing below.
    forks: Vec<(u16, u16)>,
    /// How many lanes this row's own graph needs, capped. Per row, so a commit
    /// alone on the trunk gets nearly the whole terminal for its subject
    /// instead of starting behind the widest merge in the repository.
    lanes: usize,
    /// Whether this row actually *has* lanes past the cap.
    ///
    /// Not `lanes == MAX_LANES`, which is the plausible wrong answer: a
    /// repository with exactly twelve lanes hides nothing, and dimming its last
    /// column would say there is more history over there when there is not.
    capped: bool,
    initials: String,
}

/// The commit list.
///
/// Holds the data, the viewport and the cursor; knows nothing about keys. Every
/// method is a command, exactly as in [`crate::diff::Diff`] and for the same
/// reason.
pub struct Commits {
    commits: Vec<Commit>,
    draws: Vec<Draw>,
    glyphs: Glyphs,
    /// The honest lane count, uncapped, for the status line.
    lanes: usize,
    /// Widest gutter of any row, so the subject column starts in the same place
    /// down the whole list.
    ///
    /// Per-row widths are what the window does, because it can scroll a
    /// container wider than itself; a terminal cannot, and a subject that starts
    /// in a different column on every row is a list the eye cannot scan. So the
    /// gutter is one width and it is this one.
    gutter: usize,
    height: usize,
    cols: usize,
    top: usize,
    cursor: usize,
}

impl Commits {
    /// No `Host`, deliberately: nothing here is resolved at load that a theme
    /// could change. A lane's colour is `theme.lane(hue)` and an author's is a
    /// hash and an index, both read on the frame that draws them — so editing
    /// `plait.toml` recolours the list without rebuilding it. The shell resolves
    /// its author colours once at construction and does not.
    pub fn new(commits: Vec<Commit>) -> Self {
        Self::with_glyphs(commits, Glyphs::default())
    }

    /// The constructor an extension uses to draw the gutter in its own alphabet
    /// — `Glyphs::ascii()`, or a Nerd Font set.
    pub fn with_glyphs(commits: Vec<Commit>, glyphs: Glyphs) -> Self {
        let rows = assign_lanes(&commits);
        let lanes = lane_count(&rows);
        let draws = draws(&commits, &rows);
        // The widest *row*, not the widest lane index: a row's gutter is only as
        // wide as its own lanes, and the trunk-only rows of a busy repository
        // are the majority.
        let gutter = draws.iter().map(|d| d.lanes * LANE_W).max().unwrap_or(0);
        Self {
            commits,
            draws,
            glyphs,
            lanes,
            gutter,
            height: 0,
            cols: 0,
            top: 0,
            cursor: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.commits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn top(&self) -> usize {
        self.top
    }

    /// The commit under the cursor, for whatever opens a diff from it.
    pub fn current(&self) -> Option<&Commit> {
        self.commits.get(self.cursor)
    }

    pub fn resize(&mut self, cols: usize, height: usize) {
        self.cols = cols;
        self.height = height;
        self.follow();
    }

    pub fn move_by(&mut self, by: isize) {
        let last = self.commits.len().saturating_sub(1);
        self.cursor = match by.is_negative() {
            true => self.cursor.saturating_sub(by.unsigned_abs()),
            false => (self.cursor + by as usize).min(last),
        };
        self.follow();
    }

    pub fn down(&mut self) {
        self.move_by(1);
    }

    pub fn up(&mut self) {
        self.move_by(-1);
    }

    pub fn page(&mut self, pages: isize) {
        let step = self.height.saturating_sub(1).max(1) as isize;
        self.move_by(pages * step);
    }

    pub fn to_top(&mut self) {
        self.cursor = 0;
        self.follow();
    }

    pub fn to_bottom(&mut self) {
        self.cursor = self.commits.len().saturating_sub(1);
        self.follow();
    }

    /// Puts a saved row on screen, for a session restored across a restart.
    pub fn go_to(&mut self, row: usize) {
        self.cursor = row.min(self.commits.len().saturating_sub(1));
        self.follow();
    }

    fn follow(&mut self) {
        if self.height == 0 {
            self.top = self.cursor;
            return;
        }
        let pad = match self.height > 2 * 3 + 1 {
            true => 3,
            false => 0,
        };
        self.top = self.top.min(self.commits.len().saturating_sub(1));
        if self.cursor < self.top + pad {
            self.top = self.cursor.saturating_sub(pad);
        }
        if self.cursor + pad > self.top + self.height.saturating_sub(1) {
            self.top = (self.cursor + pad + 1).saturating_sub(self.height);
        }
        self.top = self.top.min(self.commits.len().saturating_sub(self.height));
    }

    /// Draws the visible rows into `screen`, starting at row `y`.
    pub fn paint(&self, screen: &mut Screen, y: usize, host: &Host) {
        let theme = &host.theme;
        let blank = Ink::new(theme.chrome.dim, theme.chrome.bg);
        for i in 0..self.height {
            let row = y + i;
            let Some(index) = Some(self.top + i).filter(|n| *n < self.commits.len()) else {
                screen.row(row).wash(blank);
                continue;
            };
            let bg = match index == self.cursor {
                true => theme.chrome.selection_bg,
                false => theme.chrome.bg,
            };
            let mut pen = screen.row(row);
            self.row(&mut pen, index, bg, theme);
        }
    }

    /// lazygit's order — sha, author, graph, subject — because the graph is the
    /// column that changes width and putting it last would move the subject.
    fn row(&self, pen: &mut Pen, index: usize, bg: Rgb, theme: &Theme) {
        let c = &self.commits[index];
        let d = &self.draws[index];
        // A sub-pen per column, so neither can push the graph sideways however
        // long the sha or the name is — a fixed column that a long value moves
        // is not a column.
        let dim = Ink::new(theme.chrome.dim, bg);
        pen.take(SHA_W - 1).put(&c.short, dim);
        pen.put(" ", dim);
        pen.take(WHO_W - 1).put(&d.initials, Ink::new(theme.author(&c.author), bg));
        pen.put(" ", dim);

        let after = pen.col() + self.gutter;
        self.gutter(pen, d, bg, theme);
        // One width for the whole list, so the subject starts in the same column
        // on every row even though each row's graph is only as wide as its own.
        pen.seek(after);
        pen.put(" ", Ink::new(theme.chrome.fg, bg));
        pen.put(&c.subject, Ink::new(theme.chrome.fg, bg));
        pen.wash(Ink::new(theme.chrome.fg, bg));
    }

    /// One row of the graph.
    ///
    /// Painted in one left-to-right pass over the *cells*, not over the lanes,
    /// because a connector occupies cells between two lanes and a lane may be
    /// crossed by one. Each cell asks what belongs in it and the answer is
    /// local, which is what keeps this a `for` over at most 24 columns rather
    /// than a sort of segments.
    fn gutter(&self, pen: &mut Pen, d: &Draw, bg: Rgb, theme: &Theme) {
        let g = &self.glyphs;
        let dot = (d.lane as usize).min(MAX_LANES - 1);
        // Where the horizontal connectors have to reach: the furthest lane that
        // joins this dot, on each side.
        let reach = |ends: &[(u16, u16)]| {
            ends.iter()
                .map(|(l, _)| (*l as usize).min(MAX_LANES - 1))
                .fold((dot, dot), |(lo, hi), l| (lo.min(l), hi.max(l)))
        };
        let (m_lo, m_hi) = reach(&d.merges);
        let (f_lo, f_hi) = reach(&d.forks);
        let (lo, hi) = (m_lo.min(f_lo), m_hi.max(f_hi));

        let hue_of = |lane: usize| -> Option<u16> {
            d.through
                .iter()
                .chain(&d.merges)
                .chain(&d.forks)
                .find(|(l, _)| (*l as usize).min(MAX_LANES - 1) == lane)
                .map(|(_, h)| *h)
        };

        for cell in 0..d.lanes * LANE_W {
            let lane = cell / LANE_W;
            let on_lane = cell % LANE_W == 0;
            let overflow = d.capped && lane == MAX_LANES - 1;

            let (ch, hue) = if on_lane && lane == dot {
                (if d.merge { g.merge_dot } else { g.dot }, Some(d.hue))
            } else if on_lane {
                match (
                    d.merges.iter().find(|(l, _)| (*l as usize).min(MAX_LANES - 1) == lane),
                    d.forks.iter().find(|(l, _)| (*l as usize).min(MAX_LANES - 1) == lane),
                    d.through.iter().find(|(l, _)| (*l as usize).min(MAX_LANES - 1) == lane),
                ) {
                    // A lane that arrives from above and turns toward the dot.
                    // Its corner points *up* because the row above is newer.
                    (Some((_, h)), _, _) => {
                        (if lane < dot { g.up_left } else { g.up_right }, Some(*h))
                    }
                    // A lane born here, continuing into older history below.
                    (None, Some((_, h)), _) => {
                        (if lane < dot { g.down_left } else { g.down_right }, Some(*h))
                    }
                    // A lane minding its own business — crossed by a connector
                    // if one passes over it.
                    (None, None, Some((_, h))) => {
                        let crossed = lane > lo && lane < hi;
                        (if crossed { g.cross } else { g.through }, Some(*h))
                    }
                    (None, None, None) => (' ', None),
                }
            } else {
                // Between two lanes: connector, or nothing.
                let between = cell > lo * LANE_W && cell < hi * LANE_W;
                (if between { g.run } else { ' ' }, hue_of(lane).or(Some(d.hue)))
            };

            // Past the cap every lane shares one column, so they share one grey
            // and stop pretending to be individuals.
            let fg = match (overflow, hue) {
                (true, _) => theme.lane_overflow,
                (false, Some(h)) => theme.lane(h as usize),
                (false, None) => theme.chrome.bg,
            };
            pen.put(&ch.to_string(), Ink::new(fg, bg));
        }
    }

    /// One line describing the list, for whatever draws a status bar. The lane
    /// count is the uncapped one: "280 lanes" is worth knowing when twelve are
    /// drawn.
    pub fn status(&self) -> String {
        let mut out = format!(
            "{}/{} · {} lanes",
            (self.cursor + 1).min(self.commits.len()),
            self.commits.len(),
            self.lanes,
        );
        if self.lanes > MAX_LANES {
            out.push_str(&format!(" · {MAX_LANES} drawn"));
        }
        out
    }
}

/// Walks the history once, resolving every row's lanes to hues.
///
/// The order of the claims and releases is not incidental — see
/// [`plait_core::graph::Hues`], which documents it, because getting it wrong
/// wastes a colour per merge and a busy repository then runs out.
fn draws(commits: &[Commit], rows: &[GraphRow]) -> Vec<Draw> {
    let mut hues = Hues::new();
    let mut out = Vec::with_capacity(rows.len());
    for (c, r) in commits.iter().zip(rows) {
        let hue = hues.claim(r.lane);
        let merges: Vec<(u16, u16)> =
            r.merges.iter().map(|&m| (m as u16, hues.claim(m))).collect();
        let through: Vec<(u16, u16)> =
            r.through.iter().map(|&t| (t as u16, hues.claim(t))).collect();

        // Branches that end here give their colour back, and a root gives up its
        // own lane, *before* the forks below claim theirs.
        for &m in &r.merges {
            hues.release(m);
        }
        if c.parents.is_empty() {
            hues.release(r.lane);
        }
        let forks: Vec<(u16, u16)> = r.forks.iter().map(|&f| (f as u16, hues.claim(f))).collect();

        let widest = r
            .through
            .iter()
            .chain(&r.merges)
            .chain(&r.forks)
            .copied()
            .chain(std::iter::once(r.lane))
            .max()
            .unwrap_or(0);

        out.push(Draw {
            lane: r.lane as u16,
            hue,
            merge: c.parents.len() > 1,
            through,
            merges,
            forks,
            lanes: widest.min(MAX_LANES - 1) + 1,
            capped: widest >= MAX_LANES,
            initials: initials(&c.author),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use plait_core::parse_log;

    /// A merge of two branches off one root, newest first:
    ///
    /// ```text
    ///   m   merge of a and b
    ///   a   on the trunk
    ///   b   on a branch
    ///   r   root
    /// ```
    const LOG: &str = "\
m\x1fmmmmmmm\x1fa b\x1fAda Lovelace\x1f1700000400\x1fMerge branch\x1e\
a\x1faaaaaaa\x1fr\x1fAda Lovelace\x1f1700000300\x1fOn the trunk\x1e\
b\x1fbbbbbbb\x1fr\x1fGrace Hopper\x1f1700000200\x1fOn a branch\x1e\
r\x1frrrrrrr\x1f\x1fAda Lovelace\x1f1700000100\x1fRoot\x1e";

    fn view(log: &str, cols: usize, height: usize) -> (Commits, Host) {
        let host = Host::new();
        let mut c = Commits::new(parse_log(log));
        c.resize(cols, height);
        (c, host)
    }

    fn painted(c: &Commits, host: &Host) -> Vec<String> {
        let mut screen = Screen::new(c.cols, c.height + 2);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        c.paint(&mut screen, 0, host);
        (0..c.height).map(|y| screen.row_text(y)).collect()
    }

    #[test]
    fn a_row_is_sha_initials_graph_then_subject() {
        let (c, host) = view(LOG, 60, 4);
        let rows = painted(&c, &host);
        assert!(rows[0].starts_with("mmmmmmm AL "), "{:?}", rows[0]);
        assert!(rows[0].ends_with("Merge branch"), "{:?}", rows[0]);
        assert!(rows[2].contains("GH"), "the author column is per commit: {:?}", rows[2]);
    }

    #[test]
    fn the_subject_starts_in_the_same_column_on_every_row() {
        // A terminal cannot scroll a container wider than itself, so a per-row
        // gutter would put the subject in a different column on every line.
        let (c, host) = view(LOG, 60, 4);
        let rows = painted(&c, &host);
        // Display columns, not bytes: box drawing is three bytes a glyph, so a
        // `find` compares the wrong thing and passes or fails by the graph's
        // shape rather than by its width.
        let at = |row: &String, word: &str| {
            crate::screen::width(&row[..row.find(word).expect(word)])
        };
        let first = at(&rows[0], "Merge branch");
        assert_eq!(first, 16, "{:?}", rows[0]);
        assert_eq!(at(&rows[1], "On the trunk"), first);
        assert_eq!(at(&rows[2], "On a branch"), first);
        assert_eq!(at(&rows[3], "Root"), first);
    }

    #[test]
    fn a_merge_commit_is_drawn_heavier_than_an_ordinary_one() {
        let (c, host) = view(LOG, 60, 4);
        let rows = painted(&c, &host);
        assert!(rows[0].contains('◉'), "{:?}", rows[0]);
        assert!(rows[1].contains('●') && !rows[1].contains('◉'), "{:?}", rows[1]);
    }

    #[test]
    fn a_forked_lane_points_down_and_a_converging_one_points_up() {
        // The orientation trap: `git log` is newest-first, so a row below is
        // older. Backwards, this draws branches merging into their children.
        let (c, host) = view(LOG, 60, 4);
        let rows = painted(&c, &host);
        // The merge forks lane 1 for its second parent: it continues downward.
        assert!(rows[0].contains('╮'), "the fork did not point down: {:?}", rows[0]);
        // The root has both branches converging on it from above.
        assert!(rows[3].contains('╯'), "the merge did not point up: {:?}", rows[3]);
    }

    #[test]
    fn a_lane_passing_through_is_a_vertical_and_the_connector_crosses_it() {
        // A fork that has to reach *over* an occupied lane. `t` puts `b` in lane
        // 1, so `m`'s second parent lands in lane 2 and the run from `m`'s dot
        // crosses a lane that has nothing to do with it.
        let log = "\
t\x1ft\x1fm b\x1fA\x1f1\x1ftip\x1e\
m\x1fm\x1fa c\x1fA\x1f1\x1fmerge\x1e\
b\x1fb\x1fr\x1fA\x1f1\x1fother\x1e\
a\x1fa\x1fr\x1fA\x1f1\x1ftrunk\x1e\
c\x1fc\x1fr\x1fA\x1f1\x1fthird\x1e\
r\x1fr\x1f\x1fA\x1f1\x1froot\x1e";
        let (c, host) = view(log, 70, 6);
        let rows = painted(&c, &host);
        assert!(rows[1].contains('─'), "no connector: {:?}", rows[1]);
        assert!(rows[1].contains('┼'), "the connector broke a lane: {:?}", rows[1]);
    }

    #[test]
    fn a_branch_keeps_its_colour_and_a_recycled_lane_does_not() {
        let (c, host) = view(LOG, 60, 4);
        let mut screen = Screen::new(60, 4);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        c.paint(&mut screen, 0, &host);
        // The trunk is the first colour the wheel hands out.
        let dot = (0..60).find(|x| screen.char_at(*x, 1) == Some('●')).unwrap();
        assert_eq!(screen.ink(dot, 1).unwrap().fg, host.theme.lane(0));
        // The branch is not.
        let branch = (0..60).find(|x| screen.char_at(*x, 2) == Some('●')).unwrap();
        assert_ne!(screen.ink(branch, 2).unwrap().fg, host.theme.lane(0));
    }

    #[test]
    fn lanes_past_the_cap_share_one_dim_column() {
        // git/git runs 280 concurrent lanes; a gutter that wide pushes the
        // subject off the screen entirely.
        let mut log = String::from("h\x1fh\x1f");
        let parents: Vec<String> = (0..30).map(|i| format!("p{i}")).collect();
        log.push_str(&parents.join(" "));
        log.push_str("\x1fA\x1f1\x1foctopus\x1e");
        for p in &parents {
            log.push_str(&format!("{p}\x1f{p}\x1f\x1fA\x1f1\x1fparent\x1e"));
        }
        let (c, host) = view(&log, 120, 8);
        assert!(c.lanes > MAX_LANES, "{} lanes", c.lanes);
        assert!(c.status().contains("drawn"), "{}", c.status());
        assert!(c.gutter <= MAX_LANES * LANE_W, "the gutter blew past the cap");
        let mut screen = Screen::new(120, 8);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        c.paint(&mut screen, 0, &host);
        let last = SHA_W + WHO_W + (MAX_LANES - 1) * LANE_W;
        assert_eq!(screen.ink(last, 0).unwrap().fg, host.theme.lane_overflow);
    }

    #[test]
    fn exactly_the_cap_is_not_dimmed() {
        // The plausible wrong answer is `lanes == MAX_LANES`. A repository with
        // exactly twelve lanes hides nothing, and dimming its last column says
        // there is more history over there when there is not.
        let mut log = String::from("h\x1fh\x1f");
        let parents: Vec<String> = (0..MAX_LANES).map(|i| format!("p{i}")).collect();
        log.push_str(&parents.join(" "));
        log.push_str("\x1fA\x1f1\x1foctopus\x1e");
        for p in &parents {
            log.push_str(&format!("{p}\x1f{p}\x1f\x1fA\x1f1\x1fparent\x1e"));
        }
        let (c, host) = view(&log, 120, 4);
        assert_eq!(c.lanes, MAX_LANES);
        assert!(!c.status().contains("drawn"), "{}", c.status());
        let mut screen = Screen::new(120, 4);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        c.paint(&mut screen, 0, &host);
        let last = SHA_W + WHO_W + (MAX_LANES - 1) * LANE_W;
        assert_ne!(screen.ink(last, 0).unwrap().fg, host.theme.lane_overflow);
    }

    #[test]
    fn a_replaceable_glyph_set_reaches_the_screen() {
        // Rule 1 for the one thing in here that is purely appearance: an
        // extension swaps the alphabet without touching `paint`.
        let host = Host::new();
        let mut c = Commits::with_glyphs(parse_log(LOG), Glyphs::ascii());
        c.resize(60, 4);
        let rows = painted(&c, &host);
        assert!(rows[0].contains('@'), "{:?}", rows[0]);
        assert!(rows[1].contains('*'), "{:?}", rows[1]);
        assert!(!rows.iter().any(|r| r.contains('│')), "box drawing survived: {rows:?}");
    }

    #[test]
    fn the_selected_row_is_a_bar_across_the_whole_width() {
        let (mut c, host) = view(LOG, 60, 4);
        c.down();
        let mut screen = Screen::new(60, 4);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        c.paint(&mut screen, 0, &host);
        let bar = host.theme.chrome.selection_bg;
        assert_eq!(screen.ink(0, 1).unwrap().bg, bar);
        assert_eq!(screen.ink(59, 1).unwrap().bg, bar);
        assert_ne!(screen.ink(0, 0).unwrap().bg, bar);
    }

    #[test]
    fn the_cursor_and_the_viewport_behave_as_the_diff_views_do() {
        let log: String = (0..100)
            .map(|i| format!("{i:040}\x1f{i:07}\x1f\x1fA\x1f1\x1fc{i}\x1e"))
            .collect();
        let (mut c, _) = view(&log, 60, 10);
        c.up();
        assert_eq!(c.cursor(), 0);
        c.page(1);
        assert_eq!(c.cursor(), 9);
        c.to_bottom();
        assert_eq!(c.cursor(), 99);
        assert_eq!(c.top(), 90);
        c.go_to(9999);
        assert_eq!(c.cursor(), 99);
        assert_eq!(c.current().map(|x| x.subject.as_str()), Some("c99"));
    }

    #[test]
    fn an_empty_log_draws_nothing_and_panics_nowhere() {
        let (mut c, host) = view("", 40, 6);
        assert!(c.is_empty());
        c.down();
        c.page(-1);
        c.to_bottom();
        assert_eq!(c.cursor(), 0);
        assert_eq!(c.current(), None);
        let rows = painted(&c, &host);
        assert!(rows.iter().all(|r| r.is_empty()), "{rows:?}");
        assert_eq!(c.status(), "0/0 · 1 lanes");
    }

    #[test]
    fn a_narrow_terminal_clips_the_subject_rather_than_the_graph() {
        // The graph is the part that cannot be reconstructed by reading; a
        // truncated subject still says what it is.
        let (c, host) = view(LOG, 20, 4);
        let rows = painted(&c, &host);
        assert!(rows[0].contains('◉'), "the graph was clipped first: {:?}", rows[0]);
    }
}
