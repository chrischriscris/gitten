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
    /// Replay it, but stop to edit the message. Not drivable tonight — see
    /// [`TodoScript::validate`] for why, said where a user will read it.
    Reword,
    /// Replay it, but stop for amending. Same story as [`Action::Reword`].
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
    /// editor — the one thing this client cannot drive tonight, because a
    /// scripted `GIT_SEQUENCE_EDITOR` says nothing about `GIT_EDITOR`.
    fn needs_an_editor(self) -> bool {
        matches!(self, Action::Reword | Action::Edit)
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
    /// `reword` and `edit` each stop and open `GIT_EDITOR`, a second editor
    /// beyond the sequencer one, with no scripted answer tonight; so does
    /// `fixup -c`, whose lowercase flag exists precisely to edit the melded
    /// message (capital `-C` keeps it and opens nothing, so it passes).
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
    fn reword_and_edit_are_named_by_the_validation_that_refuses_them() {
        let mut script = TodoScript::default();
        script.push_step(Action::Pick, b"1111111");
        script.push_step(Action::Squash, b"2222222");
        assert_eq!(script.validate(), Ok(()));

        for action in [Action::Reword, Action::Edit] {
            let mut bad = TodoScript::default();
            bad.push_step(action, b"1111111");
            let err = bad.validate().expect_err("refused");
            assert!(err.contains(action.word()), "{action:?} named: {err}");
            assert!(err.contains("editor"), "{err}");
        }

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
}
