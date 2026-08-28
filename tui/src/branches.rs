//! The repository's branches, as a list of cells.
//!
//! The terminal's tenant over [`gitten_core::refs`]'s ref model — the same
//! flattened shape the window's branches view draws, in a different medium:
//! detached HEAD as its own top row, then a `LOCAL` heading with its branches,
//! then a `REMOTE` heading with the tracking copies, each group present only
//! when it has something in it. What a verb needs of a row travels beside what
//! an eye needs of it, because the name a Latin-1 branch displays and the name
//! git addresses are not the same bytes — display decodes lossily once, at
//! flatten, and every verb aims at the raw bytes the read handed in.
//!
//! The list idioms are [`crate::files`]'s, on purpose: one
//! [`gitten_core::view::Viewport`], the rows flattened **once per refresh**
//! into owned display strings so the render path allocates nothing per frame,
//! the scrollbar over the pane's own last column, and the cursor never resting
//! on a heading. What is this pane's alone is the armed delete — the one
//! destructive verb confirms on the keyboard, the exact pattern the window
//! runs — and the marks: a filled ● is a ref living locally (HEAD in the
//! accent, every other local in a lane ink of its own), a hollow ○ is a
//! fetched copy, and the ASCII set says the same three things in glyphs so the
//! distinction never rests on colour alone.

use crate::screen::{width, Ink, Pen, Screen};
use crate::scrollbar::{self, Bar};
use gitten_core::host::Host;
use gitten_core::refs::{Branch, HeadState, RefName, RemoteBranch, Upstream};
use gitten_core::theme::Rgb;
use gitten_core::view::Viewport;

/// What each kind of ref row opens with: one character, decided by the
/// constructor and not by the frame. A struct and not literals for the same
/// reason [`Glyphs`](crate::commits::Glyphs) and [`Bar`](crate::scrollbar::Bar)
/// are: `--ascii` has to be able to replace the alphabet, and so does an
/// extension that would rather have a Nerd Font's.
///
/// The shipped grammar is the window's: a filled ● is a ref living locally,
/// tinted by what the row *is* — the current branch alone wears the accent —
/// and a hollow ○ marks a remote-tracking copy. The current mark is a
/// *different glyph* from another local's, so `●` against `•` survives a
/// palette with no accent to speak of; the ASCII set does the same job with
/// `*` against `o`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marks {
    /// A local branch HEAD is not on.
    pub local: &'static str,
    /// A remote-tracking copy of elsewhere.
    pub remote: &'static str,
    /// The branch HEAD sits on.
    pub current: &'static str,
}

impl Default for Marks {
    fn default() -> Self {
        Self {
            local: "•",
            remote: "○",
            current: "●",
        }
    }
}

impl Marks {
    /// Nothing outside ASCII, for a terminal or a font that cannot draw the
    /// rest. Current stays distinguishable from another local by shape.
    pub fn ascii() -> Self {
        Self {
            local: "o",
            remote: "o",
            current: "*",
        }
    }
}

/// What the keyboard is on, as verbs aim at it: bytes, never display text.
///
/// The same three shapes the window's branches pane carries — checkout
/// accepts a local or a remote row, rename and delete and tag accept a local
/// only, and the detached row is a place every verb refuses by name rather
/// than guessing which branch was meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A local branch, named relative to `refs/heads`.
    Local(RefName),
    /// A remote-tracking branch. The two halves stay apart because either may
    /// hold a slash; a verb that wants the full refname joins them itself.
    Remote { remote: RefName, branch: RefName },
    /// The detached-HEAD row: a place, not a branch.
    Detached,
}

/// One flat row of the pane: a group heading, the detached-HEAD row, or one
/// ref. Flattened once per refresh — never per frame. Everything a draw needs
/// that costs allocation (the lossy names, the tracking distance, the
/// spelled-out count) is computed at flatten time; what a draw reads per frame
/// is an enum match and a live theme lookup.
#[derive(Debug, Clone)]
pub enum Row {
    /// Detached HEAD, its own top row: the honest state, not hidden.
    Detached {
        /// `(detached at abc12345…)` — abbreviated once, at flatten.
        text: String,
        /// Where this row sits among the pane's selectable rows, from one.
        n: usize,
    },
    /// A group heading, drawn only because the group under it is non-empty.
    Heading {
        /// `LOCAL` or `REMOTE` — the caps the design gives the groups.
        label: &'static str,
        /// How many branches are in the group, spelled out once.
        count: String,
    },
    Local(LocalRow),
    Remote(RemoteRow),
}

/// One local branch.
#[derive(Debug, Clone)]
pub struct LocalRow {
    /// The addressing form, byte for byte. Never decoded in place: this is
    /// what a checkout, a rename, a delete or a tag is aimed at.
    pub name: RefName,
    /// The display form, decoded lossily once at flatten. Drawing's copy, and
    /// nobody else's.
    pub text: String,
    /// What its tracking pair says, pre-rendered as distance only — see
    /// [`upstream_counts`]. `None` draws no cell at all: the branch tracks
    /// nothing, or is in sync with what it tracks.
    pub tracking: Option<String>,
    /// True when a pair exists but cannot be compared — the state the word
    /// *gone* names, drawn faint so it never reads as "in sync".
    pub gone: bool,
    /// HEAD is attached here. Exactly one row carries this in a normal
    /// session, none while detached.
    pub current: bool,
    /// Where this branch sits among the pane's locals, from zero — the live
    /// theme's lane inks are resolved from it *at paint*, so a config reload
    /// recolours the next frame exactly as [`crate::commits`] recolours.
    pub hue: usize,
    /// Where this row sits among the pane's selectable rows, from one — what
    /// the status line says without counting anything per frame.
    pub n: usize,
}

/// One remote-tracking branch, as the last fetch left it.
#[derive(Debug, Clone)]
pub struct RemoteRow {
    /// The remote it came from, as named locally.
    pub remote: RefName,
    /// The branch name on that remote.
    pub branch: RefName,
    /// `origin/main` — the display form, joined once at flatten. The two
    /// halves above stay separate because the join loses information.
    pub label: String,
    /// Where this row sits among the pane's selectable rows, from one.
    pub n: usize,
}

/// The distance half of one local row, rendered once.
///
/// Zeros stay silent — an in-sync branch reads as a bare name, and `↑0 ↓0` is
/// furniture nobody reads past the first time. Unknowable is the other word:
/// a pair configured against a ref that is no longer there gets `(gone)`,
/// because a missing number must not dress up as a zero.
fn upstream_counts(u: &Upstream) -> (Option<String>, bool) {
    let mut text = String::new();
    for (count, arrow) in [(u.ahead, "↑"), (u.behind, "↓")] {
        let Some(n) = count else {
            return (Some("(gone)".into()), true);
        };
        if n > 0 {
            // Joined by a single space; the first arrow comes alone.
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(arrow);
            text.push_str(&n.to_string());
        }
    }
    ((!text.is_empty()).then_some(text), false)
}

/// [`Branch`]/[`RemoteBranch`]/[`HeadState`] flattened to rows, plus the
/// header label. Pure — this is the unit-tested half of a refresh, and
/// everything the pane stores beside its viewport comes out of it.
pub struct Prepared {
    pub rows: Vec<Row>,
    /// The header strip: whose repository, and how much of each group there
    /// is — the window's own title-strip line, one cell row tall here.
    pub label: String,
}

/// Flattens the repository's refs into display rows: detached HEAD first,
/// then the local group, then the remote group — each heading only when its
/// group is non-empty. Pure, and theme-free on purpose: hue indices and
/// current flags land on the rows, the colours they resolve to are a paint
/// decision, so a reload recolours without a re-read.
pub fn prepare(
    local: &[Branch],
    remotes: &[RemoteBranch],
    head: Option<&HeadState>,
    describe: &str,
) -> Prepared {
    let mut rows: Vec<Row> = Vec::new();
    let mut n = 0;
    if let Some(HeadState::Detached { commit }) = head {
        // Eight characters is what `git log --oneline` abbreviates to and
        // what every git UI shows; the full OID stays in the model.
        n += 1;
        rows.push(Row::Detached {
            text: format!("(detached at {}…)", &commit[..commit.len().min(8)]),
            n,
        });
    }
    if !local.is_empty() {
        rows.push(Row::Heading {
            label: "LOCAL",
            count: local.len().to_string(),
        });
        for (i, b) in local.iter().enumerate() {
            n += 1;
            let (tracking, gone) = b.upstream.as_ref().map_or((None, false), upstream_counts);
            rows.push(Row::Local(LocalRow {
                name: b.name.clone(),
                text: b.display().into_owned(),
                tracking,
                gone,
                current: b.head,
                // The row's place among locals, whether or not HEAD marks it:
                // the hue follows the row, the accent follows HEAD.
                hue: i,
                n,
            }));
        }
    }
    if !remotes.is_empty() {
        rows.push(Row::Heading {
            label: "REMOTE",
            count: remotes.len().to_string(),
        });
        for r in remotes {
            n += 1;
            rows.push(Row::Remote(RemoteRow {
                remote: r.remote.clone(),
                branch: r.branch.clone(),
                label: format!(
                    "{}/{}",
                    r.remote.to_string_lossy(),
                    r.branch.to_string_lossy()
                ),
                n,
            }));
        }
    }
    Prepared {
        rows,
        label: format!(
            "{describe} · {} local · {} remote",
            local.len(),
            remotes.len()
        ),
    }
}

/// The header label of a branches pane whose ref reads failed.
///
/// Deliberately not `· 0 local · 0 remote`: a read that did not come back must
/// never be drawn where an empty repository goes. The tenant is still
/// registered and still refreshes — the next successful read replaces both the
/// rows and this sentence.
pub fn unavailable_label(describe: &str) -> String {
    format!("{describe} · branches unavailable")
}

/// What an armed delete asks, once, on the status line — the window's exact
/// sentence, because the confirmation pattern is the window's too.
pub fn delete_question(shown: &str) -> String {
    format!("delete branch {shown}? press again to confirm")
}

/// A row's verb target, when it has one — headings do not.
fn row_target(row: &Row) -> Option<Target> {
    match row {
        Row::Detached { .. } => Some(Target::Detached),
        Row::Local(l) => Some(Target::Local(l.name.clone())),
        Row::Remote(r) => Some(Target::Remote {
            remote: r.remote.clone(),
            branch: r.branch.clone(),
        }),
        Row::Heading { .. } => None,
    }
}

/// Whether a row can hold the cursor. A heading is a label over rows, and a
/// verb aimed at one has nowhere to go.
fn selectable(row: Option<&Row>) -> bool {
    !matches!(row, Some(Row::Heading { .. }))
}

/// The branches pane: flattened rows, a viewport, and the delete that is
/// waiting for its second press.
///
/// Knows nothing about keys or jobs — every method is a command or a read,
/// exactly as in [`crate::files`]. The verbs themselves live in the app, which
/// reads [`Branches::current`] and builds the write from the bytes it names;
/// this pane holds the *confirmation* state, because what the second press
/// confirms is a row of this list.
pub struct Branches {
    rows: Vec<Row>,
    /// The cursor, the top row and the height — [`Viewport`], the same model
    /// every other list holds.
    view: Viewport,
    cols: usize,
    bar: Bar,
    /// The marks every ref row opens with. Constructor-owned, so `--ascii`
    /// never grows a branch in paint.
    marks: Marks,
    /// The delete awaiting its second press: the exact bytes of the row that
    /// asked. One slot — arming a different row moves the question, it does
    /// not queue two. Outliving a switch to another pane and back is
    /// deliberate — the question still sits on the row it was asked about —
    /// and every *attention* change kills it: a cursor move, a wheel, a mouse
    /// row change, a prompt, a reload, a refresh. The app disarms it when the
    /// pane loses the keyboard, through [`Branches::disarm`].
    armed: Option<Target>,
    /// Where in the scrollbar's thumb it was taken hold of, while it is held.
    grabbed: Option<usize>,
    /// How many of the rows are selectable — the status line's denominator,
    /// counted by the same pass that numbers them.
    total: usize,
}

impl Branches {
    /// A pane over flattened rows — a successful read, empty included, which
    /// is a state and not a failure. The cursor opens on the first row a verb
    /// can act on: a heading is a label over rows, and a verb aimed at one
    /// has nowhere to go.
    pub fn new(rows: Vec<Row>) -> Self {
        Self::with_marks(rows, Marks::default())
    }

    /// [`Branches::new`] with the marks replaced — the `--ascii` constructor,
    /// and the seam an extension uses to bring its own alphabet.
    pub fn with_marks(rows: Vec<Row>, marks: Marks) -> Self {
        let mut view = Viewport::new();
        view.set_len(rows.len());
        if let Some(first) = rows.iter().position(|r| selectable(Some(r))) {
            view.go_to(first);
        }
        let total = rows.iter().filter(|r| selectable(Some(r))).count();
        Self {
            rows,
            view,
            cols: 0,
            bar: Bar::default(),
            marks,
            armed: None,
            grabbed: None,
            total,
        }
    }

    /// How many columns the pane draws into, and how many rows it shows.
    pub fn resize(&mut self, cols: usize, height: usize) {
        self.cols = cols;
        self.view.set_height(height);
    }

    /// How much lead the cursor keeps at the edge. `[view] scrolloff`.
    pub fn set_scrolloff(&mut self, rows: usize) {
        self.view.set_scrolloff(rows);
    }

    /// The glyphs the scrollbar is drawn with. `--ascii`, or an extension.
    pub fn set_bar(&mut self, bar: Bar) {
        self.bar = bar;
    }

    /// The cursor's row, for whatever reads it.
    pub fn cursor(&self) -> usize {
        self.view.cursor()
    }

    /// The first row drawn.
    pub fn top(&self) -> usize {
        self.view.top()
    }

    /// Moves the cursor off a row that cannot hold it — a heading — onward
    /// from `from`, in the direction it was travelling. Called after every
    /// cursor move and every refresh, so the cursor never rests on a heading.
    fn settle(&mut self, from: usize) {
        self.view.settle(from, |i| selectable(self.rows.get(i)));
    }

    /// Swaps in refreshed rows, keeping the keyboard on its branch.
    ///
    /// Only a selectable row anchors, and on what a verb aims at — the exact
    /// bytes. A heading is a fact about the last refresh's grouping, not a
    /// thing the eye was reading. A branch that vanished falls back to
    /// clamping, like a commit list whose sha left the log, and then settles
    /// forward the way a fresh open does.
    ///
    /// A refresh is the repository saying things moved; an armed delete was a
    /// a promise about how they were, so it dies here first — and so does the
    /// mouse's hold on a thumb that may no longer mean anything.
    pub fn replace(&mut self, rows: Vec<Row>) {
        self.armed = None;
        self.grabbed = None;
        let old = self.view;
        let anchored = match self.rows.get(old.cursor()) {
            Some(r) => row_target(r),
            None => None,
        };
        self.rows = rows;
        self.total = self.rows.iter().filter(|r| selectable(Some(r))).count();
        // The old scroll position first, then the anchor: `go_to` drags the
        // viewport after the cursor, and the surviving branch's row must be
        // the one on screen when it survives.
        let mut view = old;
        view.set_len(self.rows.len());
        view.scroll_to(old.top());
        let cursor = anchored
            .and_then(|target| {
                self.rows
                    .iter()
                    .position(|r| row_target(r).as_ref() == Some(&target))
            })
            .unwrap_or_else(|| old.cursor().min(self.rows.len().saturating_sub(1)));
        view.go_to(cursor);
        self.view = view;
        // A vanished anchor can leave the cursor on whatever heading took its
        // row; the direction is "where it was", so it walks on to the next
        // selectable row rather than back to the previous group's last.
        self.settle(old.cursor());
    }

    // ------------------------------------------------------------------ verbs

    /// What the keyboard is on, as verbs aim at it. `None` on an empty pane —
    /// and on a heading, which the cursor never rests on.
    pub fn current(&self) -> Option<Target> {
        match self.rows.get(self.view.cursor()) {
            Some(r) => row_target(r),
            None => None,
        }
    }

    /// Arms — or confirms — a delete of this exact target. The first call on
    /// a target stores it and returns false: ask, don't act. A second call on
    /// the same target clears the arm and returns true: act. Anything else
    /// re-arms onto the new target and returns false again, so there is no
    /// state here a caller has to remember.
    pub fn confirm_or_arm_delete(&mut self, target: &Target) -> bool {
        let already = self.armed.as_ref() == Some(target);
        self.armed = match already {
            true => None,
            false => Some(target.clone()),
        };
        already
    }

    /// Drops the question, whatever it was about — what the app calls when
    /// the pane loses the keyboard, because a destructive verb armed to a row
    /// nobody is looking at is an accident waiting for its second press.
    pub fn disarm(&mut self) {
        self.armed = None;
    }

    /// Whether a delete is waiting for its second press — the paint's tint
    /// of the row the question is about, and the tests' window on it.
    pub fn armed_row(&self) -> Option<Target> {
        self.armed.clone()
    }

    /// The row an armed delete sits on, found per frame — the tint is a
    /// property of the question, not of the draw.
    fn armed_index(&self) -> Option<usize> {
        self.armed.as_ref().and_then(|target| {
            self.rows
                .iter()
                .position(|r| row_target(r).as_ref() == Some(target))
        })
    }

    // -------------------------------------------------------------- commands

    /// One move of the cursor, past whatever heading it lands on, and the arm
    /// it drops. Every keyboard move is a move of attention: whatever was
    /// armed was armed to what the keyboard used to be on.
    fn move_by(&mut self, by: isize) {
        let from = self.view.cursor();
        self.view.move_by(by);
        self.settle(from);
        self.armed = None;
    }

    pub fn down(&mut self) {
        self.move_by(1);
    }

    pub fn up(&mut self) {
        self.move_by(-1);
    }

    pub fn page(&mut self, pages: isize) {
        let from = self.view.cursor();
        self.view.page(pages);
        self.settle(from);
        self.armed = None;
    }

    /// Scrolls without moving the cursor further than it has to — the wheel.
    /// Also a move of attention, and it disarms like one.
    pub fn scroll_y(&mut self, by: isize) {
        let from = self.view.cursor();
        self.view.scroll_by(by);
        self.settle(from);
        self.armed = None;
    }

    pub fn to_top(&mut self) {
        self.view.to_top();
        self.settle(0);
        self.armed = None;
    }

    pub fn to_bottom(&mut self) {
        self.view.to_bottom();
        self.settle(self.rows.len().saturating_sub(1));
        self.armed = None;
    }

    /// A press in the list: the cursor moves there, unless the press landed
    /// on the scrollbar — its own last column, not the screen's.
    ///
    /// A press on another row takes the armed question's row out from under
    /// it; a press on the same row leaves the question standing. There is no
    /// drag selection over a ref list: the mouse moves the cursor and
    /// nothing else.
    pub fn press(&mut self, col: usize, row: usize, _extend: bool, host: &Host) {
        if scrollbar::hit(col, self.cols, &self.view, host) {
            let row = row.min(self.view.height().saturating_sub(1));
            let before = self.view.top();
            self.grabbed = Some(scrollbar::grab(&mut self.view, host, row));
            // A press on the track can jump the thumb: a moving scrollbar is
            // a moving list, and the question was asked about a row of the
            // list that was.
            if self.view.top() != before {
                self.armed = None;
            }
            return;
        }
        let Some(index) = self.view.row_at(row) else {
            return;
        };
        let armed_on = self.armed_index();
        let from = self.view.cursor();
        self.view.go_to(index);
        self.settle(from);
        if self.armed.is_some() && Some(self.view.cursor()) != armed_on {
            self.armed = None;
        }
    }

    /// The pointer moved with the button down. Only the scrollbar's own grab
    /// means anything — a ref list has no drag selection to grow.
    pub fn drag(&mut self, row: isize, host: &Host) {
        if let Some(grabbed) = self.grabbed {
            let before = self.view.top();
            scrollbar::drag(&mut self.view, host, row.max(0) as usize, grabbed);
            if self.view.top() != before {
                self.armed = None;
            }
        }
    }

    pub fn release(&mut self) {
        self.grabbed = None;
    }

    /// What `copy.selection` copies here: the row the keyboard is on, as git
    /// would spell it — the bare refname, or the joined `remote/branch`. A
    /// heading and the detached row copy nothing, which is what makes the
    /// empty result skip the clipboard entirely.
    pub fn copy_text(&self) -> String {
        match self.current() {
            Some(Target::Local(name)) => name.to_string_lossy().into_owned(),
            Some(Target::Remote { remote, branch }) => {
                format!("{}/{}", remote.to_string_lossy(), branch.to_string_lossy())
            }
            Some(Target::Detached) | None => String::new(),
        }
    }

    /// What the *mouse* has selected — nothing, ever: a ref list has no drag
    /// selection, so copy-on-select has nothing to fire on.
    pub fn selection(&self) -> String {
        String::new()
    }

    /// `select.all` is inert here, the same answer the commit graph gives.
    pub fn select_all(&mut self) {}

    /// `select.none`. Says there was no range to drop, so `esc` falls
    /// through to whatever it means next.
    pub fn select_none(&mut self) -> bool {
        false
    }

    /// One line describing where the keyboard is, for the status row. The
    /// ordinal and the denominator are both selectable-row counts, decided by
    /// the same flatten that numbered the rows — nothing here counts per
    /// frame.
    pub fn status(&self) -> String {
        if self.total == 0 {
            return "no branches".into();
        }
        let at = match self.rows.get(self.view.cursor()) {
            Some(Row::Detached { n, .. }) => *n,
            Some(Row::Local(l)) => l.n,
            Some(Row::Remote(r)) => r.n,
            _ => 0,
        };
        let named = match self.rows.get(self.view.cursor()) {
            Some(Row::Detached { text, .. }) => text.clone(),
            Some(Row::Local(l)) => l.text.clone(),
            Some(Row::Remote(r)) => r.label.clone(),
            _ => String::new(),
        };
        format!("{at}/{} · {named}", self.total)
    }

    // ------------------------------------------------------------------ draw

    /// Draws the visible rows into `screen`, at `x` of row `y` onward, inside
    /// this pane's own columns.
    ///
    /// Every row goes through [`Screen::span`], never [`Screen::row`]: the
    /// pane is a guest in the row, and a long name that wrote to the whole
    /// screen would overwrite the divider and whatever sits beside it. The
    /// cursor background runs the full width only when this pane holds the
    /// keyboard — `focused` is the caller's answer, not something the view
    /// knows — and an armed row wears the error ink whether focused or not,
    /// because the question stands in both states.
    pub fn paint(&self, screen: &mut Screen, x: usize, y: usize, focused: bool, host: &Host) {
        let c = &host.theme.chrome;
        let plain = Ink::new(c.fg, c.bg);
        // An empty repository is one quiet line, not an empty box — an unborn
        // or branchless repository's honest answer, at the first content row.
        if self.rows.is_empty() {
            let mut pen = screen.span(y, x, self.cols);
            pen.put("no branches yet", Ink::new(c.faint, c.bg));
            return;
        }
        let armed = self.armed_index();
        for i in 0..self.view.height() {
            let row = y + i;
            let Some(index) = self.view.row_at(i) else {
                screen.span(row, x, self.cols).wash(plain);
                continue;
            };
            let bg = match focused && index == self.view.cursor() {
                true => c.selection_bg,
                false => c.bg,
            };
            let mut pen = screen.span(row, x, self.cols);
            self.row(&mut pen, index, bg, host, armed == Some(index));
        }
        if self.cols > 0 {
            // Last, and over the rows rather than beside them — at this
            // pane's own last column, which is not the screen's.
            scrollbar::paint(screen, self.bar, x + self.cols - 1, y, &self.view, host);
        }
    }

    /// One row: a quiet caps heading with its count at the right, or a mark,
    /// one column of air, a name — and, for a tracked local, the distance
    /// pushed to the right edge — and the whole thing in `chrome.error` while
    /// this is the row an armed delete is waiting on, so the thing a second
    /// press will destroy is named by its own colour and not only by the band
    /// above it.
    ///
    /// The row's background travels in every piece's ink, the way the commit
    /// list draws — `wash` is the *last* write, running whatever background
    /// the row earned out to the edge, not a first coat the text goes on top
    /// of. The distance is reserved *before* the name is drawn, so a long
    /// name clips rather than shoving `↑2` out of the pane: the distance is
    /// the fact a narrow sidebar is being glanced at for.
    fn row(&self, pen: &mut Pen, index: usize, bg: Rgb, host: &Host, armed: bool) {
        let c = &host.theme.chrome;
        match &self.rows[index] {
            Row::Heading { label, count } => {
                pen.put(label, Ink::new(c.faint, bg));
                // The count at the pane's right edge, clear of the scrollbar
                // column, and never pushed backwards by a label that does not
                // fit.
                let at = self.cols.saturating_sub(1 + width(count));
                pen.fill(at.saturating_sub(pen.col()), ' ', Ink::new(c.fg, bg));
                pen.seek(at.max(pen.col()));
                pen.put(count, Ink::new(c.faint, bg));
                pen.wash(Ink::new(c.fg, bg));
            }
            Row::Detached { text, .. } => {
                let ink = match armed {
                    true => Ink::new(c.error, bg),
                    false => Ink::new(c.dim, bg),
                };
                pen.put(self.marks.current, Ink::new(c.dim, bg));
                pen.put(" ", ink);
                pen.put(text, ink);
                pen.wash(ink);
            }
            Row::Local(l) => {
                let name = match armed {
                    true => Ink::new(c.error, bg),
                    false => Ink::new(c.fg, bg),
                };
                let mark = match armed {
                    true => Ink::new(c.error, bg),
                    false => Ink::new(
                        match l.current {
                            true => c.accent,
                            false => host.theme.lane(l.hue),
                        },
                        bg,
                    ),
                };
                pen.put(
                    match l.current {
                        true => self.marks.current,
                        false => self.marks.local,
                    },
                    mark,
                );
                pen.put(" ", name);
                // The distance's left edge, reserved before the name takes
                // its width: the one column the scrollbar owns, then the
                // distance, then whatever is left is the name's to clip in.
                if let Some(tracking) = &l.tracking {
                    let at = self.cols.saturating_sub(1 + width(tracking)).max(pen.col());
                    let mut body = pen.take(at - pen.col());
                    body.put(&l.text, name);
                    let tracking_ink = match armed {
                        true => Ink::new(c.error, bg),
                        false => Ink::new(
                            match l.gone {
                                true => c.faint,
                                false => c.dim,
                            },
                            bg,
                        ),
                    };
                    pen.put(tracking, tracking_ink);
                    pen.wash(name);
                } else {
                    pen.put(&l.text, name);
                    pen.wash(name);
                }
            }
            Row::Remote(r) => {
                let ink = match armed {
                    true => Ink::new(c.error, bg),
                    false => Ink::new(c.dim, bg),
                };
                pen.put(self.marks.remote, Ink::new(c.faint, bg));
                pen.put(" ", ink);
                pen.put(&r.label, ink);
                pen.wash(ink);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full-length OID-looking commit id, for shapes rather than values.
    fn sha() -> String {
        "0123456789abcdef0123456789abcdef01234567".to_string()
    }

    fn local(name: &str, head: bool) -> Branch {
        Branch {
            name: RefName::from(name),
            commit: sha(),
            upstream: None,
            head,
        }
    }

    fn tracked(name: &str, head: bool, ahead: Option<u32>, behind: Option<u32>) -> Branch {
        Branch {
            upstream: Some(Upstream {
                remote: RefName::from("origin"),
                branch: RefName::from(name),
                ahead,
                behind,
            }),
            ..local(name, head)
        }
    }

    fn remote(name: &str) -> RemoteBranch {
        RemoteBranch {
            remote: RefName::from("origin"),
            branch: RefName::from(name),
            commit: sha(),
        }
    }

    /// Detached HEAD, two locals — one tracked ahead and behind, one whose
    /// upstream is gone — and two remotes: every grammar decision the pane
    /// makes, in one read.
    fn fixture() -> (Vec<Branch>, Vec<RemoteBranch>, HeadState) {
        (
            vec![
                tracked("held", true, Some(2), Some(3)),
                Branch {
                    name: RefName::from_bytes(b"f\xe9ature"),
                    ..local("unused", false)
                },
            ],
            vec![remote("main"), remote("wip")],
            HeadState::Detached { commit: sha() },
        )
    }

    /// Headings and rows in draw order — the shape the tests read.
    fn outline(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Detached { text, .. } => format!("[detached {text}]"),
                Row::Heading { label, count } => format!("[{label}·{count}]"),
                Row::Local(l) => match &l.tracking {
                    Some(t) => format!("{} {t}", l.text),
                    None => l.text.clone(),
                },
                Row::Remote(r) => r.label.clone(),
            })
            .collect()
    }

    /// A pane over a prepare, at a size, as a refresh would leave it.
    fn view(
        local: &[Branch],
        remotes: &[RemoteBranch],
        head: Option<&HeadState>,
        cols: usize,
        height: usize,
    ) -> (Branches, Host) {
        let prepared = prepare(local, remotes, head, "fake (main)");
        let mut b = Branches::new(prepared.rows);
        b.resize(cols, height);
        (b, Host::new())
    }

    /// What the pane drew, at `x`, one string per visible row.
    fn painted(b: &Branches, host: &Host, x: usize, w: usize) -> Vec<String> {
        let mut screen = Screen::new(w, b.view.height().max(1));
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        b.paint(&mut screen, x, 0, true, host);
        (0..b.view.height()).map(|y| screen.row_text(y)).collect()
    }

    #[test]
    fn prepare_sections_detached_current_remote_and_tracking_once() {
        let (locals, remotes, head) = fixture();
        let prepared = prepare(&locals, &remotes, Some(&head), "fake (main)");
        assert_eq!(
            outline(&prepared.rows),
            vec![
                "[detached (detached at 01234567…)]",
                "[LOCAL·2]",
                "held ↑2 ↓3",
                "f\u{FFFD}ature",
                "[REMOTE·2]",
                "origin/main",
                "origin/wip",
            ],
            "detached first, then LOCAL and its branches, then REMOTE and its copies"
        );
        // The label counts both groups, spelled once at prepare.
        assert_eq!(prepared.label, "fake (main) · 2 local · 2 remote");

        // Display decodes lossily; addressing keeps the bytes. The two never
        // swap jobs: the verb aim is the raw one, end to end.
        let Row::Local(f) = &prepared.rows[3] else {
            panic!("the Latin-1 local row expected");
        };
        assert_eq!(f.name.as_bytes(), b"f\xe9ature", "addressing keeps bytes");
        assert!(
            f.text.contains('\u{FFFD}'),
            "display decodes lossily instead of failing"
        );
        let Row::Remote(wip) = &prepared.rows[6] else {
            panic!("the remote row expected");
        };
        assert_eq!(wip.branch.as_bytes(), b"wip");
        assert_eq!(wip.remote.as_bytes(), b"origin");

        // Zero suppression: an in-sync branch draws no tracking cell at all.
        let synced = vec![tracked("synced", false, Some(0), Some(0))];
        let rows = prepare(&synced, &[], None, "").rows;
        match &rows[1] {
            Row::Local(l) => assert_eq!(l.tracking, None, "an in-sync branch is bare"),
            other => panic!("the synced row expected, got {other:?}"),
        }
        // A gone upstream is named, not read as zero.
        let gone = vec![tracked("old", false, None, None)];
        let rows = prepare(&gone, &[], None, "").rows;
        match &rows[1] {
            Row::Local(l) => {
                assert!(l.gone);
                assert_eq!(l.tracking.as_deref(), Some("(gone)"));
            }
            other => panic!("the gone row expected, got {other:?}"),
        }

        // An attached HEAD draws no detached row, and no tags row exists at
        // any point — tags are another pane's read and this one makes none.
        let attached = HeadState::Branch {
            name: RefName::from("held"),
            commit: Some(sha()),
        };
        let rows = prepare(&locals, &remotes, Some(&attached), "").rows;
        assert!(
            !rows.iter().any(|r| matches!(r, Row::Detached { .. })),
            "an attached HEAD drew a detached row"
        );
        assert!(!outline(&rows).iter().any(|r| r.contains("tag")));
    }

    #[test]
    fn current_branch_has_a_textual_mark_in_default_and_ascii_frames() {
        let main = vec![local("main", true), local("other", false)];
        let remotes = vec![remote("main")];
        let head = HeadState::Branch {
            name: RefName::from("main"),
            commit: Some(sha()),
        };
        let (b, host) = view(&main, &remotes, Some(&head), 40, 8);
        let rows = painted(&b, &host, 0, 40);
        // The cursor opens past the LOCAL heading, on `main` — the current
        // branch — and the pane drew all three rows.
        assert!(
            rows[1].contains("●") && rows[1].contains("main"),
            "{rows:?}"
        );
        assert!(
            rows[2].contains("•") && rows[2].contains("other"),
            "{rows:?}"
        );
        assert!(
            rows[4].contains("○") && rows[4].contains("origin/main"),
            "{rows:?}"
        );

        // The inks: HEAD's branch alone wears the accent, a remote copy is
        // faint, and the marks are the *default* set.
        let c = &host.theme.chrome;
        assert_eq!(b.marks.current, "●");
        assert_eq!(b.marks.local, "•");
        assert_eq!(b.marks.remote, "○");
        assert_eq!(screen_mark_ink(&b, &host, 1).fg, c.accent, "current");
        assert_eq!(
            screen_mark_ink(&b, &host, 2).fg,
            host.theme.lane(1),
            "other local"
        );
        assert_eq!(screen_mark_ink(&b, &host, 4).fg, c.faint, "remote");

        // A long name clips before it can displace the distance: `↑2 ↓3`
        // keeps its right-hand reservation whatever the name does.
        let long = vec![tracked(&"n".repeat(80), false, Some(2), Some(3))];
        let (b, host) = view(&long, &[], None, 20, 4);
        let rows = painted(&b, &host, 0, 20);
        assert!(
            rows[1].contains("↑2 ↓3"),
            "the distance was shoved out: {rows:?}"
        );
        assert!(
            crate::screen::width(&rows[1]) <= 20,
            "the row overflowed the pane"
        );

        // The ASCII set: no non-ASCII mark anywhere in the frame, and the
        // current branch still distinguishable by shape.
        let mut b = Branches::with_marks(
            prepare(&main, &remotes, Some(&head), "").rows,
            Marks::ascii(),
        );
        b.resize(40, 8);
        let mut screen = Screen::new(40, 8);
        b.paint(&mut screen, 0, 0, true, &host);
        let text: String = (0..8).map(|y| screen.row_text(y)).collect();
        assert!(
            text.contains('*'),
            "the current mark did not draw: {text:?}"
        );
        assert!(
            !text.contains("●") && !text.contains("•") && !text.contains("○"),
            "a default mark reached an ascii frame: {text:?}"
        );
    }

    /// The ink of the mark cell of pane row `row`, painted focused — the
    /// tests' window on what colour a row's first glyph drew in.
    fn screen_mark_ink(b: &Branches, host: &Host, row: usize) -> Ink {
        let mut screen = Screen::new(40, 8);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        b.paint(&mut screen, 0, 0, true, host);
        screen.ink(0, row).unwrap()
    }

    #[test]
    fn headings_never_hold_selection_and_empty_repositories_say_so() {
        let main = vec![local("main", true)];
        let remotes = vec![remote("main")];
        let head = HeadState::Branch {
            name: RefName::from("main"),
            commit: Some(sha()),
        };
        // Local + remote: LOCAL heading, main, REMOTE heading, origin/main.
        let (mut b, host) = view(&main, &remotes, Some(&head), 40, 8);
        assert_eq!(
            b.current(),
            Some(Target::Local(RefName::from("main"))),
            "the pane did not open past its heading"
        );
        // Down walks onto the remote row, up walks back, pages and ends
        // settle too — no resting cursor on either heading.
        b.down();
        assert_eq!(
            b.current(),
            Some(Target::Remote {
                remote: RefName::from("origin"),
                branch: RefName::from("main")
            })
        );
        b.down();
        assert_eq!(
            b.current(),
            Some(Target::Remote {
                remote: RefName::from("origin"),
                branch: RefName::from("main")
            }),
            "down past the last row moved the cursor"
        );
        b.up();
        assert_eq!(b.current(), Some(Target::Local(RefName::from("main"))));
        b.up();
        assert_eq!(
            b.current(),
            Some(Target::Local(RefName::from("main"))),
            "up onto the first heading parked on it"
        );
        b.to_top();
        assert_eq!(b.current(), Some(Target::Local(RefName::from("main"))));
        b.to_bottom();
        assert!(matches!(b.current(), Some(Target::Remote { .. })));
        b.page(1);
        b.page(-1);
        assert!(b.current().is_some(), "a page parked on a heading");
        b.scroll_y(2);
        b.scroll_y(-2);
        assert!(b.current().is_some(), "a scroll parked on a heading");

        // A mouse press on a heading takes the next honest row instead.
        b.to_top();
        // `to_top` settled forward off the LOCAL heading, so it sits at row 0
        // and `main` — the settled cursor — is at row 1.
        assert!(matches!(
            b.rows[b.view.row_at(0).expect("on screen")],
            Row::Heading { .. }
        ));
        b.press(5, 0, false, &host);
        assert!(
            matches!(b.rows[b.view.cursor()], Row::Local(_)),
            "a heading click parked the cursor on itself"
        );

        // Remote-only: the REMOTE heading is row 0 and the pane opens past it.
        let (b, _host) = view(&[], &remotes, None, 40, 8);
        assert!(
            matches!(b.rows[b.view.cursor()], Row::Remote(_)),
            "a remote-only pane opened on its heading"
        );

        // Detached-only: one row, and it is the detached row — a place every
        // verb can refuse honestly.
        let detached = HeadState::Detached { commit: sha() };
        let (b, _host) = view(&[], &[], Some(&detached), 40, 8);
        assert_eq!(b.current(), Some(Target::Detached));

        // Empty and bare: one quiet sentence, at the first content row.
        let (b, host) = view(&[], &[], None, 40, 8);
        assert_eq!(b.current(), None);
        assert_eq!(b.status(), "no branches");
        let rows = painted(&b, &host, 0, 40);
        assert!(rows[0].contains("no branches yet"), "{rows:?}");
        // ...and the same draw survives a zero-width pane and a zero-height
        // screen without panicking on either.
        let mut b = Branches::new(Vec::new());
        b.resize(0, 0);
        let mut screen = Screen::new(0, 0);
        b.paint(&mut screen, 0, 0, true, &host);
    }

    #[test]
    fn refresh_anchors_raw_target_and_clears_delete_arm() {
        let (locals, remotes, head) = fixture();
        let (mut b, _host) = view(&locals, &remotes, Some(&head), 40, 8);
        // Onto the Latin-1 branch: two steps down from the detached row —
        // one crosses the LOCAL heading — then arm a delete of it and take a
        // hold of the scrollbar thumb, so both kinds of held state are
        // observable across the refresh.
        b.down();
        b.down();
        let Some(Target::Local(raw)) = b.current() else {
            panic!("the Latin-1 branch is under the keyboard");
        };
        assert_eq!(raw.as_bytes(), b"f\xe9ature");
        assert!(!b.confirm_or_arm_delete(&Target::Local(raw.clone())));
        assert_eq!(b.armed_row().as_ref(), Some(&Target::Local(raw.clone())));
        b.grabbed = Some(1);

        // A refresh that *reorders* the locals: the anchor is the bytes, so
        // the keyboard goes with the branch to wherever it now sits.
        let reordered = vec![
            local("other", false),
            local("main", false),
            Branch {
                name: RefName::from_bytes(b"f\xe9ature"),
                ..local("unused", false)
            },
            tracked("held", true, None, None),
        ];
        let mut remotes = remotes.clone();
        remotes.reverse();
        let detached = HeadState::Detached { commit: sha() };
        b.replace(prepare(&reordered, &remotes, Some(&detached), "fake (main)").rows);
        assert_eq!(
            b.current(),
            Some(Target::Local(RefName::from_bytes(b"f\xe9ature"))),
            "the anchor did not survive the reorder byte-for-byte"
        );
        assert_eq!(b.armed_row(), None, "the arm survived its own refresh");
        assert_eq!(b.grabbed, None, "the grab survived its own refresh");

        // The branch itself is gone: clamp onto what survives, then settle
        // onto a row a verb can act on.
        let survivor = vec![local("main", true)];
        b.replace(prepare(&survivor, &[], None, "").rows);
        assert_eq!(
            b.current(),
            Some(Target::Local(RefName::from("main"))),
            "a vanished anchor did not settle onto a selectable row"
        );

        // Emptied wholesale: the cursor and viewport both come home, and the
        // dimensions and the bar the pane was sized with are untouched.
        let (cols, height, bar) = (b.cols, b.view.height(), b.bar);
        b.replace(prepare(&[], &[], None, "").rows);
        assert_eq!((b.view.cursor(), b.view.top()), (0, 0));
        assert_eq!((b.cols, b.view.height(), b.bar), (cols, height, bar));
        assert_eq!(b.current(), None);
        assert_eq!(b.armed_row(), None);
        assert_eq!(b.copy_text(), "");
    }

    #[test]
    fn delete_arms_same_raw_target_then_submits_non_force_once() {
        let main = vec![local("main", true), local("feature", false)];
        let (mut b, host) = view(&main, &[], None, 40, 8);
        b.down();
        let target = b.current().expect("a branch under the keyboard");

        // First press arms and asks; the identical second spends the arm.
        assert!(!b.confirm_or_arm_delete(&target));
        assert_eq!(b.armed_row(), Some(target.clone()));
        assert!(b.confirm_or_arm_delete(&target));
        assert_eq!(b.armed_row(), None, "the arm did not spend");

        // A different row re-arms rather than inheriting the question.
        assert!(!b.confirm_or_arm_delete(&target));
        b.down();
        assert_eq!(b.armed_row(), None, "a keyboard move kept the arm");

        // The armed row wears the error ink unfocused — the question stands
        // in both states — and the tint is that row's alone.
        b.to_top();
        b.down();
        let armed_at = b.cursor();
        assert!(!b.confirm_or_arm_delete(&b.current().unwrap()));
        let mut screen = Screen::new(40, 8);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        b.paint(&mut screen, 0, 0, false, &host);
        let c = &host.theme.chrome;
        assert_eq!(
            screen.ink(2, armed_at).unwrap().fg,
            c.error,
            "the armed row did not tint"
        );
        assert_ne!(
            screen.ink(2, 0).unwrap().fg,
            c.error,
            "the tint crossed onto another row"
        );

        // The question is the branch, spoken once, exactly.
        assert_eq!(
            delete_question("feature"),
            "delete branch feature? press again to confirm"
        );
    }
}
