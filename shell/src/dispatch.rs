//! The window's half of input: a platform keystroke in, a [`command::Key`] out.
//!
//! This is the file that makes the window read `[keys]`. Everything past it is
//! shared — [`command::Keymap`] resolves, the shell dispatches the name — so
//! what belongs here is exactly what a second client would have to write again:
//! how *its* platform spells a key.
//!
//! The terminal's is `plait-tui`'s `term.rs`, over crossterm events. This one is
//! over GPUI's [`Keystroke`], whose spelling differs in four ways worth
//! writing down:
//!
//! - **Shift is on the modifiers, not the character**, for letters: `shift-a`
//!   arrives as key `"a"` with `shift` set. Every other client reports the
//!   capital itself, and the shipped map binds `G` and `T` by their capitals —
//!   so the shift goes back where every other client has it: into the char.
//! - **A keystroke carries two characters**: `key`, the physical key's own
//!   name, and `key_char`, the character the press would have inserted. On a
//!   non-US layout these part ways — `?` typed on a German keyboard arrives as
//!   key `´` with insert `?`, and a binding written `?` must follow the insert.
//!   GPUI matches a differing insert against a target with **control only**
//!   held ([`Keystroke::should_match`]: shift is already inside the character,
//!   and alt was part of composing it), so the same modifiers survive here.
//!   When the two spellings agree — an unshifted letter, a plain symbol — the
//!   physical path keeps its own semantics, alt and all, exactly as GPUI falls
//!   back to it. An insert that is not one character is IME composition state:
//!   nothing is invented from it, and if the physical half is no single
//!   character either, the press translates to nothing.
//! - **A shifted symbol arrives as the symbol**: `?` is key `"?"` with `shift`
//!   *cleared*, because macOS reports the shifted character as the key. Left
//!   alone that would bind `shift-?` and never fire; [`Key::new`] would drop
//!   the flag anyway.
//! - **The platform modifier owns its keys.** `cmd-q`, `cmd-c` and `cmd-a` are
//!   the menu's, not this map's — they are real macOS bindings a Mac user's
//!   fingers already know, and they stay native. A keystroke with `platform`
//!   set does not translate.

use gpui::Keystroke;
use plait_core::command::{Code, Key};

/// One GPUI keystroke as a [`Key`], or `None` for anything no client can bind:
/// the platform modifier, function keys, and lone modifiers (which GPUI
/// synthesizes for binding matching and never delivers here).
pub fn translate(k: &Keystroke) -> Option<Key> {
    // Cmd-c means copy to the OS, whatever `plait.toml` says. See the module
    // note: the menu adapters own these, and translating them too would be a
    // command that fires twice.
    if k.modifiers.platform || k.modifiers.function {
        return None;
    }
    // The two characters a keystroke carries. The insert matters only when it
    // differs from the physical spelling — that difference is exactly the case
    // the layout broke, and the insert is the half every layout shares. Named
    // keys below ignore both halves entirely.
    let physical = sole(&k.key);
    let inserted = k
        .key_char
        .as_deref()
        .and_then(sole)
        .filter(|c| Some(*c) != physical);
    let code = match k.key.as_str() {
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
        // Shift-tab is backtab everywhere else; GPUI reports tab with shift set,
        // and the shift goes no further — a terminal folds it into the code the
        // same way, so `[keys]` says `backtab` and nothing else.
        "tab" if k.modifiers.shift => {
            return Some(Key::plain(Code::BackTab));
        }
        "tab" => Code::Tab,
        "backspace" => Code::Backspace,
        // The forward-delete key; `insert` has no binding anywhere and is not
        // quietly turned into one.
        "delete" | "del" => Code::Delete,
        "esc" | "escape" => Code::Esc,
        // Everything the named arms above did not take, decided entirely by
        // the two characters computed before the match.
        _ => {
            let c = inserted.or(physical)?;
            // Capitals are folded wherever they came from — the insert of a
            // shifted letter already *is* the capital, and a shift-flagged
            // physical letter becomes one here, which is both what the other
            // clients report and what the shipped map binds.
            Code::Char(
                match inserted.is_none() && k.modifiers.shift && c.is_ascii_lowercase() {
                    true => c.to_ascii_uppercase(),
                    false => c,
                },
            )
        }
    };
    // Modifiers that survive a character which arrived through the insert are
    // GPUI's own matching rule: control still means "command", while shift is
    // inside the character and alt went into composing it. Everything else —
    // named keys, and characters read off the physical key — keeps what was
    // actually held.
    let (ctrl, alt, shift) = match inserted.is_some() {
        true => (k.modifiers.control, false, false),
        false => (k.modifiers.control, k.modifiers.alt, k.modifiers.shift),
    };
    Some(Key::new(code, ctrl, alt, shift))
}

/// The one character of `s`, or nothing: multi-character strings are IME
/// mid-composition state or key names no [`Code`](plait_core::command::Code)
/// has, and neither is a key any client can bind.
fn sole(s: &str) -> Option<char> {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;
    use plait_core::command::{Keymap, Modes, Resolve};

    fn key(key: &str, m: Modifiers) -> Option<Key> {
        translate(&Keystroke {
            modifiers: m,
            key: key.into(),
            key_char: None,
        })
    }

    /// A keystroke as a real keyboard delivers it: physical name and insert.
    fn typed(key: &str, insert: Option<&str>, m: Modifiers) -> Option<Key> {
        translate(&Keystroke {
            modifiers: m,
            key: key.into(),
            key_char: insert.map(Into::into),
        })
    }

    fn plain(s: &str) -> Modifiers {
        let mut m = Modifiers::default();
        for part in s.split('-') {
            match part {
                "ctrl" => m.control = true,
                "alt" => m.alt = true,
                "shift" => m.shift = true,
                "cmd" => m.platform = true,
                _ => {}
            }
        }
        m
    }

    fn t(spelling: &str, mods: &str) -> String {
        key(spelling, plain(mods))
            .expect("nothing came back")
            .to_string()
    }

    /// How GPUI spells a key on a Mac, for the round-trip below.
    fn spelling(code: Code) -> String {
        match code {
            Code::Char(' ') => "space".into(),
            Code::Char(c) => c.to_string(),
            Code::Esc => "escape".into(),
            // Shift-tab arrives as tab with shift set; see `translate`.
            Code::BackTab => "tab".into(),
            // The named keys GPUI reports are already lowercase names, and they
            // are the same words `Code::parse` takes — except escape, above.
            other => format!("{other:?}").to_lowercase(),
        }
    }

    #[test]
    fn the_keys_the_map_binds_all_translate_to_themselves() {
        // The property `[keys]` rests on: a chord written in plait.toml is the
        // chord a keystroke becomes. Every binding in the shipped map, spelled
        // as GPUI spells it. The wheel is the exception that proves it: it is in
        // the map, but a window delivers it as deltas rather than keystrokes —
        // see `DevShell::on_wheel` for where its translation lives.
        let k = Keymap::builtin();
        assert!(k.bindings().len() > 10);
        let mut checked = 0;
        for b in k.bindings() {
            assert_eq!(b.chord.len(), 1, "the shipped map resolves on one key");
            let want = &b.chord[0];
            if matches!(want.code, Code::WheelUp | Code::WheelDown) {
                continue;
            }
            checked += 1;
            let got = key(&spelling(want.code), modifiers_of(want))
                .unwrap_or_else(|| panic!("{} did not translate", spelling(want.code)));
            assert_eq!(&got, want);
            // Resolved in its own mode — `diff.*` keys are not global, and that
            // is the point of them.
            let mut modes = Modes::new();
            modes.push(b.mode.as_str());
            assert_eq!(
                k.resolve(&modes, &[got]),
                Resolve::Run(&b.command),
                "{} did not resolve",
                b.command
            );
        }
        assert!(checked > 20, "almost nothing was checked");
    }

    fn modifiers_of(key: &Key) -> Modifiers {
        Modifiers {
            control: key.ctrl,
            alt: key.alt,
            // BackTab arrives as tab with shift set; a shifted character never
            // does — the capital is its own code.
            shift: key.shift || matches!(key.code, Code::BackTab),
            ..Modifiers::default()
        }
    }

    #[test]
    fn shift_on_a_letter_is_the_capital_the_other_clients_report() {
        assert_eq!(t("t", "shift"), "T");
        assert_eq!(t("g", "shift"), "G");
        // ...and unshifted stays lowercase.
        assert_eq!(t("j", ""), "j");
    }

    #[test]
    fn a_shifted_symbol_arrives_as_itself() {
        // macOS reports the shifted character as the key; the flag is cleared.
        // A binding on "?" must fire, and one on "shift-?" must not exist twice.
        let got = key("?", plain("shift")).unwrap();
        assert_eq!(got, Key::char('?'));
        assert!(!got.shift);
    }

    #[test]
    fn named_keys_translate() {
        assert_eq!(t("escape", ""), "esc");
        assert_eq!(t("pageup", ""), "pageup");
        assert_eq!(t("pagedown", ""), "pagedown");
        assert_eq!(t("home", ""), "home");
        assert_eq!(t("end", ""), "end");
        assert_eq!(t("space", ""), "space");
        assert_eq!(t("return", ""), "enter");
    }

    #[test]
    fn shift_tab_is_backtab_like_everywhere_else() {
        assert_eq!(t("tab", "shift"), "backtab");
        assert_eq!(t("tab", ""), "tab");
    }

    #[test]
    fn ctrl_and_alt_survive() {
        assert_eq!(t("d", "ctrl"), "ctrl-d");
        assert_eq!(t("e", "ctrl"), "ctrl-e");
        assert_eq!(t("u", "ctrl"), "ctrl-u");
        assert_eq!(t("y", "ctrl"), "ctrl-y");
        assert_eq!(t("a", "ctrl"), "ctrl-a");
        assert_eq!(t("c", "ctrl"), "ctrl-c");
        assert_eq!(t("enter", "alt"), "alt-enter");
    }

    #[test]
    fn the_platform_modifier_does_not_translate() {
        // The menu's keys. Translating them too would run a command twice.
        assert_eq!(key("q", plain("cmd")), None);
        assert_eq!(key("c", plain("cmd")), None);
        assert_eq!(key("a", plain("cmd")), None);
    }

    #[test]
    fn a_punctuation_binding_follows_the_character_not_the_key_cap() {
        // The non-US case that broke: on a German layout `?` sits where US has
        // shift-´, and GPUI reports key "/" with insert "?". A binding written
        // "?" is a binding on the *character* — every other client reports the
        // character — so the insert wins whenever the two spellings part ways.
        assert_eq!(typed("/", Some("?"), plain("shift")), Some(Key::char('?')));
        // The shipped diff bindings, which are punctuation too.
        assert_eq!(typed("ù", Some("["), plain("")), Some(Key::char('[')));
        assert_eq!(typed("à", Some("]"), plain("")), Some(Key::char(']')));
        // ...and with no insert at all the physical spelling still answers.
        assert_eq!(key("[", plain("")), Some(Key::char('[')));
    }

    #[test]
    fn the_insert_keeps_only_the_modifiers_gpui_would_match_it_with() {
        // `Keystroke::should_match` compares a differing key_char against a
        // target holding control alone: shift is already inside the character
        // (alt-ç inserts `$` on a Czech keyboard; the `$` needs no alt), and
        // alt was part of composing it. Control survives, because ctrl-a is
        // still a command and not a character anybody typed.
        let got = typed("ç", Some("$"), plain("alt")).expect("$");
        assert_eq!(got, Key::char('$'));
        assert!(!got.alt && !got.shift);

        let got = typed("x", Some("X"), plain("ctrl-shift")).expect("ctrl-X");
        assert_eq!(got, Key::ctrl(Code::Char('X')));
        assert!(got.ctrl && !got.shift);
    }

    #[test]
    fn an_insert_that_agrees_with_the_key_changes_nothing() {
        // When the two spellings are the same there is no layout story, and
        // GPUI falls back to matching the physical key with its own modifiers —
        // so this file does too. alt-j stays alt-j rather than dissolving into
        // a bare j because the insert happened to say j.
        let got = typed("j", Some("j"), plain("alt")).expect("alt-j");
        assert_eq!(got, Key::parse("alt-j").unwrap());
        let got = typed("d", Some("d"), plain("ctrl")).expect("ctrl-d");
        assert_eq!(got, Key::parse("ctrl-d").unwrap());
    }

    #[test]
    fn capitals_survive_whether_they_came_from_the_key_or_the_insert() {
        // Both spellings of shift-g land on the capital the map binds.
        assert_eq!(typed("g", Some("G"), plain("shift")), Some(Key::char('G')));
        assert_eq!(typed("g", None, plain("shift")), Some(Key::char('G')));
        // An unshifted letter whose insert is itself stays lowercase.
        assert_eq!(typed("g", Some("g"), plain("")), Some(Key::char('g')));
    }

    #[test]
    fn named_keys_ignore_the_insert_entirely() {
        // Enter inserts \r and space inserts a space, but a binding on them is
        // a binding on the keys — and escape inserts nothing at all.
        assert_eq!(
            typed("return", Some("\r"), plain("")),
            Some(Key::plain(Code::Enter))
        );
        assert_eq!(
            typed("space", Some(" "), plain("")),
            Some(Key::plain(Code::Char(' ')))
        );
        assert_eq!(
            typed("escape", None, plain("")),
            Some(Key::plain(Code::Esc))
        );
    }

    #[test]
    fn unsupported_inserts_are_not_invented_into_keys() {
        // Multi-character inserts are IME composition state, not keystrokes:
        // nothing is taken from them, though a physical spelling underneath
        // still answers for itself.
        assert_eq!(typed("a", Some("ab"), plain("")), Some(Key::char('a')));
        // And where neither half is a single character — the Insert key, a
        // media key — the press is nothing, exactly as before.
        assert_eq!(typed("insert", Some("help"), plain("")), None);
        assert_eq!(typed("insert", None, plain("")), None);
        assert_eq!(typed("f3", None, plain("")), None);
        assert_eq!(typed("media", Some("play"), plain("")), None);
    }

    #[test]
    fn what_no_client_can_bind_comes_back_none() {
        // Function keys have no Code variant; multi-character names that are
        // not keys are not invented into one.
        assert_eq!(key("f3", plain("")), None);
        assert_eq!(key("media", plain("")), None);
    }
}
