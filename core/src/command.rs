//! Keys, commands, and the mode stack.
//!
//! The last thing `docs/architecture.md` listed as missing from `core`, and the
//! one that most had to be here: a keybinding is the promise that gitten behaves
//! the same in a window, a browser and a terminal, and a promise kept in three
//! places is not kept.
//!
//! # A key is data and a command is a name
//!
//! Nothing in here is a function pointer, and that is
//! [decisions/0012](../docs/decisions/0012-config-is-data-behaviour-is-not.md)
//! applied to input: a settings panel has to be able to rewrite `gitten.toml` in
//! place, and it cannot round-trip a closure. So a binding says
//! `"ctrl-d" = "view.page-down"` and *what that does* lives in whatever is being
//! driven.
//!
//! The consequence is the interesting part. `core` resolves a keypress to a
//! command **name**; a client turns that name into a method call on a view it
//! owns. So the same `gitten.toml` drives a GPUI window and a terminal, and an
//! extension binds a key without either of them knowing it exists.
//!
//! ```text
//!   a keypress ──► Key ──► Keymap::resolve(&modes, pending) ──► "diff.next-file"
//!    per client    core              core                        per client
//! ```
//!
//! # What is not here
//!
//! **A timeout.** `g` followed by `g` is a chord; `g` on its own is a binding;
//! a terminal that waits 400 ms to tell them apart needs a clock, and `core` has
//! none and should not. So [`Keymap::bind`] *rejects* a binding that is a prefix
//! of another, and the ambiguity cannot arise. Two keys is the useful depth
//! anyway.
//!
//! **Any command's behaviour.** [`Commands`] is a registry of names, one-line
//! descriptions and — for some — the short label a footer draws beside a key:
//! enough for a help screen, and enough for the config layer to say "no such
//! command" instead of binding a key to nothing.

use std::fmt;

/// Which physical key, ignoring modifiers.
///
/// Deliberately small: what a keyboard-first app binds, and nothing else. A key
/// with no variant here is one no client can report anyway, because each of them
/// has to map its own platform's event onto this.
///
/// **The wheel is in here**, which looks like an exception and is the rule: a
/// notch is a control every client can report, it takes modifiers the way a key
/// does, and what it should *do* is exactly as much a matter of taste as what
/// `j` should do. Kept out, it would be a `match` in each client deciding that
/// the wheel scrolls — a keymap the client owned alone, with no line in
/// `gitten.toml` and no row on the help screen. A mouse *position* is another
/// matter and is not a key: it belongs to whatever was clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Code {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Esc,
    WheelUp,
    WheelDown,
}

impl Code {
    fn name(self) -> String {
        match self {
            Code::Char(' ') => "space".into(),
            Code::Char(c) => c.to_string(),
            Code::Up => "up".into(),
            Code::Down => "down".into(),
            Code::Left => "left".into(),
            Code::Right => "right".into(),
            Code::Home => "home".into(),
            Code::End => "end".into(),
            Code::PageUp => "pageup".into(),
            Code::PageDown => "pagedown".into(),
            Code::Enter => "enter".into(),
            Code::Tab => "tab".into(),
            Code::BackTab => "backtab".into(),
            Code::Backspace => "backspace".into(),
            Code::Delete => "delete".into(),
            Code::Esc => "esc".into(),
            Code::WheelUp => "wheelup".into(),
            Code::WheelDown => "wheeldown".into(),
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "space" => Code::Char(' '),
            "up" => Code::Up,
            "down" => Code::Down,
            "left" => Code::Left,
            "right" => Code::Right,
            "home" => Code::Home,
            "end" => Code::End,
            "pageup" => Code::PageUp,
            "pagedown" => Code::PageDown,
            "enter" | "return" => Code::Enter,
            "tab" => Code::Tab,
            "backtab" => Code::BackTab,
            "backspace" => Code::Backspace,
            "delete" | "del" => Code::Delete,
            "esc" | "escape" => Code::Esc,
            "wheelup" => Code::WheelUp,
            "wheeldown" => Code::WheelDown,
            _ => {
                let mut chars = s.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None;
                }
                Code::Char(c)
            }
        })
    }
}

/// One keypress.
///
/// **Shift is never set on a [`Code::Char`]**, and that is not a simplification.
/// Every platform reports `Shift-a` as the character `A`; a binding on
/// `shift-a` would then never fire, and one written both ways would fire twice.
/// [`Key::new`] drops it, so the invariant holds however a client builds one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub code: Code,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Key {
    pub fn new(code: Code, ctrl: bool, alt: bool, shift: bool) -> Self {
        Self {
            code,
            ctrl,
            alt,
            shift: shift && !matches!(code, Code::Char(_)),
        }
    }

    pub fn plain(code: Code) -> Self {
        Self::new(code, false, false, false)
    }

    pub fn char(c: char) -> Self {
        Self::plain(Code::Char(c))
    }

    pub fn ctrl(code: Code) -> Self {
        Self::new(code, true, false, false)
    }

    /// `ctrl-d`, `alt-enter`, `g`, `space`, `-`.
    ///
    /// Modifiers first, in that order, then the key. The separator is also a
    /// bindable key: `ctrl--` is control and minus, because the parse stops at
    /// the first word that is not a modifier and takes the rest verbatim.
    pub fn parse(s: &str) -> Option<Self> {
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        let mut rest = s;
        while let Some((head, tail)) = rest.split_once('-') {
            match head {
                "ctrl" => ctrl = true,
                "alt" | "opt" => alt = true,
                "shift" => shift = true,
                _ => break,
            }
            rest = tail;
        }
        Some(Self::new(Code::parse(rest)?, ctrl, alt, shift))
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("ctrl-")?;
        }
        if self.alt {
            f.write_str("alt-")?;
        }
        if self.shift {
            f.write_str("shift-")?;
        }
        f.write_str(&self.code.name())
    }
}

/// A sequence of keys that runs one command. Usually one key.
pub type Chord = Vec<Key>;

/// `g g`, `ctrl-d`, `shift-tab`. Whitespace between keys.
pub fn parse_chord(s: &str) -> Option<Chord> {
    let chord: Option<Chord> = s.split_whitespace().map(Key::parse).collect();
    chord.filter(|c| !c.is_empty())
}

pub fn chord_string(chord: &[Key]) -> String {
    chord
        .iter()
        .map(Key::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The mode every client is always in, and the one an unqualified binding lands
/// in.
pub const GLOBAL: &str = "global";

/// Which modes are active, innermost last.
///
/// A stack rather than one name because that is what modality actually is: a
/// diff view inside a search prompt is still a diff view, and `esc` has to leave
/// the prompt rather than the view. Lookup runs innermost-first and falls
/// through, so [`GLOBAL`] never has to be repeated in a mode's own bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Modes(Vec<String>);

impl Default for Modes {
    fn default() -> Self {
        Self::new()
    }
}

impl Modes {
    pub fn new() -> Self {
        Self(vec![GLOBAL.into()])
    }

    pub fn push(&mut self, mode: impl Into<String>) {
        self.0.push(mode.into());
    }

    /// Leaves the innermost mode. [`GLOBAL`] is never popped: a client with no
    /// modes at all still has to be able to quit.
    pub fn pop(&mut self) -> Option<String> {
        match self.0.len() > 1 {
            true => self.0.pop(),
            false => None,
        }
    }

    pub fn top(&self) -> &str {
        self.0.last().map(String::as_str).unwrap_or(GLOBAL)
    }

    pub fn contains(&self, mode: &str) -> bool {
        self.0.iter().any(|m| m == mode)
    }

    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

/// One binding, exactly as a config file states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub mode: String,
    pub chord: Chord,
    pub command: String,
}

/// What a keypress meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolve<'a> {
    /// Nothing is bound to this, and nothing could be by typing more.
    None,
    /// A longer chord in scope starts with what has been typed. Keep the pending
    /// keys and wait.
    Pending,
    Run(&'a str),
}

/// Every binding, and which mode each belongs to.
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: Vec<Binding>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Keymap {
    pub fn empty() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }

    /// The shipped keyboard. lazygit's, where lazygit has an opinion, and vi's
    /// where it does not.
    ///
    /// Everything a *list* does is in [`GLOBAL`], because every view is a list
    /// and a key that scrolls one should scroll all of them. What is bound per
    /// mode is only what is genuinely particular: a layout to cycle, a commit to
    /// open.
    pub fn builtin() -> Self {
        let mut k = Self::empty();
        let mut bind = |mode: &str, chord: &str, command: &str| {
            k.bind(mode, chord, command)
                .expect("a shipped binding conflicts with another")
        };

        bind(GLOBAL, "q", "quit");
        bind(GLOBAL, "ctrl-c", "quit");
        bind(GLOBAL, "?", "help");
        // Global and not diff-only: a palette is the whole window's, and the
        // commit graph is drawn out of the same one. Shifted, because cycling a
        // theme is a thing done twice a month and `t` is worth more than that.
        bind(GLOBAL, "T", "theme.cycle");
        // ` is unclaimed by every mode and untyped by lazygit's defaults, and
        // the message it opens is read once and dismissed — a key at the edge
        // of the keyboard for a panel at the edge of the app's life.
        bind(GLOBAL, "`", "message.show");
        bind(GLOBAL, "esc", "back");

        bind(GLOBAL, "j", "view.down");
        bind(GLOBAL, "down", "view.down");
        bind(GLOBAL, "k", "view.up");
        bind(GLOBAL, "up", "view.up");
        bind(GLOBAL, "ctrl-d", "view.page-down");
        bind(GLOBAL, "pagedown", "view.page-down");
        bind(GLOBAL, "ctrl-u", "view.page-up");
        bind(GLOBAL, "pageup", "view.page-up");
        // vi's, and the pair a wheel resolves to as well: moving the view is a
        // different verb from moving the cursor, so it is a different command
        // and not a modifier on `view.down`.
        bind(GLOBAL, "ctrl-e", "view.scroll-down");
        bind(GLOBAL, "ctrl-y", "view.scroll-up");
        bind(GLOBAL, "wheeldown", "view.scroll-down");
        bind(GLOBAL, "wheelup", "view.scroll-up");
        // `g`/`G` and not `gg`, so nothing in the shipped map is a prefix of
        // anything else and the whole thing resolves on one key.
        bind(GLOBAL, "g", "view.top");
        bind(GLOBAL, "home", "view.top");
        bind(GLOBAL, "G", "view.bottom");
        bind(GLOBAL, "end", "view.bottom");
        // The mouse's keyboard half. `y` is vi's yank and lazygit's copy, and
        // the pair below it is what a client with no selection of its own
        // ignores — a command nothing handles is a key that does nothing.
        bind(GLOBAL, "y", "copy.selection");
        bind(GLOBAL, "ctrl-a", "select.all");
        // lazygit's pane moves: h/l and the arrows walk the keyboard between
        // the window's panes — sidebar sections, the commit column, the diff
        // — whatever each is showing. The sideways text scroll these used to
        // carry moves to lazygit's own pair, `<`/`>`, which is where a diff
        // wider than the window scrolls once the arrows mean "which pane".
        bind(GLOBAL, "h", "pane.left");
        bind(GLOBAL, "left", "pane.left");
        bind(GLOBAL, "l", "pane.right");
        bind(GLOBAL, "right", "pane.right");
        bind(GLOBAL, "<", "view.left");
        bind(GLOBAL, ">", "view.right");
        // lazygit's list-panel pair — capitals of the pane moves, scrolling
        // the text instead. Same commands as `<`/`>`, lazygit's spellings.
        bind(GLOBAL, "H", "view.left");
        bind(GLOBAL, "L", "view.right");

        // The panel numbers read down the window's left stack, lazygit's
        // order: 1 STATUS, 2 FILES, 3 BRANCHES, 4 COMMITS, 5 STASH — the
        // stash under the commits, where parking ends a session's work. A
        // direct jump is global so it works whatever is focused, and
        // registered in that order, so 1 → 5 walks the stack top to bottom.
        bind(GLOBAL, "1", "status.focus");
        bind(GLOBAL, "2", "files.focus");
        bind(GLOBAL, "3", "branches.focus");
        bind(GLOBAL, "4", "commits.focus");
        bind(GLOBAL, "5", "stashes.focus");
        // lazygit's 0: the main view, whichever list holds the keyboard.
        bind(GLOBAL, "0", "diff.focus");

        // lazygit's global R: refresh the git state — re-run every pane's
        // reads, not a fetch. The queue's own finish does the same dance
        // after every write; this is the same wave, asked for by hand.
        bind(GLOBAL, "R", "repo.refresh");
        // lazygit's sync keys, global because they aim past every pane
        // at the branch HEAD sits on: P sends it, p pulls onto its upstream,
        // f updates what the remotes hold. The capital is lazygit's own
        // asymmetry between sending work and asking for it.
        bind(GLOBAL, "P", "repo.push");
        bind(GLOBAL, "p", "repo.pull");
        bind(GLOBAL, "f", "repo.fetch");

        // lazygit's files panel: space acts on the row the keyboard is on, by
        // the side of the index it sits on, and c commits what the index
        // holds. Both are particular to this pane, so they are not globals.
        bind("files", "space", "files.stage");
        bind("files", "c", "files.commit");
        // Amending rides the commit path — same field, same staged content —
        // and takes lazygit's amend capital A, one key off its lowercase
        // sibling stage-all because rewriting HEAD is not staging.
        bind("files", "A", "files.amend");
        // The rest of the panel's verbs, on lazygit's keys where lazygit has
        // one: D discards (twice-pressed — see files.discard's doc), a acts
        // on every row by the side of the index the keyboard sits in, i
        // stops git listing an untracked file.
        bind("files", "D", "files.discard");
        bind("files", "a", "files.stage-all");
        bind("files", "i", "files.ignore");
        // lazygit's shift-stash: park what the working tree holds and start
        // again from HEAD.
        bind("files", "s", "files.stash");

        // The stash stack, on lazygit's own three: space applies and keeps,
        // g pops — apply, then drop only when the apply was clean — and d
        // drops (twice-pressed, like files.discard). `g` and `d` are bound
        // globally to view movements; here they mean the row, which is the
        // mode-overrides-global rule doing its job, with home/end/G still
        // reaching the same places.
        bind("stashes", "space", "stashes.apply");
        bind("stashes", "g", "stashes.pop");
        bind("stashes", "d", "stashes.drop");

        // The branches panel, on lazygit's own letters: space checks out the
        // branch under the keyboard, n names a new one, r rebases the
        // checked-out branch onto the row (lazygit's r; it rewrites this
        // branch's own commits, so it asks twice), R renames the row
        // (lazygit's R), d deletes — twice-pressed like every destruction —
        // and T tags the row's commit.
        bind("branches", "space", "branches.checkout");
        bind("branches", "n", "branches.new");
        bind("branches", "r", "commits.rebase-onto");
        bind("branches", "R", "branches.rename");
        bind("branches", "d", "branches.delete");
        bind("branches", "T", "branches.new-tag");

        bind("diff", "s", "diff.cycle-layout");
        bind("diff", "w", "diff.cycle-wrap");
        bind("diff", "]", "diff.next-file");
        bind("diff", "[", "diff.prev-file");
        bind("diff", "tab", "diff.next-file");
        bind("diff", "backtab", "diff.prev-file");
        // The keyboard acts on the hunk it sits on, on lazygit's staging key:
        // space sends the hunk to the index, `u` brings it back (one less
        // finger than a shifted key, and nothing else claims it here — the
        // same trade [branches] makes for rename), and capital D discards
        // from the working tree, twice-pressed like files.discard. Only a
        // working-tree diff can answer any of them; a commit's diff has no
        // index to aim at, and says so.
        bind("diff", "space", "diff.stage-hunk");
        bind("diff", "u", "diff.unstage-hunk");
        bind("diff", "D", "diff.discard-hunk");

        bind("commits", "enter", "commits.open-diff");
        bind("commits", "/", "commits.search");
        // Resetting to the commit under the keyboard, exactly lazygit's
        // shape: `g` opens the question — a mode of its own, pushed only
        // while it stands — and s/m/h inside it pick the strength, the same
        // letters lazygit's reset menu lists. Outside the question those
        // letters are nobody's, and `h` stays the pane move it is
        // everywhere else; the question captures `h` for hard reset only
        // while it stands, which is the menu doing its job.
        bind("commits", "g", "commits.reset-menu");
        bind("reset", "s", "commits.reset-soft");
        bind("reset", "m", "commits.reset-mixed");
        bind("reset", "h", "commits.reset-hard");
        // lazygit's revert key. Nothing is destroyed — the undo arrives as a
        // new commit — so it takes no confirmation dance.
        bind("commits", "t", "commits.revert");
        // Folding the commit under the keyboard into its parent, on
        // lazygit's own squash and fixup letters. `f` shadows repo.fetch
        // inside this pane — lazygit makes the same trade — and the fetch
        // stays one pane away, on any other list. Drop is lazygit's own
        // letter, free in this mode. All three rewrite history, so each
        // asks twice.
        bind("commits", "s", "commits.squash-up");
        bind("commits", "f", "commits.fixup-up");
        bind("commits", "d", "commits.drop-commit");
        // The way out of a stranded rebase — one that stopped mid-flight on
        // a conflict or a refusal and left its state standing. lazygit
        // offers these through a menu that appears during a rebase; here
        // they are two named commands on the history pane's own keys,
        // capitals because neither is an everyday press and both act on a
        // rewrite in progress.
        bind("commits", "A", "rebase.abort");
        bind("commits", "C", "rebase.continue");
        // Cherry-picking the commit under the keyboard onto the current
        // branch. A free capital beside revert's `t`, for the verb that
        // shares its shape — history grows, nothing existing moves — and so
        // takes no confirmation dance either: dropping the copy undoes the
        // pick.
        bind("commits", "Y", "commits.cherry-pick");
        // Tagging the commit under the keyboard, on lazygit's own T. It
        // shadows theme.cycle inside this pane — a tag belongs here and the
        // theme is reachable everywhere else — which is the same
        // mode-overrides-global trade [branches] makes for its own T.
        bind("commits", "T", "commits.new-tag");
        // lazygit's n: a new branch growing from the commit under the
        // keyboard, named over the field the branches pane names its own
        // with.
        bind("commits", "n", "commits.new-branch");
        // lazygit's space on a commit: check it out detached — the same move
        // the branches pane's space makes onto a remote-tracking row, aimed
        // at a commit instead. HEAD's old branch keeps its name, so the
        // branches pane walks you back.
        bind("commits", "space", "commits.checkout");
        // The way out of a stranded cherry-pick, beside rebase.abort /
        // rebase.continue's capitals: those answer a *rebase* state, and
        // git's own reply to them over CHERRY_PICK_HEAD is "no rebase in
        // progress" — true, and useless. Capitals because neither press is
        // everyday and both act on a rewrite in progress; Z backs out (the
        // ctrl-z reflex) and X carries onward, its neighbour on the bottom
        // row there being no free letter left that begins either word.
        bind("commits", "Z", "commits.cherry-pick-abort");
        bind("commits", "X", "commits.cherry-pick-continue");

        // Text itself belongs to the platform input service. These are the two
        // transitions around it, kept as named commands so a config file can
        // move them without teaching a client another keymap.
        bind("input", "enter", "input.accept");
        bind("input", "esc", "input.cancel");

        // The help overlay owns the keyboard for as long as it stands: a client
        // resolves against this mode *alone* while it is up, so a chord that is
        // not here runs nothing underneath — a panel of keys that arms a file
        // discard behind itself is a trap, and it had one. Which is also why
        // the way out and the way down are spelled again here: the same command
        // names the lists use, bound in this mode so they stay reachable when
        // inheriting the globals no longer happens. `?` toggles the panel shut
        // and `esc` takes `back`'s own spelling, which closes help first.
        bind("help", "?", "help");
        bind("help", "esc", "back");
        bind("help", "j", "view.scroll-down");
        bind("help", "down", "view.scroll-down");
        bind("help", "k", "view.scroll-up");
        bind("help", "up", "view.scroll-up");
        bind("help", "g", "view.top");
        bind("help", "home", "view.top");
        bind("help", "G", "view.bottom");
        bind("help", "end", "view.bottom");

        bind("panes", "ctrl-j", "pane.next");
        bind("panes", "ctrl-k", "pane.prev");
        k
    }

    /// Adds a binding, replacing any on the same chord in the same mode.
    ///
    /// Rejects a chord that is a prefix of an existing one in the same mode, or
    /// that one of them is a prefix of — the alternative is a timeout, and a
    /// timeout needs a clock `core` does not have. The rejected binding is *not*
    /// added, so the map is never in a state that cannot resolve.
    pub fn bind(&mut self, mode: &str, chord: &str, command: &str) -> Result<(), String> {
        let Some(chord) = parse_chord(chord) else {
            return Err(format!("{chord:?} is not a key"));
        };
        if let Some(other) = self.prefix_conflict(mode, &chord) {
            return Err(format!(
                "{} conflicts with {} in [{mode}] — one is a prefix of the other, and telling \
                 them apart needs a timeout",
                chord_string(&chord),
                chord_string(&other),
            ));
        }
        let binding = Binding {
            mode: mode.into(),
            chord,
            command: command.into(),
        };
        // A replacement is the newest binding too. This matters when one
        // physical press has two spellings: resolution walks a mode newest
        // first, just as GPUI does, so replacing an older logical spelling must
        // move it past a physical alternative added in between.
        self.bindings
            .retain(|b| !(b.mode == mode && b.chord == binding.chord));
        self.bindings.push(binding);
        Ok(())
    }

    /// Removes a binding. What a config file does with `"j" = ""` — unbinding a
    /// built-in has to be expressible, or a shipped key can only be moved and
    /// never removed.
    pub fn unbind(&mut self, mode: &str, chord: &str) -> bool {
        let Some(chord) = parse_chord(chord) else {
            return false;
        };
        let before = self.bindings.len();
        self.bindings
            .retain(|b| !(b.mode == mode && b.chord == chord));
        self.bindings.len() != before
    }

    fn prefix_conflict(&self, mode: &str, chord: &[Key]) -> Option<Chord> {
        self.bindings
            .iter()
            .filter(|b| b.mode == mode && b.chord != chord)
            .find(|b| b.chord.starts_with(chord) || chord.starts_with(&b.chord))
            .map(|b| b.chord.clone())
    }

    /// What `pending` means with these modes active.
    ///
    /// Innermost mode first, falling through outwards, so a mode overrides
    /// [`GLOBAL`] by binding the same chord and inherits everything it does not.
    pub fn resolve(&self, modes: &Modes, pending: &[Key]) -> Resolve<'_> {
        self.resolve_any(
            modes,
            &pending
                .iter()
                .map(std::slice::from_ref)
                .collect::<Vec<&[Key]>>(),
        )
    }

    /// [`resolve`](Self::resolve) when a press may carry more than one
    /// spelling.
    ///
    /// A window's keystroke names a physical key *and* the character it would
    /// insert, and on anything but a US layout those part ways: option-s on a
    /// German keyboard inserts `ß`. GPUI's own matcher,
    /// `Keystroke::should_match`, answers for *each binding* whether it
    /// matches either spelling — logical first, then physical — so which one
    /// wins is decided by the map, never by the client picking a favourite
    /// before asking.
    ///
    /// So this takes one candidate list per press and matches each binding
    /// against **any** of its spellings, per position. Trying whole candidates
    /// in order instead would be wrong twice over: a global `ß` would beat a
    /// diff-mode `alt-s` because the logical spelling went first, and mode
    /// precedence — the walk below — is the invariant every other test here
    /// rests on. With the disjunction inside the match, the innermost mode's
    /// binding wins whichever spelling it was written in, exactly as one pass
    /// over GPUI's bindings in map order would land.
    ///
    /// Within a mode, an exact binding wins before a chord that could continue,
    /// preserving [`Self::resolve`]'s clockless contract. GPUI can defer the
    /// exact binding and replay it after a timeout; core owns no clock or replay
    /// queue, so waiting here would swallow the exact command forever. When
    /// alternate spellings make two exact bindings match, the later binding
    /// wins, as GPUI's keymap does for user bindings over defaults.
    ///
    /// `pending[i]` must be non-empty; [`Self::resolve`] is the
    /// single-spelling case.
    pub fn resolve_any(&self, modes: &Modes, pending: &[&[Key]]) -> Resolve<'_> {
        if pending.is_empty() || pending.iter().any(|alts| alts.is_empty()) {
            return Resolve::None;
        }
        for mode in modes.as_slice().iter().rev() {
            let resolved = self.resolve_mode_any(mode, pending);
            if resolved != Resolve::None {
                return resolved;
            }
        }
        Resolve::None
    }

    /// Resolves against exactly one mode, without inheriting [`GLOBAL`].
    ///
    /// A native text field needs this distinction: `j` is text while the field
    /// is focused, not the global `view.down`, while an explicit binding in
    /// `[keys.input]` still has to win before the platform inserts anything.
    pub fn resolve_mode_any<'a>(&'a self, mode: &str, pending: &[&[Key]]) -> Resolve<'a> {
        if pending.is_empty() || pending.iter().any(|alts| alts.is_empty()) {
            return Resolve::None;
        }
        let matches = |chord: &[Key]| {
            chord.len() == pending.len()
                && chord.iter().zip(pending).all(|(k, alts)| alts.contains(k))
        };
        let prefixes = |chord: &[Key]| {
            chord.len() > pending.len()
                && chord.iter().zip(pending).all(|(k, alts)| alts.contains(k))
        };
        if let Some(b) = self
            .bindings
            .iter()
            .rev()
            .find(|b| b.mode == mode && matches(&b.chord))
        {
            return Resolve::Run(&b.command);
        }
        if self
            .bindings
            .iter()
            .any(|b| b.mode == mode && prefixes(&b.chord))
        {
            return Resolve::Pending;
        }
        Resolve::None
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// Which keys run `command`, for a help screen and for a footer.
    pub fn keys_for(&self, command: &str) -> Vec<String> {
        self.bindings
            .iter()
            .filter(|b| b.command == command)
            .map(|b| chord_string(&b.chord))
            .collect()
    }

    /// Whether anything typed so far as `chord` could still reach a binding in
    /// mode `at` of `active`: an inner mode that binds the chord exactly, or a
    /// longer chord starting with it, or a prefix of it, answers first or keeps
    /// the keys waiting — the three ways [`resolve`] never arrives here.
    ///
    /// This is [`prefix_conflict`](Self::prefix_conflict) across modes instead
    /// of within one, which is why the same relation appears: a conflict inside
    /// a mode is refused at `bind` time, but between modes it is the whole
    /// point — it is how a mode overrides the globals it inherits.
    fn shadowed(&self, active: &[String], at: usize, chord: &[Key]) -> bool {
        active[at + 1..].iter().any(|inner| {
            self.bindings.iter().any(|o| {
                o.mode == *inner && (o.chord.starts_with(chord) || chord.starts_with(&o.chord))
            })
        })
    }

    /// The chords that run `command` **right now**, with `modes` active.
    ///
    /// [`keys_for`](Self::keys_for) lists everything ever bound; this walks the
    /// modes the way [`resolve`] does, innermost first, and drops every chord an
    /// inner mode shadows. What a close hint is written from: naming a key that
    /// would not fire is worse than naming none.
    pub fn live_keys_for(&self, command: &str, modes: &Modes) -> Vec<String> {
        let active = modes.as_slice();
        let mut out: Vec<String> = Vec::new();
        for (at, mode) in active.iter().enumerate().rev() {
            for b in self
                .bindings
                .iter()
                .filter(|b| b.mode == *mode && b.command == command)
            {
                if self.shadowed(active, at, &b.chord) {
                    continue;
                }
                let spelled = chord_string(&b.chord);
                if !out.contains(&spelled) {
                    out.push(spelled);
                }
            }
        }
        out
    }

    /// What the help screen shows with `modes` active: which key runs what,
    /// **now**.
    ///
    /// A projection and not a drawing — no colours, no widths, no panel —
    /// because what it says is a property of the keymap, the registry and the
    /// mode stack alone, and all three are the same in every client. The window
    /// and the terminal draw it differently; neither of them may say something
    /// different. That is also why it takes the *effective* modes rather than a
    /// screen: which bindings are live is decided by [`Keymap::resolve`]'s same
    /// innermost-first walk, so a key listed here is a key that would actually
    /// fire.
    pub fn help(&self, commands: &Commands, modes: &Modes) -> Vec<HelpRow> {
        let active = modes.as_slice();
        let mut out = Vec::new();
        for (at, mode) in active.iter().enumerate() {
            // Grouped by command, in the order this map holds them, so a config
            // file's own order survives into the help. A command bound to several
            // keys is one row with them joined, because that is one thing you can
            // do and not three.
            //
            // A chord an inner mode shadows is left out entirely: a key listed
            // here that would not fire is a lie in the one place that exists to
            // stop you guessing — and if every chord of a command is shadowed,
            // the command has no row and its mode may have no heading.
            let mut seen: Vec<&str> = Vec::new();
            let mut rows: Vec<HelpRow> = Vec::new();
            for b in self.bindings().iter().filter(|b| b.mode == *mode) {
                if seen.contains(&b.command.as_str()) {
                    continue;
                }
                seen.push(&b.command);
                let all = self
                    .bindings()
                    .iter()
                    .filter(|o| o.mode == *mode && o.command == b.command)
                    .filter(|o| !self.shadowed(active, at, &o.chord))
                    .map(|o| chord_string(&o.chord))
                    .collect::<Vec<_>>()
                    .join(" / ");
                if all.is_empty() {
                    continue;
                }
                let doc = commands
                    .get(&b.command)
                    .map(|c| c.doc.clone())
                    .unwrap_or_default();
                rows.push(HelpRow::Command {
                    name: b.command.clone(),
                    keys: all,
                    doc,
                });
            }
            if rows.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(HelpRow::Blank);
            }
            out.push(HelpRow::Mode(mode.clone()));
            out.extend(rows);
        }
        out
    }
}

/// One row of a help screen, as [`Keymap::help`] projects it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpRow {
    /// Which mode the rows under it (until the next heading) are bound in.
    Mode(String),
    /// Every key that runs one command there, joined, and what it does.
    Command {
        /// The command's registry name — what a client dispatches on, and how
        /// a client filters these rows without re-walking [`Commands`].
        name: String,
        /// The chords that run it, joined with `" / "` in keymap order.
        keys: String,
        /// The command's own one-liner, from [`Commands`].
        doc: String,
    },
    /// Air between two modes.
    Blank,
}

/// One command a client can be asked to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub name: String,
    /// One line, for a help screen. Present tense, no full stop — it sits in a
    /// column beside a key.
    pub doc: String,
    /// One or two words a status bar draws beside the key, when there is one:
    /// [`doc`](Self::doc) is a help-screen sentence and will not fit a footer.
    pub hint: Option<String>,
}

/// Every command name that exists.
///
/// A registry and not an enum, because an extension adds one and `core` cannot
/// know its name at compile time. What it buys is the two things a name-based
/// system otherwise loses: a help screen that lists what is actually there, and
/// a config file that can say *no such command* instead of silently binding a
/// key to nothing.
#[derive(Debug, Clone)]
pub struct Commands(Vec<Command>);

impl Default for Commands {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Commands {
    pub fn empty() -> Self {
        Self(Vec::new())
    }

    /// Every command the shipped clients implement.
    ///
    /// A client that cannot do one of these ignores it — a browser tab has no
    /// `quit` — and that is not a hole: a command nothing handles is a key that
    /// does nothing, which is what an unbound key does too.
    pub fn builtin() -> Self {
        let mut c = Self::empty();
        // The third column is the footer hint — `Some` where a status bar
        // should draw a word or two beside the key. Everything after it is
        // still [`Command::doc`], the help screen's sentence.
        for (name, doc, hint) in [
            ("quit", "leave", Some("quit")),
            ("help", "show the keys", Some("keys")),
            ("back", "leave the innermost mode", Some("back")),
            ("view.down", "one row down", None),
            ("view.up", "one row up", None),
            ("view.page-down", "a screenful down", None),
            ("view.page-up", "a screenful up", None),
            ("view.scroll-down", "the view down, not the cursor", None),
            ("view.scroll-up", "the view up, not the cursor", None),
            ("view.top", "the first row", None),
            ("view.bottom", "the last row", None),
            ("view.left", "scroll the text left", None),
            ("view.right", "scroll the text right", None),
            (
                "diff.next-file",
                "the next file's header",
                Some("next file"),
            ),
            (
                "diff.prev-file",
                "the previous file's header",
                Some("prev file"),
            ),
            ("diff.cycle-layout", "the next presentation", None),
            ("diff.cycle-wrap", "the next wrap", None),
            (
                "diff.stage-hunk",
                "stage the hunk under the keyboard into the index",
                Some("stage hunk"),
            ),
            (
                "diff.unstage-hunk",
                "take the hunk under the keyboard back out of the index",
                Some("unstage hunk"),
            ),
            (
                "diff.discard-hunk",
                "discard the hunk under the keyboard from the working tree, asked twice",
                Some("discard hunk"),
            ),
            ("theme.cycle", "the next theme", None),
            (
                "commits.open-diff",
                "show the diff pane, loaded with this commit",
                Some("diff"),
            ),
            ("commits.search", "search the commits", Some("search")),
            (
                "commits.reset-soft",
                "move this branch here, keeping every change staged",
                Some("reset soft"),
            ),
            (
                "commits.reset-mixed",
                "move this branch here, unstaging what it holds",
                Some("reset mixed"),
            ),
            (
                "commits.reset-hard",
                "move this branch here and discard the changes, asked twice",
                Some("reset hard"),
            ),
            (
                "commits.revert",
                "undo this commit with a new inverse commit",
                Some("revert"),
            ),
            (
                "commits.squash-up",
                "fold this commit into the one beneath it, keeping both messages, asked twice",
                Some("squash up"),
            ),
            (
                "commits.fixup-up",
                "fold this commit into the one beneath it, discarding this message, asked twice",
                Some("fixup up"),
            ),
            (
                "commits.drop-commit",
                "remove this commit from the branch, asked twice",
                Some("drop"),
            ),
            (
                "commits.rebase-onto",
                "move the current branch onto the selected branch, asked twice",
                Some("rebase onto"),
            ),
            (
                "rebase.abort",
                "give up the rebase in progress and put everything back where it was",
                None,
            ),
            (
                "rebase.continue",
                "carry on the rebase in progress once conflicts are resolved",
                None,
            ),
            (
                "commits.cherry-pick",
                "apply this commit onto the current branch as a new commit",
                Some("cherry-pick"),
            ),
            (
                "commits.new-tag",
                "name this commit with a new tag",
                Some("tag"),
            ),
            (
                "commits.reset-menu",
                "choose a strength to reset to this commit",
                Some("reset"),
            ),
            (
                "commits.new-branch",
                "grow a new branch from this commit",
                Some("new branch"),
            ),
            (
                "commits.checkout",
                "check out this commit, detaching HEAD",
                Some("checkout"),
            ),
            (
                "commits.cherry-pick-abort",
                "give up the cherry-pick in progress and put everything back where it was",
                None,
            ),
            (
                "commits.cherry-pick-continue",
                "carry on the cherry-pick in progress once conflicts are resolved",
                None,
            ),
            ("status.focus", "focus the status pane", None),
            ("files.focus", "focus the working-tree pane", None),
            (
                "files.stage",
                "stage or unstage the selected file",
                Some("stage"),
            ),
            ("files.commit", "commit the staged changes", Some("commit")),
            (
                "files.amend",
                "rewrite HEAD to hold the staged changes under a new message",
                Some("amend"),
            ),
            (
                "files.discard",
                "discard the selected file's changes, asked twice",
                Some("discard"),
            ),
            (
                "files.stage-all",
                "stage everything unstaged, or unstage everything staged",
                Some("stage all"),
            ),
            (
                "files.ignore",
                "add the selected untracked file to .gitignore",
                Some("ignore"),
            ),
            ("branches.focus", "focus the branches pane", None),
            (
                "branches.checkout",
                "check out the selected branch",
                Some("checkout"),
            ),
            ("branches.new", "create a branch", Some("new branch")),
            (
                "branches.rename",
                "rename the selected branch",
                Some("rename"),
            ),
            (
                "branches.delete",
                "delete the selected branch, asked twice",
                Some("delete"),
            ),
            (
                "branches.new-tag",
                "name the selected branch's commit with a new tag",
                Some("tag"),
            ),
            ("stashes.focus", "focus the stash list", None),
            ("commits.focus", "focus the commit list", None),
            (
                "files.stash",
                "park the working tree's changes on the stash stack",
                Some("stash"),
            ),
            (
                "stashes.apply",
                "apply this stash, keeping it",
                Some("apply"),
            ),
            (
                "stashes.pop",
                "apply this stash and drop it when the apply is clean",
                Some("pop"),
            ),
            ("stashes.drop", "drop this stash, asked twice", Some("drop")),
            (
                "repo.push",
                "send the current branch to its remote, setting the upstream if needed",
                Some("push"),
            ),
            (
                "repo.pull",
                "fast-forward the current branch onto its upstream",
                Some("pull"),
            ),
            (
                "repo.fetch",
                "update the remote-tracking branches",
                Some("fetch"),
            ),
            (
                "repo.refresh",
                "re-run every pane's reads from the repository",
                Some("refresh"),
            ),
            ("diff.focus", "focus the diff view", None),
            ("input.accept", "accept the text", None),
            ("input.cancel", "discard the text", None),
            ("pane.next", "the next list in the column", None),
            ("pane.prev", "the previous list in the column", None),
            ("pane.left", "the pane on the left", None),
            ("pane.right", "the pane on the right", None),
            ("select.all", "select the whole view", None),
            ("select.none", "drop the selection", None),
            (
                "copy.selection",
                "copy the selection, or the row the cursor is on",
                None,
            ),
            (
                "message.show",
                "show the full text of the last message",
                None,
            ),
        ] {
            c.add(name, doc, hint);
        }
        c
    }

    /// Adds one, replacing any with the same name — so a built-in's description
    /// can be corrected rather than only added to.
    pub fn register(&mut self, name: impl Into<String>, doc: impl Into<String>) {
        self.add(name, doc, None);
    }

    /// [`register`](Self::register)'s engine; only the shipped table carries a
    /// hint to pass it.
    fn add(&mut self, name: impl Into<String>, doc: impl Into<String>, hint: Option<&str>) {
        let command = Command {
            name: name.into(),
            doc: doc.into(),
            hint: hint.map(Into::into),
        };
        match self.0.iter().position(|c| c.name == command.name) {
            Some(i) => self.0[i] = command,
            None => self.0.push(command),
        }
    }

    pub fn known(&self, name: &str) -> bool {
        self.0.iter().any(|c| c.name == name)
    }

    pub fn get(&self, name: &str) -> Option<&Command> {
        self.0.iter().find(|c| c.name == name)
    }

    /// The short label a status bar draws beside the key, when the command has
    /// one.
    ///
    /// Not [`get`](Self::get)'s [`Command::doc`] — that is the help screen's
    /// sentence, and a footer has room for "stage hunk", not for what a hunk is.
    pub fn hint(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(|c| c.hint.as_deref())
    }

    pub fn all(&self) -> &[Command] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(s: &str) -> Chord {
        parse_chord(s).expect("a chord")
    }

    #[test]
    fn a_key_round_trips_through_its_own_spelling() {
        // The property a config file rests on: a settings panel writes what it
        // read, and `gitten config` emits a file that parses back.
        for s in [
            "j",
            "G",
            "?",
            "-",
            "space",
            "esc",
            "enter",
            "tab",
            "backtab",
            "up",
            "pagedown",
            "ctrl-d",
            "alt-enter",
            "ctrl-alt-left",
            "shift-tab",
            "ctrl--",
        ] {
            let key = Key::parse(s).unwrap_or_else(|| panic!("{s} did not parse"));
            assert_eq!(key.to_string(), s, "{s} did not round-trip");
        }
    }

    #[test]
    fn shift_is_dropped_on_a_character_and_kept_on_a_named_key() {
        // Every platform reports `Shift-a` as `A`. A binding on `shift-a` would
        // never fire, and one written both ways would fire twice.
        let a = Key::parse("shift-a").unwrap();
        assert!(!a.shift);
        assert_eq!(a, Key::char('a'));
        assert!(Key::parse("shift-tab").unwrap().shift);
        // ...and the capital is its own key, which is what actually arrives.
        assert_ne!(Key::char('A'), Key::char('a'));
    }

    #[test]
    fn nonsense_is_none_rather_than_a_guess() {
        assert_eq!(Key::parse(""), None);
        assert_eq!(Key::parse("ctrl-"), None);
        assert_eq!(
            Key::parse("hyper-j"),
            None,
            "an unknown modifier is not a key name"
        );
        assert_eq!(Key::parse("pgdown"), None);
        assert_eq!(parse_chord("   "), None);
    }

    #[test]
    fn a_chord_is_keys_with_spaces_between_them() {
        assert_eq!(keys("g g").len(), 2);
        assert_eq!(chord_string(&keys("ctrl-w  l")), "ctrl-w l");
    }

    #[test]
    fn the_shipped_map_resolves_on_one_key() {
        let k = Keymap::builtin();
        let modes = Modes::new();
        assert_eq!(k.resolve(&modes, &keys("j")), Resolve::Run("view.down"));
        assert_eq!(
            k.resolve(&modes, &keys("ctrl-d")),
            Resolve::Run("view.page-down")
        );
        assert_eq!(k.resolve(&modes, &keys("G")), Resolve::Run("view.bottom"));
        assert_eq!(k.resolve(&modes, &keys("z")), Resolve::None);
    }

    #[test]
    fn exact_mode_resolution_does_not_turn_text_into_global_commands() {
        let k = Keymap::builtin();
        assert_eq!(
            k.resolve_mode_any("input", &[&[Key::char('j')]]),
            Resolve::None
        );
        assert_eq!(
            k.resolve_mode_any("input", &[&[Key::plain(Code::Enter)]]),
            Resolve::Run("input.accept")
        );
    }

    #[test]
    fn the_help_mode_swallows_the_pane_verbs_it_is_only_describing() {
        let k = Keymap::builtin();
        // What a client does while the panel stands: resolve against `help`
        // alone, exactly as it does against `input` for a focused field. The
        // full walk still finds the files pane's `D` underneath — a discard
        // armed behind a panel that is only *describing* it, which is the one
        // thing a screen full of key names must not do.
        let mut modes = Modes::new();
        modes.push("files");
        modes.push("help");
        let d = keys("D");
        assert_eq!(k.resolve_any(&modes, &[&d]), Resolve::Run("files.discard"));
        assert_eq!(k.resolve_mode_any("help", &[&d]), Resolve::None);
        // And what it does answer: the way out, and its own scroll.
        assert_eq!(
            k.resolve_mode_any("help", &[&keys("?")]),
            Resolve::Run("help")
        );
        assert_eq!(
            k.resolve_mode_any("help", &[&keys("esc")]),
            Resolve::Run("back")
        );
        assert_eq!(
            k.resolve_mode_any("help", &[&keys("j")]),
            Resolve::Run("view.scroll-down")
        );
        assert_eq!(
            k.resolve_mode_any("help", &[&keys("end")]),
            Resolve::Run("view.bottom")
        );
    }

    #[test]
    fn the_wheel_is_a_key_like_any_other() {
        let k = Keymap::builtin();
        let modes = Modes::new();
        assert_eq!(
            k.resolve(&modes, &keys("wheeldown")),
            Resolve::Run("view.scroll-down")
        );
        // Round-trips, so `./dev config` writes a line that parses back.
        assert_eq!(Key::parse("wheelup").unwrap().to_string(), "wheelup");
        assert_eq!(
            Key::parse("ctrl-wheeldown").unwrap().to_string(),
            "ctrl-wheeldown"
        );
    }

    #[test]
    fn the_pane_moves_are_globals_on_lazygits_pair_and_the_arrows() {
        let k = Keymap::builtin();
        let modes = Modes::new();
        // Both spellings, letter and arrow — the letter is lazygit's h/l and
        // the arrow is what a Mac user's fingers also know. The text scroll
        // these keys used to carry keeps its commands on lazygit's own pair.
        for (chord, name) in [
            ("h", "pane.left"),
            ("left", "pane.left"),
            ("l", "pane.right"),
            ("right", "pane.right"),
            ("<", "view.left"),
            (">", "view.right"),
        ] {
            assert_eq!(
                k.resolve(&modes, &keys(chord)),
                Resolve::Run(name),
                "{chord} did not reach {name}"
            );
        }
        // Both registry halves: the command is in the projection a help panel
        // reads, and the key that resolves it is the one it was bound with.
        let commands = Commands::builtin();
        for name in ["pane.left", "pane.right"] {
            assert!(commands.known(name), "{name} is not registered");
            assert!(!k.keys_for(name).is_empty(), "{name} is bound to nothing");
        }
        // And the pane move survives in every mode: no panel owns `h` at
        // the panel level — lazygit's reset-hard lives inside its reset
        // question ([reset]), not on the plain key.
        let mut commits = Modes::new();
        commits.push("commits");
        assert_eq!(k.resolve(&commits, &keys("h")), Resolve::Run("pane.left"));
    }

    #[test]
    fn the_reset_question_is_its_own_mode_over_commits() {
        let k = Keymap::builtin();
        // Opening it is `g`, lazygit's key; the strengths answer only while
        // the question's mode is pushed, and `h` inside it is hard reset —
        // the one place the pane move loses its letter.
        let mut modes = Modes::new();
        modes.push("commits");
        assert_eq!(
            k.resolve(&modes, &keys("g")),
            Resolve::Run("commits.reset-menu")
        );
        assert_eq!(k.resolve(&modes, &keys("h")), Resolve::Run("pane.left"));
        modes.push("reset");
        for (chord, name) in [
            ("s", "commits.reset-soft"),
            ("m", "commits.reset-mixed"),
            ("h", "commits.reset-hard"),
        ] {
            assert_eq!(
                k.resolve(&modes, &keys(chord)),
                Resolve::Run(name),
                "{chord} did not reach {name} in [reset]"
            );
        }
        // Movement and escape survive inside the question: it is a question,
        // not a trap.
        assert_eq!(k.resolve(&modes, &keys("j")), Resolve::Run("view.down"));
        assert_eq!(k.resolve(&modes, &keys("esc")), Resolve::Run("back"));
        // The command is registered, so `?` names it.
        assert!(Commands::builtin().known("commits.reset-menu"));
    }

    #[test]
    fn a_mode_overrides_global_and_inherits_the_rest() {
        let mut k = Keymap::builtin();
        let mut modes = Modes::new();
        modes.push("diff");
        // Its own.
        assert_eq!(
            k.resolve(&modes, &keys("s")),
            Resolve::Run("diff.cycle-layout")
        );
        // Inherited, with nothing repeated in the mode to get it.
        assert_eq!(k.resolve(&modes, &keys("j")), Resolve::Run("view.down"));
        // Overridden.
        k.bind("diff", "j", "diff.next-file").unwrap();
        assert_eq!(
            k.resolve(&modes, &keys("j")),
            Resolve::Run("diff.next-file")
        );
        // ...and only inside that mode.
        assert_eq!(
            k.resolve(&Modes::new(), &keys("j")),
            Resolve::Run("view.down")
        );
    }

    #[test]
    fn a_mode_that_is_not_pushed_binds_nothing() {
        let k = Keymap::builtin();
        assert_eq!(k.resolve(&Modes::new(), &keys("s")), Resolve::None);
        assert_eq!(k.resolve(&Modes::new(), &keys("enter")), Resolve::None);
    }

    #[test]
    fn the_files_verbs_resolve_in_files_mode_and_nowhere_else() {
        let k = Keymap::builtin();
        let mut modes = Modes::new();
        modes.push("files");
        assert_eq!(
            k.resolve(&modes, &keys("space")),
            Resolve::Run("files.stage")
        );
        assert_eq!(k.resolve(&modes, &keys("c")), Resolve::Run("files.commit"));
        // Particular to the pane, so another context keeps its own meanings —
        // `c` is unbound globally and space belongs to no list.
        assert_eq!(k.resolve(&Modes::new(), &keys("space")), Resolve::None);
        assert_eq!(k.resolve(&Modes::new(), &keys("c")), Resolve::None);

        let commands = Commands::builtin();
        for (name, key) in [("files.stage", "space"), ("files.commit", "c")] {
            assert!(commands.known(name), "{name} is not registered");
            assert_eq!(k.keys_for(name), vec![key]);
        }
        // The spellings round-trip through a config file's.
        assert_eq!(Key::parse("space"), Some(Key::plain(Code::Char(' '))));
    }

    #[test]
    fn the_files_verbs_project_into_the_help_with_no_help_specific_code() {
        let mut modes = Modes::new();
        modes.push("files");
        let rows = shown(&Keymap::builtin(), &Commands::builtin(), &modes);
        assert!(
            rows.iter()
                .any(|r| r.contains("space · stage or unstage the selected file")),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("commit the staged changes")),
            "{rows:?}"
        );
    }

    #[test]
    fn the_file_level_verbs_resolve_on_lazygits_keys_and_nowhere_else() {
        let k = Keymap::builtin();
        let mut modes = Modes::new();
        modes.push("files");
        for (chord, name) in [
            ("D", "files.discard"),
            ("a", "files.stage-all"),
            ("i", "files.ignore"),
        ] {
            assert_eq!(
                k.resolve(&modes, &keys(chord)),
                Resolve::Run(name),
                "{chord} did not reach {name} in [files]"
            );
            // Particular to this pane: another context keeps its own
            // meanings, and a capital is not its lowercase twin's.
            assert_ne!(
                k.resolve(&Modes::new(), &keys(chord)),
                Resolve::Run(name),
                "{name} leaked out of [files]"
            );
        }
        // The capital D is its own binding and `d` is nobody's.
        assert_eq!(k.resolve(&modes, &keys("d")), Resolve::None);

        let commands = Commands::builtin();
        for (name, doc) in [
            ("files.discard", Some("asked twice")),
            ("files.stage-all", None),
            ("files.ignore", None),
        ] {
            let command = commands.get(name).unwrap_or_else(|| panic!("{name}"));
            if let Some(needle) = doc {
                assert!(command.doc.contains(needle), "{}: {}", name, command.doc);
            }
        }
    }

    #[test]
    fn the_branch_verbs_resolve_in_branches_mode_and_project_into_the_help() {
        let k = Keymap::builtin();
        let mut modes = Modes::new();
        modes.push("branches");
        for (chord, name) in [
            ("space", "branches.checkout"),
            ("n", "branches.new"),
            ("R", "branches.rename"),
            ("T", "branches.new-tag"),
            ("d", "branches.delete"),
        ] {
            assert_eq!(
                k.resolve(&modes, &keys(chord)),
                Resolve::Run(name),
                "{chord} did not reach {name} in [branches]"
            );
            // Particular to this pane: another context keeps its own
            // meanings — space belongs to no list, and `d` is nobody's
            // outside [files]' capital D.
            assert_ne!(
                k.resolve(&Modes::new(), &keys(chord)),
                Resolve::Run(name),
                "{name} leaked out of [branches]"
            );
        }
        // lazygit's r: rebase the checked-out branch onto the row. The name
        // sits in the commits family — it rewrites history — but the pane
        // decides the aim.
        assert_eq!(
            k.resolve(&modes, &keys("r")),
            Resolve::Run("commits.rebase-onto")
        );
        // The panel jump is global, numbered by the pane layout in lazygit's
        // order: 3 here, under status and files.
        assert_eq!(
            k.resolve(&Modes::new(), &keys("3")),
            Resolve::Run("branches.focus")
        );

        let commands = Commands::builtin();
        for name in [
            "branches.focus",
            "branches.checkout",
            "branches.new",
            "branches.rename",
            "branches.new-tag",
            "branches.delete",
        ] {
            assert!(commands.known(name), "{name} is not registered");
            assert_eq!(k.keys_for(name).len(), 1, "{name}: one key");
        }
        let rows = shown(&k, &commands, &modes);
        assert!(
            rows.iter()
                .any(|r| r.contains("space · check out the selected branch")),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("asked twice")),
            "the delete row says it confirms"
        );
    }

    #[test]
    fn the_stash_verbs_resolve_in_stashes_mode_and_focus_is_global() {
        let k = Keymap::builtin();
        // The direct jump works whatever is focused, like files.focus — 5
        // here, the foot of the stack.
        assert_eq!(
            k.resolve(&Modes::new(), &keys("5")),
            Resolve::Run("stashes.focus")
        );

        // The panel's three verbs, on lazygit's keys, and nowhere else.
        let mut modes = Modes::new();
        modes.push("stashes");
        for (chord, name) in [
            ("space", "stashes.apply"),
            ("g", "stashes.pop"),
            ("d", "stashes.drop"),
        ] {
            assert_eq!(
                k.resolve(&modes, &keys(chord)),
                Resolve::Run(name),
                "{chord} did not reach {name} in [stashes]"
            );
            assert_ne!(
                k.resolve(&Modes::new(), &keys(chord)),
                Resolve::Run(name),
                "{name} leaked out of [stashes]"
            );
        }
        // The mode-overrides-global rule at work: `g` means the row here and
        // view.top everywhere else, where `home` still reaches.
        assert_eq!(
            k.resolve(&Modes::new(), &keys("g")),
            Resolve::Run("view.top")
        );
        assert_eq!(
            k.resolve(&modes, &keys("home")),
            Resolve::Run("view.top"),
            "the movement vocabulary survives inside the mode"
        );

        // And parking the working tree belongs to the files pane's keyboard.
        let mut files = Modes::new();
        files.push("files");
        assert_eq!(k.resolve(&files, &keys("s")), Resolve::Run("files.stash"));

        let commands = Commands::builtin();
        for name in [
            "stashes.focus",
            "files.stash",
            "stashes.apply",
            "stashes.pop",
            "stashes.drop",
        ] {
            assert!(commands.known(name), "{name} is not registered");
            assert!(!k.keys_for(name).is_empty(), "{name} is bound to nothing");
        }
    }

    #[test]
    fn the_history_verbs_resolve_in_their_panes_on_lazygits_letters() {
        let k = Keymap::builtin();
        let commands = Commands::builtin();

        // Amend rides commit's pane and takes lazygit's amend capital, one
        // key off stage-all because rewriting HEAD is not staging.
        let mut files = Modes::new();
        files.push("files");
        assert_eq!(k.resolve(&files, &keys("A")), Resolve::Run("files.amend"));
        assert_ne!(
            k.resolve(&files, &keys("a")),
            Resolve::Run("files.amend"),
            "the capital is its own binding"
        );
        assert!(commands.known("files.amend"));

        // lazygit's own letters for this panel: s squash, f fixup, t revert,
        // d drop, T tag, n new-branch, space checkout — and g opens the
        // reset question rather than any strength firing directly. `f`
        // shadows repo.fetch inside the pane, lazygit's own trade; `h` is
        // nobody's here, so the pane move keeps it.
        let mut commits = Modes::new();
        commits.push("commits");
        for (chord, name) in [
            ("s", "commits.squash-up"),
            ("f", "commits.fixup-up"),
            ("d", "commits.drop-commit"),
            ("t", "commits.revert"),
            ("g", "commits.reset-menu"),
            ("Y", "commits.cherry-pick"),
            ("T", "commits.new-tag"),
            ("n", "commits.new-branch"),
            ("space", "commits.checkout"),
            ("Z", "commits.cherry-pick-abort"),
            ("X", "commits.cherry-pick-continue"),
        ] {
            assert_eq!(
                k.resolve(&commits, &keys(chord)),
                Resolve::Run(name),
                "{chord} did not reach {name} in [commits]"
            );
            // Particular to this pane: a commit list has no stash, no
            // pane.left worth keeping, and no other mode's meanings.
            assert_ne!(
                k.resolve(&Modes::new(), &keys(chord)),
                Resolve::Run(name),
                "{name} leaked out of [commits]"
            );
            let command = commands.get(name).unwrap_or_else(|| panic!("{name}"));
            if name == "commits.drop-commit" {
                assert!(command.doc.contains("asked twice"), "{}", command.doc);
            }
        }
        // The movement vocabulary survives inside the mode, minus what the
        // panel's own letters took over.
        assert_eq!(k.resolve(&commits, &keys("j")), Resolve::Run("view.down"));
    }

    #[test]
    fn the_sync_verbs_are_globals_on_lazygits_keys() {
        let k = Keymap::builtin();
        for (chord, name) in [("P", "repo.push"), ("p", "repo.pull"), ("f", "repo.fetch")] {
            // Repo-level actions: no pane pushed, nothing focused — they aim
            // at the branch HEAD sits on wherever the keyboard happens to be,
            // which is why these are globals and not a mode's bindings.
            assert_eq!(
                k.resolve(&Modes::new(), &keys(chord)),
                Resolve::Run(name),
                "{chord} did not reach {name} globally"
            );
            // Inherited inside a pane too, never re-bound there.
            let mut modes = Modes::new();
            modes.push("branches");
            assert_eq!(k.resolve(&modes, &keys(chord)), Resolve::Run(name));
        }
        // A capital is not its lowercase twin's binding: sending and asking
        // stay two commands on lazygit's pair.
        assert_ne!(
            k.resolve(&Modes::new(), &keys("p")),
            k.resolve(&Modes::new(), &keys("P"))
        );

        let commands = Commands::builtin();
        for name in ["repo.push", "repo.pull", "repo.fetch"] {
            assert!(commands.known(name), "{name} is not registered");
            assert_eq!(k.keys_for(name).len(), 1, "{name}: one key");
        }
    }

    #[test]
    fn a_chord_waits_for_its_second_key() {
        let mut k = Keymap::empty();
        k.bind(GLOBAL, "ctrl-w l", "pane.right").unwrap();
        let modes = Modes::new();
        assert_eq!(k.resolve(&modes, &keys("ctrl-w")), Resolve::Pending);
        assert_eq!(
            k.resolve(&modes, &keys("ctrl-w l")),
            Resolve::Run("pane.right")
        );
        assert_eq!(k.resolve(&modes, &keys("ctrl-w x")), Resolve::None);
    }

    #[test]
    fn a_prefix_conflict_is_refused_rather_than_timed_out() {
        // Both directions: the shorter arriving second is the same ambiguity.
        let mut k = Keymap::empty();
        k.bind(GLOBAL, "g", "view.top").unwrap();
        let e = k.bind(GLOBAL, "g g", "view.top").unwrap_err();
        assert!(e.contains("prefix"), "{e}");
        assert!(e.contains("timeout"), "the message did not say why: {e}");

        let mut k = Keymap::empty();
        k.bind(GLOBAL, "g g", "view.top").unwrap();
        assert!(k.bind(GLOBAL, "g", "view.top").is_err());
        // The rejected one was not added, so the map still resolves.
        assert_eq!(k.resolve(&Modes::new(), &keys("g")), Resolve::Pending);

        // A different mode is a different namespace and does not conflict.
        assert!(k.bind("diff", "g", "view.top").is_ok());
    }

    #[test]
    fn nothing_shipped_conflicts_with_anything_else_shipped() {
        // `Keymap::builtin` panics on a conflict, so this is really a test that
        // it is still constructible — which is the check that matters when
        // somebody adds a two-key binding to it.
        let k = Keymap::builtin();
        assert!(!k.bindings().is_empty());
        for b in k.bindings() {
            assert_eq!(
                b.chord.len(),
                1,
                "{} is a chord in the shipped map",
                chord_string(&b.chord)
            );
        }
    }

    #[test]
    fn rebinding_replaces_and_unbinding_removes() {
        let mut k = Keymap::builtin();
        k.bind(GLOBAL, "j", "view.up").unwrap();
        assert_eq!(
            k.resolve(&Modes::new(), &keys("j")),
            Resolve::Run("view.up")
        );
        // In *this* mode: `j` is spelled again in `[help]`, where it means the
        // panel's own scroll, and a replacement here must not touch that one.
        assert_eq!(
            k.bindings()
                .iter()
                .filter(|b| b.mode == GLOBAL && b.chord == keys("j"))
                .count(),
            1
        );
        // A built-in has to be removable, not only movable.
        assert!(k.unbind(GLOBAL, "j"));
        assert_eq!(k.resolve(&Modes::new(), &keys("j")), Resolve::None);
        assert!(!k.unbind(GLOBAL, "j"), "unbinding twice reported a change");
    }

    #[test]
    fn a_command_can_be_found_by_the_keys_that_run_it() {
        let k = Keymap::builtin();
        let mut found = k.keys_for("view.down");
        found.sort();
        assert_eq!(found, vec!["down", "j"]);
        assert!(k.keys_for("nothing.at.all").is_empty());
    }

    #[test]
    fn every_shipped_binding_names_a_registered_command() {
        // What stops a key being bound to nothing: the config layer runs this
        // same check against the file, and the shipped map has to pass it too.
        let commands = Commands::builtin();
        for b in Keymap::builtin().bindings() {
            assert!(commands.known(&b.command), "{} is not a command", b.command);
        }
    }

    #[test]
    fn every_registered_command_says_what_it_does() {
        for c in Commands::builtin().all() {
            assert!(!c.doc.is_empty(), "{} has no description", c.name);
            assert!(
                !c.doc.ends_with('.'),
                "{} reads as a sentence, not a label",
                c.name
            );
        }
    }

    #[test]
    fn a_hint_resolves_by_name_and_an_unknown_name_has_none() {
        let commands = Commands::builtin();
        for c in commands.all() {
            if let Some(hint) = &c.hint {
                // Round-trips through the accessor a status bar reads: same
                // lookup as `get`, so a registered command answers by name.
                assert_eq!(commands.hint(&c.name), Some(hint.as_str()), "{}", c.name);
                // One or two words is what fits beside a key.
                assert!(
                    hint.split_whitespace().count() <= 2,
                    "{}: \"{hint}\" will not fit a footer",
                    c.name
                );
            } else {
                assert_eq!(commands.hint(&c.name), None, "{}", c.name);
            }
        }
        assert_eq!(commands.hint("no.such.command"), None);
    }

    #[test]
    fn an_extension_registers_a_command_and_binds_a_key_to_it() {
        // Rule 1, for input: nine lines, and nothing in `core` had to know.
        let mut commands = Commands::builtin();
        let mut keys_ = Keymap::builtin();
        commands.register("blame.toggle", "show blame beside the diff");
        keys_.bind("diff", "b", "blame.toggle").unwrap();

        let mut modes = Modes::new();
        modes.push("diff");
        assert!(commands.known("blame.toggle"));
        assert_eq!(
            keys_.resolve(&modes, &keys("b")),
            Resolve::Run("blame.toggle")
        );
        assert_eq!(keys_.keys_for("blame.toggle"), vec!["b"]);
    }

    #[test]
    fn the_mode_stack_always_has_something_in_it() {
        let mut m = Modes::new();
        assert_eq!(m.top(), GLOBAL);
        m.push("diff");
        m.push("search");
        assert_eq!(m.top(), "search");
        assert!(m.contains("diff"));
        assert_eq!(m.pop().as_deref(), Some("search"));
        assert_eq!(m.pop().as_deref(), Some("diff"));
        assert_eq!(m.pop(), None, "global was popped and nothing can quit");
        assert_eq!(m.top(), GLOBAL);
    }

    #[test]
    fn an_empty_press_resolves_to_nothing() {
        assert_eq!(Keymap::builtin().resolve(&Modes::new(), &[]), Resolve::None);
    }

    // ------------------------------------------------- alternate spellings
    //
    // The window client reports a keystroke as a physical key *and* an insert;
    // where they differ, GPUI matches a binding against either. These hold the
    // contract `resolve_any` exists for: the map decides, never the spelling.

    /// Option-s on a German layout: insert `ß`, physical `s` with alt.
    fn option_s() -> Vec<Key> {
        vec![Key::char('ß'), Key::parse("alt-s").unwrap()]
    }

    #[test]
    fn either_spelling_of_a_press_fires_whichever_binding_exists() {
        let mut modes = Modes::new();
        modes.push("diff");

        // The logical spelling is bound: it runs.
        let mut k = Keymap::empty();
        k.bind("diff", "ß", "layout.ssharp").unwrap();
        assert_eq!(
            k.resolve_any(&modes, &[&option_s()]),
            Resolve::Run("layout.ssharp")
        );

        // Only the physical one is bound: it runs too — the press is not lost
        // for lack of the character.
        let mut k = Keymap::empty();
        k.bind("diff", "alt-s", "layout.alts").unwrap();
        assert_eq!(
            k.resolve_any(&modes, &[&option_s()]),
            Resolve::Run("layout.alts")
        );
    }

    #[test]
    fn mode_precedes_spelling_order() {
        // The trap a candidate *order* would fall into: `ß` is the logical
        // spelling and would be tried first, and a global binding on it would
        // beat a diff-mode binding on alt-s. GPUI checks bindings, not
        // spellings — so does this walk.
        let mut k = Keymap::builtin();
        k.bind(GLOBAL, "ß", "global.ssharp").unwrap();
        k.bind("diff", "alt-s", "diff.alts").unwrap();
        let mut modes = Modes::new();
        modes.push("diff");
        assert_eq!(
            k.resolve_any(&modes, &[&option_s()]),
            Resolve::Run("diff.alts"),
            "the inner mode won with its own spelling"
        );

        // Outside the mode the same press reaches the global after all.
        assert_eq!(
            k.resolve_any(&Modes::new(), &[&option_s()]),
            Resolve::Run("global.ssharp")
        );
    }

    #[test]
    fn an_exact_alternate_wins_without_a_clock_and_a_lone_chord_still_waits() {
        // GPUI can wait and replay an exact binding after a timeout. Core has
        // neither facility, so an exact logical spelling must not disappear
        // forever behind a physical spelling's longer chord.
        let mut k = Keymap::empty();
        k.bind(GLOBAL, "ß", "insert.ssharp").unwrap();
        k.bind(GLOBAL, "alt-s n", "pane.next").unwrap();
        let modes = Modes::new();

        assert_eq!(
            k.resolve_any(&modes, &[&option_s()]),
            Resolve::Run("insert.ssharp")
        );

        // With no exact binding, both spellings remain alive while the chord
        // waits and its physical branch can complete.
        let mut k = Keymap::empty();
        k.bind(GLOBAL, "alt-s n", "pane.next").unwrap();
        assert_eq!(k.resolve_any(&modes, &[&option_s()]), Resolve::Pending);
        let n = [Key::char('n')];
        assert_eq!(
            k.resolve_any(&modes, &[&option_s(), &n]),
            Resolve::Run("pane.next")
        );
    }

    #[test]
    fn a_later_binding_wins_when_two_spellings_match_in_one_mode() {
        let mut k = Keymap::empty();
        k.bind(GLOBAL, "[", "default.logical").unwrap();
        k.bind(GLOBAL, "alt-5", "user.physical").unwrap();
        let candidates = [Key::char('['), Key::parse("alt-5").unwrap()];
        assert_eq!(
            k.resolve_any(&Modes::new(), &[&candidates]),
            Resolve::Run("user.physical")
        );

        // Replacing the older chord is itself the newest registration.
        k.bind(GLOBAL, "[", "user.logical").unwrap();
        assert_eq!(
            k.resolve_any(&Modes::new(), &[&candidates]),
            Resolve::Run("user.logical")
        );
    }

    #[test]
    fn resolve_and_resolve_any_agree_on_a_single_spelling() {
        let k = Keymap::builtin();
        let mut modes = Modes::new();
        modes.push("commits");
        for chord in ["j", "?", "enter", "ctrl-d", "z"] {
            let keys = keys(chord);
            let singles: Vec<&[Key]> = keys.iter().map(std::slice::from_ref).collect();
            assert_eq!(
                k.resolve(&modes, &keys),
                k.resolve_any(&modes, &singles),
                "{chord} resolved differently through alternatives"
            );
        }
    }

    #[test]
    fn an_empty_candidate_list_resolves_to_nothing() {
        // A press no client can bind translates to nothing; feeding that
        // nothing onward must not read as "the first key of every chord".
        let k = Keymap::builtin();
        let j = [Key::char('j')];
        assert_eq!(k.resolve_any(&Modes::new(), &[&[]]), Resolve::None);
        assert_eq!(k.resolve_any(&Modes::new(), &[&[], &j]), Resolve::None);
    }

    fn shown(keys: &Keymap, commands: &Commands, modes: &Modes) -> Vec<String> {
        keys.help(commands, modes)
            .iter()
            .map(|row| match row {
                HelpRow::Mode(name) => format!("[{name}]"),
                // The name rides along but this projection says only what the
                // help screen has always said.
                HelpRow::Command { name: _, keys, doc } => format!("{keys} · {doc}"),
                HelpRow::Blank => String::new(),
            })
            .collect()
    }

    #[test]
    fn the_projection_lists_keys_and_what_they_do() {
        let rows = shown(&Keymap::builtin(), &Commands::builtin(), &Modes::new());
        // Keys and descriptions, both.
        assert!(rows
            .iter()
            .any(|r| r.contains("j / down") || r.contains("down / j")));
        assert!(rows.iter().any(|r| r.contains("one row down")));
        assert!(
            rows.iter().any(|r| r.contains("leave")),
            "no description for quit"
        );
        assert!(
            rows.iter().any(|r| r.contains("ctrl-d")),
            "a modified key was not spelled out"
        );
        // The mode is named.
        assert!(rows.iter().any(|r| r == "[global]"));
    }

    #[test]
    fn the_projection_shows_only_the_active_modes() {
        // A key bound in `commits` is not a key you can press in a diff, and
        // listing it is a lie in the one place that exists to stop you guessing.
        let (k, c) = (Keymap::builtin(), Commands::builtin());
        let global = shown(&k, &c, &Modes::new());
        assert!(!global.iter().any(|r| r.contains("the next presentation")));

        let mut modes = Modes::new();
        modes.push("diff");
        let in_diff = shown(&k, &c, &modes);
        assert!(in_diff.iter().any(|r| r.contains("the next presentation")));
        assert!(
            in_diff.iter().any(|r| r == "[diff]"),
            "the mode is not named"
        );
        assert!(
            !in_diff
                .iter()
                .any(|r| r.contains("the diff for this commit")),
            "a commits key leaked in"
        );
        // ...and the mode's rows come after the globals they resolve through,
        // which is the order the bindings resolve in reversed.
        let g = in_diff.iter().position(|r| r == "[global]").unwrap();
        let d = in_diff.iter().position(|r| r == "[diff]").unwrap();
        assert!(g < d);
    }

    #[test]
    fn a_command_with_several_keys_is_one_projected_row() {
        let k = Keymap::builtin();
        let rows = k.help(&Commands::builtin(), &Modes::new());
        let hits = rows
            .iter()
            .filter(|r| matches!(r, HelpRow::Command { doc, .. } if doc == "one row down"))
            .count();
        assert_eq!(hits, 1, "one command, one row");
    }

    #[test]
    fn a_binding_from_the_config_file_is_projected_without_being_told_to() {
        // The whole point of the projection being a function of the registry.
        let mut k = Keymap::builtin();
        let mut c = Commands::builtin();
        c.register("blame.toggle", "show blame beside the diff");
        k.bind("global", "b", "blame.toggle").unwrap();
        assert!(shown(&k, &c, &Modes::new())
            .iter()
            .any(|r| r.contains("show blame beside the diff")));
    }

    #[test]
    fn an_inner_mode_shadows_an_outer_binding_out_of_the_projection() {
        // `?` is bound globally, and a mode that binds it too answers first —
        // resolve says so, so the help has to agree or it lists a key that
        // does nothing.
        let mut k = Keymap::empty();
        k.bind(GLOBAL, "?", "help").unwrap();
        k.bind(GLOBAL, "y", "copy.selection").unwrap();
        k.bind(GLOBAL, "h", "view.left").unwrap();
        let mut c = Commands::builtin();
        c.register("diff.where", "where am I");
        k.bind("diff", "?", "diff.where").unwrap();
        // A chord is shadowed by an inner *prefix* as well: typing `y` waits
        // for the mode's second key and never reaches the global binding.
        k.bind("diff", "y n", "diff.next").unwrap();
        c.register("diff.next", "the next one");

        let mut modes = Modes::new();
        modes.push("diff");
        let rows = shown(&k, &c, &modes);

        // The global rows are gone; the mode's own are there.
        let global = rows.iter().position(|r| r == "[global]").unwrap();
        let diff = rows.iter().position(|r| r == "[diff]").unwrap();
        assert!(
            !rows[global..diff].iter().any(|r| r.contains("leave")),
            "a shadowed key was still projected: {rows:?}"
        );
        assert!(
            rows[global..diff]
                .iter()
                .any(|r| r.contains("scroll the text left")),
            "an unshadowed global row must survive beside the shadowed ones"
        );
        assert!(
            !rows[..global].iter().any(|r| !r.is_empty()),
            "nothing is projected before the outermost mode"
        );
        assert!(rows[diff..].iter().any(|r| r.contains("where am I")));

        // ...and without the mode pushed, nothing was shadowed.
        assert_eq!(shown(&k, &c, &Modes::new()).len(), rows.len() - 2);
    }

    #[test]
    fn a_chord_shadowed_by_a_shorter_inner_binding_leaves_too() {
        // The other direction of the same relation: the mode binds the prefix,
        // the globals bind the chord. Typing `g` fires the mode's command
        // before `g g` can ever complete.
        let mut k = Keymap::empty();
        k.bind(GLOBAL, "g g", "view.top").unwrap();
        k.bind("diff", "g", "goto.line").unwrap();
        let mut c = Commands::builtin();
        c.register("goto.line", "a line by number");

        let mut modes = Modes::new();
        modes.push("diff");
        let rows = shown(&k, &c, &modes);
        assert!(
            !rows
                .iter()
                .any(|r| r.contains("the first row") || r.contains("g g")),
            "the two-key chord survived its own prefix: {rows:?}"
        );
        assert!(rows.iter().any(|r| r.contains("a line by number")));
    }

    #[test]
    fn a_fully_shadowed_command_takes_its_row_and_heading_with_it() {
        let mut k = Keymap::builtin();
        k.bind("diff", "?", "help").unwrap(); // rebinds the only `?`
        let mut modes = Modes::new();
        modes.push("diff");
        let rows = shown(&k, &Commands::builtin(), &modes);
        // `show the keys` still appears — from the *diff* row now, which is the
        // one that would fire — but `q`'s twin below it did not move.
        assert!(rows.iter().filter(|r| r.contains("show the keys")).count() == 1);

        // Every global binding shadowed: the heading goes with them.
        let mut all = Keymap::builtin();
        for b in Keymap::builtin().bindings() {
            if b.mode == GLOBAL {
                let chord = chord_string(&b.chord);
                all.bind("diff", &chord, "diff.take")
                    .expect("shipped chords are single keys and do not collide");
            }
        }
        let rows = shown(&all, &Commands::builtin(), &modes);
        assert!(
            !rows.iter().any(|r| r == "[global]"),
            "nothing under it fires any more: {rows:?}"
        );
    }

    #[test]
    fn the_live_keys_for_a_command_follow_the_modes() {
        // What a close hint is written from: with help open, the key that
        // closes it is whatever runs `help` through the modes actually live.
        let k = Keymap::builtin();
        assert_eq!(k.live_keys_for("help", &Modes::new()), vec!["?"]);

        let mut open = Modes::new();
        open.push("help");
        assert_eq!(
            k.live_keys_for("help", &open),
            vec!["?"],
            "the help mode's own `?` is the one that fires"
        );

        // A mode that takes `?` over leaves the hint nothing to say — with the
        // help mode's own `?` unbound, as a config file may do it, because
        // otherwise the innermost mode is the one holding the key and no outer
        // mode can shadow it.
        let mut k = Keymap::builtin();
        assert!(k.unbind("help", "?"));
        k.bind("diff", "?", "diff.cycle-layout").unwrap();
        let mut in_diff = Modes::new();
        in_diff.push("diff");
        in_diff.push("help");
        assert!(k.live_keys_for("help", &in_diff).is_empty());
        // And the same map outside the mode still names it.
        assert_eq!(k.live_keys_for("help", &Modes::new()), vec!["?"]);
    }

    #[test]
    fn an_unbound_key_leaves_the_projection_and_an_unknown_mode_is_silent() {
        let mut k = Keymap::builtin();
        let c = Commands::builtin();
        assert!(shown(&k, &c, &Modes::new())
            .iter()
            .any(|r| r.contains("one row down")));
        k.unbind("global", "j");
        k.unbind("global", "down");
        assert!(!shown(&k, &c, &Modes::new())
            .iter()
            .any(|r| r.contains("one row down")));

        // A pushed mode with no bindings of its own adds nothing, not even a
        // heading — it inherits everything and repeats nothing.
        let mut modes = Modes::new();
        modes.push("empty-mode");
        assert_eq!(k.help(&c, &modes), k.help(&c, &Modes::new()));
    }
}
