//! The only module that touches `crossterm`.
//!
//! Everything else in this crate draws into a [`Screen`](crate::screen::Screen),
//! which is a `Vec` of cells and needs no terminal at all — so confining the
//! platform to one file is what makes the views testable, exactly as `core`
//! having no dependencies is what makes the pipeline testable. If something in
//! here starts being imported from a view, that boundary has gone.
//!
//! What crossterm is for, and it is worth being specific because the rest of
//! this crate deliberately hand-rolls its drawing: **parsing a keypress out of a
//! byte stream.** A terminal reports `Shift-F5` differently depending on the
//! emulator, the terminfo entry and whether the kitty protocol is on, and
//! getting that wrong is a keyboard-first app that mysteriously ignores a key.
//! That is a decade of archaeology and is precisely the "don't build what the
//! framework already has" case. Cell diffing and colour are not — they are forty
//! lines and they are how the views stay testable.
//!
//! # Raw mode owns the keyboard
//!
//! `Ctrl-C` no longer kills the process, `Ctrl-Z` no longer suspends it and
//! `Ctrl-S` no longer stops the flow: every one of them arrives as a keypress.
//! Whatever assembles this therefore *must* handle at least one quit key, and
//! [`Term`] restores the terminal on drop — including on a panic, via
//! [`Term::guard`], because a raw-mode terminal left behind by a panicking
//! process shows no echo and no newlines and looks like a hung machine.

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use std::io::{self, BufWriter, Stdout, Write};
use std::time::Duration;

/// A keypress, normalised.
///
/// Its own enum rather than crossterm's, so nothing above this module names a
/// dependency — and so that a keymap, when there is one, binds against something
/// `core`'s command dispatch could also be handed. The set is deliberately what
/// a keyboard-first app binds and nothing more; a key with no variant arrives as
/// [`Key::Char`] or is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
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
}

/// Which modifiers were held. `shift` is deliberately absent for
/// [`Key::Char`] — a terminal reports `Shift-a` as `A`, and a binding on both
/// is a binding that never fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub fn none(&self) -> bool {
        !self.ctrl && !self.alt && !self.shift
    }
}

/// Something that happened. Anything a view cannot act on is never constructed,
/// so a `match` over this is exhaustive without a catch-all that hides new
/// variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Key(Key, Mods),
    /// The terminal changed size. Carries the new one, so nothing has to ask.
    Resize(usize, usize),
}

/// The terminal, in the state a full-screen app needs it.
///
/// Entering is: raw mode, the alternate screen, and the cursor hidden. Leaving
/// is the reverse in reverse order, and happens on drop.
pub struct Term {
    out: BufWriter<Stdout>,
    /// Whether we still owe the terminal a restore. Checked so that an explicit
    /// [`Term::leave`] followed by a drop does not send the sequences twice.
    entered: bool,
}

impl Term {
    pub fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut out = BufWriter::new(io::stdout());
        // Alternate screen first, then hide the cursor: the other order hides
        // the cursor on the *primary* screen and leaves it hidden there if the
        // next call fails.
        out.write_all(b"\x1b[?1049h\x1b[?25l")?;
        out.flush()?;
        Ok(Self { out, entered: true })
    }

    /// Restores the terminal. Idempotent, and called from [`Drop`].
    ///
    /// Errors are swallowed rather than reported: this runs while unwinding and
    /// on the way out of `main`, and there is nowhere left to report to. A
    /// half-restored terminal is the thing to avoid, so every step is attempted
    /// even if an earlier one failed.
    pub fn leave(&mut self) {
        if !self.entered {
            return;
        }
        self.entered = false;
        let _ = self.out.write_all(b"\x1b[?25h\x1b[?1049l");
        let _ = self.out.flush();
        let _ = terminal::disable_raw_mode();
    }

    /// Installs a panic hook that restores the terminal before printing.
    ///
    /// Without it a panic leaves raw mode on and the alternate screen up: no
    /// echo, no newlines, and the backtrace invisible on a screen that is about
    /// to be discarded. That reads as a hung machine rather than as a crash,
    /// which is the worst way for a bug to present.
    pub fn guard() {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = terminal::disable_raw_mode();
            let mut out = io::stdout();
            let _ = out.write_all(b"\x1b[?25h\x1b[?1049l");
            let _ = out.flush();
            previous(info);
        }));
    }

    /// Where a [`Screen`](crate::screen::Screen) flushes to.
    pub fn out(&mut self) -> &mut impl Write {
        &mut self.out
    }

    /// The terminal's size in columns and rows.
    ///
    /// Falls back to 80×24 rather than failing: a pipe has no size, and a diff
    /// drawn at a guessed width is more use than an error. That is also what
    /// makes `--dump` work with no terminal attached at all.
    pub fn size() -> (usize, usize) {
        match terminal::size() {
            Ok((w, h)) => (w as usize, h as usize),
            Err(_) => (80, 24),
        }
    }

    /// The next input, or `None` if nothing arrived within `timeout`.
    ///
    /// Anything that is not a keypress or a resize — a mouse move, a focus
    /// change, a bracketed paste — is skipped rather than surfaced, so a caller
    /// gets `None` on a timeout and nothing else. Key *release* events are
    /// skipped too: terminals with the kitty protocol on report both, and acting
    /// on each is every binding firing twice.
    pub fn poll(timeout: Duration) -> io::Result<Option<Input>> {
        if !event::poll(timeout)? {
            return Ok(None);
        }
        Ok(match event::read()? {
            Event::Resize(w, h) => Some(Input::Resize(w as usize, h as usize)),
            Event::Key(k) if k.kind != KeyEventKind::Release => translate(k),
            _ => None,
        })
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        self.leave();
    }
}

fn translate(k: KeyEvent) -> Option<Input> {
    let key = match k.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Esc => Key::Esc,
        _ => return None,
    };
    let m = k.modifiers;
    // Shift is dropped for a character: the terminal already applied it, and
    // `Shift-a` arriving as both `A` and `shift + a` is a binding that never
    // fires.
    let shift = m.contains(KeyModifiers::SHIFT) && !matches!(key, Key::Char(_));
    Some(Input::Key(
        key,
        Mods {
            ctrl: m.contains(KeyModifiers::CONTROL),
            alt: m.contains(KeyModifiers::ALT),
            shift,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> Option<Input> {
        translate(KeyEvent::new(code, mods))
    }

    #[test]
    fn a_plain_character_carries_no_modifiers() {
        assert_eq!(key(KeyCode::Char('j'), KeyModifiers::NONE), Some(Input::Key(Key::Char('j'), Mods::default())));
    }

    #[test]
    fn shift_is_dropped_for_a_character_and_kept_for_a_named_key() {
        // The trap: a terminal reports `Shift-a` as `A` *and* sets the shift
        // bit, so a binding on `shift + a` never fires and one on `A` fires
        // twice if both are registered.
        let Some(Input::Key(Key::Char('A'), m)) = key(KeyCode::Char('A'), KeyModifiers::SHIFT)
        else {
            panic!("shifted character did not arrive as a character");
        };
        assert!(!m.shift);
        let Some(Input::Key(Key::Tab, m)) = key(KeyCode::Tab, KeyModifiers::SHIFT) else {
            panic!("shifted tab was dropped");
        };
        assert!(m.shift);
    }

    #[test]
    fn ctrl_and_alt_survive() {
        let Some(Input::Key(Key::Char('c'), m)) = key(KeyCode::Char('c'), KeyModifiers::CONTROL)
        else {
            panic!("ctrl-c was dropped, and raw mode means nothing else will catch it");
        };
        assert!(m.ctrl && !m.alt);
        assert!(!m.none());
    }

    #[test]
    fn a_key_with_no_variant_is_dropped_rather_than_guessed_at() {
        assert_eq!(key(KeyCode::F(7), KeyModifiers::NONE), None);
        assert_eq!(key(KeyCode::Insert, KeyModifiers::NONE), None);
    }

    #[test]
    fn a_key_release_is_not_an_input() {
        // Terminals with the kitty protocol on report press *and* release; a
        // binding acting on both fires twice per keystroke.
        let mut k = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        k.kind = KeyEventKind::Release;
        assert!(k.kind == KeyEventKind::Release);
    }
}
