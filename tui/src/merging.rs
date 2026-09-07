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
//! resolved. The stack is this view's own session: it dies with the view,
//! and it dies when the file stops being conflicted — a stack that promises
//! to undo yesterday's answer onto today's resolved file is a stack that
//! lies. An external change *between* an answer and its undo is overwritten
//! by the undo: the snapshot is what is restored, and that is the
//! documentation, not an accident.

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
    /// clamps; the undo stack survives a refresh of an *unresolved* file —
    /// the same conflict, re-read, is the session's own file — and dies
    /// when the file has none left, because there is no honest answer an
    /// old snapshot gives a resolved one. See the [module
    /// documentation](self) for the boundary this draws.
    pub fn replace(&mut self, file: ConflictFile, stages: Vec<UnmergedStage>) {
        self.dragging = false;
        let (cursor, top) = (self.view.cursor(), self.view.top());
        self.file = file;
        self.stages = stages;
        self.places = flatten(&self.file);
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
        if !self.file.is_conflicted() {
            self.undo.clear();
        }
    }

    /// Whether the file still carries regions — the gate every answer verb
    /// reads before anything is submitted.
    pub fn is_conflicted(&self) -> bool {
        self.file.is_conflicted()
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
        // The marker lines, lossily decoded once per line here and nowhere
        // else: the bytes are the region's, the string is only what is drawn.
        let lines = display_lines(&self.file.bytes);
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
            if let Some(text) = lines.get(vis) {
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
    let mut at = 0usize;
    for (ri, region) in file.regions.iter().enumerate() {
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
        .map(|line| String::from_utf8_lossy(line.strip_suffix(b"\n").unwrap_or(line)).into_owned())
        .collect()
}
