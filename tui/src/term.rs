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
//! # Taking the mouse takes the terminal's selection with it
//!
//! There is no tracking mode that reports the wheel and nothing else, so asking
//! for a notch means asking for clicks — and an emulator that is forwarding
//! clicks is no longer drag-selecting text with them. gitten therefore has to
//! *have* a selection of its own, which it does: [`Input::Mouse`] routes to a
//! view, the model is `gitten_core::select` and `y` copies. The emulator's own
//! override is still there for the times you want the whole screen rather than a
//! diff — `shift` (`option` on iTerm) — and `--no-mouse` is the other half, for a
//! terminal that has neither.
//!
//! So: mode 1000, mode 1002 and mode 1006, and deliberately *not* 1003. 1002
//! reports motion **only while a button is held**, which is a drag and is the
//! feature; 1003 reports every cell the pointer crosses over an idle screen,
//! which is a packet per cell for nothing. `EnableMouseCapture` turns on all of
//! them, which is why the modes are written out here.
//!
//! # Raw mode owns the keyboard
//!
//! `Ctrl-C` no longer kills the process, `Ctrl-Z` no longer suspends it and
//! `Ctrl-S` no longer stops the flow: every one of them arrives as a keypress.
//! Whatever assembles this therefore *must* handle at least one quit key, and
//! [`Term`] restores the terminal on drop — including on a panic, via
//! [`Term::guard`], because a raw-mode terminal left behind by a panicking
//! process shows no echo and no newlines and looks like a hung machine.

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::terminal;
use gitten_core::command::{Code, Key};
use std::io::{self, BufWriter, Stdout, Write};
use std::time::Duration;

/// Something that happened.
///
/// The key is [`gitten_core::command::Key`] and not a type of this crate's, which
/// is the whole reason `term.rs` exists as a boundary: a keypress becomes
/// `core`'s idea of a keypress at the edge, and everything inland — the keymap,
/// the modes, `gitten.toml` — is shared with every other client. A second client
/// on a second platform writes this function and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A keypress — and a wheel notch, which [`Code`] holds a variant for and
    /// which therefore needs nothing here. That is the point of it arriving as a
    /// key: what a notch does is a line in `gitten.toml` like everything else,
    /// and this enum stays a list of things that happened rather than a list of
    /// things to do.
    Key(Key),
    /// A button, a drag or a release, and where it happened.
    ///
    /// Not a [`Key`], and that is the line `core::command` draws: a wheel notch
    /// is a control with no coordinate and resolves through the keymap, and a
    /// click is a *position* and belongs to whatever was under it. Routing one is
    /// a hit test, which is the assembly's job and then a view's.
    Mouse(Mouse),
    /// The terminal changed size. Carries the new one, so nothing has to ask.
    Resize(usize, usize),
    /// A bracketed paste, whole and unedited.
    ///
    /// Text, and deliberately not a sequence of [`Key`]s: the negotiation in
    /// [`Term::enter`] means the emulator hands the whole paste over as one
    /// event, so a pasted `q` is a character in a string and never a keypress
    /// the keymap could resolve. Whether the text has anywhere to go is the
    /// caller's decision — see `App::input` — and it is never made here.
    Paste(String),
}

/// What the pointer did, in cells of the terminal.
///
/// Plain data with no crossterm in it, so the assembly can route one and the
/// views can be tested against one without a terminal — the same boundary
/// [`Key`] crosses at this file's edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mouse {
    pub kind: MouseKind,
    /// Zero-based, from the top-left of the terminal — not of any view. Whoever
    /// assembled the screen owns the title bar and subtracts it.
    pub col: usize,
    pub row: usize,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// The three things a button can be doing.
///
/// Only the left button is reported. The middle one is paste on X11 and the
/// right one is a context menu nothing here has, and a client that swallowed
/// either would be taking a gesture from the emulator and doing nothing with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Down,
    /// Motion with the button held. Mode 1002, and the reason it is on.
    Drag,
    Up,
}

/// Button tracking (1000), drag tracking (1002) and SGR encoding (1006).
///
/// 1006 because the default encoding puts the coordinates in single bytes and
/// dies past column 223, which is a normal width for a window on a normal
/// screen. Not 1003; see the note at the top of the file.
const MOUSE_ON: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1006h";
const MOUSE_OFF: &[u8] = b"\x1b[?1006l\x1b[?1002l\x1b[?1000l";

/// Bracketed paste (2004). On, the emulator wraps a paste in ESC[200~ /
/// ESC[201~ sentinels and crossterm hands over one `Event::Paste` instead of
/// streaming N key events — see `translate_event`. Off is the default in
/// every terminal, so old ones ignore both sequences silently.
const PASTE_ON: &[u8] = b"\x1b[?2004h";
const PASTE_OFF: &[u8] = b"\x1b[?2004l";

/// The terminal, in the state a full-screen app needs it.
///
/// Entering is: raw mode, the alternate screen, the cursor hidden, and
/// bracketed paste negotiated — plus the mouse-tracking modes when asked for.
/// Leaving runs on drop and unsets what entering set, the off sequences going
/// out beside their on counterparts rather than in strict reverse order.
pub struct Term {
    out: BufWriter<Stdout>,
    /// Whether we still owe the terminal a restore. Checked so that an explicit
    /// [`Term::leave`] followed by a drop does not send the sequences twice.
    entered: bool,
}

impl Term {
    /// Enters, and asks for the wheel if `mouse` is set.
    ///
    /// The wheel is a request and not a given because taking it takes
    /// drag-to-select with it — see the note at the top of this file.
    /// Bracketed paste is negotiated so that a paste arrives as one event
    /// instead of being typed into the keymap character by character.
    pub fn enter(mouse: bool) -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut out = BufWriter::new(io::stdout());
        // Alternate screen first, then hide the cursor: the other order hides
        // the cursor on the *primary* screen and leaves it hidden there if the
        // next call fails.
        out.write_all(b"\x1b[?1049h\x1b[?25l")?;
        if mouse {
            out.write_all(MOUSE_ON)?;
        }
        out.write_all(PASTE_ON)?;
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
        // The mouse off unconditionally: a terminal left in a tracking mode
        // prints `<35;61;9M` at the shell every time the pointer moves, and
        // asking whether we turned it on is one more thing to get wrong on the
        // path that exists to leave nothing behind. Paste mode goes the same
        // way.
        let _ = self.out.write_all(MOUSE_OFF);
        let _ = self.out.write_all(PASTE_OFF);
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
            let _ = out.write_all(MOUSE_OFF);
            let _ = out.write_all(PASTE_OFF);
            let _ = out.write_all(b"\x1b[?25h\x1b[?1049l");
            let _ = out.flush();
            previous(info);
        }));
    }

    /// Puts `text` on the system clipboard, through the terminal.
    ///
    /// **OSC 52**, and not `pbcopy` or a clipboard crate. A terminal is often not
    /// on the machine the clipboard is on — over ssh, in a container, inside tmux
    /// — and OSC 52 is the one mechanism that follows the *session* rather than
    /// the process: the emulator at the near end does the copying. It also costs
    /// no dependency and no subprocess, and this crate has two dependencies on
    /// purpose.
    ///
    /// The cost, and it is real: an emulator that does not implement it, or that
    /// has it turned off for safety, copies nothing and says nothing — there is
    /// no reply to read. kitty, Ghostty, WezTerm, foot, Alacritty and iTerm2 all
    /// do; Terminal.app does not. tmux needs `set -g set-clipboard on`, which is
    /// its default in 3.x, and passes it through to the emulator.
    ///
    /// `c` is the selection: the clipboard proper, rather than X11's primary.
    pub fn copy(&mut self, text: &str) -> io::Result<()> {
        self.out.write_all(b"\x1b]52;c;")?;
        self.out.write_all(base64(text.as_bytes()).as_bytes())?;
        // BEL and not ST, because it is the terminator every emulator that
        // implements this understands, including the ones that predate ST.
        self.out.write_all(b"\x07")?;
        self.out.flush()
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
    /// Because bracketed paste is negotiated in [`Term::enter`], an emulator
    /// delivers a paste as one [`Event::Paste`], which leaves here as one
    /// [`Input::Paste`] — the text whole, and never a sequence of keys. A pasted
    /// `q` inside a paragraph is a character for the caller to place, where
    /// without the negotiation it would be typed into the keymap character by
    /// character and could quit something. Anything else that is not a keypress,
    /// a wheel notch, a left button or a resize — a focus change, the right
    /// button — is skipped rather than surfaced, so a caller gets `None` on a
    /// timeout and nothing else. Key *release* events are skipped too: terminals
    /// with the kitty protocol on report both, and acting on each is every
    /// binding firing twice.
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

/// Standard base64, which is what OSC 52 carries.
///
/// Twenty lines rather than a dependency: this is the whole of what a clipboard
/// write needs, the alphabet has not changed since 1987, and the alternative is
/// pulling a crate into the client whose dependency list is a stated design
/// constraint.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            // A chunk of one encodes two characters and a chunk of two encodes
            // three; the rest is padding, which is not optional here — an
            // emulator decoding a truncated group drops the last character.
            let c = match i <= chunk.len() {
                true => ALPHABET[(n >> (18 - 6 * i)) as usize & 0x3f] as char,
                false => '=',
            };
            out.push(c);
        }
    }
    out
}

/// Which events are inputs at all.
///
/// Anything that is not a keypress, a wheel notch, a left button, a resize or a
/// paste — a focus change, a horizontal wheel — is dropped, so a caller sees
/// `None` for a timeout and for noise alike. Key *release* events are dropped
/// too: terminals with the kitty protocol on report both, and acting on each is
/// every binding firing twice.
///
/// A paste leaves as one [`Input::Paste`]: text the caller owns, and never a
/// sequence of keys the keymap could resolve. Separate from [`Term::poll`] so it
/// can be tested without a terminal, which is the same reason every other module
/// in this crate can be.
fn translate_event(event: Event) -> Option<Input> {
    match event {
        Event::Resize(w, h) => Some(Input::Resize(w as usize, h as usize)),
        Event::Key(k) if k.kind != KeyEventKind::Release => translate(k),
        Event::Mouse(m) => mouse(m),
        // A bracketed paste lands here whole. It goes out as text and as
        // nothing else: turning it into keypresses is what would let a pasted
        // `q` quit, and this is the line that makes that impossible to write.
        Event::Paste(text) => Some(Input::Paste(text)),
        _ => None,
    }
}

/// A mouse event as the two different things it can be.
///
/// **A wheel notch is a key.** It has no coordinate anything needs — there is one
/// scrollable thing under the pointer and the view already knows which — so it
/// resolves through the keymap like `j`, appears on the `?` panel and is
/// rebindable in `gitten.toml`. See [`Code::WheelUp`].
///
/// **A button is a position**, and a position cannot be a key: `gitten.toml`
/// cannot hold a hit test. So it leaves here as an [`Input::Mouse`] and whoever
/// assembled the screen decides which view it landed in.
///
/// Horizontal wheel events and the middle and right buttons are dropped rather
/// than guessed at, exactly as an unmapped key is.
fn mouse(m: MouseEvent) -> Option<Input> {
    let mods = m.modifiers;
    let (ctrl, alt, shift) = (
        mods.contains(KeyModifiers::CONTROL),
        mods.contains(KeyModifiers::ALT),
        mods.contains(KeyModifiers::SHIFT),
    );
    let kind = match m.kind {
        MouseEventKind::ScrollUp => {
            return Some(Input::Key(Key::new(Code::WheelUp, ctrl, alt, shift)))
        }
        MouseEventKind::ScrollDown => {
            return Some(Input::Key(Key::new(Code::WheelDown, ctrl, alt, shift)))
        }
        MouseEventKind::Down(MouseButton::Left) => MouseKind::Down,
        MouseEventKind::Drag(MouseButton::Left) => MouseKind::Drag,
        MouseEventKind::Up(MouseButton::Left) => MouseKind::Up,
        _ => return None,
    };
    Some(Input::Mouse(Mouse {
        kind,
        col: m.column as usize,
        row: m.row as usize,
        ctrl,
        alt,
        shift,
    }))
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
    use gitten_core::command::{Keymap, Modes, Resolve};

    fn key(code: KeyCode, mods: KeyModifiers) -> Option<Key> {
        match translate(KeyEvent::new(code, mods)) {
            Some(Input::Key(k)) => Some(k),
            _ => None,
        }
    }

    fn at(kind: MouseEventKind, mods: KeyModifiers) -> Option<Input> {
        translate_event(Event::Mouse(MouseEvent {
            kind,
            column: 4,
            row: 9,
            modifiers: mods,
        }))
    }

    #[test]
    fn a_wheel_notch_becomes_a_key_and_keeps_no_coordinate() {
        let notch = |kind| at(kind, KeyModifiers::NONE);
        let down = Some(Input::Key(Key::plain(Code::WheelDown)));
        assert_eq!(notch(MouseEventKind::ScrollDown), down);
        assert_eq!(
            notch(MouseEventKind::ScrollUp),
            Some(Input::Key(Key::plain(Code::WheelUp)))
        );
        // A horizontal wheel is unmapped, exactly as an unmapped key is.
        assert_eq!(notch(MouseEventKind::ScrollLeft), None);
        // And the whole point of it being a key: the shipped map already binds it.
        let k = Keymap::builtin();
        let press = [Key::plain(Code::WheelDown)];
        assert_eq!(
            k.resolve(&Modes::new(), &press),
            Resolve::Run("view.scroll-down")
        );
    }

    #[test]
    fn a_button_becomes_a_position_and_the_other_buttons_do_not() {
        // The line this module draws: a notch is a key and a click is a place,
        // because `gitten.toml` cannot hold a hit test.
        let left = event::MouseButton::Left;
        let down = at(MouseEventKind::Down(left), KeyModifiers::SHIFT);
        assert_eq!(
            down,
            Some(Input::Mouse(Mouse {
                kind: MouseKind::Down,
                col: 4,
                row: 9,
                ctrl: false,
                alt: false,
                shift: true,
            }))
        );
        let kind = |e| match at(e, KeyModifiers::NONE) {
            Some(Input::Mouse(m)) => Some(m.kind),
            _ => None,
        };
        assert_eq!(kind(MouseEventKind::Drag(left)), Some(MouseKind::Drag));
        assert_eq!(kind(MouseEventKind::Up(left)), Some(MouseKind::Up));
        // Motion with nothing held: mode 1002 does not report it and mode 1003
        // is not on, so this is noise either way.
        assert_eq!(kind(MouseEventKind::Moved), None);
        // The middle button is paste on X11 and the right one is a menu nothing
        // here has. Swallowing either takes a gesture and gives nothing back.
        assert_eq!(kind(MouseEventKind::Down(event::MouseButton::Middle)), None);
        assert_eq!(kind(MouseEventKind::Down(event::MouseButton::Right)), None);
    }

    #[test]
    fn base64_is_the_one_in_the_rfc() {
        // The vectors from RFC 4648, because a clipboard that pastes almost the
        // right thing is worse than one that pastes nothing.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // A line of a diff is not ASCII in general.
        assert_eq!(
            base64("héllo — wörld".as_bytes()),
            "aMOpbGxvIOKAlCB3w7ZybGQ="
        );
    }

    #[test]
    fn a_keypress_becomes_cores_own_key() {
        // The boundary this module exists to be: what leaves here is the type
        // `gitten.toml` and the keymap already speak.
        assert_eq!(
            key(KeyCode::Char('j'), KeyModifiers::NONE),
            Some(Key::char('j'))
        );
        assert_eq!(
            key(KeyCode::Esc, KeyModifiers::NONE),
            Some(Key::plain(Code::Esc))
        );
        assert_eq!(
            key(KeyCode::Char('d'), KeyModifiers::CONTROL),
            Some(Key::ctrl(Code::Char('d')))
        );
        // ...and it spells itself the way a config file writes it.
        assert_eq!(
            key(KeyCode::Char('d'), KeyModifiers::CONTROL)
                .unwrap()
                .to_string(),
            "ctrl-d"
        );
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
        assert_eq!(
            key(KeyCode::Enter, KeyModifiers::NONE),
            Some(Key::plain(Code::Enter))
        );
        assert_eq!(
            key(KeyCode::Char('j'), KeyModifiers::CONTROL),
            Some(Key::plain(Code::Enter))
        );
        // ...and an unmodified `j` is still `j`.
        assert_eq!(
            key(KeyCode::Char('j'), KeyModifiers::NONE),
            Some(Key::char('j'))
        );
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
    fn a_paste_is_one_text_input_and_never_a_key_sequence() {
        // Bracketed paste is negotiated on purpose, and this is the pin: the
        // emulator hands the whole paste over as one event, and it leaves here
        // as one inert piece of text — the original string, unedited. A pasted
        // `q` is a character in it, and the keymap never sees a thing.
        let got = match translate_event(Event::Paste("q?\nengine".into())) {
            Some(Input::Paste(text)) => text,
            other => panic!("{other:?}"),
        };
        assert_eq!(got, "q?\nengine", "the paste did not arrive whole");
    }

    #[test]
    fn slash_arrives_as_cores_plain_character_key() {
        // `/` is a character like `j` — no special case anywhere in this file.
        // What it means is the keymap's business, which is why the search can
        // open through it without this module knowing search exists.
        assert_eq!(
            translate_event(Event::Key(KeyEvent::new(
                KeyCode::Char('/'),
                KeyModifiers::NONE
            ))),
            Some(Input::Key(Key::char('/')))
        );
        let map = Keymap::builtin();
        let mut modes = Modes::new();
        modes.push("commits");
        let press = [Key::char('/')];
        assert_eq!(map.resolve(&modes, &press), Resolve::Run("commits.search"));
    }

    #[test]
    fn the_shipped_keymap_resolves_what_this_module_produces() {
        // End to end across the boundary: crossterm's event, `core`'s key,
        // `core`'s keymap, a command name — and not one line of it in between
        // belongs to this client.
        let map = Keymap::builtin();
        let press = key(KeyCode::Char('d'), KeyModifiers::CONTROL).unwrap();
        assert_eq!(
            map.resolve(&Modes::new(), &[press]),
            Resolve::Run("view.page-down")
        );
    }
}
