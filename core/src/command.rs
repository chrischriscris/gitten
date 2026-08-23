//! Keys, commands, and the mode stack.
//!
//! The last thing `docs/architecture.md` listed as missing from `core`, and the
//! one that most had to be here: a keybinding is the promise that plait behaves
//! the same in a window, a browser and a terminal, and a promise kept in three
//! places is not kept.
//!
//! # A key is data and a command is a name
//!
//! Nothing in here is a function pointer, and that is
//! [decisions/0012](../docs/decisions/0012-config-is-data-behaviour-is-not.md)
//! applied to input: a settings panel has to be able to rewrite `plait.toml` in
//! place, and it cannot round-trip a closure. So a binding says
//! `"ctrl-d" = "view.page-down"` and *what that does* lives in whatever is being
//! driven.
//!
//! The consequence is the interesting part. `core` resolves a keypress to a
//! command **name**; a client turns that name into a method call on a view it
//! owns. So the same `plait.toml` drives a GPUI window and a terminal, and an
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
//! **Any command's behaviour.** [`Commands`] is a registry of names and one-line
//! descriptions — enough for a help screen, and enough for the config layer to
//! say "no such command" instead of binding a key to nothing.

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
/// `plait.toml` and no row on the help screen. A mouse *position* is another
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
        bind(GLOBAL, "h", "view.left");
        bind(GLOBAL, "left", "view.left");
        bind(GLOBAL, "l", "view.right");
        bind(GLOBAL, "right", "view.right");

        bind("diff", "s", "diff.cycle-layout");
        bind("diff", "w", "diff.cycle-wrap");
        bind("diff", "]", "diff.next-file");
        bind("diff", "[", "diff.prev-file");
        bind("diff", "tab", "diff.next-file");
        bind("diff", "backtab", "diff.prev-file");

        bind("commits", "enter", "commits.open-diff");
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
        let matches = |chord: &[Key]| {
            chord.len() == pending.len()
                && chord.iter().zip(pending).all(|(k, alts)| alts.contains(k))
        };
        let prefixes = |chord: &[Key]| {
            chord.len() > pending.len()
                && chord.iter().zip(pending).all(|(k, alts)| alts.contains(k))
        };
        for mode in modes.as_slice().iter().rev() {
            if let Some(b) = self
                .bindings
                .iter()
                .rev()
                .find(|b| b.mode == *mode && matches(&b.chord))
            {
                return Resolve::Run(&b.command);
            }
            if self
                .bindings
                .iter()
                .any(|b| b.mode == *mode && prefixes(&b.chord))
            {
                return Resolve::Pending;
            }
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
                rows.push(HelpRow::Command { keys: all, doc });
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
        for (name, doc) in [
            ("quit", "leave"),
            ("help", "show the keys"),
            ("back", "leave the innermost mode"),
            ("view.down", "one row down"),
            ("view.up", "one row up"),
            ("view.page-down", "a screenful down"),
            ("view.page-up", "a screenful up"),
            ("view.scroll-down", "the view down, not the cursor"),
            ("view.scroll-up", "the view up, not the cursor"),
            ("view.top", "the first row"),
            ("view.bottom", "the last row"),
            ("view.left", "scroll the text left"),
            ("view.right", "scroll the text right"),
            ("diff.next-file", "the next file's header"),
            ("diff.prev-file", "the previous file's header"),
            ("diff.cycle-layout", "the next presentation"),
            ("diff.cycle-wrap", "the next wrap"),
            ("theme.cycle", "the next theme"),
            ("commits.open-diff", "the diff for this commit"),
            ("select.all", "select the whole view"),
            ("select.none", "drop the selection"),
            (
                "copy.selection",
                "copy the selection, or the row the cursor is on",
            ),
        ] {
            c.register(name, doc);
        }
        c
    }

    /// Adds one, replacing any with the same name — so a built-in's description
    /// can be corrected rather than only added to.
    pub fn register(&mut self, name: impl Into<String>, doc: impl Into<String>) {
        let command = Command {
            name: name.into(),
            doc: doc.into(),
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
        // read, and `plait config` emits a file that parses back.
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
        assert_eq!(
            k.bindings().iter().filter(|b| b.chord == keys("j")).count(),
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
                HelpRow::Command { keys, doc } => format!("{keys} · {doc}"),
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
            "help itself binds nothing"
        );

        // A mode that takes `?` over leaves the hint nothing to say.
        let mut k = Keymap::builtin();
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
