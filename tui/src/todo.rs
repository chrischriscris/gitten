//! The rebase plan, on screen and under the keyboard.
//!
//! Interactive rebase is git's own machinery for rewriting a stretch of
//! history, and the whole of a client's part in it is one editable list:
//! which commit is replayed, folded, reworded, dropped, and in what order.
//! [`gitten_core::rebase::Plan`] is that list as data — the constraints,
//! the reordering and the bytes git reads all live there, shared with every
//! other client — and this is drawing and input over it, which is all a
//! client is.
//!
//! It floats, and it owns the keyboard while it does. A press it does not
//! name must run nothing underneath, for the same reason the help panel
//! works that way and rather more urgently: the keys underneath rewrite
//! history.
//!
//! The rows are newest first, exactly as the commit list draws them, which
//! is [`Plan`]'s order too — so "fold into the commit below" means one
//! thing in the model, on the screen and in the sentence a reader is told.

use crate::screen::{Ink, Screen};
use gitten_core::command::Availability;
use gitten_core::host::Host;
use gitten_core::rebase::{Action, Plan};

/// The open plan: what it says, and the row the keyboard is on.
pub struct Todo {
    plan: Plan,
    cursor: usize,
    top: usize,
    /// How many rows the last paint had room for, so the moves can scroll
    /// the window without a view that has never been drawn guessing at one.
    shown: usize,
}

impl Todo {
    pub fn new(plan: Plan) -> Self {
        Self {
            plan,
            cursor: 0,
            top: 0,
            shown: 1,
        }
    }

    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    /// The plan, taken — what the run hands to the queue.
    pub fn into_plan(self) -> Plan {
        self.plan
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The commit the keyboard is on, as a person reads it: `a1b2c3d fixed
    /// the thing`. For the sentences the prompts and refusals build.
    pub fn selected_label(&self) -> String {
        match self.plan.entries().get(self.cursor) {
            Some(entry) => format!("{} {}", entry.short, entry.subject),
            None => String::new(),
        }
    }

    /// The subject of the row the keyboard is on — what a reword field
    /// opens prefilled with.
    pub fn selected_subject(&self) -> String {
        self.plan
            .entries()
            .get(self.cursor)
            .map(|e| e.subject.clone())
            .unwrap_or_default()
    }

    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.plan.len().saturating_sub(1));
        self.follow();
    }

    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
        self.follow();
    }

    pub fn to_top(&mut self) {
        self.cursor = 0;
        self.follow();
    }

    pub fn to_bottom(&mut self) {
        self.cursor = self.plan.len().saturating_sub(1);
        self.follow();
    }

    /// Sets the row's action, answering the refusal when there is one — the
    /// oldest row has nothing below it to fold into, and says so where the
    /// press happened.
    pub fn set_action(&mut self, action: Action) -> Result<(), String> {
        self.plan.set_action(self.cursor, action)
    }

    /// Rewords the row with bytes a reader typed.
    pub fn set_message(&mut self, message: Vec<u8>) -> Result<(), String> {
        self.plan.set_message(self.cursor, message)
    }

    /// Moves the row and follows it with the keyboard: the eye is on a
    /// commit, not on a position, and a move that left the cursor behind
    /// would have the next press act on the wrong one.
    pub fn move_by(&mut self, up: bool) -> Result<(), String> {
        let landed = match up {
            true => self.plan.move_up(self.cursor)?,
            false => self.plan.move_down(self.cursor)?,
        };
        self.cursor = landed;
        self.follow();
        Ok(())
    }

    /// git's `--autosquash` over the plan, answering the sentence to say.
    pub fn autosquash(&mut self) -> String {
        // The row under the keyboard is followed by its sha, because
        // autosquash is exactly the operation that moves rows around it.
        let held = self.plan.entries().get(self.cursor).map(|e| e.sha.clone());
        let moved = self.plan.autosquash();
        if let Some(sha) = held {
            if let Some(index) = self.plan.index_of(&sha) {
                self.cursor = index;
                self.follow();
            }
        }
        match moved {
            0 => "no fixup! or squash! subject names a commit in this plan".into(),
            1 => "1 commit folded onto the one it names".into(),
            n => format!("{n} commits folded onto the ones they name"),
        }
    }

    /// The status sentence: what the plan will do, in the words a reader
    /// needs before pressing enter.
    pub fn summary(&self) -> String {
        let mut picks = 0;
        let mut changed = 0;
        for entry in self.plan.entries() {
            match entry.action {
                Action::Pick => picks += 1,
                _ => changed += 1,
            }
        }
        let n = self.plan.len();
        let head = match n {
            1 => "1 commit".to_string(),
            n => format!("{n} commits"),
        };
        match changed {
            0 => format!("{head} from {}, all picked", self.plan.base()),
            _ => format!(
                "{head} from {} — {changed} changed, {picks} picked",
                self.plan.base()
            ),
        }
    }

    fn follow(&mut self) {
        let shown = self.shown.max(1);
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + shown {
            self.top = self.cursor + 1 - shown;
        }
        // And never past the last window that has rows in it: a move made
        // before the first paint knows only that one row fits, and a box
        // that then drew from there would leave its own top half empty.
        self.top = self.top.min(self.plan.len().saturating_sub(shown));
    }

    /// The box, over the body: a title, the rows that fit, and the summary
    /// under them.
    ///
    /// Every column here is measured rather than assumed — the action word
    /// is the widest of the ones actually in the plan, which keeps a plan of
    /// picks from carrying six columns of space for a word nobody chose.
    pub fn paint(
        &mut self,
        screen: &mut Screen,
        y: usize,
        height: usize,
        host: &Host,
        keys: &Availability,
    ) {
        let _ = keys;
        if height < 4 || self.plan.is_empty() {
            return;
        }
        let c = &host.theme.chrome;
        let title = " rebase plan ";
        // Three rows of furniture: the title, the summary and the frame's
        // own bottom margin.
        let shown = height.saturating_sub(4).max(1).min(self.plan.len());
        self.shown = shown;
        self.follow();
        let action_w = self
            .plan
            .entries()
            .iter()
            .map(|e| e.action.word().len())
            .max()
            .unwrap_or(4);
        let sha_w = self
            .plan
            .entries()
            .iter()
            .map(|e| e.short.len())
            .max()
            .unwrap_or(7);
        let text_w = self
            .plan
            .entries()
            .iter()
            .map(|e| crate::screen::width(&e.subject))
            .max()
            .unwrap_or(0);
        let width = (action_w + sha_w + text_w + 4)
            .max(crate::screen::width(&self.summary()) + 2)
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
                pen.put(&self.summary(), Ink::new(c.dim, c.status_bg));
                pen.wash(Ink::new(c.dim, c.status_bg));
                continue;
            }
            if i == shown + 2 {
                pen.wash(Ink::new(c.dim, c.bg));
                continue;
            }
            let source = self.top + (i - 1);
            let Some(entry) = self.plan.entries().get(source) else {
                pen.wash(Ink::new(c.dim, c.bg));
                continue;
            };
            let bg = match source == self.cursor {
                true => c.selection_bg,
                false => c.bg,
            };
            // A row that is not a plain pick is the whole point of the
            // screen, so the word carries the accent and a pick does not.
            // A drop goes the other way, to `faint`: the commit is leaving,
            // and a colour that shouted would read as a failure instead.
            let word_ink = match entry.action {
                Action::Pick => Ink::new(c.dim, bg),
                Action::Drop => Ink::new(c.faint, bg),
                _ => Ink::new(c.accent, bg),
            };
            // Left-aligned, padded to the widest word actually in the plan:
            // this is a column of words and not of numbers, and the eye runs
            // down their first letter.
            pen.put(&format!("{:<action_w$}", entry.action.word()), word_ink);
            pen.put(" ", Ink::new(c.dim, bg));
            pen.put(&entry.short, Ink::new(c.dim, bg));
            pen.put(" ", Ink::new(c.fg, bg));
            let subject = match &entry.message {
                // A reworded row shows the message it will land under, not
                // the one it has: the plan is what is about to happen.
                Some(message) => String::from_utf8_lossy(message)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string(),
                None => entry.subject.clone(),
            };
            pen.put(&subject, Ink::new(c.fg, bg));
            pen.wash(Ink::new(c.fg, bg));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitten_core::Commit;

    fn window() -> Vec<Commit> {
        ["head", "mid", "under", "root"]
            .iter()
            .enumerate()
            .map(|(i, name)| Commit {
                sha: format!("{name}-sha"),
                short: format!("{name}00"),
                parents: match i + 1 < 4 {
                    true => vec![format!("{}-sha", ["head", "mid", "under", "root"][i + 1])],
                    false => vec![],
                }
                .into_boxed_slice(),
                author: "".into(),
                timestamp: 0,
                subject: (*name).into(),
            })
            .collect()
    }

    fn open() -> Todo {
        Todo::new(Plan::over(&window(), 2).expect("a straight window"))
    }

    #[test]
    fn the_keyboard_follows_the_commit_a_move_carried() {
        let mut todo = open();
        assert_eq!(todo.selected_subject(), "head");
        todo.move_by(false).expect("down");
        // The eye is on a commit, not on a row: the cursor went with it.
        assert_eq!(todo.cursor(), 1);
        assert_eq!(todo.selected_subject(), "head");
        todo.move_by(true).expect("up");
        assert_eq!(todo.cursor(), 0);

        // The edges refuse in the plan's own words.
        assert!(todo.move_by(true).is_err());
        todo.to_bottom();
        assert!(todo.move_by(false).is_err());
    }

    #[test]
    fn the_summary_counts_what_the_plan_changes() {
        let mut todo = open();
        assert!(todo.summary().contains("all picked"), "{}", todo.summary());
        todo.down();
        todo.set_action(Action::Drop).expect("a drop");
        let summary = todo.summary();
        assert!(summary.contains("1 changed"), "{summary}");
        assert!(summary.contains("2 picked"), "{summary}");
    }

    #[test]
    fn the_oldest_row_refuses_a_fold_where_the_press_happened() {
        let mut todo = open();
        todo.to_bottom();
        let err = todo.set_action(Action::Squash).expect_err("refused");
        assert!(err.contains("nothing below it"), "{err}");
    }

    #[test]
    fn autosquash_says_what_it_did_and_keeps_the_keyboard_on_its_commit() {
        let mut commits = window();
        commits[0].subject = "fixup! under".into();
        let mut todo = Todo::new(Plan::over(&commits, 2).expect("a window"));
        assert_eq!(todo.selected_subject(), "fixup! under");
        let said = todo.autosquash();
        assert!(said.contains('1'), "{said}");
        // The row moved to sit on the commit it names, and the keyboard
        // went with it.
        assert_eq!(todo.cursor(), 1);
        assert_eq!(todo.selected_subject(), "fixup! under");
        assert_eq!(todo.plan().entries()[1].action, Action::Fixup);

        let mut todo = open();
        assert!(todo.autosquash().contains("no fixup!"));
    }

    #[test]
    fn the_plan_draws_its_rows_and_its_summary() {
        let mut screen = Screen::new(60, 12);
        let host = Host::new();
        let mut todo = open();
        todo.down();
        todo.set_action(Action::Squash).expect("a fold");
        todo.paint(&mut screen, 0, 10, &host, &Availability::strict());
        let body: Vec<String> = (0..12).map(|y| screen.row_text(y)).collect();
        let text = body.join("\n");
        assert!(text.contains("rebase plan"), "{text}");
        assert!(text.contains("pick   head00 head"), "{text}");
        assert!(text.contains("squash mid00 mid"), "{text}");
        assert!(text.contains("1 changed"), "{text}");
    }

    #[test]
    fn a_reworded_row_draws_the_message_it_will_land_under() {
        let mut screen = Screen::new(60, 12);
        let host = Host::new();
        let mut todo = open();
        todo.set_message(b"a better subject".to_vec())
            .expect("a message");
        todo.paint(&mut screen, 0, 10, &host, &Availability::strict());
        let text: String = (0..12)
            .map(|y| screen.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("reword head00 a better subject"), "{text}");
        assert!(!text.contains("reword head00 head"), "{text}");
    }
}
