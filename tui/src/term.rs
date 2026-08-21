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
use plait_core::command::{Code, Key};
use std::io::{self, BufWriter, Stdout, Write};
use std::time::Duration;

/// Something that happened.
///
/// The key is [`plait_core::command::Key`] and not a type of this crate's, which
/// is the whole reason `term.rs` exists as a boundary: a keypress becomes
/// `core`'s idea of a keypress at the edge, and everything inland — the keymap,
/// the modes, `plait.toml` — is shared with every other client. A second client
/// on a second platform writes this function and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Key(Key),
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
    /// drawn at a guessed width is more use than an error.
    ///
    /// **Zero counts as no size**, which is not the same check as an error and
    /// is the one that actually fires: a pty opened by `script`, by a CI runner
    /// or by a test harness reports `Ok((0, 0))`, and a client that believes it
    /// draws a blank screen and looks hung.
    pub fn size() -> (usize, usize) {
        match terminal::size() {
            Ok((w, h)) if w > 0 && h > 0 => (w as usize, h as usize),
            _ => (80, 24),
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
        Ok(translate_event(event::read()?))
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        self.leave();
    }
}

/// Which events are inputs at all.
///
/// Anything that is not a keypress or a resize — a mouse move, a focus change, a
/// bracketed paste — is dropped, so a caller sees `None` for a timeout and for
/// noise alike. Key *release* events are dropped too: terminals with the kitty
/// protocol on report both, and acting on each is every binding firing twice.
///
/// Separate from [`Term::poll`] so it can be tested without a terminal, which is
/// the same reason every other module in this crate can be.
fn translate_event(event: Event) -> Option<Input> {
    match event {
        Event::Resize(w, h) => Some(Input::Resize(w as usize, h as usize)),
        Event::Key(k) if k.kind != KeyEventKind::Release => translate(k),
        _ => None,
    }
}

/// crossterm's event into `core`'s key. The whole of what this module is for.
///
/// A key with no [`Code`] is dropped rather than guessed at: a binding that
/// fires on the wrong key is worse than one that does not fire, and `Code` is
/// deliberately only what a keyboard-first app binds.
fn translate(k: KeyEvent) -> Option<Input> {
    let m = k.modifiers;
    // A terminal in raw mode sends CR for the return key and crossterm reports
    // that as `Enter`. Some layers in between — `script`, a few ssh setups, a
    // pty opened by a test harness — send LF instead, which arrives as
    // `Ctrl-J`. Folded into `Enter` here, **modifier and all**: leaving the
    // control bit set produces `ctrl-enter`, which is not a key anything binds
    // either. Nothing wants `ctrl-j` for itself, and "the return key does
    // nothing" is the worst failure a keyboard-first app has.
    let feed = matches!(k.code, KeyCode::Char('j')) && m.contains(KeyModifiers::CONTROL);
    let code = match k.code {
        _ if feed => Code::Enter,
        KeyCode::Char(c) => Code::Char(c),
        KeyCode::Up => Code::Up,
        KeyCode::Down => Code::Down,
        KeyCode::Left => Code::Left,
        KeyCode::Right => Code::Right,
        KeyCode::Home => Code::Home,
        KeyCode::End => Code::End,
        KeyCode::PageUp => Code::PageUp,
        KeyCode::PageDown => Code::PageDown,
        KeyCode::Enter => Code::Enter,
        KeyCode::Tab => Code::Tab,
        KeyCode::BackTab => Code::BackTab,
        KeyCode::Backspace => Code::Backspace,
        KeyCode::Delete => Code::Delete,
        KeyCode::Esc => Code::Esc,
        _ => return None,
    };
    // `Key::new` drops shift on a character, which is the invariant that stops
    // `Shift-a` arriving as both `A` and `shift-a`.
    Some(Input::Key(Key::new(
        code,
        m.contains(KeyModifiers::CONTROL) && !feed,
        m.contains(KeyModifiers::ALT),
        m.contains(KeyModifiers::SHIFT),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plait_core::command::{Keymap, Modes, Resolve};

    fn key(code: KeyCode, mods: KeyModifiers) -> Option<Key> {
        match translate(KeyEvent::new(code, mods)) {
            Some(Input::Key(k)) => Some(k),
            _ => None,
        }
    }

    #[test]
    fn a_keypress_becomes_cores_own_key() {
        // The boundary this module exists to be: what leaves here is the type
        // `plait.toml` and the keymap already speak.
        assert_eq!(key(KeyCode::Char('j'), KeyModifiers::NONE), Some(Key::char('j')));
        assert_eq!(key(KeyCode::Esc, KeyModifiers::NONE), Some(Key::plain(Code::Esc)));
        assert_eq!(
            key(KeyCode::Char('d'), KeyModifiers::CONTROL),
            Some(Key::ctrl(Code::Char('d')))
        );
        // ...and it spells itself the way a config file writes it.
        assert_eq!(key(KeyCode::Char('d'), KeyModifiers::CONTROL).unwrap().to_string(), "ctrl-d");
    }

    #[test]
    fn shift_is_dropped_for_a_character_and_kept_for_a_named_key() {
        // The trap: a terminal reports `Shift-a` as `A` *and* sets the shift
        // bit, so a binding on `shift-a` never fires and one written both ways
        // fires twice.
        let a = key(KeyCode::Char('A'), KeyModifiers::SHIFT).unwrap();
        assert_eq!(a, Key::char('A'));
        assert!(!a.shift);
        assert!(key(KeyCode::Tab, KeyModifiers::SHIFT).unwrap().shift);
    }

    #[test]
    fn a_line_feed_is_the_return_key() {
        // What `script`, and a few ssh setups, send instead of a carriage
        // return. Both have to be Enter or the key that opens things is dead.
        assert_eq!(key(KeyCode::Enter, KeyModifiers::NONE), Some(Key::plain(Code::Enter)));
        assert_eq!(
            key(KeyCode::Char('j'), KeyModifiers::CONTROL),
            Some(Key::plain(Code::Enter))
        );
        // ...and an unmodified `j` is still `j`.
        assert_eq!(key(KeyCode::Char('j'), KeyModifiers::NONE), Some(Key::char('j')));
    }

    #[test]
    fn a_key_with_no_variant_is_dropped_rather_than_guessed_at() {
        assert_eq!(key(KeyCode::F(7), KeyModifiers::NONE), None);
        assert_eq!(key(KeyCode::Insert, KeyModifiers::NONE), None);
    }

    #[test]
    fn a_resize_carries_the_new_size() {
        let got = match translate_event(Event::Resize(120, 40)) {
            Some(Input::Resize(w, h)) => (w, h),
            other => panic!("{other:?}"),
        };
        assert_eq!(got, (120, 40));
    }

    #[test]
    fn a_key_release_is_not_an_input() {
        // Terminals with the kitty protocol on report press *and* release, and
        // acting on both is every binding firing twice.
        let mut k = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        k.kind = KeyEventKind::Release;
        assert_eq!(translate_event(Event::Key(k)), None);
    }

    #[test]
    fn the_shipped_keymap_resolves_what_this_module_produces() {
        // End to end across the boundary: crossterm's event, `core`'s key,
        // `core`'s keymap, a command name — and not one line of it in between
        // belongs to this client.
        let map = Keymap::builtin();
        let press = key(KeyCode::Char('d'), KeyModifiers::CONTROL).unwrap();
        assert_eq!(map.resolve(&Modes::new(), &[press]), Resolve::Run("view.page-down"));
    }
}
