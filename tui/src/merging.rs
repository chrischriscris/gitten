//! The merging view: one conflicted file, its regions, and the answers.
//!
//! What a conflict looks like on this pane is what git wrote: the file's own
//! markers, ours above, theirs below, the diff3 base between them when the
//! repository asks for one. Nothing is re-flowed into a three-way layout,
//! because the markers are the one presentation every tool agrees on — and
//! because a resolution chosen here is checked against exactly those bytes
//! at write time, a view drawn from anything else would be checking against
//! something the reader never saw.
//!
//! # The session's undo
//!
//! Every answer snapshots the file and the unmerged stages *before* it ran,
//! and `z` restores the last snapshot — bytes to the working tree, stages
//! back through `update-index --index-info`, so the path is unmerged again
//! in git's own eyes, not merely marker-fested while the index says
//! resolved — including the answer that resolved the file whole, which is
//! the undo that matters most. The stack is this view's own session: it
//! dies with the view, and nothing else. An external change *between* an
//! answer and its undo is overwritten by the undo: the snapshot is what is
//! restored, and that is the documentation, not an accident.

use crate::screen::{Ink, Screen};
use crate::scrollbar::{self, Bar};
use gitten_core::conflict::{Answer, ConflictFile};
use gitten_core::host::Host;
use gitten_core::status::PathBytes;
use gitten_core::view::Viewport;
use gitten_git::UnmergedStage;

/// Which half of a region a row belongs to — the thing `space` asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Half {
    Ours,
    Theirs,
}

impl Half {
    fn answer(self) -> Answer {
        match self {
            Half::Ours => Answer::Ours,
            Half::Theirs => Answer::Theirs,
        }
    }
}

/// Where in its region a row sits — what the verbs need from the keyboard's
/// place, and nothing more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Open,
    Ours,
    BaseMarker,
    Base,
    Sep,
    Theirs,
    Close,
}

/// One display row: which part of which region it is, and the lossy text it
/// draws. The address is kept so `space` and the conflict jumps need no
/// re-derivation per keypress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// `Some` when the line sits inside (or delimits) a region.
    pub place: Option<(usize, Part)>,
}

/// One undo step: the file and the stages, exactly as an answer found them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoStep {
    pub bytes: Vec<u8>,
    pub stages: Vec<UnmergedStage>,
}

/// The view. Holds the snapshot it opened on, the rows flattened from it —
/// once per open and once per refresh, never per frame — and the session's
/// undo stack. Knows nothing about keys.
pub struct Merging {
    path: PathBytes,
    file: ConflictFile,
    stages: Vec<UnmergedStage>,
    /// One entry per line of the file: `None` for context, the region place
    /// otherwise. Parallel to the file's lines, not to the viewport.
    places: Vec<Row>,
    /// The file's lines, lossily decoded — drawn from here, never decoded
    /// per paint. Rebuilt on open and replace beside `places`, for the
    /// same reason: nothing on the render path recomputes what a cache
    /// could hold.
    lines: Vec<String>,
    /// Where each region's first row landed, ascending — the jump list.
    region_rows: Vec<usize>,
    view: Viewport,
    cols: usize,
    bar: Bar,
    undo: Vec<UndoStep>,
    dragging: bool,
}

impl Merging {
    /// A view on a freshly read conflict. A file with no regions is a valid
    /// view — the resolved state a refresh can arrive at — and draws as
    /// plain content with nothing to answer.
    pub fn new(path: PathBytes, file: ConflictFile, stages: Vec<UnmergedStage>) -> Self {
        let places = flatten(&file);
        let lines = display_lines(&file.bytes);
        let region_rows = places
            .iter()
            .enumerate()
            .filter_map(|(row, place)| matches!(place.place, Some((_, Part::Open))).then_some(row))
            .collect();
        let mut view = Viewport::new();
        view.set_len(places.len());
        Self {
            path,
            file,
            stages,
            places,
            lines,
            region_rows,
            view,
            cols: 0,
            bar: Bar::default(),
            undo: Vec::new(),
            dragging: false,
        }
    }

    /// The path this view is about — what its refresh re-reads and what its
    /// verbs aim at.
    pub fn path(&self) -> &PathBytes {
        &self.path
    }

    /// Swaps in a re-read of the same path. The keyboard keeps its row and
    /// clamps. The undo stack survives: these snapshots are *this session's*
    /// answers, and an undo that cannot reach the resolution that emptied
    /// the markers is an undo that lies about `z`. What it restores is the
    /// snapshot — see the [module documentation](self) for the boundary
    /// that draws on external changes.
    pub fn replace(&mut self, file: ConflictFile, stages: Vec<UnmergedStage>) {
        self.dragging = false;
        let (cursor, top) = (self.view.cursor(), self.view.top());
        self.file = file;
        self.stages = stages;
        self.places = flatten(&self.file);
        self.lines = display_lines(&self.file.bytes);
        self.region_rows = self
            .places
            .iter()
            .enumerate()
            .filter_map(|(row, place)| matches!(place.place, Some((_, Part::Open))).then_some(row))
            .collect();
        self.view.scroll_to(top);
        self.view.set_len(self.places.len());
        self.view
            .go_to(cursor.min(self.places.len().saturating_sub(1)));
    }

    /// Whether the file still carries regions — the gate every answer verb
    /// reads before anything is submitted.
    pub fn is_conflicted(&self) -> bool {
        self.file.is_conflicted()
    }

    /// Whether the file's regions overlap — the gate that sends the
    /// answer verbs to the whole-file answers instead.
    pub fn is_nested(&self) -> bool {
        self.file.is_nested()
    }

    // ------------------------------------------------------------- the viewport

    pub fn set_scrolloff(&mut self, rows: usize) {
        self.view.set_scrolloff(rows);
    }

    pub fn set_bar(&mut self, bar: Bar) {
        self.bar = bar;
    }

    pub fn resize(&mut self, cols: usize, height: usize) {
        self.cols = cols;
        self.view.set_height(height);
    }

    pub fn move_by(&mut self, by: isize) {
        self.view.move_by(by);
    }

    pub fn down(&mut self) {
        self.move_by(1);
    }

    pub fn up(&mut self) {
        self.move_by(-1);
    }

    pub fn page(&mut self, pages: isize) {
        self.view.page(pages);
    }

    pub fn scroll_y(&mut self, by: isize) {
        self.view.pan_by(by);
    }

    pub fn to_top(&mut self) {
        self.view.to_top();
    }

    pub fn to_bottom(&mut self) {
        self.view.to_bottom();
    }

    /// The previous or next conflict's first row. From inside a region the
    /// walk starts at its own edge; from context between regions, at the
    /// first region on the far side of the keyboard.
    pub fn jump_region(&mut self, by: isize) {
        if self.region_rows.is_empty() {
            return;
        }
        let at = self.view.cursor();
        let target = match by {
            by if by > 0 => self
                .region_rows
                .iter()
                .find(|&&row| row > at)
                .or(self.region_rows.last())
                .copied(),
            _ => self
                .region_rows
                .iter()
                .rev()
                .find(|&&row| row < at)
                .or(self.region_rows.first())
                .copied(),
        };
        if let Some(row) = target {
            self.view.go_to(row);
        }
    }

    // ----------------------------------------------------------------- the verbs

    /// What `merge.take-side` aims at: the region under the keyboard and
    /// the half of it the keyboard is on. The marker lines and the base are
    /// nobody's half — a choice asked for there is refused, not guessed.
    pub fn under_the_keyboard(&self) -> Option<(usize, Answer)> {
        let place = self.places.get(self.view.cursor())?.place?;
        let (region, part) = place;
        let half = match part {
            Part::Ours => Half::Ours,
            Part::Theirs => Half::Theirs,
            _ => return None,
        };
        Some((region, half.answer()))
    }

    /// A named answer to the region the keyboard is on, wherever in it the
    /// keyboard sits — `o`/`t`/`b` do not ask which half, only which
    /// region. `None` when the keyboard is on context between regions or
    /// the file has none left.
    pub fn current_region(&self) -> Option<usize> {
        let (region, _) = self.places.get(self.view.cursor())?.place?;
        Some(region)
    }

    /// The snapshot an answer must take before it runs — the file and the
    /// stages exactly as they stand in this session. Held by the caller
    /// until the job is submitted; a refused job pushes nothing.
    pub fn snapshot(&self) -> UndoStep {
        UndoStep {
            bytes: self.file.bytes.clone(),
            stages: self.stages.clone(),
        }
    }

    /// Records the snapshot a successful answer took. Called only after the
    /// job was accepted by the queue.
    pub fn push_undo(&mut self, step: UndoStep) {
        self.undo.push(step);
    }

    /// The snapshot `merge.undo` restores, taking it off the stack.
    /// `None` when the session has answered nothing — the honest refusal,
    /// not a silent no-op.
    pub fn pop_undo(&mut self) -> Option<UndoStep> {
        self.undo.pop()
    }

    // ----------------------------------------------------------------- the mouse

    pub fn press(&mut self, _col: usize, row: usize, _extend: bool, _host: &Host) {
        if let Some(index) = self.view.row_at(row) {
            self.view.go_to(index);
            // The drag handler is armed by the press, the way the diff
            // pane's is: a press that becomes a drag walks the keyboard.
            self.dragging = true;
        }
    }

    pub fn drag(&mut self, row: isize, _host: &Host) {
        if !self.dragging {
            return;
        }
        let height = self.view.height() as isize;
        let row = match row {
            r if r < 0 => {
                self.view.scroll_by(r);
                0
            }
            r if r >= height => {
                self.view.scroll_by(r - height + 1);
                height.saturating_sub(1).max(0)
            }
            r => r,
        };
        if let Some(index) = self.view.row_at(row as usize) {
            self.view.go_to(index);
        }
    }

    pub fn release(&mut self) {
        self.dragging = false;
    }

    /// `copy.selection` here: nothing. A conflict file is not a range the
    /// verbs act on, and the row's text is what the diff pane's copy is for.
    pub fn selection(&self) -> String {
        String::new()
    }

    pub fn select_all(&mut self) {}

    pub fn select_none(&mut self) -> bool {
        false
    }

    // ---------------------------------------------------------------- the drawing

    /// Draws the visible rows into `screen`, at `x` of row `y` onward.
    ///
    /// The colours are the diff's own two hues, because the halves *are* an
    /// addition and a deletion waiting to be chosen: ours wears the
    /// additions' ink, theirs the removals', the markers the gutter's, the
    /// base faint. Backgrounds stay the pane's — the choice is not yet
    /// made, and a row painted added-green before anybody chose it would
    /// say something false.
    pub fn paint(&self, screen: &mut Screen, x: usize, y: usize, focused: bool, host: &Host) {
        let theme = &host.theme;
        let blank = Ink::new(theme.chrome.dim, theme.chrome.bg);
        if self.places.is_empty() {
            let mut pen = screen.span(y, x, self.cols);
            pen.put("empty file", Ink::new(theme.chrome.faint, theme.chrome.bg));
            pen.wash(blank);
            return;
        }
        // The marker lines, lossily decoded once per open and replace and
        // drawn from the cache here: the bytes are the region's, the string
        // is only what is drawn.
        for i in 0..self.view.height() {
            let row = y + i;
            let mut pen = screen.span(row, x, self.cols);
            let Some(vis) = self.view.row_at(i) else {
                pen.wash(blank);
                continue;
            };
            let Some(line) = self.places.get(vis) else {
                pen.wash(blank);
                continue;
            };
            let bg = match focused && vis == self.view.cursor() {
                true => theme.chrome.selection_bg,
                false => theme.chrome.bg,
            };
            let ink = match line.place {
                Some((_, Part::Ours)) => Ink::new(theme.diff.adds_fg, bg),
                Some((_, Part::Theirs)) => Ink::new(theme.diff.dels_fg, bg),
                Some((_, Part::Base)) => Ink {
                    italic: true,
                    ..Ink::new(theme.chrome.faint, bg)
                },
                Some((_, Part::Open))
                | Some((_, Part::BaseMarker))
                | Some((_, Part::Sep))
                | Some((_, Part::Close)) => Ink::new(theme.diff.gutter_fg, bg),
                None => Ink::new(theme.chrome.fg, bg),
            };
            if let Some(text) = self.lines.get(vis) {
                pen.put(text, ink);
            }
            pen.wash(ink);
        }
    }

    /// The bar at the edge geometry hands it. The pane does not choose.
    pub fn paint_bar(
        &self,
        screen: &mut Screen,
        x: usize,
        divider: Option<usize>,
        y: usize,
        host: &Host,
    ) {
        scrollbar::paint(screen, self.bar, x, divider, y, &self.view, host);
    }

    /// One line describing the pane: the conflict the keyboard is in, over
    /// how many — or the word that says the file has none left to answer.
    pub fn status(&self) -> String {
        if !self.file.is_conflicted() {
            return "no conflict markers — resolved".into();
        }
        let shown = self.places.len();
        let here = self
            .places
            .get(self.view.cursor())
            .and_then(|row| row.place)
            .map(|(region, _)| region);
        let count = self.region_rows.len();
        match here {
            Some(region) => format!(
                "{}/{shown} · conflict {}/{count}",
                self.view.cursor() + 1,
                region + 1,
            ),
            None => format!("{}/{shown} · between conflicts", self.view.cursor() + 1),
        }
    }
}

/// Flattens the file into display rows, one per line of the file.
fn flatten(file: &ConflictFile) -> Vec<Row> {
    let lines = gitten_core::conflict::line_count(&file.bytes);
    let mut places = Vec::with_capacity(lines);
    // Regions arrive in close order; an inner region closes before the
    // outer holding it. The rows need file order, and a region starting
    // inside an already-drawn one keeps the outer's rows — its lines are
    // already on the pane, and drawing them twice would double the file.
    // The skipped region keeps its close-order index in the rows the
    // outer drew, but the answer verbs refuse a nested file anyway.
    let mut order: Vec<usize> = (0..file.regions.len()).collect();
    order.sort_by_key(|&ri| file.regions[ri].start);
    let mut at = 0usize;
    for ri in order {
        let region = &file.regions[ri];
        if region.start < at {
            continue;
        }
        for _ in at..region.start {
            places.push(Row { place: None });
        }
        places.push(Row {
            place: Some((ri, Part::Open)),
        });
        for _ in region.ours() {
            places.push(Row {
                place: Some((ri, Part::Ours)),
            });
        }
        if region.base.is_some() {
            places.push(Row {
                place: Some((ri, Part::BaseMarker)),
            });
            for _ in region.base_lines().unwrap_or_default() {
                places.push(Row {
                    place: Some((ri, Part::Base)),
                });
            }
        }
        places.push(Row {
            place: Some((ri, Part::Sep)),
        });
        for _ in region.theirs() {
            places.push(Row {
                place: Some((ri, Part::Theirs)),
            });
        }
        places.push(Row {
            place: Some((ri, Part::Close)),
        });
        at = region.end + 1;
    }
    for _ in at..lines {
        places.push(Row { place: None });
    }
    places
}

/// The lines of the file, lossily decoded — the display's only conversion.
fn display_lines(bytes: &[u8]) -> Vec<String> {
    bytes
        .split_inclusive(|b| *b == b'\n')
        .map(|line| {
            // The carriage return is a line ending, not content: a CRLF
            // file that drew it would wash every row with a glyph.
            let line = line.strip_suffix(b"\n").unwrap_or(line);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            String::from_utf8_lossy(line).into_owned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::Screen;

    /// Two regions, the shape every assertion below is pinned to.
    const TWO: &[u8] = b"top\n<<<<<<< ours\nours one\n=======\ntheirs one\n>>>>>>> them\nmid\n\
<<<<<<< ours\nours two\n=======\ntheirs two\n>>>>>>> them\ntail\n";

    fn stages() -> Vec<UnmergedStage> {
        vec![
            UnmergedStage {
                mode: "100644".into(),
                oid: "a".repeat(40),
                stage: 2,
            },
            UnmergedStage {
                mode: "100644".into(),
                oid: "b".repeat(40),
                stage: 3,
            },
        ]
    }

    fn view() -> Merging {
        Merging::new(
            PathBytes::from("f.txt"),
            ConflictFile::parse(PathBytes::from("f.txt"), TWO.to_vec()),
            stages(),
        )
    }

    #[test]
    fn the_rows_say_which_region_and_half_the_keyboard_is_on() {
        let mut v = view();
        v.resize(40, 13);
        assert_eq!(v.status(), "1/13 · between conflicts");
        v.down();
        v.down();
        assert_eq!(
            v.under_the_keyboard(),
            Some((0, Answer::Ours)),
            "row 2 is region 1's ours line"
        );
        assert_eq!(v.status(), "3/13 · conflict 1/2");
        // The seam refuses a side: opener, separator, close, base.
        v.up();
        assert_eq!(v.under_the_keyboard(), None);
    }

    #[test]
    fn the_conflict_jump_walks_regions_and_stops() {
        let mut v = view();
        v.resize(40, 13);
        v.jump_region(1);
        assert_eq!(v.current_region(), Some(0));
        v.jump_region(1);
        assert_eq!(v.current_region(), Some(1));
        v.jump_region(1);
        assert_eq!(v.current_region(), Some(1), "the last region holds");
        v.jump_region(-1);
        assert_eq!(v.current_region(), Some(0), "the first region holds");
    }

    #[test]
    fn the_undo_stack_is_the_session_and_it_is_honest_about_being_empty() {
        let mut v = view();
        v.resize(40, 13);
        assert_eq!(v.pop_undo(), None, "nothing answered yet");
        v.push_undo(v.snapshot());
        let step = v.pop_undo().expect("the snapshot came back");
        assert_eq!(step.bytes, TWO.to_vec());
        assert_eq!(step.stages.len(), 2);
        assert_eq!(v.pop_undo(), None);
    }

    #[test]
    fn a_resolved_file_draws_quiet_and_refuses_every_answer() {
        let mut v = Merging::new(
            PathBytes::from("f.txt"),
            ConflictFile::parse(PathBytes::from("f.txt"), b"all settled\n".to_vec()),
            Vec::new(),
        );
        v.resize(40, 3);
        assert!(v.status().contains("no conflict markers"));
        assert_eq!(v.under_the_keyboard(), None);
        assert_eq!(v.current_region(), None);

        // The content is content even with nothing to answer; the state
        // lives in the status line, which is the one place a reader is
        // told the markers are gone.
        let host = Host::new();
        let mut screen = Screen::new(40, 3);
        v.paint(&mut screen, 0, 0, true, &host);
        assert!(
            screen.row_text(0).contains("all settled"),
            "{:?}",
            screen.row_text(0)
        );
    }

    /// A merge inside a rebase inside a merge: the inner region closes
    /// first, so it sorts before the outer holding it.
    const NESTED: &[u8] = b"<<<<<<<<< outer\nouter ours\n<<<<<<< inner\ninner ours\n=======\ninner theirs\n>>>>>>> inner\nouter theirs\n=========\nouter theirs side\n>>>>>>>>> outer\n";

    fn nested_view() -> Merging {
        Merging::new(
            PathBytes::from("f.txt"),
            ConflictFile::parse(PathBytes::from("f.txt"), NESTED.to_vec()),
            stages(),
        )
    }

    #[test]
    fn a_nested_file_flattens_without_duplicating_rows() {
        let v = nested_view();
        assert!(v.is_nested());
        assert_eq!(
            v.places.len(),
            11,
            "eleven lines, eleven rows — the inner region keeps the outer's"
        );
        assert_eq!(
            v.region_rows,
            vec![0],
            "only the outer region's opener is a jump target"
        );
        // The outer drew lines 0..=10, so the inner's own rows are the
        // outer's: row 3 sits in the inner's ours text under the outer's
        // index.
        assert_eq!(v.places[3].place, Some((1, Part::Ours)));
        // And it draws: the cached lines and the rows cover each other.
        let host = Host::new();
        let mut screen = Screen::new(40, 11);
        let mut v = nested_view();
        v.resize(40, 11);
        v.paint(&mut screen, 0, 0, true, &host);
        assert!(
            screen.row_text(0).contains("<<<<<<<<<"),
            "{:?}",
            screen.row_text(0)
        );
    }

    #[test]
    fn a_press_arms_the_drag_and_the_drag_walks_the_keyboard() {
        let host = Host::new();
        let mut v = view();
        v.resize(40, 13);
        v.press(0, 2, false, &host);
        v.drag(5, &host);
        assert_eq!(v.view.cursor(), 5, "the press became a drag");
        v.release();
        v.drag(9, &host);
        assert_eq!(v.view.cursor(), 5, "released, the drag is dead again");
    }

    #[test]
    fn ours_wears_the_addition_ink_and_theirs_the_removal_ink() {
        let host = Host::new();
        let mut v = view();
        v.resize(40, 13);
        let mut screen = Screen::new(40, 13);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        v.paint(&mut screen, 0, 0, false, &host);
        // Row 2 is ours, row 4 theirs: the diff's two hues, on the pane's
        // own background — the choice is not yet made, so no row paints an
        // added or removed background.
        let ours = screen.ink(0, 2).unwrap();
        assert_eq!(ours.fg, host.theme.diff.adds_fg);
        assert_eq!(ours.bg, host.theme.chrome.bg);
        let theirs = screen.ink(0, 4).unwrap();
        assert_eq!(theirs.fg, host.theme.diff.dels_fg);
        // The markers are the gutter's ink.
        assert_eq!(screen.ink(0, 1).unwrap().fg, host.theme.diff.gutter_fg);
        // Context is the pane's ordinary text.
        assert_eq!(screen.ink(0, 0).unwrap().fg, host.theme.chrome.fg);
    }
}
