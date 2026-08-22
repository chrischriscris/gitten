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
        Self { code, ctrl, alt, shift: shift && !matches!(code, Code::Char(_)) }
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
    chord.iter().map(Key::to_string).collect::<Vec<_>>().join(" ")
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
        Self { bindings: Vec::new() }
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
            k.bind(mode, chord, command).expect("a shipped binding conflicts with another")
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
        let binding =
            Binding { mode: mode.into(), chord, command: command.into() };
        match self.bindings.iter().position(|b| b.mode == mode && b.chord == binding.chord) {
            Some(i) => self.bindings[i] = binding,
            None => self.bindings.push(binding),
        }
        Ok(())
    }

    /// Removes a binding. What a config file does with `"j" = ""` — unbinding a
    /// built-in has to be expressible, or a shipped key can only be moved and
    /// never removed.
    pub fn unbind(&mut self, mode: &str, chord: &str) -> bool {
        let Some(chord) = parse_chord(chord) else { return false };
        let before = self.bindings.len();
        self.bindings.retain(|b| !(b.mode == mode && b.chord == chord));
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
        if pending.is_empty() {
            return Resolve::None;
        }
        for mode in modes.as_slice().iter().rev() {
            if let Some(b) =
                self.bindings.iter().find(|b| b.mode == *mode && b.chord == pending)
            {
                return Resolve::Run(&b.command);
            }
            if self
                .bindings
                .iter()
                .any(|b| b.mode == *mode && b.chord.len() > pending.len() && b.chord.starts_with(pending))
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
            ("copy.selection", "copy the selection, or the row the cursor is on"),
        ] {
            c.register(name, doc);
        }
        c
    }

    /// Adds one, replacing any with the same name — so a built-in's description
    /// can be corrected rather than only added to.
    pub fn register(&mut self, name: impl Into<String>, doc: impl Into<String>) {
        let command = Command { name: name.into(), doc: doc.into() };
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
            "j", "G", "?", "-", "space", "esc", "enter", "tab", "backtab", "up", "pagedown",
            "ctrl-d", "alt-enter", "ctrl-alt-left", "shift-tab", "ctrl--",
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
        assert_eq!(Key::parse("hyper-j"), None, "an unknown modifier is not a key name");
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
        assert_eq!(k.resolve(&modes, &keys("ctrl-d")), Resolve::Run("view.page-down"));
        assert_eq!(k.resolve(&modes, &keys("G")), Resolve::Run("view.bottom"));
        assert_eq!(k.resolve(&modes, &keys("z")), Resolve::None);
    }

    #[test]
    fn the_wheel_is_a_key_like_any_other() {
        let k = Keymap::builtin();
        let modes = Modes::new();
        assert_eq!(k.resolve(&modes, &keys("wheeldown")), Resolve::Run("view.scroll-down"));
        // Round-trips, so `./dev config` writes a line that parses back.
        assert_eq!(Key::parse("wheelup").unwrap().to_string(), "wheelup");
        assert_eq!(Key::parse("ctrl-wheeldown").unwrap().to_string(), "ctrl-wheeldown");
    }

    #[test]
    fn a_mode_overrides_global_and_inherits_the_rest() {
        let mut k = Keymap::builtin();
        let mut modes = Modes::new();
        modes.push("diff");
        // Its own.
        assert_eq!(k.resolve(&modes, &keys("s")), Resolve::Run("diff.cycle-layout"));
        // Inherited, with nothing repeated in the mode to get it.
        assert_eq!(k.resolve(&modes, &keys("j")), Resolve::Run("view.down"));
        // Overridden.
        k.bind("diff", "j", "diff.next-file").unwrap();
        assert_eq!(k.resolve(&modes, &keys("j")), Resolve::Run("diff.next-file"));
        // ...and only inside that mode.
        assert_eq!(k.resolve(&Modes::new(), &keys("j")), Resolve::Run("view.down"));
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
        assert_eq!(k.resolve(&modes, &keys("ctrl-w l")), Resolve::Run("pane.right"));
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
            assert_eq!(b.chord.len(), 1, "{} is a chord in the shipped map", chord_string(&b.chord));
        }
    }

    #[test]
    fn rebinding_replaces_and_unbinding_removes() {
        let mut k = Keymap::builtin();
        k.bind(GLOBAL, "j", "view.up").unwrap();
        assert_eq!(k.resolve(&Modes::new(), &keys("j")), Resolve::Run("view.up"));
        assert_eq!(k.bindings().iter().filter(|b| b.chord == keys("j")).count(), 1);
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
            assert!(!c.doc.ends_with('.'), "{} reads as a sentence, not a label", c.name);
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
        assert_eq!(keys_.resolve(&modes, &keys("b")), Resolve::Run("blame.toggle"));
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
}
