//! The patch builder, over the clipboard and under the keyboard.
//!
//! Picking carries hunks from any diff onto
//! [`PatchClipboard`](gitten_core::patchclip::PatchClipboard); this is
//! drawing and input over what was picked, which is all a client is. It
//! floats, and it owns the keyboard while it does — a press it does not
//! name must run nothing underneath, on exactly the rebase plan's terms,
//! because the clipboard it edits is one keypress from landing somewhere.
//!
//! One row per file header plus one per hunk. A file header toggles the
//! whole file; a hunk row toggles the hunk. The toggles run on the
//! clipboard itself, so what the builder shows included is exactly what an
//! apply would carry — there is no second copy to drift.

use crate::screen::{Ink, Screen};
use gitten_core::command::Availability;
use gitten_core::host::Host;
use gitten_core::patchclip::PatchClipboard;

/// The open builder: the row the keyboard is on, and the window around
/// it. The clipboard stays on the app — this holds no copy of it, so a
/// pick made while the builder stands (there is none; the builder owns
/// the keyboard) could not stale it.
pub struct PatchBuilder {
    cursor: usize,
    top: usize,
    /// How many rows the last paint had room for, so the moves can scroll
    /// the window without a view that has never been drawn guessing at one.
    shown: usize,
}

impl Default for PatchBuilder {
    fn default() -> Self {
        Self {
            cursor: 0,
            top: 0,
            shown: 1,
        }
    }
}

impl PatchBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// File headers plus hunk rows, in clipboard order.
    fn rows(clip: &PatchClipboard) -> usize {
        clip.files().iter().map(|f| 1 + f.hunks.len()).sum()
    }

    /// Which file — and which of its hunks, `None` for the header row — a
    /// row addresses. `None` past the last row, which a move never leaves
    /// standing: every move clamps first.
    fn locate(clip: &PatchClipboard, row: usize) -> Option<(usize, Option<usize>)> {
        let mut at = 0;
        for (fi, file) in clip.files().iter().enumerate() {
            if at == row {
                return Some((fi, None));
            }
            at += 1;
            for (hi, _) in file.hunks.iter().enumerate() {
                if at == row {
                    return Some((fi, Some(hi)));
                }
                at += 1;
            }
        }
        None
    }

    fn clamp(&mut self, clip: &PatchClipboard) {
        let rows = Self::rows(clip);
        if rows == 0 {
            self.cursor = 0;
            self.top = 0;
            return;
        }
        self.cursor = self.cursor.min(rows - 1);
        self.follow(clip);
    }

    fn follow(&mut self, clip: &PatchClipboard) {
        let shown = self.shown.max(1);
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + shown {
            self.top = self.cursor + 1 - shown;
        }
        self.top = self.top.min(Self::rows(clip).saturating_sub(shown));
    }

    pub fn down(&mut self, clip: &PatchClipboard) {
        self.cursor = self
            .cursor
            .saturating_add(1)
            .min(Self::rows(clip).saturating_sub(1));
        self.follow(clip);
    }

    pub fn up(&mut self, clip: &PatchClipboard) {
        self.cursor = self.cursor.saturating_sub(1);
        self.follow(clip);
    }

    pub fn to_top(&mut self, clip: &PatchClipboard) {
        self.cursor = 0;
        self.follow(clip);
    }

    pub fn to_bottom(&mut self, clip: &PatchClipboard) {
        self.cursor = Self::rows(clip).saturating_sub(1);
        self.follow(clip);
    }

    /// Space on the keyboard's row: a hunk row toggles the hunk, a header
    /// row the whole file. The sentence names what moved and what it
    /// means for the next apply.
    pub fn toggle(&mut self, clip: &mut PatchClipboard) -> String {
        self.clamp(clip);
        match Self::locate(clip, self.cursor) {
            Some((fi, Some(hi))) => {
                let path = clip.files()[fi].path.clone();
                match clip.toggle_hunk(&path, hi) {
                    Some(true) => format!("{path} hunk {} included", hi + 1),
                    Some(false) => format!("{path} hunk {} excluded", hi + 1),
                    None => "nothing there to toggle".into(),
                }
            }
            Some((fi, None)) => {
                let path = clip.files()[fi].path.clone();
                match clip.toggle_file(&path) {
                    Some(true) => format!("{path} wholly included"),
                    Some(false) => format!("{path} wholly excluded"),
                    None => "nothing there to toggle".into(),
                }
            }
            None => "the patch clipboard is empty".into(),
        }
    }

    /// `a` on the keyboard's row: the file it belongs to, header or hunk
    /// alike — the press names a file, so it answers for all of it.
    pub fn toggle_file_here(&mut self, clip: &mut PatchClipboard) -> String {
        self.clamp(clip);
        match Self::locate(clip, self.cursor) {
            Some((fi, _)) => {
                let path = clip.files()[fi].path.clone();
                match clip.toggle_file(&path) {
                    Some(true) => format!("{path} wholly included"),
                    Some(false) => format!("{path} wholly excluded"),
                    None => "nothing there to toggle".into(),
                }
            }
            None => "the patch clipboard is empty".into(),
        }
    }

    /// `D` on the keyboard's row: the file leaves the clipboard whole.
    pub fn drop_file_here(&mut self, clip: &mut PatchClipboard) -> String {
        self.clamp(clip);
        match Self::locate(clip, self.cursor) {
            Some((fi, _)) => {
                let path = clip.files()[fi].path.clone();
                if clip.drop_file(&path) {
                    self.clamp(clip);
                    format!("{path} off the patch")
                } else {
                    "nothing there to drop".into()
                }
            }
            None => "the patch clipboard is empty".into(),
        }
    }

    /// The box, over the body: a title, the rows that fit, and the status
    /// under them. An empty clipboard draws one honest row, not a box of
    /// nothing — picking is a keypress away and the row says which.
    pub fn paint(
        &mut self,
        screen: &mut Screen,
        y: usize,
        height: usize,
        host: &Host,
        keys: &Availability,
        clip: &PatchClipboard,
    ) {
        let _ = keys;
        if height < 4 {
            return;
        }
        let c = &host.theme.chrome;
        let title = " patch ";
        let rows = Self::rows(clip);
        // Three rows of furniture: the title, the status and the frame's
        // own bottom margin. An empty clipboard still takes its one row.
        let shown = height.saturating_sub(4).max(1).min(rows.max(1));
        self.shown = shown;
        self.follow(clip);
        let path_w = clip
            .files()
            .iter()
            .map(|f| crate::screen::width(&f.path))
            .max()
            .unwrap_or(0);
        let width = (path_w + 24)
            .max(crate::screen::width(&clip.status()) + 2)
            .max(title.len())
            .min(screen.width().saturating_sub(2))
            .max(18);
        for i in 0..shown + 3 {
            let at = y + 1 + i;
            let mut pen = screen.span(at, 1, width);
            if i == 0 {
                pen.put(title, Ink::new(c.accent, c.status_bg));
                pen.wash(Ink::new(c.dim, c.status_bg));
                continue;
            }
            if i == shown + 1 {
                pen.put(&clip.status(), Ink::new(c.dim, c.status_bg));
                pen.wash(Ink::new(c.dim, c.status_bg));
                continue;
            }
            if i == shown + 2 {
                pen.wash(Ink::new(c.dim, c.bg));
                continue;
            }
            if rows == 0 {
                pen.put(
                    "empty — pick hunks with p in any diff",
                    Ink::new(c.dim, c.bg),
                );
                pen.wash(Ink::new(c.dim, c.bg));
                continue;
            }
            let source = self.top + (i - 1);
            let Some((fi, hi)) = Self::locate(clip, source) else {
                pen.wash(Ink::new(c.dim, c.bg));
                continue;
            };
            let file = &clip.files()[fi];
            let bg = match source == self.cursor {
                true => c.selection_bg,
                false => c.bg,
            };
            match hi {
                None => {
                    let included = file.hunks.iter().filter(|h| h.included).count();
                    let mark = match included == file.hunks.len() {
                        true => "[x]",
                        false => match included {
                            0 => "[ ]",
                            _ => "[-]",
                        },
                    };
                    pen.put(mark, Ink::new(c.accent, bg));
                    pen.put(" ", Ink::new(c.dim, bg));
                    pen.put(&file.path, Ink::new(c.fg, bg));
                    pen.put(" ", Ink::new(c.dim, bg));
                    pen.put(
                        &format!("{} of {}", included, file.hunks.len()),
                        Ink::new(c.dim, bg),
                    );
                    pen.put(" ", Ink::new(c.dim, bg));
                    pen.put(&file.anchor.word(), Ink::new(c.faint, bg));
                    pen.wash(Ink::new(c.fg, bg));
                }
                Some(hi) => {
                    let hunk = &file.hunks[hi];
                    let mark = match hunk.included {
                        true => "[x]",
                        false => "[ ]",
                    };
                    let mark_ink = match hunk.included {
                        true => Ink::new(c.accent, bg),
                        false => Ink::new(c.faint, bg),
                    };
                    pen.put("  ", Ink::new(c.dim, bg));
                    pen.put(mark, mark_ink);
                    pen.put(" ", Ink::new(c.dim, bg));
                    pen.put(&hunk.hunk.header, Ink::new(c.fg, bg));
                    pen.wash(Ink::new(c.fg, bg));
                }
            }
        }
    }
}
