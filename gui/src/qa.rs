//! The QA doors: what a screenshot run needs that no argument can say.
//!
//! `AGENTS.md` forbids launching a client unannounced, so a visual check has
//! to be reproducible from a command line — and the command line already
//! says which repository, which revspec, and (through `diff -`) which patch.
//! What it cannot say is the *window's* state: how big it opened, which
//! destination it shows, which overlay stands over it, and which palette it
//! draws with. Those four are what lives here, and they are doors and not
//! settings: read once at startup, nothing persisted, nothing a user is
//! expected to set.
//!
//! ```sh
//! GITTEN_QA_GEOM=1280x800 ./dev gui diff .
//! GITTEN_QA_VIEW=history  ./dev gui diff .
//! GITTEN_QA_DIALOG=commands ./dev gui diff .
//! GITTEN_QA_THEME=github-dark ./dev gui diff .
//! ```
//!
//! Two rules, because a harness that lies is worse than no harness:
//!
//! **A door says what it did.** Anything unrecognised — a width that is not
//! `WxH`, a dialog name nothing opens, a theme not in the registry — is
//! written to stderr in the same voice the config loader uses and then
//! ignored. A typo that silently photographs the *default* state is the one
//! failure a screenshot run cannot see for itself.
//!
//! **A door opens existing doors.** Everything here goes through the same
//! named command or the same entry point a keystroke uses — `workspace.history`,
//! `commands.palette`, `theme.picker` — so a door can only reach a state the
//! window already had, and a screenshot taken through one is a screenshot of
//! the real window.

use gpui::Context;

/// What the environment asked for, after parsing. Every field is `None` when
/// its variable is unset, which is the ordinary launch.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Doors {
    /// `GITTEN_QA_GEOM`, as `WxH` in logical pixels.
    pub geom: Option<(f32, f32)>,
    /// `GITTEN_QA_VIEW`: `changes` or `history`.
    pub view: Option<String>,
    /// `GITTEN_QA_DIALOG`: `commands`, `themes`, `settings` or `commit`.
    pub dialog: Option<String>,
    /// `GITTEN_QA_THEME`: a palette by the name the picker shows.
    pub theme: Option<String>,
}

impl Doors {
    /// The doors as the process was launched with them.
    pub(crate) fn from_env() -> Self {
        Self::read(&|name| std::env::var(name).ok())
    }

    /// The parse, over any lookup — which is what makes it testable without
    /// mutating the process environment, a thing no test can undo.
    pub(crate) fn read(get: &dyn Fn(&str) -> Option<String>) -> Self {
        Self {
            geom: geom(get("GITTEN_QA_GEOM").as_deref()),
            view: named(
                get("GITTEN_QA_VIEW").as_deref(),
                &["changes", "history"],
                "view",
            ),
            dialog: named(
                get("GITTEN_QA_DIALOG").as_deref(),
                &["commands", "themes", "settings", "commit"],
                "dialog",
            ),
            theme: get("GITTEN_QA_THEME").filter(|t| !t.is_empty()),
        }
    }

    /// Whether anything was asked for, so a launch with no doors does not pay
    /// for the read twice.
    pub(crate) fn any(&self) -> bool {
        self != &Self::default()
    }
}

/// `WxH`, in logical pixels. A size that is not one, or not positive, is said
/// and dropped.
fn geom(value: Option<&str>) -> Option<(f32, f32)> {
    let value = value?;
    let parsed = value.split_once(['x', 'X']).and_then(|(w, h)| {
        let w: f32 = w.trim().parse().ok()?;
        let h: f32 = h.trim().parse().ok()?;
        (w >= 1.0 && h >= 1.0).then_some((w, h))
    });
    if parsed.is_none() {
        eprintln!("gitten: {value:?} is not a WxH geometry — wanted e.g. 1280x800");
    }
    parsed
}

/// One of a known set of spellings, or a sentence on stderr. A typo in a
/// screenshot run is otherwise invisible: the window opens on the default and
/// the capture looks fine.
fn named(value: Option<&str>, allowed: &[&str], what: &str) -> Option<String> {
    let value = value?;
    if allowed.contains(&value) {
        return Some(value.to_string());
    }
    eprintln!(
        "gitten: {value:?} is not a {what} this window has — wanted one of {}",
        allowed.join(", ")
    );
    None
}

impl crate::DevShell {
    /// Apply every door this launch asked for, once, on the first frame.
    ///
    /// Called from `render`'s own first-frame one-shot, which is the earliest
    /// point the shell can be talked to at all: the window exists, the host is
    /// installed, and nothing has been drawn that a door's state would
    /// disagree with.
    ///
    /// Each door goes through the entry point a person's input reaches — the
    /// destination through [`DevShell::enter_history`], the overlays through
    /// the same openers the toolbar and the keyboard use — so a screenshot
    /// taken through a door is a screenshot of the real window, and a door can
    /// only ever reach a state somebody could have reached by hand.
    pub(crate) fn apply_doors(&mut self, doors: &Doors, cx: &mut Context<Self>) {
        if !doors.any() {
            return;
        }
        match doors.view.as_deref() {
            Some("history") => self.enter_history(cx),
            Some("changes") => self.enter_workspace(cx),
            _ => {}
        }
        if let Some(name) = doors.theme.as_deref() {
            let host = crate::config::host(cx);
            let named = host.themes.names().iter().position(|n| *n == name);
            match named {
                // The same call a pick makes, so the palette it lands on is
                // the palette the picker would have shown as current. It
                // writes nothing: the choice is an in-memory global, and
                // `gitten.toml` is not this door's to edit.
                Some(at) => self.choose_theme_at(at, cx),
                None => eprintln!(
                    "gitten: {name:?} is not a palette this build has — one of {}",
                    host.themes.names().join(", ")
                ),
            }
        }
        match doors.dialog.as_deref() {
            Some("commands") => self.open_palette(cx),
            Some("themes") => self.open_theme_picker(cx),
            Some("settings") => self.run_command("settings", cx),
            // The confirmation stands only with staged content, which is the
            // operation's own rule; a door that insisted would be a second
            // one, so this opens the same door a person's key does and takes
            // the same refusal.
            Some("commit") => self.run_command("workspace.commit", cx),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Doors;

    fn at(pairs: &[(&str, &str)]) -> Doors {
        let get = |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        };
        Doors::read(&get)
    }

    #[test]
    fn no_variables_is_the_ordinary_launch() {
        let none = at(&[]);
        assert!(!none.any());
        assert_eq!(none, Doors::default());
    }

    #[test]
    fn a_geometry_is_read_in_logical_pixels() {
        assert_eq!(
            at(&[("GITTEN_QA_GEOM", "1280x800")]).geom,
            Some((1280.0, 800.0))
        );
        assert_eq!(
            at(&[("GITTEN_QA_GEOM", "1728 X 1084")]).geom,
            Some((1728.0, 1084.0))
        );
        // Said and dropped, never guessed: a run with a typo must not
        // photograph the default size and call it the requested one.
        for bad in ["1280", "1280x", "x800", "1280x0", "-1x800", "big"] {
            assert_eq!(at(&[("GITTEN_QA_GEOM", bad)]).geom, None, "{bad}");
        }
    }

    #[test]
    fn a_destination_or_a_dialog_must_be_one_this_window_has() {
        assert_eq!(
            at(&[("GITTEN_QA_VIEW", "history")]).view.as_deref(),
            Some("history")
        );
        assert_eq!(at(&[("GITTEN_QA_VIEW", "commits")]).view, None);
        assert_eq!(
            at(&[("GITTEN_QA_DIALOG", "commands")]).dialog.as_deref(),
            Some("commands")
        );
        // The set is the overlays this window actually has: a name that
        // sounds like one it does not is said and dropped, never opened as
        // something else.
        assert_eq!(at(&[("GITTEN_QA_DIALOG", "palette")]).dialog, None);
        assert_eq!(at(&[("GITTEN_QA_DIALOG", "branches")]).dialog, None);
    }

    #[test]
    fn every_door_is_read_and_an_empty_theme_is_no_theme() {
        let doors = at(&[
            ("GITTEN_QA_GEOM", "1280x800"),
            ("GITTEN_QA_VIEW", "history"),
            ("GITTEN_QA_DIALOG", "commands"),
            ("GITTEN_QA_THEME", "github-dark"),
        ]);
        assert!(doors.any());
        assert_eq!(doors.geom, Some((1280.0, 800.0)));
        assert_eq!(doors.view.as_deref(), Some("history"));
        assert_eq!(doors.dialog.as_deref(), Some("commands"));
        assert_eq!(doors.theme.as_deref(), Some("github-dark"));
        assert_eq!(at(&[("GITTEN_QA_THEME", "")]).theme, None);
    }
}
