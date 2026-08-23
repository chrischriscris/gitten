//! The window's half of input: a platform keystroke in, a [`command::Key`] out.
//!
//! This is the file that makes the window read `[keys]`. Everything past it is
//! shared — [`command::Keymap`] resolves, the shell dispatches the name — so
//! what belongs here is exactly what a second client would have to write again:
//! how *its* platform spells a key.
//!
//! The terminal's is `plait-tui`'s `term.rs`, over crossterm events. This one is
//! over GPUI's [`Keystroke`], whose spelling differs in three ways worth
//! writing down:
//!
//! - **Shift is on the modifiers, not the character**, for letters: `shift-a`
//!   arrives as key `"a"` with `shift` set. Every other client reports the
//!   capital itself, and the shipped map binds `G` and `T` by their capitals —
//!   so the shift goes back where every other client has it: into the char.
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
        // One character, whatever it is: letters, digits, punctuation. A
        // shifted letter becomes its capital, which is both what the other
        // clients report and what the shipped map binds.
        other => {
            let mut chars = other.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            Code::Char(match k.modifiers.shift && c.is_ascii_lowercase() {
                true => c.to_ascii_uppercase(),
                false => c,
            })
        }
    };
    Some(Key::new(
        code,
        k.modifiers.control,
        k.modifiers.alt,
        k.modifiers.shift,
    ))
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
    fn what_no_client_can_bind_comes_back_none() {
        // Function keys have no Code variant; multi-character names that are
        // not keys are not invented into one.
        assert_eq!(key("f3", plain("")), None);
        assert_eq!(key("media", plain("")), None);
    }
}
