//! The config file, and reloading it while the window is open.
//!
//! `Theme` and `Font` were already plain data in `core` with no dependencies —
//! this is the other end of that. A file of hex colours and numbers, read into a
//! `Host`, watched, and applied again every time it is saved. No rebuild, no
//! relaunch, no lost scroll position.
//!
//! It lives in the shell rather than in `core` for one reason: reading a file is
//! I/O, and `core` does none. When a `cli/` arrives and wants the same file, this
//! becomes its own crate — the way `plait-git` is the only crate that talks to a
//! repository.
//!
//! # The one rule about what may go in here
//!
//! **Data only. Never behaviour.** Not a stylistic preference — a settings panel
//! has to be able to rewrite this file in place, keeping the comments and the key
//! order, and it cannot round-trip a function. So a keybinding names its command
//! (`lua = "my.toggle"`) and the behaviour lives in a plugin file that this format
//! only ever points at.
//!
//! That is also why there are no expressions and no computed colours here. When a
//! field is only ever "that one, but lighter", derive it in `Theme` rather than
//! asking the file for it — `Theme::rebuild` already resolves every syntax colour
//! against every surface that way. See
//! `docs/decisions/0012-config-is-data-behaviour-is-not.md`.
//!
//! # The split that makes this testable
//!
//! [`apply`] is a pure function of a string. Every test below runs it on a
//! literal, with no file and no watcher; [`load`] and [`watch`] are the only
//! parts that touch a disk, and they are three lines each.
//!
//! # One list, both directions
//!
//! Every colour is named once, in the `rgb_fields!` invocation below, which
//! generates the setter *and* the writer from that one list. So `plait config`
//! emits a file that reads back identically, and a field cannot be added to the
//! theme in a way that is settable but not dumpable — there is a round-trip test
//! over the whole thing.
//!
//! # What reloads live and what does not
//!
//! Colours, the font family and the font size are read on the render path, so
//! they land on the next frame. Two things cannot:
//!
//! - **`font.monospaced`** decides whether Markdown table columns are padded into
//!   a grid, and that padding rewrites the row *text* during `prepare`. Changing
//!   it needs the diff rebuilt, so it takes effect on the next launch.
//! - **`font.advance`** picks which row `uniform_list` measures for its scroll
//!   width, computed once at load. A stale value costs a slightly wrong scroll
//!   width and nothing else.
//!
//! Both are applied to the `Host` regardless, so a relaunch picks them up, and
//! [`apply`] says so in its warnings rather than leaving you guessing.

use gpui::{App, Global};
use plait_core::font::Font;
use plait_core::host::Host;
use plait_core::syntax::Kind;
use plait_core::theme::{Rgb, Style, Theme};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The live [`Host`], as a GPUI global.
///
/// Views read their theme and font through [`host`] rather than through a clone
/// captured when they were built — a captured `Rc` is a snapshot, and the whole
/// point of a watched config file is that it stops being one.
///
/// Replaced wholesale on reload rather than mutated in place, so no view can ever
/// see half of a new theme.
pub struct Active(pub Rc<Host>);

impl Global for Active {}

/// The current host. Called on the render path, so it is a refcount bump.
pub fn host(cx: &App) -> Rc<Host> {
    cx.global::<Active>().0.clone()
}

/// Names every `Rgb` field of a [`Theme`] once, and generates both directions.
///
/// A macro rather than two hand-written matches because two lists drift: the day
/// a colour is added to the theme and only to the setter, `plait config` starts
/// emitting a file that is quietly missing it.
macro_rules! rgb_fields {
    ($( $table:literal : $( $name:literal = $($field:ident).+ ),+ $(,)? ; )+) => {
        /// Sets one colour by table and name. False when nothing matched.
        fn set_rgb(theme: &mut Theme, table: &str, name: &str, v: Rgb) -> bool {
            match (table, name) {
                $( $( ($table, $name) => { theme.$($field).+ = v; true } )+ )+
                _ => false,
            }
        }

        /// Visits every colour, for writing a file out.
        fn each_rgb(theme: &Theme, mut f: impl FnMut(&str, &str, Rgb)) {
            $( $( f($table, $name, theme.$($field).+); )+ )+
        }
    };
}

rgb_fields! {
    "chrome":
        "bg" = chrome.bg, "fg" = chrome.fg, "dim" = chrome.dim,
        "faint" = chrome.faint, "accent" = chrome.accent,
        "title_bg" = chrome.title_bg, "status_bg" = chrome.status_bg;
    "diff":
        "file_bg" = diff.file_bg, "file_fg" = diff.file_fg,
        "adds_fg" = diff.adds_fg, "dels_fg" = diff.dels_fg,
        "hunk_bg" = diff.hunk_bg, "hunk_fg" = diff.hunk_fg,
        "gutter_fg" = diff.gutter_fg,
        "context_bg" = diff.context_bg, "context_fg" = diff.context_fg,
        "added_bg" = diff.added_bg, "added_fg" = diff.added_fg,
        "added_word_bg" = diff.added_word_bg,
        "removed_bg" = diff.removed_bg, "removed_fg" = diff.removed_fg,
        "removed_word_bg" = diff.removed_word_bg;
    "markdown":
        "code_bar" = markdown.code_bar, "quote_bar" = markdown.quote_bar,
        "marker" = markdown.marker, "rule" = markdown.rule;
    "graph":
        "lane_overflow" = lane_overflow;
}

/// The syntax classes, by the name they take in the file.
const KINDS: [(&str, Kind); Kind::COUNT] = [
    ("comment", Kind::Comment),
    ("string", Kind::Str),
    ("number", Kind::Number),
    ("keyword", Kind::Keyword),
    ("type", Kind::Type),
    ("constant", Kind::Constant),
    ("function", Kind::Func),
    ("property", Kind::Property),
    ("heading", Kind::Heading),
    ("strong", Kind::Strong),
    ("emphasis", Kind::Emphasis),
    ("link", Kind::Link),
];

/// Where the config lives: `$PLAIT_CONFIG`, else `./plait.toml`.
///
/// The working directory rather than a home directory, deliberately for now — a
/// dev loop wants the file next to the code it is describing, and a per-user
/// location is a product decision this does not need to make yet.
pub fn path() -> PathBuf {
    std::env::var_os("PLAIT_CONFIG").map(PathBuf::from).unwrap_or_else(|| "plait.toml".into())
}

/// Reads and applies the config, if there is one.
///
/// A missing file is not an error — it means the shipped defaults, which is the
/// common case. An unreadable or malformed one *is* worth saying, but never worth
/// failing over: a half-typed file mid-edit must not take the window down.
pub fn load(host: &mut Host, path: &Path) -> Vec<String> {
    match std::fs::read(path) {
        Ok(bytes) => apply(host, &String::from_utf8_lossy(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => vec![format!("{}: {e}", path.display())],
    }
}

/// Applies a config to a host, returning anything worth telling the user.
///
/// Pure, and forgiving on purpose. This runs on every save of a file somebody is
/// in the middle of editing, so a bad line warns and is skipped rather than
/// throwing the rest away — a typo in one colour should not blank a theme.
pub fn apply(host: &mut Host, text: &str) -> Vec<String> {
    let mut warn = Vec::new();
    let doc: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            // The whole file is unparseable; the host is left exactly as it was.
            return vec![format!("config: {e}")];
        }
    };

    if let Some(font) = doc.get("font") {
        apply_font(&mut host.font, font, &mut warn);
    }
    if let Some(theme) = doc.get("theme") {
        apply_theme(&mut host.theme, theme, &mut warn);
        // Required after touching fields directly: the resolved
        // syntax-by-surface table is what the render path reads.
        host.theme.rebuild();
    }
    for key in doc.keys() {
        if !matches!(key.as_str(), "font" | "theme") {
            warn.push(format!("config: unknown section [{key}]"));
        }
    }
    warn
}

fn apply_font(font: &mut Font, value: &toml::Value, warn: &mut Vec<String>) {
    let Some(t) = value.as_table() else {
        warn.push("config: [font] is not a table".into());
        return;
    };
    for (key, v) in t {
        match key.as_str() {
            "family" => match v.as_str() {
                Some(s) if !s.trim().is_empty() => font.family = s.to_string(),
                _ => warn.push("config: font.family must be a non-empty string".into()),
            },
            "size" => match number(v) {
                // A size of zero is invisible and a huge one draws off the row;
                // both are easy to type by accident in a live-reloaded file.
                Some(n) if (4.0..=96.0).contains(&n) => font.size = n,
                _ => warn.push("config: font.size must be between 4 and 96".into()),
            },
            "advance" => match number(v) {
                Some(n) if (0.1..=2.0).contains(&n) => {
                    // Only worth saying when it actually moved: this file is
                    // re-read on every save, and a warning that fires for an
                    // unchanged value trains you to ignore all of them.
                    if (n - font.advance).abs() > f32::EPSILON {
                        font.advance = n;
                        warn.push("config: font.advance applies on the next launch".into());
                    }
                }
                _ => warn.push("config: font.advance must be between 0.1 and 2.0".into()),
            },
            "monospaced" => match v.as_bool() {
                Some(b) => {
                    if b != font.monospaced {
                        font.monospaced = b;
                        warn.push("config: font.monospaced applies on the next launch".into());
                    }
                }
                None => warn.push("config: font.monospaced must be true or false".into()),
            },
            other => warn.push(format!("config: unknown key font.{other}")),
        }
    }
}

fn apply_theme(theme: &mut Theme, value: &toml::Value, warn: &mut Vec<String>) {
    let Some(t) = value.as_table() else {
        warn.push("config: [theme] is not a table".into());
        return;
    };
    for (key, v) in t {
        match key.as_str() {
            "name" => match v.as_str() {
                Some(s) => theme.name = s.to_string(),
                None => warn.push("config: theme.name must be a string".into()),
            },
            "min_contrast" => match number(v) {
                // 1.0 is "no floor at all", 21.0 is black on white. Outside that
                // the contrast resolver has nothing to aim at.
                Some(n) if (1.0..=21.0).contains(&n) => theme.min_contrast = n,
                _ => warn.push("config: theme.min_contrast must be between 1 and 21".into()),
            },
            "lanes" => match colors(v, warn, "theme.lanes") {
                Some(c) if !c.is_empty() => theme.lanes = c,
                _ => warn.push("config: theme.lanes must be a non-empty list".into()),
            },
            "authors" => match colors(v, warn, "theme.authors") {
                Some(c) if !c.is_empty() => theme.authors = c,
                _ => warn.push("config: theme.authors must be a non-empty list".into()),
            },
            "syntax" => apply_syntax(theme, v, warn),
            table => apply_palette(theme, table, v, warn),
        }
    }
}

/// One of the colour tables — `[theme.diff]`, `[theme.chrome]` and so on.
fn apply_palette(theme: &mut Theme, table: &str, value: &toml::Value, warn: &mut Vec<String>) {
    let Some(t) = value.as_table() else {
        warn.push(format!("config: [theme.{table}] is not a table"));
        return;
    };
    for (name, v) in t {
        match rgb(v) {
            Some(c) => {
                if !set_rgb(theme, table, name, c) {
                    warn.push(format!("config: unknown colour theme.{table}.{name}"));
                }
            }
            None => warn.push(format!("config: theme.{table}.{name} is not a #rrggbb colour")),
        }
    }
}

/// `comment = "#615a52 italic"` — a colour, then any of `bold` and `italic`.
///
/// Weight and slant are part of a syntax style because emphasis in prose is not
/// a colour; putting them in the same string keeps one line per class, which is
/// what makes the block scannable when you are tuning it.
fn apply_syntax(theme: &mut Theme, value: &toml::Value, warn: &mut Vec<String>) {
    let Some(t) = value.as_table() else {
        warn.push("config: [theme.syntax] is not a table".into());
        return;
    };
    for (name, v) in t {
        let Some(kind) = KINDS.iter().find(|(n, _)| *n == name).map(|(_, k)| *k) else {
            warn.push(format!("config: unknown syntax class {name}"));
            continue;
        };
        let Some(s) = v.as_str() else {
            warn.push(format!("config: theme.syntax.{name} must be a string"));
            continue;
        };
        match style(s) {
            Some(st) => theme.set_syntax(kind, st),
            None => warn.push(format!("config: theme.syntax.{name}: {s:?} is not a style")),
        }
    }
}

fn style(s: &str) -> Option<Style> {
    let mut parts = s.split_whitespace();
    let mut out = Style::fg(parse_hex(parts.next()?)?);
    for word in parts {
        match word {
            "bold" => out = out.bold(),
            "italic" => out = out.italic(),
            _ => return None,
        }
    }
    Some(out)
}

fn colors(v: &toml::Value, warn: &mut Vec<String>, what: &str) -> Option<Vec<Rgb>> {
    let list = v.as_array()?;
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        match rgb(item) {
            Some(c) => out.push(c),
            None => {
                warn.push(format!("config: {what} has an entry that is not a #rrggbb colour"));
                return None;
            }
        }
    }
    Some(out)
}

fn rgb(v: &toml::Value) -> Option<Rgb> {
    match v {
        toml::Value::String(s) => parse_hex(s),
        // A bare `0x16241a` is an integer to TOML, and it is what someone
        // copying out of `theme.rs` will type.
        toml::Value::Integer(i) => u32::try_from(*i).ok().filter(|c| *c <= 0xffffff),
        _ => None,
    }
}

/// `#rrggbb`, `rrggbb` or `0xrrggbb`.
fn parse_hex(s: &str) -> Option<Rgb> {
    let t = s.trim();
    let t = t.strip_prefix('#').or_else(|| t.strip_prefix("0x")).unwrap_or(t);
    (t.len() == 6 && t.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(t, 16).ok())
        .flatten()
}

/// TOML tells floats and integers apart; a size of `14` and `14.0` are both fine.
fn number(v: &toml::Value) -> Option<f32> {
    match v {
        toml::Value::Float(f) => Some(*f as f32),
        toml::Value::Integer(i) => Some(*i as f32),
        _ => None,
    }
}

// ------------------------------------------------------------------- writing

/// The whole of a host's appearance as a config file.
///
/// What `plait config` prints, so there is a complete and correct starting point
/// rather than a page of documentation to copy from. Generated from the same
/// field list that reads it back, and a test round-trips it.
pub fn dump(host: &Host) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str("# plait — saved while the window is open and applied on the next frame.\n");
    out.push_str("# Colours are #rrggbb. Delete anything to fall back to the default.\n\n");

    let f = &host.font;
    out.push_str("[font]\n");
    out.push_str(&format!("family = {:?}\n", f.family));
    out.push_str(&format!("size = {:?}\n", f.size));
    out.push_str("# Both of these apply on the next launch, not on save.\n");
    out.push_str(&format!("monospaced = {}\n", f.monospaced));
    out.push_str(&format!("advance = {:?}\n\n", f.advance));

    let t = &host.theme;
    out.push_str("[theme]\n");
    out.push_str(&format!("name = {:?}\n", t.name));
    out.push_str(&format!("min_contrast = {:?}\n", t.min_contrast));
    out.push_str(&format!("lanes = [{}]\n", hex_list(&t.lanes)));
    out.push_str(&format!("authors = [{}]\n", hex_list(&t.authors)));

    // Grouped by table, in the order the field list declares them, so the file
    // reads the way `theming.md` describes the palettes.
    let mut current = String::new();
    each_rgb(t, |table, name, c| {
        if table != current {
            out.push_str(&format!("\n[theme.{table}]\n"));
            current = table.to_string();
        }
        out.push_str(&format!("{name} = \"{}\"\n", hex(c)));
    });

    out.push_str("\n[theme.syntax]\n");
    for (name, kind) in KINDS {
        let s = t.syntax(kind);
        let mut v = hex(s.fg);
        if s.bold {
            v.push_str(" bold");
        }
        if s.italic {
            v.push_str(" italic");
        }
        out.push_str(&format!("{name} = \"{v}\"\n"));
    }
    out
}

fn hex(c: Rgb) -> String {
    format!("#{c:06x}")
}

fn hex_list(cs: &[Rgb]) -> String {
    cs.iter().map(|c| format!("\"{}\"", hex(*c))).collect::<Vec<_>>().join(", ")
}

// ------------------------------------------------------------------ watching

/// Calls `on_change` whenever the config file is written.
///
/// Returns the watcher, which must be kept alive — dropping it stops the
/// watching, silently, which is a good way to lose an afternoon.
///
/// The file's *directory* is watched rather than the file: editors rename a
/// temporary file over the original rather than writing it in place, which
/// destroys the inode a file watch is holding, and the save after the first one
/// then goes unnoticed.
pub fn watch(
    path: &Path,
    mut on_change: impl FnMut() + Send + 'static,
) -> notify::Result<Box<dyn notify::Watcher + Send>> {
    use notify::{EventKind, RecursiveMode, Watcher};

    let file = path.file_name().map(|f| f.to_owned());
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        if !matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        ) {
            return;
        }
        let ours = match &file {
            Some(name) => event.paths.iter().any(|p| p.file_name() == Some(name)),
            None => true,
        };
        if ours {
            on_change();
        }
    })?;
    watcher.watch(dir, RecursiveMode::NonRecursive)?;
    Ok(Box::new(watcher))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plait_core::theme::Surface;

    fn host() -> Host {
        Host::new()
    }

    #[test]
    fn an_empty_or_missing_config_changes_nothing() {
        let mut h = host();
        let before = (h.theme.clone(), h.font.clone());
        assert!(apply(&mut h, "").is_empty());
        assert_eq!((h.theme.clone(), h.font.clone()), before);
        // And a file that does not exist is not an error.
        assert!(load(&mut h, Path::new("/nonexistent/plait.toml")).is_empty());
    }

    #[test]
    fn a_colour_reaches_the_theme() {
        let mut h = host();
        let warn = apply(&mut h, "[theme.diff]\nadded_bg = \"#123456\"\n");
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(h.theme.diff.added_bg, 0x123456);
    }

    #[test]
    fn every_spelling_of_a_colour_parses() {
        for text in ["\"#123456\"", "\"123456\"", "\"0x123456\"", "0x123456"] {
            let mut h = host();
            let warn = apply(&mut h, &format!("[theme.chrome]\nbg = {text}\n"));
            assert!(warn.is_empty(), "{text}: {warn:?}");
            assert_eq!(h.theme.chrome.bg, 0x123456, "{text}");
        }
    }

    #[test]
    fn setting_a_syntax_style_rebuilds_the_resolved_table() {
        // The render path reads `syntax_on`, not `syntax`, so forgetting the
        // rebuild would mean the change never appears.
        let mut h = host();
        let warn = apply(&mut h, "[theme.syntax]\ncomment = \"#ff0000 bold italic\"\n");
        assert!(warn.is_empty(), "{warn:?}");
        let s = h.theme.syntax(Kind::Comment);
        assert_eq!(s.fg, 0xff0000);
        assert!(s.bold && s.italic);
        // Resolved against a surface, it is still red-ish and still bold.
        assert!(h.theme.syntax_on(Kind::Comment, Surface::Context).bold);
    }

    #[test]
    fn the_font_reaches_the_host() {
        let mut h = host();
        let warn = apply(&mut h, "[font]\nfamily = \"Iosevka\"\nsize = 16\n");
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(h.font.family, "Iosevka");
        assert_eq!(h.font.size, 16.0);
    }

    #[test]
    fn the_fields_that_need_a_relaunch_say_so() {
        let mut h = host();
        let warn = apply(&mut h, "[font]\nmonospaced = false\n");
        assert!(h.font.monospaced == false, "it was still applied");
        assert!(
            warn.iter().any(|w| w.contains("next launch")),
            "no warning about needing a relaunch: {warn:?}"
        );
    }

    #[test]
    fn a_bad_line_is_skipped_and_the_rest_still_applies() {
        // The important one: this runs on every save of a file being edited, so
        // one typo may not throw away the other forty colours.
        let mut h = host();
        let warn = apply(
            &mut h,
            "[theme.diff]\nadded_bg = \"#111111\"\ndels_fg = \"nonsense\"\nhunk_bg = \"#222222\"\n",
        );
        assert_eq!(h.theme.diff.added_bg, 0x111111);
        assert_eq!(h.theme.diff.hunk_bg, 0x222222);
        assert_eq!(warn.len(), 1, "{warn:?}");
        assert!(warn[0].contains("dels_fg"), "{warn:?}");
    }

    #[test]
    fn unparseable_toml_leaves_the_host_exactly_as_it_was() {
        let mut h = host();
        let before = h.theme.clone();
        let warn = apply(&mut h, "[theme.diff\nadded_bg =");
        assert_eq!(h.theme, before, "a half-typed file changed the theme");
        assert_eq!(warn.len(), 1);
    }

    #[test]
    fn nonsense_values_are_refused_rather_than_applied() {
        let mut h = host();
        let size = h.font.size;
        let warn = apply(&mut h, "[font]\nsize = 0\n");
        assert_eq!(h.font.size, size, "a zero size was accepted");
        assert!(!warn.is_empty());

        let mut h = host();
        let warn = apply(&mut h, "[theme]\nmin_contrast = 400\n");
        assert_eq!(h.theme.min_contrast, 3.5);
        assert!(!warn.is_empty(), "{warn:?}");

        // An empty lane list would make `Theme::lane` fall back forever.
        let mut h = host();
        apply(&mut h, "[theme]\nlanes = []\n");
        assert!(!h.theme.lanes.is_empty());
    }

    #[test]
    fn unknown_keys_are_named_not_ignored() {
        let mut h = host();
        let warn = apply(&mut h, "[nope]\nx = 1\n[theme.diff]\nnot_a_colour = \"#111111\"\n");
        assert!(warn.iter().any(|w| w.contains("[nope]")), "{warn:?}");
        assert!(warn.iter().any(|w| w.contains("not_a_colour")), "{warn:?}");
    }

    #[test]
    fn a_config_that_changes_nothing_says_nothing() {
        // This file is re-read on every save. A warning that fires for an
        // unchanged value teaches you to ignore the ones that matter.
        let mut h = host();
        let text = dump(&h);
        let warn = apply(&mut h, &text);
        assert!(warn.is_empty(), "round-tripping the defaults warned: {warn:?}");
    }

    #[test]
    fn what_dump_writes_reads_back_identically() {
        // This is what guarantees the two directions cannot drift: a field that
        // is dumped but not settable, or settable but not dumped, fails here.
        let mut original = host();
        original.theme.diff.added_bg = 0x0a0b0c;
        original.theme.chrome.accent = 0x0d0e0f;
        original.theme.markdown.rule = 0x101112;
        original.theme.lane_overflow = 0x131415;
        original.theme.lanes = vec![0x161718, 0x191a1b];
        original.theme.authors = vec![0x1c1d1e];
        original.theme.min_contrast = 4.5;
        original.theme.name = "round trip".into();
        original.font = Font { family: "Iosevka".into(), size: 15.0, monospaced: true, advance: 0.5 };
        original.theme.rebuild();

        let text = dump(&original);
        let mut restored = Host::new();
        let warn = apply(&mut restored, &text);
        assert!(
            warn.iter().all(|w| w.contains("next launch")),
            "dump produced a file with real warnings: {warn:?}\n{text}"
        );
        assert_eq!(restored.theme, original.theme, "theme did not survive:\n{text}");
        assert_eq!(restored.font, original.font, "font did not survive");
    }

    #[test]
    fn dump_covers_every_syntax_class() {
        // `KINDS` is a hand-written list against an enum; this is what catches a
        // class added to `Kind` and not to the file format.
        assert_eq!(KINDS.len(), Kind::COUNT);
        let text = dump(&host());
        for k in Kind::ALL {
            assert!(
                KINDS.iter().any(|(_, kind)| *kind == k),
                "{k:?} has no name in the config format"
            );
        }
        for (name, _) in KINDS {
            assert!(text.contains(&format!("{name} = ")), "{name} missing from dump");
        }
    }

    #[test]
    fn every_colour_in_the_theme_is_reachable_by_name() {
        // Walks the generated list and sets each field to a unique value through
        // `apply`, then checks the dump reports it back. A colour the macro list
        // forgot is a colour nobody can configure.
        let mut seen = 0;
        each_rgb(&host().theme, |table, name, _| {
            let mut h = host();
            let text = format!("[theme.{table}]\n{name} = \"#abcdef\"\n");
            let warn = apply(&mut h, &text);
            assert!(warn.is_empty(), "theme.{table}.{name}: {warn:?}");
            let mut found = false;
            each_rgb(&h.theme, |t2, n2, c| {
                if t2 == table && n2 == name {
                    assert_eq!(c, 0xabcdef, "theme.{table}.{name} did not take");
                    found = true;
                }
            });
            assert!(found);
            seen += 1;
        });
        assert!(seen >= 27, "only {seen} colours are configurable");
    }

    #[test]
    fn a_style_needs_a_colour_and_takes_only_known_words() {
        assert!(style("#ffffff").is_some());
        assert!(style("#ffffff bold").is_some());
        assert!(style("#ffffff bold italic").is_some());
        assert!(style("bold").is_none(), "accepted a style with no colour");
        assert!(style("#ffffff wobbly").is_none(), "accepted an unknown word");
        assert!(style("").is_none());
    }

    #[test]
    fn hex_parsing_refuses_what_it_should() {
        assert_eq!(parse_hex("#abcdef"), Some(0xabcdef));
        assert_eq!(parse_hex("  #abcdef  "), Some(0xabcdef));
        assert_eq!(parse_hex("#abcde"), None, "five digits");
        assert_eq!(parse_hex("#abcdefa"), None, "seven digits");
        assert_eq!(parse_hex("#gggggg"), None);
        assert_eq!(parse_hex(""), None);
    }
}
