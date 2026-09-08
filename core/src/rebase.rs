//! The interactive-rebase todo file: git's plan for a rewrite, as data.
//!
//! `git rebase -i` writes a plan — one line per commit it means to replay —
//! into `.git/rebase-merge/git-rebase-todo` and opens it in an editor. What a
//! client does with that is the whole of "interactive rebase": reorder the
//! lines and history is reordered, delete one and the commit is dropped. So
//! the plan is modelled here, purely, the way [`crate::patch`] models a patch:
//! parse git's bytes, decide, emit git's bytes, and let the acquisition layer
//! aim the result at a repository it never interprets.
//!
//! Three modelling rules, inherited from the rest of the crate:
//!
//! **Names are bytes.** An abbreviated SHA and an `exec` command travel
//! exactly as git wrote them. A SHA is hex today and a command is shell
//! tomorrow; neither is ours to decode.
//!
//! **Tolerance keeps everything.** A line this module does not fully
//! understand — a comment from git's header, an action added by a newer git,
//! a spelling it never wrote — is kept verbatim and emitted verbatim. A
//! round-trip through this module loses nothing, which is the only safe
//! posture for a file that *is* somebody's history while it is open.
//!
//! **The plan is decided once.** Which commit folds into which lives in
//! [`compose`], beside the [`Commit`](crate::Commit) model it reads — the
//! same reason pairing lives in `crate::align`. A client that reshuffles the
//! plan differently is a client whose squash landed somewhere else.

use crate::Commit;

// --------------------------------------------------------------------- actions

/// One action word git's todo file understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Replay the commit.
    Pick,
    /// Replay it, but stop to edit the message. Never emitted as this word:
    /// git opens `GIT_EDITOR` on it and nothing here can answer that prompt,
    /// so a [`Plan`] carries the new message itself and reaches git as a
    /// pick plus an `exec`. Parsed, because somebody else's todo file may
    /// hold one; refused by [`TodoScript::validate`] for the same reason.
    Reword,
    /// Replay it, then stop with the rebase standing so a human can amend
    /// it. No editor opens — the pause *is* the rebase state, which
    /// [`crate::operation`] models and the lifecycle keys carry on from.
    Edit,
    /// Replay it and meld it into the commit above it, keeping both messages.
    Squash,
    /// Replay it and meld it in, discarding its message.
    Fixup,
    /// Skip the commit — its changes leave the branch.
    Drop,
    /// Run the rest of the line in the shell, between picks.
    Exec,
    /// Stop here; `git rebase --continue` picks up later.
    Break,
    /// Name the current HEAD, for a later [`Action::Reset`].
    Label,
    /// Move HEAD back to a named [`Action::Label`].
    Reset,
}

impl Action {
    /// The full word git writes into the file.
    pub fn word(self) -> &'static str {
        match self {
            Action::Pick => "pick",
            Action::Reword => "reword",
            Action::Edit => "edit",
            Action::Squash => "squash",
            Action::Fixup => "fixup",
            Action::Drop => "drop",
            Action::Exec => "exec",
            Action::Break => "break",
            Action::Label => "label",
            Action::Reset => "reset",
        }
    }

    /// Reads one action word off a todo line.
    ///
    /// Only two spellings are recognized: git's full word, and git's own
    /// documented single-letter abbreviation (`x` for exec, `t` for reset —
    /// the letters git chose *because* `e` prefixes both `edit` and `exec`
    /// and `r` both `reword` and `reset`). A longer abbreviation like `re`
    /// prefixes two words, and picking a winner would be inventing git's
    /// tie-break; such a line falls through to [`Line::Verbatim`] and
    /// survives whole.
    fn from_word(word: &[u8]) -> Option<Self> {
        let exact = |w: &str| word == w.as_bytes();
        let letter = |c: u8| word.len() == 1 && word[0] == c;
        if exact("pick") || letter(b'p') {
            return Some(Action::Pick);
        }
        if exact("reword") || letter(b'r') {
            return Some(Action::Reword);
        }
        if exact("edit") || letter(b'e') {
            return Some(Action::Edit);
        }
        if exact("squash") || letter(b's') {
            return Some(Action::Squash);
        }
        if exact("fixup") || letter(b'f') {
            return Some(Action::Fixup);
        }
        if exact("drop") || letter(b'd') {
            return Some(Action::Drop);
        }
        if exact("exec") || letter(b'x') {
            return Some(Action::Exec);
        }
        if exact("break") || letter(b'b') {
            return Some(Action::Break);
        }
        if exact("label") || letter(b'l') {
            return Some(Action::Label);
        }
        if exact("reset") || letter(b't') {
            return Some(Action::Reset);
        }
        None
    }

    /// Whether acting on this action stops mid-rebase to open *another*
    /// editor with a question only a human can answer — the one thing a
    /// scripted `GIT_SEQUENCE_EDITOR` says nothing about.
    ///
    /// `reword` alone. `edit` also stops, but it stops with the *rebase*
    /// standing and no editor open: that is the lifecycle's own state, the
    /// one the banner and `rebase.continue` were built for, so it is a
    /// pause this client drives rather than a prompt it cannot see. A
    /// reworded message travels as [`Action::Reword`] on a [`Plan`] and
    /// reaches git as a pick and an `exec`, which is why nothing here ever
    /// emits the word itself.
    fn needs_an_editor(self) -> bool {
        matches!(self, Action::Reword)
    }
}

/// One understood line of a todo file: an action, what it acts on, and the
/// bytes that trailed it.
///
/// `arg` is the next word — a commit's abbreviated SHA for the commit
/// actions, a name for label/reset, the whole command for exec. `rest` is
/// everything after it, kept verbatim, because git writes the commit's
/// subject there for the human reading the file and a round-trip that ate it
/// would be a round-trip that lied about losing nothing.
///
/// The parser never checks that `arg` looks like a SHA. A newer git's
/// `fixup -C <commit>` therefore parses with `-C` standing in the arg slot —
/// odd to read, and exactly right to write back out: emit reconstructs the
/// original bytes, which is the whole contract here. Nothing composes such a
/// line, and nothing interprets one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub action: Action,
    pub arg: Vec<u8>,
    /// Raw bytes after `arg`, including the space git puts before a subject.
    /// Empty when the line had none.
    pub rest: Vec<u8>,
}

/// One line of a todo file: a [`Step`] this module understood, or the raw
/// bytes of one it did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Step(Step),
    /// A comment from git's header, a blank line, `merge` or `update-ref`
    /// from a `--rebase-merges` plan, an ambiguous abbreviation, a bare
    /// action with nothing to act on — carried through byte for byte.
    Verbatim(Vec<u8>),
}

// ---------------------------------------------------------------- the script

/// A todo file: an ordered plan, oldest pick first, exactly as git reads it.
///
/// Built by [`TodoScript::parse`] from git's own bytes or by hand — a client
/// composing a rewrite pushes steps oldest-first, which is the order the
/// file itself is in — and turned back into bytes by [`TodoScript::emit`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TodoScript {
    lines: Vec<Line>,
}

impl TodoScript {
    /// Parses git's bytes.
    ///
    /// Lines are split on `\n` and nothing else; a `\r` some editor left
    /// behind rides inside the line's bytes and comes back out with it. The
    /// newline that terminates the file is a terminator and not a blank
    /// final line — keeping it as one would add a line on every round trip,
    /// and a plan that grows each time it is edited is a plan nobody wrote.
    pub fn parse(bytes: &[u8]) -> Self {
        let mut lines: Vec<Line> = bytes.split(|b| *b == b'\n').map(Self::parse_line).collect();
        if bytes.ends_with(b"\n") {
            lines.pop();
        }
        Self { lines }
    }

    fn parse_line(raw: &[u8]) -> Line {
        let trimmed = trim_start(raw);
        if trimmed.first() == Some(&b'#') || trimmed.is_empty() {
            return Line::Verbatim(raw.to_vec());
        }
        let (word, tail) = split_word(trimmed);
        let Some(action) = Action::from_word(word) else {
            return Line::Verbatim(raw.to_vec());
        };
        let step = match action {
            // `exec` takes the rest of the line verbatim — a command's inner
            // spacing is its own business — and `break` takes nothing at all.
            Action::Exec => Step {
                action,
                arg: trim_start(tail).to_vec(),
                rest: Vec::new(),
            },
            Action::Break => Step {
                action,
                arg: Vec::new(),
                rest: Vec::new(),
            },
            _ => {
                let body = match trim_start(tail) {
                    b"" => return Line::Verbatim(raw.to_vec()),
                    body => body,
                };
                let (arg, rest) = split_word(body);
                Step {
                    action,
                    arg: arg.to_vec(),
                    rest: rest.to_vec(),
                }
            }
        };
        Line::Step(step)
    }

    /// The lines, for a caller that wants to read the plan rather than run it.
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// Appends one step — the way a composer builds a plan, oldest pick
    /// first. `arg` travels as given; bytes in, bytes out.
    pub fn push_step(&mut self, action: Action, arg: &[u8]) {
        self.lines.push(Line::Step(Step {
            action,
            arg: arg.to_vec(),
            rest: Vec::new(),
        }));
    }

    /// The bytes git reads. Full action words, one space between fields, and
    /// a terminating newline — a file git itself could have written.
    ///
    /// Two normalizations happen on the way through and neither loses a
    /// decision: a step's run of spaces collapses to one between the word
    /// and its argument (git writes one; a hand-edited file may not), and a
    /// parsed plan always ends in a newline even when the original did not.
    /// Every unrecognized byte — comments, subjects, unknown actions — comes
    /// back exactly as it went in.
    pub fn emit(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(24 * self.lines.len());
        for line in &self.lines {
            match line {
                Line::Verbatim(raw) => out.extend_from_slice(raw),
                Line::Step(step) => {
                    out.extend_from_slice(step.action.word().as_bytes());
                    if !step.arg.is_empty() {
                        out.push(b' ');
                        out.extend_from_slice(&step.arg);
                    }
                    out.extend_from_slice(&step.rest);
                }
            }
            out.push(b'\n');
        }
        out
    }

    /// Whether git would run this plan hands-off — without stopping mid-rebase
    /// to ask a human something through an editor this client does not drive.
    ///
    /// `reword` stops and opens `GIT_EDITOR`, a second editor beyond the
    /// sequencer one, with no scripted answer; so does `fixup -c`, whose
    /// lowercase flag exists precisely to edit the melded message (capital
    /// `-C` keeps it and opens nothing, so it passes). `edit` stops too and
    /// passes: it opens nothing, and what it leaves standing is a rebase the
    /// lifecycle already drives.
    /// Refusing here, before any process runs, is what makes the refusal a
    /// sentence about the plan rather than a background job hung on an
    /// invisible prompt. Everything else in the vocabulary replays unattended.
    pub fn validate(&self) -> Result<(), String> {
        for line in &self.lines {
            if let Line::Step(step) = line {
                if step.action.needs_an_editor()
                    || (step.action == Action::Fixup && step.arg == b"-c")
                {
                    let word = match step.action.needs_an_editor() {
                        true => step.action.word().to_string(),
                        false => "fixup -c".to_string(),
                    };
                    return Err(format!(
                        "'{word}' is not supported yet: it opens an editor \
                         mid-rebase, which this client cannot drive"
                    ));
                }
            }
        }
        Ok(())
    }
}

fn trim_start(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.first(), Some(b' ') | Some(b'\t') | Some(b'\r')) {
        bytes = &bytes[1..];
    }
    bytes
}

/// Splits off the first whitespace-delimited word. The tail keeps everything
/// after it, separators included.
fn split_word(bytes: &[u8]) -> (&[u8], &[u8]) {
    let end = bytes
        .iter()
        .position(|b| *b == b' ' || *b == b'\t')
        .unwrap_or(bytes.len());
    (&bytes[..end], &bytes[end..])
}

// ---------------------------------------------------------------- composition

/// The rewrite a keypress means, said once so every client means the same
/// thing by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rewrite {
    /// Meld the commit under the keyboard into its parent, keeping both
    /// messages.
    SquashUp,
    /// The same meld, discarding the folded commit's message.
    FixupUp,
    /// Remove the commit under the keyboard from the branch.
    Drop,
}

/// Builds the plan for one of [`Rewrite`]'s rewrites, over history as a
/// client's log presents it: newest first, `index` the row under the keyboard.
///
/// Returns the revspec the rebase must sit on — the parent of the deepest
/// commit the plan touches — together with a script covering every commit
/// from there to HEAD.
///
/// Wholesale is the point and the danger. Our sequencer editor *replaces*
/// what git generated, so the plan is only complete when the window it was
/// built from *is* the range. The refusals below guarantee that: HEAD down
/// to the keyboard must be a straight single-parent line — no merge anywhere
/// in it, because `git rebase -i` without `--rebase-merges` flattens one;
/// no side-branch commit interleaved into the window, because a plan built
/// without it would silently drop that branch's changes from the result. A
/// tangled stretch of history refuses in words rather than rewriting itself
/// into something else; a straight one — most solo work — composes.
/// Builds the plan for one of [`Rewrite`]'s rewrites, over history as a
/// client's log presents it: newest first, `index` the row under the keyboard.
///
/// Returns the revspec the rebase must sit on — beneath the deepest commit
/// the plan touches — together with a script covering every commit from
/// there to HEAD.
///
/// Wholesale is the point and the danger. Our sequencer editor *replaces*
/// what git generated, so the plan is only complete when the window it was
/// built from *is* the range. The refusals below guarantee that: HEAD down
/// to the keyboard must be a straight single-parent line — no merge anywhere
/// in it, because `git rebase -i` without `--rebase-merges` flattens one;
/// no side-branch commit interleaved into the window, because a plan built
/// without it would silently drop that branch's changes from the result. A
/// tangled stretch of history refuses in words rather than rewriting itself
/// into something else; a straight one — most solo work — composes.
///
/// A fold has one more constraint than a drop: git refuses any plan whose
/// *first* line is a squash or a fixup ("cannot 'squash' without a previous
/// commit"), because there is nothing above it to meld into yet. So the fold
/// opens with a pick of the parent itself, and sits one generation deeper —
/// on the parent's parent. That reach is also the refusal: a parent at the
/// edge of the loaded window hides its own parent from us; a root parent
/// would need `git rebase --root`, which this client does not drive.
pub fn compose(
    kind: Rewrite,
    commits: &[Commit],
    index: usize,
) -> Result<(Vec<u8>, TodoScript), String> {
    let Some(selected) = commits.get(index) else {
        return Err("nothing under the keyboard to rewrite".into());
    };
    match selected.parents.len() {
        0 => {
            return Err(
                "the commit under the keyboard is the root; there is nothing \
                 beneath it to rebuild onto"
                    .into(),
            )
        }
        n if n > 1 => {
            return Err("the commit under the keyboard is a merge; rebasing would \
                 flatten it"
                .into())
        }
        _ => {}
    }
    straight_line(commits, index)?;

    let mut script = TodoScript::default();
    let upstream = match kind {
        Rewrite::Drop => {
            // Oldest first, the way the file itself is ordered, with the
            // selected commit simply absent.
            for j in (0..=index).rev() {
                if j == index {
                    continue;
                }
                script.push_step(Action::Pick, commits[j].sha.as_bytes());
            }
            if script.lines().is_empty() {
                // Dropping the one commit the range holds leaves git an empty
                // todo, which it refuses — and the move the keypress meant
                // already has a name in this app: reset --hard to this
                // commit's parent.
                return Err(
                    "this commit is the only one the plan would touch; dropping \
                     it would leave an empty plan — reset to its parent instead"
                        .into(),
                );
            }
            selected.parents[0].clone().into_bytes()
        }
        Rewrite::SquashUp | Rewrite::FixupUp => {
            // The fold lands on the parent, so the parent is replayed by the
            // plan — which makes the plan open with its pick (git refuses a
            // squash/fixup first line) and sit on the parent's own parent.
            let Some(parent) = commits.get(index + 1) else {
                return Err("the commit to fold into sits at the edge of the loaded \
                     history, so the plan cannot say what lies beneath it"
                    .into());
            };
            if selected.parents[0] != parent.sha {
                return Err("the loaded history is not a straight line down to \
                     this commit, so a plan built from it would not cover \
                     everything the rebase would touch"
                    .into());
            }
            match parent.parents.len() {
                0 => {
                    return Err("the commit under the keyboard folds into a root \
                         commit; folding into a root needs git's --root, \
                         which this client does not drive"
                        .into())
                }
                n if n > 1 => {
                    return Err("the commit under the keyboard folds into a merge; \
                         rebasing would flatten it"
                        .into())
                }
                _ => {}
            }
            let action = match kind {
                Rewrite::SquashUp => Action::Squash,
                _ => Action::Fixup,
            };
            script.push_step(Action::Pick, parent.sha.as_bytes());
            script.push_step(action, selected.sha.as_bytes());
            for j in (0..index).rev() {
                script.push_step(Action::Pick, commits[j].sha.as_bytes());
            }
            parent.parents[0].clone().into_bytes()
        }
    };
    Ok((upstream, script))
}

// ------------------------------------------------------------------ the plan

/// What a plan does to a commit *besides* replaying it — the amendments git
/// has no todo word for, and which reach it as an `exec` beside the pick.
///
/// One member today, and a member rather than a `bool` because the next one
/// is already visible: re-authoring is the amendment `commits.reset-author`
/// needs on a commit deeper than HEAD, and `--reset-author` is one flag of
/// several that `git commit --amend` takes without opening anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Amend {
    /// Hand the replayed commit's authorship to whoever is running git —
    /// `git commit --amend --reset-author`, message and tree untouched.
    ResetAuthor,
}

/// One row of an editable plan: a commit, and what is to become of it.
///
/// The sha travels as bytes and the two strings are for a reader — a todo UI
/// draws the short sha and the subject beside the action word, and neither
/// ever reaches git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub sha: Vec<u8>,
    pub short: String,
    pub subject: String,
    pub action: Action,
    /// The message an [`Action::Reword`] carries, as the bytes a commit
    /// message is. `None` for every other action, and the thing
    /// [`Plan::validate`] refuses a reword without: a reword whose message
    /// nobody typed would silently keep the old one.
    pub message: Option<Vec<u8>>,
    /// An amendment to run after this commit is replayed.
    pub amend: Option<Amend>,
    /// For an [`Action::Fixup`] only: keep *this* commit's message for the
    /// melded result instead of the one it folds into — git's `fixup -C`.
    ///
    /// The third of a fold's three message answers, beside squash (keep
    /// both) and plain fixup (keep the older one). It is a flag rather than
    /// a fourth action because it is the same fold: everything that reasons
    /// about folding — the first-line rule, the "nothing beneath it"
    /// refusal, the autosquash landing — must go on treating it as one, and
    /// a fourth action is exactly how that stops happening.
    ///
    /// git learned the spelling in 2.32; older gits reject the todo line.
    /// Whoever runs the plan is where that is checked, because a version is
    /// a fact about a machine and this file has none.
    pub keep_message: bool,
}

impl Entry {
    /// Whether replaying this entry leaves a commit behind at all — the one
    /// question the fold and drop rules are all phrased in terms of.
    fn lands(&self) -> bool {
        !matches!(self.action, Action::Drop)
    }
}

/// An editable interactive-rebase plan over one straight stretch of history.
///
/// [`compose`] answers a keypress with a finished plan; this answers a *UI*
/// with an editable one. The entries are newest first — the order every log
/// pane in this repository already draws, so a row of the list and a row of
/// the plan are the same row — and [`Plan::script`] reverses them into the
/// order git's file is in. Which means "the commit below" is one word in
/// both places: `squash` folds an entry into the entry *below* it, and
/// that entry is its parent.
///
/// Its constraints are git's, checked before any process runs:
///
/// - the window has to be a straight single-parent line, for exactly the
///   reason [`compose`] says — our sequencer editor *replaces* what git
///   generated, so a plan is only complete when the window it was built
///   from is the range;
/// - the oldest entry cannot be a fold, because git refuses a plan whose
///   first line is a squash or a fixup ("cannot 'squash' without a previous
///   commit");
/// - something has to survive, because git refuses an empty todo — and
///   dropping every commit in the range already has a name in this app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    upstream: Vec<u8>,
    base: String,
    entries: Vec<Entry>,
}

impl Plan {
    /// Builds the plan for the window from HEAD down to and including
    /// `base` — the deepest commit it may touch — over history as a client's
    /// log presents it: newest first.
    ///
    /// Every entry starts as a `pick`, which is git's own starting plan and
    /// a rebase that changes nothing. The rebase sits on `base`'s parent,
    /// so a root there refuses: replaying a root needs `git rebase --root`,
    /// which this client does not drive.
    pub fn over(commits: &[Commit], base: usize) -> Result<Self, String> {
        let Some(deepest) = commits.get(base) else {
            return Err("nothing under the keyboard to rewrite".into());
        };
        match deepest.parents.len() {
            0 => {
                return Err(
                    "the deepest commit in the plan is the root; there is nothing \
                     beneath it to rebuild onto"
                        .into(),
                )
            }
            n if n > 1 => {
                return Err("the deepest commit in the plan is a merge; rebasing \
                     would flatten it"
                    .into())
            }
            _ => {}
        }
        straight_line(commits, base)?;
        let entries = commits[..=base]
            .iter()
            .map(|c| Entry {
                sha: c.sha.clone().into_bytes(),
                short: c.short.clone(),
                subject: c.subject.clone(),
                action: Action::Pick,
                message: None,
                amend: None,
                keep_message: false,
            })
            .collect();
        Ok(Self {
            upstream: deepest.parents[0].clone().into_bytes(),
            base: deepest.short.clone(),
            entries,
        })
    }

    /// The revspec the rebase sits on: the parent of the deepest entry.
    pub fn upstream(&self) -> &[u8] {
        &self.upstream
    }

    /// The deepest entry's short sha, for a sentence a person reads.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The entries, newest first.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Where a sha sits in the plan now — the only honest way to follow a
    /// commit across a reorder, since its row moved and its identity did not.
    pub fn index_of(&self, sha: &[u8]) -> Option<usize> {
        self.entries.iter().position(|e| e.sha == sha)
    }

    /// Sets one entry's action.
    ///
    /// The oldest entry refuses a fold in its own words rather than letting
    /// git refuse the whole plan later: there is nothing below it in the
    /// window to fold into, and the fix is a deeper base, not a different
    /// key.
    pub fn set_action(&mut self, index: usize, action: Action) -> Result<(), String> {
        let last = self.entries.len().saturating_sub(1);
        let Some(entry) = self.entries.get_mut(index) else {
            return Err("that row is not in the plan".into());
        };
        if index == last && matches!(action, Action::Squash | Action::Fixup) {
            return Err("the oldest commit in the plan has nothing below it to \
                 fold into — start the rebase one commit deeper"
                .into());
        }
        entry.action = action;
        if action != Action::Reword {
            entry.message = None;
        }
        if action != Action::Fixup {
            entry.keep_message = false;
        }
        Ok(())
    }

    /// The fold that keeps *this* commit's message — `fixup -C`, the third
    /// answer to the question squash and fixup answer the other two ways.
    ///
    /// Sets the action too, because the flag means nothing without it.
    pub fn set_fixup_keeping_message(&mut self, index: usize) -> Result<(), String> {
        self.set_action(index, Action::Fixup)?;
        if let Some(entry) = self.entries.get_mut(index) {
            entry.keep_message = true;
        }
        Ok(())
    }

    /// Whether any entry asks for `fixup -C` — the one spelling in this
    /// vocabulary that a git older than 2.32 does not know, and so the one
    /// thing a runner has to check a version for.
    pub fn keeps_a_message(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.keep_message && e.action == Action::Fixup)
    }

    /// Rewords one entry: the action and the message it carries, together,
    /// because neither means anything alone.
    pub fn set_message(&mut self, index: usize, message: Vec<u8>) -> Result<(), String> {
        if message.iter().all(|b| b.is_ascii_whitespace()) {
            return Err("an empty message rewords nothing".into());
        }
        let Some(entry) = self.entries.get_mut(index) else {
            return Err("that row is not in the plan".into());
        };
        entry.action = Action::Reword;
        entry.message = Some(message);
        Ok(())
    }

    /// Hangs an amendment on one entry, to run once it has been replayed.
    pub fn set_amend(&mut self, index: usize, amend: Amend) -> Result<(), String> {
        let Some(entry) = self.entries.get_mut(index) else {
            return Err("that row is not in the plan".into());
        };
        if !entry.lands() {
            return Err("a dropped commit is not replayed, so there is nothing \
                 to amend"
                .into());
        }
        entry.amend = Some(amend);
        Ok(())
    }

    /// Moves an entry one row towards HEAD, answering where it landed.
    pub fn move_up(&mut self, index: usize) -> Result<usize, String> {
        if index >= self.entries.len() {
            return Err("that row is not in the plan".into());
        }
        if index == 0 {
            return Err("that commit is already the newest in the plan".into());
        }
        self.entries.swap(index, index - 1);
        Ok(index - 1)
    }

    /// Moves an entry one row away from HEAD, answering where it landed.
    ///
    /// The oldest row refuses: below it is the base the rebase stands on,
    /// which the plan does not replay and therefore cannot reorder past.
    pub fn move_down(&mut self, index: usize) -> Result<usize, String> {
        if index >= self.entries.len() {
            return Err("that row is not in the plan".into());
        }
        if index + 1 == self.entries.len() {
            return Err("that commit is already the oldest in the plan — the \
                 row below it is the base the rebase stands on"
                .into());
        }
        self.entries.swap(index, index + 1);
        Ok(index + 1)
    }

    /// git's `--autosquash`, as a reordering of this plan: every commit
    /// whose subject opens with `fixup!` or `squash!` moves to sit directly
    /// on top of the commit it names and takes that action. Answers how many
    /// moved.
    ///
    /// Matching is git's, minus the parts a client cannot see: the marker
    /// words are stripped — repeatedly, because `fixup! fixup! x` is a real
    /// thing git writes — and what is left is matched against a subject
    /// exactly, then as a prefix, then against a short sha. The newest
    /// candidate *older* than the marker wins, which is the only direction a
    /// fold can go. A marker naming nothing in the window is left exactly
    /// where it is, as a pick: silently folding it into a guess is how a
    /// change lands in the wrong commit.
    pub fn autosquash(&mut self) -> usize {
        // File order — oldest first — because that is the order the fold
        // rule is phrased in and the order git itself walks.
        let mut source: Vec<Entry> = self.entries.iter().rev().cloned().collect();
        let mut out: Vec<Entry> = Vec::with_capacity(source.len());
        let mut moved = 0;
        // Every marker, by the index of the entry it lands on, resolved
        // before anything moves: a marker cannot target another marker.
        let mut markers: Vec<(usize, usize, Action)> = Vec::new();
        for (i, entry) in source.iter().enumerate() {
            let Some((action, target)) = marker_of(&entry.subject) else {
                continue;
            };
            // A bare marker names nothing: `fixup!` with an empty
            // remainder would otherwise match every older commit, because
            // every subject starts with the empty string. Left standing
            // as a pick, exactly like a marker naming nothing in the
            // window — folding it into the newest guess lands a change
            // in the wrong commit.
            if target.is_empty() {
                continue;
            }
            let landing = source[..i]
                .iter()
                .enumerate()
                .filter(|(j, c)| marker_of(&c.subject).is_none() && *j < i)
                .rev()
                .find(|(_, c)| {
                    c.subject == target
                        || c.subject.starts_with(&target)
                        || (!target.is_empty() && c.short == target)
                });
            if let Some((j, _)) = landing {
                markers.push((i, j, action));
            }
        }
        for i in 0..source.len() {
            if markers.iter().any(|(marker, _, _)| *marker == i) {
                continue;
            }
            out.push(source[i].clone());
            for (marker, landing, action) in &markers {
                if *landing != i {
                    continue;
                }
                let mut folded = source[*marker].clone();
                folded.action = *action;
                folded.message = None;
                out.push(folded);
                moved += 1;
            }
        }
        source = out;
        source.reverse();
        self.entries = source;
        moved
    }

    /// Whether the plan stops mid-flight, and therefore hands the reader
    /// back a standing rebase rather than a finished one.
    pub fn pauses(&self) -> bool {
        self.entries.iter().any(|e| e.action == Action::Edit)
    }

    /// Everything git would refuse, said before any process runs.
    pub fn validate(&self) -> Result<(), String> {
        if self.entries.is_empty() {
            return Err("the plan is empty; there is nothing to rebuild".into());
        }
        if !self.entries.iter().any(Entry::lands) {
            return Err("this plan drops every commit it covers, which leaves git \
                 an empty plan — reset to the base instead"
                .into());
        }
        // File order: a fold needs something already replayed above it, and
        // "already replayed" means a landing entry deeper in the file.
        let mut landed = false;
        for entry in self.entries.iter().rev() {
            let folding = matches!(entry.action, Action::Squash | Action::Fixup);
            if folding && !landed {
                return Err(format!(
                    "{} has nothing beneath it to fold into: every commit below \
                     it in the plan is dropped or folded away",
                    entry.short
                ));
            }
            if entry.action == Action::Reword && entry.message.is_none() {
                return Err(format!("{} is reworded with no message", entry.short));
            }
            if entry.amend.is_some() && !entry.lands() {
                return Err(format!(
                    "{} is dropped, so there is nothing of it left to amend",
                    entry.short
                ));
            }
            landed |= entry.lands() && !folding;
        }
        Ok(())
    }

    /// git's own bytes for this plan — one line per entry, oldest first.
    ///
    /// `exec_for` is where the amendments come from, and it is a callback
    /// for one reason: a reworded message lives in a file somebody has to
    /// *write*, and `core` does no I/O. So the ordering — which commit,
    /// which action, which command after it — is decided exactly once, here,
    /// and the acquisition layer supplies only the shell bytes it alone can
    /// build. A dropped entry is never asked for one: nothing was replayed,
    /// so there is nothing to amend.
    ///
    /// A [`Action::Reword`] reaches git as a `pick`, never as the word
    /// itself: git's own `reword` opens an editor this client cannot answer,
    /// and the message it would have asked for is already in hand.
    pub fn script(&self, exec_for: ExecFor<'_>) -> Result<TodoScript, String> {
        self.validate()?;
        let mut script = TodoScript::default();
        for entry in self.entries.iter().rev() {
            let action = match entry.action {
                Action::Reword => Action::Pick,
                other => other,
            };
            // `fixup -C <sha>`: the flag rides in the argument slot, which
            // is exactly how the parser reads such a line back — bytes in,
            // bytes out, nothing here interpreting git's own flags.
            match entry.keep_message && action == Action::Fixup {
                true => script.push_step(action, &[b"-C ".as_slice(), &entry.sha].concat()),
                false => script.push_step(action, &entry.sha),
            }
            if !entry.lands() {
                continue;
            }
            for command in exec_for(entry)? {
                script.push_step(Action::Exec, &command);
            }
        }
        script.validate()?;
        Ok(script)
    }
}

/// Where a plan's amendments get their shell bytes: called once per
/// replayed entry, answering the `exec` lines to run after it.
///
/// A callback and not a list because the bytes cost I/O — a reworded
/// message lives in a file somebody has to write — and `core` does none.
pub type ExecFor<'a> = &'a mut dyn FnMut(&Entry) -> Result<Vec<Vec<u8>>, String>;

/// The action a `fixup!` / `squash!` subject asks for, and the subject it
/// names — with every layer of marker stripped, because git writes
/// `fixup! fixup! x` when a fixup is fixed up.
fn marker_of(subject: &str) -> Option<(Action, String)> {
    let mut rest = subject.trim_start();
    let mut action = None;
    loop {
        let next = if let Some(tail) = rest.strip_prefix("fixup!") {
            action.get_or_insert(Action::Fixup);
            tail
        } else if let Some(tail) = rest.strip_prefix("squash!") {
            action.get_or_insert(Action::Squash);
            tail
        } else if let Some(tail) = rest.strip_prefix("amend!") {
            // git's third marker. It rewords as well as folds, and the
            // message it would reword *with* is its own body — which a log
            // window does not carry. Folded as a fixup, which keeps the
            // change and loses only the rewording nothing here could read.
            action.get_or_insert(Action::Fixup);
            tail
        } else {
            break;
        };
        rest = next.trim_start();
    }
    action.map(|action| (action, rest.to_string()))
}

/// What `git commit --fixup` writes for its target: the `fixup!` marker
/// git folds away, the `amend!` marker that folds the same way while
/// keeping the target's message, and the `reword!` marker that folds and
/// then asks for the message. Three spellings of one verb, so one enum —
/// a client that stored them as strings would re-parse git's own flag on
/// every press.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FixupKind {
    /// `fixup! <subject>`: meld the change, lose the message.
    #[default]
    Fixup,
    /// `amend! <subject>`: meld the change, keep the target's message.
    Amend,
    /// `reword! <subject>`: meld the change, then ask for the message.
    Reword,
}

impl FixupKind {
    /// The flag `git commit` takes, without its target: `--fixup=` is the
    /// plain spelling, and the `amend:` / `reword:` prefixes are git's
    /// (2.32 and up — older gits refuse the prefix in their own words, and
    /// that refusal is the honest answer on such a machine).
    pub fn flag(self) -> &'static str {
        match self {
            FixupKind::Fixup => "--fixup=",
            FixupKind::Amend => "--fixup=amend:",
            FixupKind::Reword => "--fixup=reword:",
        }
    }

    /// The marker word the created commit's subject opens with.
    pub fn word(self) -> &'static str {
        match self {
            FixupKind::Fixup => "fixup!",
            FixupKind::Amend => "amend!",
            FixupKind::Reword => "reword!",
        }
    }

    /// The next kind, for the key that chooses what a fixup creation
    /// writes: fixup, then amend, then reword, then round again.
    pub fn cycle(self) -> Self {
        match self {
            FixupKind::Fixup => FixupKind::Amend,
            FixupKind::Amend => FixupKind::Reword,
            FixupKind::Reword => FixupKind::Fixup,
        }
    }

    /// The status line's word for what the creation key will write next.
    pub fn describe(self) -> &'static str {
        match self {
            FixupKind::Fixup => "fixup!",
            FixupKind::Amend => "amend!",
            FixupKind::Reword => "reword!",
        }
    }
}

/// A `fixup!` / `squash!` / `amend!` line in a loaded window, and the
/// window row it folds into — the apply flow's read-only view of what
/// [`Plan::autosquash`] will do, said before anything is armed so a marker
/// that names nothing refuses instead of riding the plan as a pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixupMark {
    /// The marker's own row, newest first like the window.
    pub index: usize,
    /// What the marker asks for.
    pub action: Action,
    /// The subject with every marker layer stripped, as [`marker_of`] reads it.
    pub remainder: String,
    /// The row it folds into, or `None` when no older row answers the
    /// name: the newest older row whose subject is the remainder, opens
    /// with it, or whose short sha it is — git's own match, newest first.
    pub target: Option<usize>,
}

/// Every marker line in the window, newest first, each with its landing
/// resolved — or `None`, which is the apply flow's refusal and never a
/// guess. A marker cannot name another marker (git resolves every landing
/// before anything moves), and it cannot name anything newer than itself
/// (a fold only goes down); a bare marker names nothing, because every
/// subject starts with the empty string.
///
/// The matching is [`Plan::autosquash`]'s, said once: resolve here and run
/// there, and the plan a press arms is the plan these landings describe.
/// A caller that reordered the window between this call and the run would
/// be arming a different plan — the run re-resolves for exactly that reason.
pub fn fixup_marks(commits: &[Commit]) -> Vec<FixupMark> {
    let mut marks = Vec::new();
    for (i, commit) in commits.iter().enumerate() {
        let Some((action, remainder)) = marker_of(&commit.subject) else {
            continue;
        };
        let target = if remainder.is_empty() {
            None
        } else {
            // Newest first, like the window itself: the enumeration runs
            // ascending, which *is* newest first here (unlike the
            // oldest-first file [`Plan::autosquash`] walks, which is why
            // that one reverses and this one must not).
            commits
                .iter()
                .enumerate()
                .filter(|(j, c)| *j > i && marker_of(&c.subject).is_none())
                .find(|(_, c)| {
                    c.subject == remainder
                        || c.subject.starts_with(&remainder)
                        || c.short == remainder
                })
                .map(|(j, _)| j)
        };
        marks.push(FixupMark {
            index: i,
            action,
            remainder,
            target,
        });
    }
    marks
}

/// Whether the window from HEAD down to `index` is one straight
/// single-parent line — the precondition every wholesale plan rests on, and
/// the same check [`compose`] makes, said once.
fn straight_line(commits: &[Commit], index: usize) -> Result<(), String> {
    for j in 1..=index {
        if commits[j - 1].parents.len() != 1 {
            return Err("history between HEAD and the keyboard holds a merge; \
                 rebasing would flatten it"
                .into());
        }
        if commits[j - 1].parents[0] != commits[j].sha {
            return Err("the loaded history is not a straight line down to this \
                 commit, so a plan built from it would not cover everything \
                 the rebase would touch"
                .into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(line: &Line) -> &Step {
        match line {
            Line::Step(s) => s,
            Line::Verbatim(raw) => panic!("expected a step, got {raw:?}"),
        }
    }

    /// One line per plan line, the way a person reads it: `action arg`, or
    /// the verbatim bytes as git wrote them.
    fn shown(script: &TodoScript) -> Vec<String> {
        script
            .lines()
            .iter()
            .map(|line| match line {
                Line::Step(s) => format!("{} {}", s.action.word(), String::from_utf8_lossy(&s.arg)),
                Line::Verbatim(raw) => String::from_utf8_lossy(raw).into_owned(),
            })
            .collect()
    }

    /// git's own header, over three commits, as a fresh todo carries it.
    const SAMPLE: &[u8] = b"\
pick 1111111 first one
pick 2222222 second one

# Rebase 0000000..3333333 onto 4444444 (3 commands)
# Commands:
# p, pick <commit> = use commit
# x, exec <command> = run command using shell
";

    #[test]
    fn a_real_todo_parses_into_steps_and_comments() {
        let script = TodoScript::parse(SAMPLE);
        assert_eq!(
            shown(&script),
            vec![
                "pick 1111111",
                "pick 2222222",
                "",
                "# Rebase 0000000..3333333 onto 4444444 (3 commands)",
                "# Commands:",
                "# p, pick <commit> = use commit",
                "# x, exec <command> = run command using shell",
            ]
        );
        // Subjects ride along untouched, for whoever reads the plan.
        assert_eq!(
            step(&script.lines()[0]),
            &Step {
                action: Action::Pick,
                arg: b"1111111".to_vec(),
                rest: b" first one".to_vec(),
            }
        );
    }

    #[test]
    fn well_formed_bytes_round_trip_byte_exact() {
        assert_eq!(TodoScript::parse(SAMPLE).emit(), SAMPLE);
    }

    /// The promise the golden tests lean on: whatever we emit parses back to
    /// the same plan, so a client can hold the model and never the bytes.
    #[test]
    fn emit_is_idempotent_under_parse() {
        let mut composed = TodoScript::default();
        composed.push_step(Action::Pick, b"aabbccd");
        composed.push_step(Action::Fixup, b"ddeeccb");
        composed.push_step(Action::Exec, b"echo done");
        composed.push_step(Action::Break, b"");
        composed.push_step(Action::Drop, b"0011001");
        let once = composed.emit();
        assert_eq!(once, TodoScript::parse(&once).emit());
        assert_eq!(TodoScript::parse(&once), composed);
    }

    #[test]
    fn every_action_lands_through_its_word_and_its_letter() {
        let raw = "\
p 1111111 via letter
pick 2222222 via word
reword 3333333 r
edit 4444444 e
s 5555555 squash
f 6666666 fixup
x echo hello   between picks
b
d 7777777 dropped
l a-name
t a-name";
        let script = TodoScript::parse(raw.as_bytes());
        assert_eq!(
            shown(&script),
            vec![
                "pick 1111111",
                "pick 2222222",
                "reword 3333333",
                "edit 4444444",
                "squash 5555555",
                "fixup 6666666",
                "exec echo hello   between picks",
                "break ",
                "drop 7777777",
                "label a-name",
                "reset a-name",
            ],
            "git's documented letters and words all land"
        );
    }

    #[test]
    fn what_this_module_does_not_understand_is_kept_whole() {
        let raw = "\
pick 1111111 understood
re 2222222 ambiguous abbreviation
merge 3333333 a --rebase-merges plan
update-ref refs/heads/x
fixup -C 4444444 a flag form nobody here produces
squash

\tindented comment
";
        let script = TodoScript::parse(raw.as_bytes());
        assert_eq!(
            shown(&script),
            vec![
                "pick 1111111",
                "re 2222222 ambiguous abbreviation",
                "merge 3333333 a --rebase-merges plan",
                "update-ref refs/heads/x",
                "fixup -C", // parsed; arg is "-C", rest carries the rest verbatim
                "squash",
                "",
                "\tindented comment",
            ],
            "only the unambiguous lines were understood"
        );
        // And emitting puts git's own bytes back, oddities included — the
        // tolerance is real because the round trip is lossless.
        assert_eq!(script.emit(), raw.as_bytes());
    }

    #[test]
    fn reword_is_named_by_the_validation_that_refuses_it_and_edit_passes() {
        let mut script = TodoScript::default();
        script.push_step(Action::Pick, b"1111111");
        script.push_step(Action::Squash, b"2222222");
        assert_eq!(script.validate(), Ok(()));

        // The word itself opens a second editor nothing here can answer, so
        // it is refused by name — a [`Plan`] reaches the same result with a
        // pick and an exec instead, which is why nothing composes it.
        let mut bad = TodoScript::default();
        bad.push_step(Action::Reword, b"1111111");
        let err = bad.validate().expect_err("refused");
        assert!(err.contains("reword"), "reword named: {err}");
        assert!(err.contains("editor"), "{err}");

        // `edit` opens nothing. It stops with the rebase standing, which is
        // the state the lifecycle keys already carry on from, so a plan that
        // holds one runs.
        let mut pausing = TodoScript::default();
        pausing.push_step(Action::Edit, b"1111111");
        assert_eq!(pausing.validate(), Ok(()));

        // git's own header validates fine: it survives the rewrite untouched.
        assert_eq!(TodoScript::parse(SAMPLE).validate(), Ok(()));
    }

    #[test]
    fn fixup_dash_c_is_refused_and_fixup_capital_c_passes() {
        // Lowercase `-c` opens GIT_EDITOR to edit the melded message — the
        // same broken contract as a reword. Capital `-C` keeps the message
        // and opens nothing, so the tolerance that carries it through parse
        // and emit unchanged extends to validation too.
        let editing = TodoScript::parse(b"fixup -c 1111111 a subject\n");
        let err = editing.validate().expect_err("fixup -c refused");
        assert!(err.contains("fixup -c"), "{err}");
        assert!(err.contains("editor"), "{err}");

        let keeping = TodoScript::parse(b"fixup -C 1111111 a subject\n");
        assert_eq!(keeping.validate(), Ok(()));
    }

    /// Five commits in a straight line, newest first — the shape a pane's log
    /// shows when solo work sits under the keyboard.
    fn linear() -> Vec<Commit> {
        let names = ["head", "mid", "under", "deep", "root"];
        names
            .iter()
            .enumerate()
            .map(|(i, name)| Commit {
                sha: format!("{name}-sha"),
                short: String::new(),
                parents: match names.get(i + 1) {
                    Some(parent) => vec![format!("{parent}-sha")],
                    None => vec![],
                }
                .into_boxed_slice(),
                author: "".into(),
                timestamp: 0,
                subject: (*name).into(),
            })
            .collect()
    }

    #[test]
    fn a_composed_plan_carries_every_commit_from_the_keyboard_to_head() {
        let commits = linear();

        // The plan covers upstream..HEAD — the keyboard and everything above
        // it. A fold replays its own parent first (git refuses a squash or
        // fixup opening the plan), so it sits one generation deeper than a
        // drop: on `root`, beneath the parent it melds into.
        let (upstream, script) = compose(Rewrite::SquashUp, &commits, 2).expect("composes");
        assert_eq!(upstream, b"root-sha", "the rebase sits under the parent");
        assert_eq!(
            shown(&script),
            vec![
                "pick deep-sha",    // the parent, replayed first
                "squash under-sha", // the selected commit, melded into it
                "pick mid-sha",
                "pick head-sha",
            ],
            "oldest first, the order the file itself is in"
        );

        let (upstream, fix) = compose(Rewrite::FixupUp, &commits, 1).expect("composes");
        assert_eq!(upstream, b"deep-sha");
        assert_eq!(
            shown(&fix),
            vec!["pick under-sha", "fixup mid-sha", "pick head-sha"]
        );

        // A drop needs no pick of its own parent — omission cannot strand —
        // so it sits exactly where it always did.
        let (upstream, dropped) = compose(Rewrite::Drop, &commits, 1).expect("composes");
        assert_eq!(upstream, b"under-sha");
        assert_eq!(
            shown(&dropped),
            vec!["pick head-sha"],
            "mid is gone, head replays"
        );
    }

    #[test]
    fn a_fold_refuses_when_the_parent_hides_its_own_parent() {
        // The parent beyond the window's edge: its pick would open the plan,
        // but nothing here can say where that pick must sit.
        let clipped = vec![commit_of("child", &["beneath-not-loaded"])];
        let err = compose(Rewrite::SquashUp, &clipped, 0)
            .expect_err("the parent is past the loaded window");
        assert!(err.contains("edge of the loaded history"), "{err}");

        // The parent in view but a root: folding into a root is git's --root
        // territory, not ours.
        let rooted = vec![commit_of("child", &["root"]), commit_of("root", &[])];
        let err = compose(Rewrite::FixupUp, &rooted, 0).expect_err("the parent is a root");
        assert!(err.contains("--root"), "{err}");

        // The row beneath the keyboard is a side branch's tip, not the
        // parent the fold claims to land on: same straight-line refusal as
        // everywhere else.
        let forked = vec![
            commit_of("head", &["c1"]),
            commit_of("side", &["elsewhere"]),
            commit_of("c1", &["c0"]),
        ];
        let err = compose(Rewrite::SquashUp, &forked, 0).expect_err("side tip is not the parent");
        assert!(err.contains("straight line"), "{err}");

        // And a merge in the parent seat flattens like any other merge.
        let merged = vec![
            commit_of("head", &["m"]),
            commit_of("m", &["a", "b"]),
            commit_of("a", &["old"]),
        ];
        let err = compose(Rewrite::FixupUp, &merged, 0).expect_err("folding into a merge");
        assert!(err.contains("flatten"), "{err}");
    }

    /// One named commit with the given parents — the fixture brick the
    /// refusal tests build from.
    fn commit_of(sha: &str, parents: &[&str]) -> Commit {
        Commit {
            sha: sha.into(),
            short: String::new(),
            parents: parents
                .iter()
                .map(|p| (*p).to_string())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            author: "".into(),
            timestamp: 0,
            subject: sha.into(),
        }
    }

    #[test]
    fn dropping_the_only_commit_the_plan_touches_is_refused() {
        // An empty todo is git's "nothing to do", and the move the keypress
        // meant already exists under another name.
        let err = compose(Rewrite::Drop, &linear(), 0).expect_err("an empty plan");
        assert!(err.contains("empty"), "{err}");
        assert!(err.contains("reset"), "{err}");
    }

    #[test]
    fn compose_refuses_every_shape_it_cannot_complete() {
        let err = compose(Rewrite::Drop, &linear(), 7).expect_err("past the end");
        assert!(err.contains("nothing under the keyboard"), "{err}");

        let merged = vec![commit_of("m", &["p1", "p2"]), commit_of("p1", &["old"])];
        let err = compose(Rewrite::Drop, &merged, 0)
            .expect_err("a merge under the keyboard would be flattened");
        assert!(err.contains("flatten"), "{err}");

        let merged_deep = vec![commit_of("c2", &["c1"]), commit_of("merge", &["a", "b"])];
        let err = compose(Rewrite::Drop, &merged_deep, 1)
            .expect_err("a merge anywhere in the range flattens");
        assert!(err.contains("flatten"), "{err}");

        let forked = vec![
            commit_of("side", &["elsewhere"]),
            commit_of("c2", &["c1"]),
            commit_of("c1", &["c0"]),
        ];
        let err = compose(Rewrite::Drop, &forked, 2)
            .expect_err("a side commit interleaved in the window breaks completeness");
        assert!(err.contains("straight line"), "{err}");
    }

    // ------------------------------------------------------------ the plan

    /// The plan's own script, with the amendments spelled as the shell
    /// bytes an acquisition layer would have supplied — a stand-in here,
    /// because `core` writes no message file and never will.
    fn planned(plan: &Plan) -> Vec<String> {
        let script = plan
            .script(&mut |entry: &Entry| {
                let mut out = Vec::new();
                if let Some(message) = &entry.message {
                    out.push([b"amend -F ".as_slice(), message].concat());
                }
                if entry.amend == Some(Amend::ResetAuthor) {
                    out.push(b"amend --reset-author".to_vec());
                }
                Ok(out)
            })
            .expect("a runnable plan");
        shown(&script)
    }

    #[test]
    fn a_fresh_plan_is_git_own_starting_point() {
        let commits = linear();
        let plan = Plan::over(&commits, 2).expect("a straight window");
        // Every entry a pick, the deepest one's parent underneath: a rebase
        // that changes nothing, which is what git generates too.
        assert_eq!(plan.upstream(), b"deep-sha");
        assert_eq!(plan.len(), 3);
        assert_eq!(
            planned(&plan),
            vec!["pick under-sha", "pick mid-sha", "pick head-sha"],
            "oldest first, the order the file itself is in"
        );
        // Newest first in the model, because that is the order the row under
        // the keyboard is in.
        assert_eq!(plan.entries()[0].subject, "head");
        assert_eq!(plan.entries()[2].subject, "under");
    }

    #[test]
    fn a_plan_refuses_the_windows_it_cannot_cover() {
        let commits = linear();
        // The root has no parent, so nothing is left to rebuild onto.
        let err = Plan::over(&commits, 4).expect_err("refused");
        assert!(err.contains("root"), "{err}");

        // A merge in the window flattens under `rebase -i`.
        let mut merged = linear();
        merged[1].parents = vec!["under-sha".into(), "other-sha".into()].into_boxed_slice();
        let err = Plan::over(&merged, 2).expect_err("refused");
        assert!(err.contains("merge"), "{err}");

        // The deepest commit itself being a merge is its own sentence.
        let err = Plan::over(&merged, 1).expect_err("refused");
        assert!(err.contains("merge"), "{err}");

        // A window that is not one line — a side branch interleaved — would
        // build a plan that does not cover what the rebase touches.
        let mut broken = linear();
        broken[1].parents = vec!["somewhere-else".into()].into_boxed_slice();
        let err = Plan::over(&broken, 3).expect_err("refused");
        assert!(err.contains("straight line"), "{err}");

        // Past the end of the window there is no row at all.
        assert!(Plan::over(&commits, 9).is_err());
    }

    #[test]
    fn folds_squash_and_fixup_into_the_commit_below() {
        let commits = linear();
        // The keyboard on `mid`, folding into `under` — which is the entry
        // below it in the list and its parent in the history.
        let mut plan = Plan::over(&commits, 2).expect("a window");
        plan.set_action(1, Action::Squash).expect("a fold");
        assert_eq!(
            planned(&plan),
            vec!["pick under-sha", "squash mid-sha", "pick head-sha"]
        );

        let mut plan = Plan::over(&commits, 2).expect("a window");
        plan.set_action(0, Action::Fixup).expect("a fold");
        assert_eq!(
            planned(&plan),
            vec!["pick under-sha", "pick mid-sha", "fixup head-sha"]
        );

        // The oldest row has nothing below it *in the plan*: refused where
        // the press happened, naming the fix, rather than by git after a
        // process started.
        let mut plan = Plan::over(&commits, 2).expect("a window");
        let err = plan.set_action(2, Action::Squash).expect_err("refused");
        assert!(err.contains("nothing below it"), "{err}");
        assert!(err.contains("deeper"), "the way out is named: {err}");
    }

    #[test]
    fn a_drop_leaves_the_rest_and_dropping_everything_refuses() {
        let commits = linear();
        let mut plan = Plan::over(&commits, 2).expect("a window");
        plan.set_action(1, Action::Drop).expect("a drop");
        assert_eq!(
            planned(&plan),
            vec!["pick under-sha", "drop mid-sha", "pick head-sha"],
            "the drop is a line git reads, not a line nobody wrote"
        );

        // Everything dropped is an empty todo, which git refuses — said
        // here, with the move that was meant named.
        for i in 0..plan.len() {
            plan.set_action(i, Action::Drop).expect("a drop");
        }
        let err = plan.validate().expect_err("refused");
        assert!(err.contains("reset to the base"), "{err}");

        // A fold with nothing left beneath it is the same refusal wearing
        // another shape, and it names the commit.
        let mut plan = Plan::over(&commits, 2).expect("a window");
        plan.set_action(2, Action::Drop).expect("a drop");
        plan.set_action(1, Action::Fixup).expect("a fold");
        let err = plan.validate().expect_err("refused");
        assert!(err.contains("nothing beneath it"), "{err}");
    }

    #[test]
    fn a_reword_travels_as_a_pick_and_its_own_message() {
        let commits = linear();
        let mut plan = Plan::over(&commits, 1).expect("a window");
        plan.set_message(0, b"a better subject".to_vec())
            .expect("a message");
        assert_eq!(plan.entries()[0].action, Action::Reword);
        assert_eq!(
            planned(&plan),
            vec![
                "pick mid-sha",
                "pick head-sha",
                "exec amend -F a better subject",
            ],
            "git's own reword word never reaches the file"
        );

        // A reword with no message would keep the old one silently.
        let mut plan = Plan::over(&commits, 1).expect("a window");
        plan.set_action(0, Action::Reword).expect("an action");
        let err = plan.validate().expect_err("refused");
        assert!(err.contains("no message"), "{err}");
        assert!(plan.set_message(0, b"   \n".to_vec()).is_err(), "blank");

        // Choosing another action afterwards drops the message with it:
        // a stale message hanging on a pick is a reword waiting to surprise
        // somebody.
        let mut plan = Plan::over(&commits, 1).expect("a window");
        plan.set_message(0, b"typed".to_vec()).expect("a message");
        plan.set_action(0, Action::Pick).expect("an action");
        assert_eq!(plan.entries()[0].message, None);
    }

    #[test]
    fn an_amendment_rides_the_pick_that_replayed_it() {
        let commits = linear();
        let mut plan = Plan::over(&commits, 2).expect("a window");
        plan.set_amend(1, Amend::ResetAuthor).expect("an amendment");
        assert_eq!(
            planned(&plan),
            vec![
                "pick under-sha",
                "pick mid-sha",
                "exec amend --reset-author",
                "pick head-sha",
            ],
            "the exec runs against the commit it followed"
        );

        // Nothing is amended about a commit that was never replayed.
        let mut plan = Plan::over(&commits, 2).expect("a window");
        plan.set_action(1, Action::Drop).expect("a drop");
        let err = plan.set_amend(1, Amend::ResetAuthor).expect_err("refused");
        assert!(err.contains("dropped"), "{err}");
    }

    #[test]
    fn moving_a_commit_swaps_it_with_its_neighbour_and_stops_at_the_edges() {
        let commits = linear();
        let mut plan = Plan::over(&commits, 2).expect("a window");
        // Down is away from HEAD, which is *earlier* in git's file.
        assert_eq!(plan.move_down(0), Ok(1));
        assert_eq!(
            planned(&plan),
            vec!["pick under-sha", "pick head-sha", "pick mid-sha"]
        );
        // And up puts it back.
        assert_eq!(plan.move_up(1), Ok(0));
        assert_eq!(
            planned(&plan),
            vec!["pick under-sha", "pick mid-sha", "pick head-sha"]
        );

        // The edges refuse in their own words rather than wrapping around:
        // below the oldest row is the base the rebase stands on.
        let err = plan.move_up(0).expect_err("refused");
        assert!(err.contains("newest"), "{err}");
        let err = plan.move_down(2).expect_err("refused");
        assert!(err.contains("base"), "{err}");
        assert!(plan.move_up(9).is_err(), "a row that is not there");
    }

    #[test]
    fn autosquash_lands_each_marker_on_the_commit_it_names() {
        let mut commits = linear();
        commits[0].subject = "fixup! under".into();
        commits[1].subject = "squash! under".into();
        let mut plan = Plan::over(&commits, 2).expect("a window");
        assert_eq!(plan.autosquash(), 2);
        assert_eq!(
            planned(&plan),
            vec!["pick under-sha", "squash mid-sha", "fixup head-sha",],
            "each marker sits directly on its target, in the order written"
        );

        // A marker naming nothing in the window keeps its place and its
        // pick: folding it into a guess lands a change in the wrong commit.
        let mut commits = linear();
        commits[0].subject = "fixup! something else entirely".into();
        let mut plan = Plan::over(&commits, 2).expect("a window");
        assert_eq!(plan.autosquash(), 0);
        assert_eq!(
            planned(&plan),
            vec!["pick under-sha", "pick mid-sha", "pick head-sha"]
        );

        // git's stacked markers: `fixup! fixup! x` names x, not a marker.
        let mut commits = linear();
        commits[0].subject = "fixup! fixup! mid".into();
        let mut plan = Plan::over(&commits, 2).expect("a window");
        assert_eq!(plan.autosquash(), 1);
        assert_eq!(
            planned(&plan),
            vec!["pick under-sha", "pick mid-sha", "fixup head-sha"]
        );
    }

    #[test]
    fn a_bare_marker_names_nothing_and_stands_as_a_pick() {
        // `fixup!` with an empty remainder, and `fixup! fixup!` stacked
        // to nothing: every subject starts with the empty string, so
        // without the guard both would fold into the newest older commit.
        for subject in ["fixup!", "fixup! fixup!", "squash!"] {
            let mut commits = linear();
            commits[0].subject = (*subject).into();
            let mut plan = Plan::over(&commits, 2).expect("a window");
            assert_eq!(plan.autosquash(), 0, "bare marker {subject:?} moved");
            assert_eq!(
                planned(&plan),
                vec!["pick under-sha", "pick mid-sha", "pick head-sha"],
                "bare marker {subject:?} did not stand"
            );
        }
    }

    #[test]
    fn a_fold_has_three_answers_to_the_message_question() {
        let commits = linear();
        // Squash keeps both messages, fixup keeps the older one, and
        // `fixup -C` keeps this commit's — one fold, three answers, which
        // is why the third is a flag and not a fourth action.
        let mut plan = Plan::over(&commits, 2).expect("a window");
        plan.set_fixup_keeping_message(1).expect("a fold");
        assert!(plan.keeps_a_message(), "the runner is not warned");
        assert_eq!(
            planned(&plan),
            vec!["pick under-sha", "fixup -C mid-sha", "pick head-sha"]
        );
        // And the bytes round-trip: parsing puts `-C` in the arg slot and
        // the sha in the rest, which reads oddly and emits exactly right —
        // the tolerance this module promises, over a flag it never
        // interprets.
        let script = plan.script(&mut |_| Ok(Vec::new())).expect("a script");
        let bytes = script.emit();
        assert!(
            bytes.starts_with(b"pick under-sha\nfixup -C mid-sha\n"),
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        assert_eq!(TodoScript::parse(&bytes).emit(), bytes);

        // Choosing another answer takes the flag with it: a stale `-C`
        // hanging on a squash is a message landing somewhere nobody chose.
        plan.set_action(1, Action::Squash).expect("a fold");
        assert!(!plan.keeps_a_message());
        assert_eq!(
            planned(&plan),
            vec!["pick under-sha", "squash mid-sha", "pick head-sha"]
        );

        // The oldest row refuses it exactly as it refuses every fold.
        let mut plan = Plan::over(&commits, 2).expect("a window");
        assert!(plan.set_fixup_keeping_message(2).is_err());
    }

    #[test]
    fn a_plan_says_when_it_will_pause() {
        let commits = linear();
        let mut plan = Plan::over(&commits, 2).expect("a window");
        assert!(!plan.pauses());
        plan.set_action(1, Action::Edit).expect("an edit");
        assert!(plan.pauses(), "an edit hands the rebase back standing");
        assert_eq!(
            planned(&plan),
            vec!["pick under-sha", "edit mid-sha", "pick head-sha"],
            "edit reaches git as itself; it opens nothing"
        );
    }

    #[test]
    fn a_marker_lands_on_the_newest_older_row_that_answers_it() {
        let mut commits = linear();
        commits[0].subject = "fixup! mid".into();
        commits[3].subject = "squash! nobody here".into();
        let marks = fixup_marks(&commits);
        assert_eq!(marks.len(), 2);
        assert_eq!(
            marks[0],
            FixupMark {
                index: 0,
                action: Action::Fixup,
                remainder: "mid".into(),
                target: Some(1),
            },
            "the fixup names mid, the row directly below"
        );
        assert_eq!(marks[1].target, None, "nobody here names no row");
    }

    #[test]
    fn a_marker_skips_markers_and_never_names_anything_newer() {
        let mut commits = linear();
        commits[0].subject = "fixup! under".into();
        commits[1].subject = "squash! under".into();
        let marks = fixup_marks(&commits);
        assert_eq!(
            marks[0].target,
            Some(2),
            "the marker at row 1 is not a landing"
        );
        assert_eq!(marks[1].target, Some(2));
        commits[1].subject = "fixup! head".into();
        // `head` sits above the marker: a fold only goes down.
        assert_eq!(fixup_marks(&commits)[1].target, None);
    }

    #[test]
    fn a_prefix_and_a_short_sha_answer_when_nothing_exact_does() {
        let mut commits = linear();
        commits[0].subject = "fixup! mi".into();
        assert_eq!(fixup_marks(&commits)[0].target, Some(1), "prefix");
        commits[0].subject = "fixup! ".into();
        commits[0].short = String::new();
        // A bare marker names nothing even though every subject starts
        // with the empty string.
        assert_eq!(fixup_marks(&commits)[0].target, None, "bare marker");
        commits[1].short = "mid-sha".into();
        commits[0].subject = "fixup! mid-sha".into();
        // `mid-sha` is not `mid`'s subject, so the short-sha arm answers.
        assert_eq!(fixup_marks(&commits)[0].target, Some(1), "short sha");
    }

    #[test]
    fn the_fixup_kind_cycles_and_spells_gits_flag() {
        assert_eq!(FixupKind::Fixup.flag(), "--fixup=");
        assert_eq!(FixupKind::Amend.flag(), "--fixup=amend:");
        assert_eq!(FixupKind::Reword.flag(), "--fixup=reword:");
        assert_eq!(FixupKind::Fixup.cycle(), FixupKind::Amend);
        assert_eq!(FixupKind::Amend.cycle(), FixupKind::Reword);
        assert_eq!(FixupKind::Reword.cycle(), FixupKind::Fixup);
        assert_eq!(FixupKind::default(), FixupKind::Fixup);
    }

    #[test]
    fn a_sha_is_followed_across_a_reorder_and_not_a_row() {
        let commits = linear();
        let mut plan = Plan::over(&commits, 2).expect("a window");
        assert_eq!(plan.index_of(b"head-sha"), Some(0));
        plan.move_down(0).expect("a move");
        assert_eq!(plan.index_of(b"head-sha"), Some(1), "the row moved");
        assert_eq!(plan.index_of(b"nobody"), None);
        assert_eq!(plan.base(), commits[2].short);
    }
}
