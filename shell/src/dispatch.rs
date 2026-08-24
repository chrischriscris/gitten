//! The window's half of input: a platform keystroke in, a [`command::Key`] out.
//!
//! This is the file that makes the window read `[keys]`. Everything past it is
//! shared — [`command::Keymap`] resolves, the shell dispatches the name — so
//! what belongs here is exactly what a second client would have to write again:
//! how *its* platform spells a key.
//!
//! The terminal's is `gitten-tui`'s `term.rs`, over crossterm events. This one is
//! over GPUI's [`Keystroke`], whose spelling differs in four ways worth
//! writing down:
//!
//! - **Shift is on the modifiers, not the character**, for letters: `shift-a`
//!   arrives as key `"a"` with `shift` set. Every other client reports the
//!   capital itself, and the shipped map binds `G` and `T` by their capitals —
//!   so the shift goes back where every other client has it: into the char.
//! - **A keystroke carries two characters**: `key`, the physical key's own
//!   name, and `key_char`, the character the press would have inserted. On a
//!   non-US layout these part ways — option-s on a German keyboard arrives as
//!   key `s` with insert `ß` — and GPUI answers for *each binding* whether it
//!   matches either spelling ([`Keystroke::should_match`]: a differing insert
//!   is matched with control alone, because shift lives inside the character
//!   and alt went into composing it; failing that, the physical key matches
//!   with its full modifiers). So one press can mean two keys, and which of
//!   them fires is the keymap's decision, not this file's: [`translate`]
//!   returns **every** spelling, logical first, and the keymap's
//!   `resolve_any` matches each binding against any of them. Choosing
//!   one here would hardcode a binding that may not exist — a plain
//!   `ß` map would never see alt-s, an alt-s map would never see `ß`,
//!   and mode precedence would be answered before it was consulted.
//!
//!   When the two spellings agree — an unshifted letter, a plain symbol — there
//!   is nothing to choose and one candidate comes back, carrying its own
//!   modifiers exactly as GPUI's physical fallback compares them. A shifted
//!   physical ASCII letter is represented by its capital, the same spelling
//!   every other client and this map use; shifted punctuation has no lossless
//!   physical representation and therefore adds no fallback.
//!   An insert that is not one character is IME composition state: nothing is
//!   invented from it, and if the physical half is no single character either,
//!   the press translates to nothing.
//! - **A shifted symbol arrives as the symbol**: `?` is key `"?"` with `shift`
//!   *cleared*, because macOS reports the shifted character as the key. Left
//!   alone that would bind `shift-?` and never fire; [`Key::new`] would drop
//!   the flag anyway.
//! - **The platform modifier owns its keys.** `cmd-q`, `cmd-c` and `cmd-a` are
//!   the menu's, not this map's — they are real macOS bindings a Mac user's
//!   fingers already know, and they stay native. A keystroke with `platform`
//!   set does not translate.

use gitten_core::command::{Code, Key};
use gpui::Keystroke;

/// Every key one GPUI keystroke could mean, in the order GPUI's own matcher
/// would try them — or empty for anything no client can bind: the platform
/// modifier, function keys, and lone modifiers (which GPUI synthesizes for
/// binding matching and never delivers here).
///
/// Most presses carry exactly one spelling. A press whose insert differs from
/// its physical key carries two; see the module note for why both survive and
/// `gitten_core::command::Keymap::resolve_any` for who decides between them.
pub fn translate(k: &Keystroke) -> Vec<Key> {
    // Cmd-c means copy to the OS, whatever `gitten.toml` says. See the module
    // note: the menu adapters own these, and translating them too would be a
    // command that fires twice.
    if k.modifiers.platform || k.modifiers.function {
        return Vec::new();
    }
    // The two characters a keystroke carries. The insert matters only when it
    // differs from the physical spelling — that difference is exactly the case
    // the layout broke. Named keys below ignore both halves entirely.
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
        // and the shift goes no further. Ctrl and alt still belong to the press.
        "tab" if k.modifiers.shift => {
            return vec![Key::new(
                Code::BackTab,
                k.modifiers.control,
                k.modifiers.alt,
                false,
            )];
        }
        "tab" => Code::Tab,
        "backspace" => Code::Backspace,
        // The forward-delete key; `insert` has no binding anywhere and is not
        // quietly turned into one.
        "delete" | "del" => Code::Delete,
        "esc" | "escape" => Code::Esc,
        // Everything the named arms above did not take, decided entirely by
        // the two characters computed before the match.
        _ => return characters(physical, inserted, &k.modifiers),
    };
    // A named key is its name: a binding on it is a binding on the key, and
    // the modifiers held are the modifiers spelled.
    vec![Key::new(
        code,
        k.modifiers.control,
        k.modifiers.alt,
        k.modifiers.shift,
    )]
}

/// The candidates of a press that is about a *character*: the insert, the
/// physical key, or both.
///
/// The logical candidate keeps only the modifiers GPUI matches a differing
/// insert against — control, because ctrl-a is still a command and not a
/// character anybody typed; shift is already inside the character and alt went
/// into composing it. The physical candidate then falls back exactly as
/// GPUI's own comparison does, with every modifier it was pressed with. Shift
/// cannot ride on a character in this map, so an ASCII letter folds it into its
/// capital; shifted punctuation has no lossless fallback.
fn characters(physical: Option<char>, inserted: Option<char>, mods: &gpui::Modifiers) -> Vec<Key> {
    let Some(c) = inserted.or(physical) else {
        return Vec::new();
    };
    match inserted.is_some() {
        true => {
            let mut out = vec![Key::new(Code::Char(c), mods.control, false, false)];
            if let Some(mut p) = physical {
                if mods.shift {
                    if !p.is_ascii_lowercase() {
                        return out;
                    }
                    p.make_ascii_uppercase();
                }
                let fallback = Key::new(Code::Char(p), mods.control, mods.alt, false);
                if !out.contains(&fallback) {
                    out.push(fallback);
                }
            }
            out
        }
        false => {
            // Capitals are folded wherever they came from — a shift-flagged
            // physical letter becomes one, which is both what the other
            // clients report and what the shipped map binds.
            let c = match mods.shift && c.is_ascii_lowercase() {
                true => c.to_ascii_uppercase(),
                false => c,
            };
            vec![Key::new(Code::Char(c), mods.control, mods.alt, mods.shift)]
        }
    }
}

/// The one character of `s`, or nothing: multi-character strings are IME
/// mid-composition state or key names no [`Code`](gitten_core::command::Code)
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
    use gitten_core::command::{Keymap, Modes, Resolve};
    use gpui::Modifiers;

    /// Every spelling of a keystroke.
    fn key(key: &str, m: Modifiers) -> Vec<Key> {
        translate(&Keystroke {
            modifiers: m,
            key: key.into(),
            key_char: None,
        })
    }

    /// A keystroke as a real keyboard delivers it: physical name and insert.
    fn typed(key: &str, insert: Option<&str>, m: Modifiers) -> Vec<Key> {
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

    /// The one spelling of an unambiguous press.
    fn t(spelling: &str, mods: &str) -> String {
        let got = key(spelling, plain(mods));
        assert_eq!(got.len(), 1, "{spelling} meant {got:?}");
        got[0].to_string()
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
        // The property `[keys]` rests on: a chord written in gitten.toml is the
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
            let got = key(&spelling(want.code), modifiers_of(want));
            assert_eq!(
                got.len(),
                1,
                "{} came back with several spellings",
                spelling(want.code)
            );
            assert_eq!(&got[0], want);
            // Resolved in its own mode — `diff.*` keys are not global, and that
            // is the point of them.
            let mut modes = Modes::new();
            modes.push(b.mode.as_str());
            assert_eq!(
                k.resolve(&modes, &[*want]),
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
        let got = key("?", plain("shift"));
        assert_eq!(got, vec![Key::char('?')]);
        assert!(!got[0].shift);
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
        assert_eq!(t("tab", "ctrl-shift"), "ctrl-backtab");
        assert_eq!(t("tab", "alt-shift"), "alt-backtab");
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
        assert!(key("q", plain("cmd")).is_empty());
        assert!(key("c", plain("cmd")).is_empty());
        assert!(key("a", plain("cmd")).is_empty());
    }

    #[test]
    fn a_punctuation_binding_follows_the_character_not_the_key_cap() {
        // The non-US case that broke: on a German layout `?` sits where US has
        // shift-´, and GPUI reports key "/" with insert "?". A binding written
        // "?" is a binding on the *character* — every other client reports the
        // character — so the insert wins whenever the two spellings part ways.
        assert_eq!(typed("/", Some("?"), plain("shift")), vec![Key::char('?')]);
        // The shipped diff bindings, which are punctuation too. No modifiers
        // held, so the physical half falls back as the bare key it is.
        assert_eq!(
            typed("ù", Some("["), plain("")),
            vec![Key::char('['), Key::char('ù')]
        );
        assert_eq!(typed("à", Some("]"), plain(""))[0], Key::char(']'));
        // ...and with no insert at all the physical spelling still answers.
        assert_eq!(key("[", plain("")), vec![Key::char('[')]);
    }

    #[test]
    fn the_insert_keeps_only_the_modifiers_gpui_would_match_it_with() {
        // `Keystroke::should_match` compares a differing key_char against a
        // target holding control alone: shift is already inside the character
        // (alt-ç inserts `$` on a Czech keyboard; the `$` needs no alt), and
        // alt was part of composing it. Control survives, because ctrl-a is
        // still a command and not a character anybody typed.
        let got = typed("ç", Some("$"), plain("alt"));
        assert_eq!(got[0], Key::char('$'));
        assert!(!got[0].alt && !got[0].shift);
        // The physical half falls back with its own modifiers, as GPUI's
        // comparison does after the logical one missed — a binding on alt-ç
        // fires here, on a map that never heard of `$`.
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[1], Key::parse("alt-ç").unwrap());

        let got = typed("x", Some("X"), plain("ctrl-shift"));
        assert_eq!(got[0], Key::ctrl(Code::Char('X')));
        assert!(got[0].ctrl && !got[0].shift);
        // The physical fallback folds onto the same capital and is deduplicated.
        assert_eq!(got.len(), 1, "{got:?}");

        // On a non-Latin layout the logical character and shifted physical key
        // differ. Both the typed character and the map's capital binding work.
        let got = typed("g", Some("Γ"), plain("shift"));
        assert_eq!(got, vec![Key::char('Γ'), Key::char('G')]);
        let got = typed("g", Some("Γ"), plain("ctrl-alt-shift"));
        assert_eq!(got[0], Key::ctrl(Code::Char('Γ')));
        assert_eq!(got[1], Key::parse("ctrl-alt-G").unwrap());
    }

    #[test]
    fn an_insert_that_agrees_with_the_key_changes_nothing() {
        // When the two spellings are the same there is no layout story, and
        // GPUI falls back to matching the physical key with its own modifiers —
        // so this file does too. alt-j stays alt-j rather than dissolving into
        // a bare j because the insert happened to say j.
        assert_eq!(
            typed("j", Some("j"), plain("alt")),
            vec![Key::parse("alt-j").unwrap()]
        );
        assert_eq!(
            typed("d", Some("d"), plain("ctrl")),
            vec![Key::parse("ctrl-d").unwrap()]
        );
    }

    #[test]
    fn capitals_survive_whether_they_came_from_the_key_or_the_insert() {
        // Both spellings of shift-g land on the capital the map binds — and on
        // the capital only: the physical `g` underneath must not come along,
        // or shift-g would fire whatever `g` is bound to.
        assert_eq!(typed("g", Some("G"), plain("shift")), vec![Key::char('G')]);
        assert_eq!(typed("g", None, plain("shift")), vec![Key::char('G')]);
        // An unshifted letter whose insert is itself stays lowercase.
        assert_eq!(typed("g", Some("g"), plain("")), vec![Key::char('g')]);
    }

    #[test]
    fn named_keys_ignore_the_insert_entirely() {
        // Enter inserts \r and space inserts a space, but a binding on them is
        // a binding on the keys — and escape inserts nothing at all.
        assert_eq!(
            typed("return", Some("\r"), plain("")),
            vec![Key::plain(Code::Enter)]
        );
        assert_eq!(
            typed("space", Some(" "), plain("")),
            vec![Key::plain(Code::Char(' '))]
        );
        assert_eq!(
            typed("escape", None, plain("")),
            vec![Key::plain(Code::Esc)]
        );
    }

    #[test]
    fn unsupported_inserts_are_not_invented_into_keys() {
        // Multi-character inserts are IME composition state, not keystrokes:
        // nothing is taken from them, though a physical spelling underneath
        // still answers for itself.
        assert_eq!(typed("a", Some("ab"), plain("")), vec![Key::char('a')]);
        // And where neither half is a single character — the Insert key, a
        // media key — the press is nothing, exactly as before.
        assert!(typed("insert", Some("help"), plain("")).is_empty());
        assert!(key("insert", plain("")).is_empty());
        assert!(key("f3", plain("")).is_empty());
        assert!(typed("media", Some("play"), plain("")).is_empty());
    }

    #[test]
    fn what_no_client_can_bind_comes_back_nothing() {
        // Function keys have no Code variant; multi-character names that are
        // not keys are not invented into one.
        assert!(key("f3", plain("")).is_empty());
        assert!(key("media", plain("")).is_empty());
    }
}

/// End-to-end: a keystroke through [`translate`] into [`Keymap::resolve_any`],
/// which is the whole of what `on_key` does. The unit tests above hold the
/// spellings; these hold the decision the spellings exist for.
#[cfg(test)]
mod resolution_tests {
    use super::translate;
    use gitten_core::command::{Code, Key, Keymap, Modes, Resolve};
    use gpui::{Keystroke, Modifiers};

    /// Option-s as a German Mac delivers it.
    fn option_s() -> Keystroke {
        Keystroke {
            modifiers: Modifiers {
                alt: true,
                ..Default::default()
            },
            key: "s".into(),
            key_char: Some("ß".into()),
        }
    }

    #[test]
    fn option_s_means_either_binding_the_map_actually_holds() {
        let mut modes = Modes::new();
        modes.push("diff");
        let candidates = translate(&option_s());
        assert_eq!(
            candidates,
            vec![
                Key::new(Code::Char('ß'), false, false, false),
                Key::parse("alt-s").unwrap(),
            ],
            "logical first, then physical"
        );
        // One press, both spellings: the candidate list rides under the one
        // key it belongs to.
        let pressed = [candidates.as_slice()];

        // A plain ß binding fires through the logical spelling…
        let mut k = Keymap::empty();
        k.bind("diff", "ß", "insert.ssharp").unwrap();
        assert_eq!(
            k.resolve_any(&modes, &pressed),
            Resolve::Run("insert.ssharp")
        );

        // …and an alt-s binding fires through the physical one, on a map with
        // no opinion about ß at all.
        let mut k = Keymap::empty();
        k.bind("diff", "alt-s", "save.now").unwrap();
        assert_eq!(k.resolve_any(&modes, &pressed), Resolve::Run("save.now"));
    }

    #[test]
    fn mode_precedence_answers_before_spelling_order_does() {
        // Deciding between the two spellings *before* consulting the map would
        // hand every option-s to the global `ß` row. The innermost mode's
        // binding wins instead, whichever way it was written.
        let mut k = Keymap::empty();
        k.bind(gitten_core::command::GLOBAL, "ß", "global.ssharp")
            .unwrap();
        k.bind("diff", "alt-s", "diff.save").unwrap();
        let mut modes = Modes::new();
        modes.push("diff");
        let candidates = translate(&option_s());
        let pressed = [candidates.as_slice()];
        assert_eq!(k.resolve_any(&modes, &pressed), Resolve::Run("diff.save"));
    }
}
