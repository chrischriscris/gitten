//! The config file, and reloading it while the window is open.
//!
//! `Theme` and `Font` were already plain data in `core` with no dependencies —
//! this is the other end of that. A file of hex colours and numbers, read into a
//! `Host`, watched, and applied again every time it is saved. No rebuild, no
//! relaunch, no lost scroll position.
//!
//! It lives here rather than in `core` for one reason: reading a file is I/O,
//! and `core` does none — the same rule that makes `gitten-git` its own crate.
//! It lived in `gitten-shell` until there were three clients, at which point the
//! window was the only one that could read the file, which is not a property a
//! config format should have.
//!
//! What stays in a client is the *reload*: how a new `Host` reaches the views
//! is a client's business, and the GPUI one swaps a global while the terminal
//! one drops it into the event loop. [`watch`] is shared; what to do when it
//! fires is not.
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
//! generates the setter *and* the writer from that one list. So `gitten config`
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
//! The whole `[diff]` table is the same: `algorithm` and `context` are read
//! during acquisition, before a window exists, and `layout` and `wrap` name what
//! the view *opens* on — `s` and `w` are what change them afterwards.
//!
//! All of them are applied to the `Host` regardless, so a relaunch picks them
//! up, and [`apply`] says so in its warnings rather than leaving you guessing.

use gitten_core::differ::Whitespace;
use gitten_core::font::Font;
use gitten_core::host::Host;
use gitten_core::syntax::Kind;
use gitten_core::theme::{Rgb, Style, Theme};
use std::path::{Path, PathBuf};

/// Names every `Rgb` field of a [`Theme`] once, and generates both directions.
///
/// A macro rather than two hand-written matches because two lists drift: the day
/// a colour is added to the theme and only to the setter, `gitten config` starts
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
        "title_bg" = chrome.title_bg, "status_bg" = chrome.status_bg,
        "border" = chrome.border,
        "raised" = chrome.raised, "keycap" = chrome.keycap,
        "selection_bg" = chrome.selection_bg,
        "selected_bg" = chrome.selected_bg, "error" = chrome.error;
    "diff":
        "file_bg" = diff.file_bg, "file_fg" = diff.file_fg,
        "adds_fg" = diff.adds_fg, "dels_fg" = diff.dels_fg,
        "hunk_bg" = diff.hunk_bg, "hunk_fg" = diff.hunk_fg,
        "gutter_fg" = diff.gutter_fg, "rule" = diff.rule,
        "context_bg" = diff.context_bg, "context_fg" = diff.context_fg,
        "added_bg" = diff.added_bg, "added_fg" = diff.added_fg,
        "added_word_bg" = diff.added_word_bg,
        "removed_bg" = diff.removed_bg, "removed_fg" = diff.removed_fg,
        "removed_word_bg" = diff.removed_word_bg,
        "moved_removed_bg" = diff.moved_removed_bg,
        "moved_added_bg" = diff.moved_added_bg,
        "absent_bg" = diff.absent_bg;
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

/// Where the config lives, most specific first:
///
/// 1. `$GITTEN_CONFIG` — an explicit path wins over everything, for a script or
///    a test that wants to say exactly which file.
/// 2. `./gitten.toml` — a project-local file, **when it exists**. This is the
///    dev loop (`./dev` runs from the repo root, where a gitignored `gitten.toml`
///    sits beside the code it themes) and the escape hatch for a repository that
///    wants to ship its own palette. It only wins when present, so it is an
///    override and not a requirement.
/// 3. `$XDG_CONFIG_HOME/gitten/gitten.toml`, else `~/.config/gitten/gitten.toml`
///    — the per-user home, which is where a normal install keeps it.
///
/// The cwd file used to be the *only* location, which meant a user who ran
/// `gitten` from anywhere but a directory they had dropped a `gitten.toml` into
/// got the built-in defaults and no way to change them — and from a bundled
/// `.app`, whose working directory is `/`, that was every launch.
pub fn path() -> PathBuf {
    resolve(
        std::env::var_os("GITTEN_CONFIG").map(PathBuf::from),
        PathBuf::from("gitten.toml").exists(),
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

/// The resolution rule as a pure function of the four inputs, so the precedence
/// is a test rather than something only a particular machine's environment
/// exercises. `path()` is the one-line call of this over the real environment.
fn resolve(
    explicit: Option<PathBuf>,
    local_exists: bool,
    xdg: Option<PathBuf>,
    home: Option<PathBuf>,
) -> PathBuf {
    let local = PathBuf::from("gitten.toml");
    if let Some(explicit) = explicit {
        return explicit;
    }
    if local_exists {
        return local;
    }
    // XDG says a relative `XDG_CONFIG_HOME` is invalid and to be ignored, which
    // is why it is filtered rather than joined onto the cwd.
    let base = xdg
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| h.join(".config")));
    match base {
        Some(base) => base.join("gitten").join("gitten.toml"),
        None => local,
    }
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
        apply_theme(host, theme, &mut warn);
    }
    if let Some(diff) = doc.get("diff") {
        apply_diff(host, diff, &mut warn);
    }
    if let Some(view) = doc.get("view") {
        apply_view(host, view, &mut warn);
    }
    if let Some(mouse) = doc.get("mouse") {
        apply_mouse(host, mouse, &mut warn);
    }
    if let Some(keys) = doc.get("keys") {
        apply_keys(host, keys, &mut warn);
    }
    for key in doc.keys() {
        if !matches!(
            key.as_str(),
            "font" | "theme" | "diff" | "view" | "mouse" | "keys"
        ) {
            warn.push(format!("config: unknown section [{key}]"));
        }
    }
    warn
}

/// `[view]` — how far a scroll goes, and how much lead the cursor keeps.
///
/// Both apply to the *next* keypress rather than on the next launch: they are
/// read where they are used and nothing is derived from them, which is what
/// makes tuning `scroll` a matter of saving the file and turning the wheel.
fn apply_view(host: &mut Host, value: &toml::Value, warn: &mut Vec<String>) {
    let Some(t) = value.as_table() else {
        warn.push("config: [view] is not a table".into());
        return;
    };
    for (key, v) in t {
        match key.as_str() {
            // Zero is not allowed where it would mean "the wheel does nothing":
            // that is `"wheeldown" = ""` in `[keys]`, which says so.
            "scroll" => match v.as_integer() {
                Some(n) if (1..=100).contains(&n) => host.view.rows = n as usize,
                _ => warn.push("config: view.scroll must be between 1 and 100 rows".into()),
            },
            // Zero *is* allowed here, and means a cursor that reaches the edge.
            "scrolloff" => match v.as_integer() {
                Some(n) if (0..=50).contains(&n) => host.view.scrolloff = n as usize,
                _ => warn.push("config: view.scrolloff must be between 0 and 50 rows".into()),
            },
            // A bool and not a width: what it looks like is the client's, and a
            // number here would be a terminal's cell in a window's pixels.
            "scrollbar" => match v.as_bool() {
                Some(on) => host.view.scrollbar = on,
                None => warn.push("config: view.scrollbar must be true or false".into()),
            },
            // A share of the window's width, not a pixel count: the window
            // has no size the file can name. Out of the band is refused and
            // named, the way every knob here answers a bad value — a silent
            // clamp would hide a typo behind a number nobody chose.
            "sidebar" => match v.as_float().map(|s| s as f32) {
                Some(s)
                    if (gitten_core::host::SIDEBAR_MIN..=gitten_core::host::SIDEBAR_MAX)
                        .contains(&s) =>
                {
                    host.sidebar_share = s
                }
                _ => warn.push("config: view.sidebar must be between 0.20 and 0.50".into()),
            },
            _ => warn.push(format!("config: unknown key view.{key}")),
        }
    }
}

/// `[mouse]` — what the mouse does besides move the cursor.
///
/// Read where it is used, like `[view]`, so saving the file changes the next
/// drag rather than the next launch.
fn apply_mouse(host: &mut Host, value: &toml::Value, warn: &mut Vec<String>) {
    let Some(t) = value.as_table() else {
        warn.push("config: [mouse] is not a table".into());
        return;
    };
    for (key, v) in t {
        match key.as_str() {
            "copy_on_select" => match v.as_bool() {
                Some(on) => host.mouse.copy_on_select = on,
                None => warn.push("config: mouse.copy_on_select must be true or false".into()),
            },
            _ => warn.push(format!("config: unknown key mouse.{key}")),
        }
    }
}

/// `[keys]` — which command each key runs.
///
/// A bare key at the top of the table is global; a sub-table is a mode, so
/// `[keys.diff]` binds only where a diff is on screen. Both are the same call
/// into [`gitten_core::command::Keymap`], which is the only place that knows what
/// a mode is.
///
/// Three things it will not do, each because the alternative is a key that
/// silently does nothing:
///
/// - **An unknown command is refused**, validated against `host.commands` rather
///   than a list here — so a command an extension registered is bindable the day
///   it exists, and a typo is named.
/// - **An unparseable key is refused**, and the warning quotes it.
/// - **A prefix conflict is refused** by the keymap itself, with its own
///   message.
///
/// `"" ` unbinds: a shipped key has to be *removable*, not only movable, or
/// `j` can never mean nothing.
fn apply_keys(host: &mut Host, value: &toml::Value, warn: &mut Vec<String>) {
    let Some(t) = value.as_table() else {
        warn.push("config: [keys] is not a table".into());
        return;
    };
    for (key, v) in t {
        match v {
            toml::Value::Table(sub) => {
                for (chord, command) in sub {
                    bind_one(host, key, chord, command, warn);
                }
            }
            _ => bind_one(host, gitten_core::command::GLOBAL, key, v, warn),
        }
    }
}

fn bind_one(
    host: &mut Host,
    mode: &str,
    chord: &str,
    command: &toml::Value,
    warn: &mut Vec<String>,
) {
    let Some(name) = command.as_str() else {
        warn.push(format!("config: keys.{chord} must be a command name"));
        return;
    };
    if name.is_empty() {
        if !host.keys.unbind(mode, chord) {
            warn.push(format!(
                "config: nothing was bound to {chord:?} in [{mode}]"
            ));
        }
        return;
    }
    if !host.commands.known(name) {
        warn.push(format!("config: no such command {name:?} for {chord:?}"));
        return;
    }
    if let Err(e) = host.keys.bind(mode, chord, name) {
        warn.push(format!("config: {e}"));
    }
}

/// `[diff]` — which algorithm, how much context, which presentation.
///
/// `algorithm` is validated against the host's own registry rather than a list
/// here, so an extension that registers a fourth one is selectable from the file
/// the same day it exists and the error message names it. `layout` is not
/// validated at all for the same reason turned the other way: the registry of
/// presentations is the diff view's, `core` cannot see it, and the view reports
/// an unknown name when it opens.
fn apply_diff(host: &mut Host, value: &toml::Value, warn: &mut Vec<String>) {
    let Some(t) = value.as_table() else {
        warn.push("config: [diff] is not a table".into());
        return;
    };
    for (key, v) in t {
        match key.as_str() {
            "algorithm" => match v.as_str() {
                Some(name) if name == host.differ.selected() => {}
                Some(name) if host.differ.select(name) => {
                    warn.push("config: diff.algorithm applies on the next launch".into())
                }
                Some(name) => warn.push(format!(
                    "config: unknown diff.algorithm {name:?}; registered: {}",
                    host.differ.names().join(", ")
                )),
                None => warn.push("config: diff.algorithm must be a string".into()),
            },
            "context" => match v.as_integer() {
                // Zero is meaningful — just the changed lines — and past a few
                // dozen a hunk is the whole file, which the diff already shows.
                Some(n) if (0..=100).contains(&n) => {
                    if n as usize != host.differ.context {
                        host.differ.context = n as usize;
                        warn.push("config: diff.context applies on the next launch".into());
                    }
                }
                _ => warn.push("config: diff.context must be between 0 and 100".into()),
            },
            "whitespace" => match v.as_str().and_then(Whitespace::from_name) {
                Some(w) if w == host.differ.whitespace => {}
                Some(w) => {
                    host.differ.whitespace = w;
                    warn.push("config: diff.whitespace applies on the next launch".into());
                }
                None => warn.push(format!(
                    "config: diff.whitespace must be one of {}",
                    Whitespace::ALL
                        .iter()
                        .map(|w| w.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            },
            // A length, not a switch: the threshold is what keeps two matching
            // `}` lines out of it, and somebody tuning that wants the number.
            "moves" => match v.as_integer() {
                Some(n) if (0..=1000).contains(&n) => {
                    if n as usize != host.differ.min_moved {
                        host.differ.min_moved = n as usize;
                        warn.push("config: diff.moves applies on the next launch".into());
                    }
                }
                _ => warn.push("config: diff.moves must be between 0 (off) and 1000 lines".into()),
            },
            "indent_heuristic" => match v.as_bool() {
                Some(b) => {
                    if b != host.differ.indent_heuristic {
                        host.differ.indent_heuristic = b;
                        warn.push(
                            "config: diff.indent_heuristic applies on the next launch".into(),
                        );
                    }
                }
                None => warn.push("config: diff.indent_heuristic must be true or false".into()),
            },
            // Validated against the host's own registry, like `algorithm` and
            // unlike `layout`: the wraps live in `core`, so this layer can name
            // what is actually registered — including an extension's.
            "wrap" => match v.as_str() {
                Some(name) if name == host.wrap.selected() => {}
                Some(name) if host.wrap.select(name) => {
                    warn.push("config: diff.wrap applies on the next launch".into())
                }
                Some(name) => warn.push(format!(
                    "config: unknown diff.wrap {name:?}; registered: {}",
                    host.wrap.names().join(", ")
                )),
                None => warn.push("config: diff.wrap must be a string".into()),
            },
            "layout" => match v.as_str() {
                Some(name) if name == host.layout => {}
                Some(name) if !name.trim().is_empty() => {
                    host.layout = name.to_string();
                    warn.push("config: diff.layout applies on the next launch".into());
                }
                _ => warn.push("config: diff.layout must be a non-empty string".into()),
            },
            other => warn.push(format!("config: unknown key diff.{other}")),
        }
    }
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

/// `[theme]` — which palette, and whatever is changed on top of it.
///
/// Two things happen here that no other table does, and both follow from a theme
/// being the one seam whose implementation is *data*:
///
/// - **`name` is read first**, whatever order the file is in, because it selects
///   the palette every other key here modifies. TOML hands a table back
///   alphabetically, so a `name` applied in its turn would land after
///   `[theme.diff]` and silently throw it away.
/// - **The result is registered** under that name when it is done. A palette
///   somebody wrote in this file is a theme, so it belongs in the same registry —
///   and therefore the same picker — as the three that ship. Naming a built-in
///   *corrects* that entry rather than adding a second one called the same
///   thing, which is what makes picking it afterwards give back what the file
///   says rather than what the binary shipped.
fn apply_theme(host: &mut Host, value: &toml::Value, warn: &mut Vec<String>) {
    let Some(t) = value.as_table() else {
        warn.push("config: [theme] is not a table".into());
        return;
    };
    if let Some(v) = t.get("name") {
        match v.as_str() {
            Some(name) if host.select_theme(name) => {}
            Some(name) => {
                // Unknown is not automatically wrong: a table that also sets
                // colours is a theme being *defined*, and it is registered under
                // this name below. One that sets nothing else has named a theme
                // that does not exist, and then nothing in the file did anything
                // at all — which is the only case worth a line on stderr.
                if t.len() == 1 {
                    warn.push(format!(
                        "config: unknown theme {name:?} and [theme] defines none of \
                         its own colours; registered: {}",
                        host.themes.names().join(", ")
                    ));
                }
                host.theme.name = name.to_string();
            }
            None => warn.push("config: theme.name must be a string".into()),
        }
    }
    let theme = &mut host.theme;
    for (key, v) in t {
        match key.as_str() {
            // Read above, before anything it is the base for.
            "name" => {}
            "min_contrast" => match number(v) {
                // 1.0 is "no floor at all", 21.0 is black on white. Outside that
                // the contrast resolver has nothing to aim at.
                Some(n) if (1.0..=21.0).contains(&n) => theme.min_contrast = n,
                _ => warn.push("config: theme.min_contrast must be between 1 and 21".into()),
            },
            // The same range, and a separate number because it is a separate
            // job: this one is the floor for line numbers and hunk coordinates,
            // which are glanced at rather than read. See `Theme::min_furniture`.
            "min_furniture" => match number(v) {
                Some(n) if (1.0..=21.0).contains(&n) => theme.min_furniture = n,
                _ => warn.push("config: theme.min_furniture must be between 1 and 21".into()),
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
    // Required after touching fields directly: the resolved syntax-by-surface
    // table is what the render path reads.
    host.theme.rebuild();
    host.themes.register(host.theme.clone());
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
            None => warn.push(format!(
                "config: theme.{table}.{name} is not a #rrggbb colour"
            )),
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
                warn.push(format!(
                    "config: {what} has an entry that is not a #rrggbb colour"
                ));
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
    let t = t
        .strip_prefix('#')
        .or_else(|| t.strip_prefix("0x"))
        .unwrap_or(t);
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
/// What `gitten config` prints, so there is a complete and correct starting point
/// rather than a page of documentation to copy from. Generated from the same
/// field list that reads it back, and a test round-trips it.
pub fn dump(host: &Host) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str("# gitten — saved while the window is open and applied on the next frame.\n");
    out.push_str("# Colours are #rrggbb. Delete anything to fall back to the default.\n\n");

    let f = &host.font;
    out.push_str("[font]\n");
    out.push_str(&format!("family = {:?}\n", f.family));
    out.push_str(&format!("size = {:?}\n", f.size));
    out.push_str("# Both of these apply on the next launch, not on save.\n");
    out.push_str(&format!("monospaced = {}\n", f.monospaced));
    out.push_str(&format!("advance = {:?}\n\n", f.advance));

    out.push_str("# All of these apply on the next launch. The first five decide what the\n");
    out.push_str("# diff *is* and are read before a window exists; the last two are how it\n");
    out.push_str("# is drawn, and `s` and `w` change those live.\n");
    out.push_str("[diff]\n");
    out.push_str(&format!(
        "algorithm = {:?}    # {}\n",
        host.differ.selected(),
        host.differ.names().join(", ")
    ));
    out.push_str(&format!("context = {}\n", host.differ.context));
    out.push_str(&format!(
        "whitespace = {:?}    # {}\n",
        host.differ.whitespace.name(),
        Whitespace::ALL
            .iter()
            .map(|w| w.name())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out.push_str(&format!(
        "moves = {}            # shortest block reported as moved; 0 is off\n",
        host.differ.min_moved
    ));
    out.push_str(&format!(
        "indent_heuristic = {}  # slide each change to a readable boundary, as git does\n",
        host.differ.indent_heuristic
    ));
    out.push_str(&format!("layout = {:?}    # unified, split\n", host.layout));
    out.push_str(&format!(
        "wrap = {:?}        # {} — `w` cycles it\n\n",
        host.wrap.selected(),
        host.wrap.names().join(", ")
    ));

    out.push_str("# How far a scroll goes. `scroll` is rows per notch of the wheel and per\n");
    out.push_str("# `ctrl-e`/`ctrl-y` — one, because a terminal already reports the wheel once\n");
    out.push_str("# per line of however fast the platform says you scrolled. `scrolloff` is the\n");
    out.push_str("# lead the cursor keeps at the edge, and 0 lets it reach the last row.\n");
    out.push_str("# `sidebar` is the left stack's slice of the window's width; the divider\n");
    out.push_str("# drag adjusts it for the session, and the file is never written back.\n");
    out.push_str(
        "# `scrollbar` draws one beside a list too long to fit, and nothing when it fits.\n",
    );
    out.push_str("[view]\n");
    out.push_str(&format!("scroll = {}\n", host.view.rows));
    out.push_str(&format!("scrolloff = {}\n", host.view.scrolloff));
    out.push_str(&format!("scrollbar = {}\n", host.view.scrollbar));
    out.push_str(&format!("sidebar = {}\n\n", host.sidebar_share));

    out.push_str("# In the terminal, finishing a drag puts it on the clipboard, the way that\n");
    out.push_str("# terminal's own selection would — gitten took the drag, so it owes you the\n");
    out.push_str("# copy. A click is a cursor move and never copies; `y` copies either way. The\n");
    out.push_str("# window has the platform's own cmd-c and ignores this.\n");
    out.push_str("[mouse]\n");
    out.push_str(&format!(
        "copy_on_select = {}\n\n",
        host.mouse.copy_on_select
    ));

    let t = &host.theme;
    out.push_str("# `name` picks one of the registered palettes and everything below is applied\n");
    out.push_str("# on top of it. The result is registered under that name, so a theme edited\n");
    out.push_str("# here is the one the picker shows — and a name nobody registered is a new\n");
    out.push_str("# theme rather than a mistake.\n");
    out.push_str("[theme]\n");
    out.push_str(&format!(
        "name = {:?}    # {} — `T` cycles it\n",
        t.name,
        host.themes.names().join(", ")
    ));
    out.push_str(&format!("min_contrast = {:?}\n", t.min_contrast));
    out.push_str(&format!("min_furniture = {:?}\n", t.min_furniture));
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

    // Every binding, grouped by mode, global first. Written out in full rather
    // than as a diff against the built-ins: a file that only lists what you
    // changed cannot be read to find out what a key does, and `gitten config`
    // exists to be read.
    out.push_str(
        "\n# Which command each key runs. A key at the top level is global; a key under\n",
    );
    out.push_str("# [keys.<mode>] applies only there and overrides the global one. Set a key to\n");
    out.push_str("# \"\" to unbind it. Commands:\n");
    for c in host.commands.all() {
        out.push_str(&format!("#   {:<20} {}\n", c.name, c.doc));
    }
    let mut modes: Vec<&str> = vec![gitten_core::command::GLOBAL];
    for b in host.keys.bindings() {
        if !modes.contains(&b.mode.as_str()) {
            modes.push(&b.mode);
        }
    }
    for mode in modes {
        let header = match mode == gitten_core::command::GLOBAL {
            true => "[keys]".to_string(),
            false => format!("[keys.{mode}]"),
        };
        out.push_str(&format!("\n{header}\n"));
        for b in host.keys.bindings().iter().filter(|b| b.mode == mode) {
            out.push_str(&format!(
                "{:?} = {:?}\n",
                gitten_core::command::chord_string(&b.chord),
                b.command
            ));
        }
    }
    out
}

fn hex(c: Rgb) -> String {
    format!("#{c:06x}")
}

fn hex_list(cs: &[Rgb]) -> String {
    cs.iter()
        .map(|c| format!("\"{}\"", hex(*c)))
        .collect::<Vec<_>>()
        .join(", ")
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
///
/// The **nearest existing** ancestor, not the file's immediate parent. The
/// per-user config lives at `~/.config/gitten/gitten.toml`, and a user who has
/// never written one has no `~/.config/gitten` to watch — watching it would
/// error, and every client turns that into a "config reload is off" line on a
/// launch where nothing was wrong. Walking up to the first directory that exists
/// (`~/.config`, or `~`, or `/` in the limit) always succeeds; the filename
/// filter below is what keeps it from firing on anything but our file, exactly as
/// before. A config created after launch is then picked up on the next start
/// rather than live, which is the right trade for never having watched a config
/// most people will not write.
pub fn watch(
    path: &Path,
    mut on_change: impl FnMut() + Send + 'static,
) -> notify::Result<Box<dyn notify::Watcher + Send>> {
    use notify::{EventKind, RecursiveMode, Watcher};

    let file = path.file_name().map(|f| f.to_owned());
    let dir = nearest_existing_dir(path);

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
    watcher.watch(&dir, RecursiveMode::NonRecursive)?;
    Ok(Box::new(watcher))
}

/// The first directory at or above `path`'s parent that exists on disk, so a
/// watch of it cannot fail for a config whose directory has not been created.
/// `.` for a bare filename, and `/` (or the root that exists) in the limit —
/// which always exists, so this returns a real directory in every case.
fn nearest_existing_dir(path: &Path) -> PathBuf {
    let mut dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    while let Some(d) = dir {
        if d.is_dir() {
            return d.to_path_buf();
        }
        dir = d.parent();
    }
    PathBuf::from(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitten_core::theme::Surface;

    fn host() -> Host {
        Host::new()
    }

    #[test]
    fn config_resolution_is_most_specific_first() {
        let p = |s: &str| PathBuf::from(s);
        let home = Some(p("/home/u"));
        let xdg = Some(p("/xdg"));

        // An explicit path wins over everything, even a present cwd file.
        assert_eq!(
            resolve(Some(p("/tmp/x.toml")), true, xdg.clone(), home.clone()),
            p("/tmp/x.toml")
        );
        // A cwd file is the override when it exists — the dev loop.
        assert_eq!(
            resolve(None, true, xdg.clone(), home.clone()),
            p("gitten.toml")
        );
        // Otherwise XDG, and gitten gets its own subdirectory.
        assert_eq!(
            resolve(None, false, xdg.clone(), home.clone()),
            p("/xdg/gitten/gitten.toml")
        );
        // No XDG: ~/.config.
        assert_eq!(
            resolve(None, false, None, home.clone()),
            p("/home/u/.config/gitten/gitten.toml")
        );
        // A relative XDG_CONFIG_HOME is invalid per the spec and ignored.
        assert_eq!(
            resolve(None, false, Some(p("relative/xdg")), home),
            p("/home/u/.config/gitten/gitten.toml")
        );
        // Nothing to hang it off: the cwd name, so a stripped environment still
        // gets a usable default rather than a panic.
        assert_eq!(resolve(None, false, None, None), p("gitten.toml"));
    }

    #[test]
    fn a_watch_dir_is_the_nearest_ancestor_that_exists() {
        let root = std::env::temp_dir().join(format!("gitten-watchdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // The immediate parent exists: watch it.
        let here = root.join("gitten.toml");
        assert_eq!(nearest_existing_dir(&here), root);

        // The parent does not exist yet (a config whose ~/.config/gitten has
        // never been created): walk up to the first that does.
        let deep = root.join("gitten").join("gitten.toml");
        assert_eq!(nearest_existing_dir(&deep), root);

        // A bare filename has no ancestor to speak of: the current directory.
        assert_eq!(
            nearest_existing_dir(Path::new("gitten.toml")),
            PathBuf::from(".")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_or_missing_config_changes_nothing() {
        let mut h = host();
        let before = (h.theme.clone(), h.font.clone());
        assert!(apply(&mut h, "").is_empty());
        assert_eq!((h.theme.clone(), h.font.clone()), before);
        // And a file that does not exist is not an error.
        assert!(load(&mut h, Path::new("/nonexistent/gitten.toml")).is_empty());
    }

    #[test]
    fn a_key_reaches_the_keymap_in_the_mode_it_was_written_in() {
        use gitten_core::command::{Modes, Resolve};
        let mut h = host();
        let warn = apply(
            &mut h,
            "[keys]\n\"x\" = \"quit\"\n\n[keys.diff]\n\"x\" = \"diff.cycle-wrap\"\n",
        );
        assert!(warn.is_empty(), "{warn:?}");
        let chord = [gitten_core::command::Key::char('x')];
        assert_eq!(h.keys.resolve(&Modes::new(), &chord), Resolve::Run("quit"));
        let mut diff = Modes::new();
        diff.push("diff");
        assert_eq!(
            h.keys.resolve(&diff, &chord),
            Resolve::Run("diff.cycle-wrap")
        );
    }

    #[test]
    fn a_key_can_be_unbound_and_not_only_moved() {
        use gitten_core::command::{Key, Modes, Resolve};
        let mut h = host();
        assert!(apply(&mut h, "[keys]\n\"j\" = \"\"\n").is_empty());
        assert_eq!(
            h.keys.resolve(&Modes::new(), &[Key::char('j')]),
            Resolve::None
        );
        // Unbinding what was never bound is worth saying: it is almost always a
        // typo in the key rather than a request.
        let warn = apply(&mut h, "[keys]\n\"j\" = \"\"\n");
        assert!(warn[0].contains("nothing was bound"), "{warn:?}");
    }

    #[test]
    fn a_binding_the_keymap_cannot_hold_warns_and_changes_nothing() {
        use gitten_core::command::{Key, Modes, Resolve};
        let mut h = host();
        // As a string, because a `Resolve` borrows the map it came from.
        let before = match h.keys.resolve(&Modes::new(), &[Key::char('j')]) {
            Resolve::Run(c) => c.to_string(),
            _ => String::new(),
        };
        let warn = apply(
            &mut h,
            "[keys]\n\"j\" = \"nope.nothing\"\n\"nonsense\" = \"quit\"\n\"g g\" = \"quit\"\n",
        );
        assert_eq!(warn.len(), 3, "{warn:?}");
        assert!(
            warn.iter().any(|w| w.contains("no such command")),
            "{warn:?}"
        );
        assert!(warn.iter().any(|w| w.contains("not a key")), "{warn:?}");
        assert!(warn.iter().any(|w| w.contains("prefix")), "{warn:?}");
        assert_eq!(
            h.keys.resolve(&Modes::new(), &[Key::char('j')]),
            Resolve::Run(&before)
        );
    }

    #[test]
    fn a_command_an_extension_registered_is_bindable_from_the_file() {
        // The reason commands are validated against the host rather than a list
        // in here: the registry is the thing that knows what exists.
        use gitten_core::command::{Key, Modes, Resolve};
        let mut h = host();
        h.commands
            .register("blame.toggle", "show blame beside the diff");
        assert!(apply(&mut h, "[keys]\n\"b\" = \"blame.toggle\"\n").is_empty());
        assert_eq!(
            h.keys.resolve(&Modes::new(), &[Key::char('b')]),
            Resolve::Run("blame.toggle")
        );
    }

    #[test]
    fn the_whole_keymap_survives_a_round_trip_through_the_file() {
        // `dump` writes every binding and `apply` reads them all back, so a
        // settings panel can rewrite the file without losing a key.
        let mut h = host();
        h.keys.bind("diff", "b", "diff.cycle-wrap").unwrap();
        h.keys.unbind("global", "j");
        let text = dump(&h);
        let mut back = Host::new();
        let warn = apply(&mut back, &text);
        assert!(warn.is_empty(), "{warn:?}");
        assert!(back
            .keys
            .keys_for("diff.cycle-wrap")
            .contains(&"b".to_string()));
        // `apply` starts from the built-ins, so an unbind is the one thing a
        // round trip cannot carry: the file says what *is* bound, and `j` is
        // absent from it rather than present as a removal.
        assert_eq!(back.keys.bindings().len(), h.keys.bindings().len() + 1);
    }

    #[test]
    fn a_colour_reaches_the_theme() {
        let mut h = host();
        let warn = apply(&mut h, "[theme.diff]\nadded_bg = \"#123456\"\n");
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(h.theme.diff.added_bg, 0x123456);
    }

    #[test]
    fn naming_a_theme_selects_the_registered_one() {
        let mut h = host();
        let warn = apply(&mut h, "[theme]\nname = \"light\"\n");
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(
            h.theme.chrome.bg,
            gitten_core::theme::Theme::light().chrome.bg
        );
        assert_eq!(h.theme.name, "light");
    }

    #[test]
    fn a_colour_lands_on_top_of_the_theme_the_file_named() {
        // The trap this exists for: TOML hands a table back in alphabetical
        // order, so `name` arrives *after* `[theme.diff]` and applying it in its
        // turn would replace the palette that had just been edited. Written here
        // in the order that fails.
        let mut h = host();
        let text = "[theme]\nname = \"light\"\n\n[theme.diff]\nadded_bg = \"#123456\"\n";
        let warn = apply(&mut h, text);
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(
            h.theme.diff.added_bg, 0x123456,
            "the theme overwrote the colour"
        );
        assert_eq!(
            h.theme.diff.removed_bg,
            gitten_core::theme::Theme::light().diff.removed_bg
        );
    }

    #[test]
    fn a_theme_written_in_the_file_is_registered_under_its_name() {
        // Which is what puts it in the picker beside the shipped three: the
        // frontend lists a registry, so a palette somebody wrote by hand has to
        // be *in* one to be reachable at all.
        let mut h = host();
        let text = "[theme]\nname = \"solarized-ish\"\n\n[theme.diff]\nadded_bg = \"#073642\"\n";
        let warn = apply(&mut h, text);
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(
            h.themes.names(),
            vec!["dark", "light", "slate", "solarized-ish"]
        );
        assert_eq!(
            h.themes.get("solarized-ish").map(|t| t.diff.added_bg),
            Some(0x073642)
        );
        // And selecting it again gives back what the file said, not the built-in
        // it was based on.
        h.select_theme("dark");
        assert!(h.select_theme("solarized-ish"));
        assert_eq!(h.theme.diff.added_bg, 0x073642);
    }

    #[test]
    fn a_file_that_edits_a_built_in_corrects_it_rather_than_cloning_it() {
        let mut h = host();
        let text = "[theme]\nname = \"slate\"\n\n[theme.chrome]\naccent = \"#ff0000\"\n";
        let warn = apply(&mut h, text);
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(
            h.themes.names(),
            vec!["dark", "light", "slate"],
            "a fourth entry appeared"
        );
        assert_eq!(
            h.themes.get("slate").map(|t| t.chrome.accent),
            Some(0xff0000)
        );
        // The other two are untouched, which is the whole reason a pick can be
        // trusted: `gitten config` dumps every colour of the theme you are on,
        // and that must not repaint the ones you are not.
        assert_eq!(h.themes.get("light").map(|t| t.chrome.bg), Some(0xfaf7f1));
    }

    #[test]
    fn a_theme_that_names_nothing_and_defines_nothing_says_so() {
        // The only shape worth a warning: a `[theme]` table holding one unknown
        // name changed nothing at all, and the usual cause is a typo. A table
        // that also sets colours is a definition and is registered instead.
        let mut h = host();
        let warn = apply(&mut h, "[theme]\nname = \"ligth\"\n");
        assert_eq!(warn.len(), 1, "{warn:?}");
        assert!(warn[0].contains("unknown theme"), "{warn:?}");
        assert!(
            warn[0].contains("light"),
            "it should name what is registered: {warn:?}"
        );
        let mut h = host();
        let text = "[theme]\nname = \"ligth\"\n\n[theme.chrome]\naccent = \"#ff0000\"\n";
        assert!(apply(&mut h, text).is_empty(), "a definition is not a typo");
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
        let warn = apply(
            &mut h,
            "[theme.syntax]\ncomment = \"#ff0000 bold italic\"\n",
        );
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
    fn the_diff_table_reaches_the_host() {
        let mut h = host();
        let warn = apply(
            &mut h,
            "[diff]\nalgorithm = \"myers\"\ncontext = 7\nlayout = \"split\"\n\
             whitespace = \"all\"\nmoves = 0\nindent_heuristic = false\n",
        );
        assert!(warn.iter().all(|w| w.contains("next launch")), "{warn:?}");
        assert_eq!(h.differ.selected(), "myers");
        assert_eq!(h.differ.context, 7);
        assert_eq!(h.layout, "split");
        assert_eq!(h.differ.whitespace, Whitespace::All);
        assert_eq!(h.differ.min_moved, 0);
        assert!(!h.differ.indent_heuristic);
    }

    #[test]
    fn an_unknown_algorithm_names_the_ones_that_exist() {
        // A list written out here would go stale the day an extension registers
        // one, so the message comes from the registry itself.
        let mut h = host();
        let warn = apply(&mut h, "[diff]\nalgorithm = \"patients\"\n");
        assert_eq!(
            h.differ.selected(),
            "histogram",
            "a typo must not change the algorithm"
        );
        assert_eq!(warn.len(), 1, "{warn:?}");
        assert!(
            warn[0].contains("histogram") && warn[0].contains("myers"),
            "{warn:?}"
        );
    }

    #[test]
    fn a_registered_algorithm_is_selectable_from_the_file() {
        // Rule 1, as a test: an extension's differ has to be reachable from the
        // config the same way a built-in's is.
        use gitten_core::differ::{Differ, Edit};
        struct Semantic;
        impl Differ for Semantic {
            fn name(&self) -> &'static str {
                "semantic"
            }
            fn diff(
                &self,
                _: &str,
                _: &[std::sync::Arc<str>],
                _: &[std::sync::Arc<str>],
            ) -> Vec<Edit> {
                Vec::new()
            }
        }
        let mut h = host();
        h.differ.register(Semantic);
        let warn = apply(&mut h, "[diff]\nalgorithm = \"semantic\"\n");
        assert!(warn.iter().all(|w| w.contains("next launch")), "{warn:?}");
        assert_eq!(h.differ.selected(), "semantic");
    }

    #[test]
    fn a_registered_wrap_is_selectable_from_the_file() {
        // The same test as the differ's, for the same reason: an extension's wrap
        // has to be reachable from the file the day it is registered, and the
        // error message has to name it.
        use gitten_core::wrap::{Break, Wrap};
        struct Sentence;
        impl Wrap for Sentence {
            fn name(&self) -> &'static str {
                "sentence"
            }
            fn breaks(&self, _: &str, _: usize, _: &mut Vec<Break>) {}
        }
        let mut h = host();
        h.wrap.register(Sentence);
        let warn = apply(&mut h, "[diff]\nwrap = \"sentence\"\n");
        assert!(warn.iter().all(|w| w.contains("next launch")), "{warn:?}");
        assert_eq!(h.wrap.selected(), "sentence");

        // And a typo leaves it alone and says what is available.
        let warn = apply(&mut h, "[diff]\nwrap = \"wrod\"\n");
        assert_eq!(h.wrap.selected(), "sentence");
        assert_eq!(warn.len(), 1, "{warn:?}");
        assert!(
            warn[0].contains("word") && warn[0].contains("sentence"),
            "{warn:?}"
        );
    }

    #[test]
    fn nonsense_in_the_diff_table_is_refused() {
        let mut h = host();
        let warn = apply(
            &mut h,
            "[diff]\ncontext = -1\nlayout = \"\"\nwobble = 1\n\
             whitespace = \"sometimes\"\nmoves = -3\nindent_heuristic = \"yes\"\n",
        );
        assert_eq!(h.differ.context, 3);
        assert_eq!(h.layout, "unified");
        assert_eq!(h.differ.whitespace, Whitespace::Exact);
        assert_eq!(h.differ.min_moved, 3);
        assert!(h.differ.indent_heuristic);
        assert_eq!(warn.len(), 6, "{warn:?}");
        // The message names the options rather than restating them here, so it
        // cannot go stale.
        assert!(warn.iter().any(|w| w.contains("trailing")), "{warn:?}");
    }

    #[test]
    fn the_fields_that_need_a_relaunch_say_so() {
        let mut h = host();
        let warn = apply(&mut h, "[font]\nmonospaced = false\n");
        assert!(!h.font.monospaced, "it was still applied");
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
        let warn = apply(
            &mut h,
            "[nope]\nx = 1\n[theme.diff]\nnot_a_colour = \"#111111\"\n",
        );
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
        assert!(
            warn.is_empty(),
            "round-tripping the defaults warned: {warn:?}"
        );
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
        original.font = Font {
            family: "Iosevka".into(),
            size: 15.0,
            monospaced: true,
            advance: 0.5,
        };
        original.differ.select("patience");
        original.differ.context = 5;
        original.differ.whitespace = Whitespace::Change;
        original.differ.min_moved = 8;
        original.differ.indent_heuristic = false;
        original.layout = "split".into();
        original.wrap.select("char");
        original.view.rows = 4;
        original.view.scrolloff = 0;
        original.view.scrollbar = false;
        original.sidebar_share = 0.44;
        original.mouse.copy_on_select = false;
        original.theme.rebuild();

        let text = dump(&original);
        let mut restored = Host::new();
        let warn = apply(&mut restored, &text);
        assert!(
            warn.iter().all(|w| w.contains("next launch")),
            "dump produced a file with real warnings: {warn:?}\n{text}"
        );
        assert_eq!(
            restored.theme, original.theme,
            "theme did not survive:\n{text}"
        );
        assert_eq!(restored.font, original.font, "font did not survive");
        assert_eq!(
            restored.differ.selected(),
            "patience",
            "diff.algorithm did not survive"
        );
        assert_eq!(restored.differ.context, 5, "diff.context did not survive");
        assert_eq!(restored.differ.whitespace, Whitespace::Change);
        assert_eq!(restored.differ.min_moved, 8);
        assert!(!restored.differ.indent_heuristic);
        assert_eq!(restored.layout, "split", "diff.layout did not survive");
        assert_eq!(
            restored.wrap.selected(),
            "char",
            "diff.wrap did not survive"
        );
        assert_eq!(restored.view.rows, 4, "view.scroll did not survive");
        assert_eq!(restored.view.scrolloff, 0, "view.scrolloff did not survive");
        assert!(!restored.view.scrollbar, "view.scrollbar did not survive");
        assert_eq!(restored.sidebar_share, 0.44, "view.sidebar did not survive");
        assert!(
            !restored.mouse.copy_on_select,
            "mouse.copy_on_select did not survive"
        );
    }

    #[test]
    fn the_scroll_step_is_data_and_a_bad_one_is_named() {
        let mut h = host();
        assert!(apply(&mut h, "[view]\nscroll = 3\nscrolloff = 0\n").is_empty());
        assert_eq!(h.view.rows, 3);
        // Zero lead is a cursor that reaches the last row, and is allowed.
        assert_eq!(h.view.scrolloff, 0);
        // A wheel that moves nothing is `"wheeldown" = ""` in [keys], not a zero
        // here — so this is a mistake and is said so.
        let warn = apply(&mut h, "[view]\nscroll = 0\n");
        assert_eq!(warn.len(), 1, "{warn:?}");
        assert!(warn[0].contains("view.scroll"), "{warn:?}");
        assert_eq!(h.view.rows, 3, "a rejected value left the old one alone");
        assert!(apply(&mut h, "[view]\nscrollbar = false\n").is_empty());
        assert!(!h.view.scrollbar);
        let warn = apply(&mut h, "[view]\nscrollbar = 3\n");
        assert!(warn[0].contains("view.scrollbar"), "{warn:?}");
        assert!(apply(&mut h, "[view]\nsidebar = 0.4\n").is_empty());
        assert_eq!(h.sidebar_share, 0.4);
        // A share out of the band is refused and named, not silently clamped
        // into a number nobody chose.
        let warn = apply(&mut h, "[view]\nsidebar = 0.9\n");
        assert_eq!(warn.len(), 1, "{warn:?}");
        assert!(warn[0].contains("view.sidebar"), "{warn:?}");
        assert_eq!(
            h.sidebar_share, 0.4,
            "a rejected value left the old one alone"
        );
        let warn = apply(&mut h, "[view]\nsidebar = \"wide\"\n");
        assert!(warn[0].contains("view.sidebar"), "{warn:?}");
        assert!(apply(&mut h, "[mouse]\ncopy_on_select = false\n").is_empty());
        assert!(!h.mouse.copy_on_select);
        let warn = apply(&mut h, "[mouse]\ncopy_on_select = \"yes\"\n");
        assert!(warn[0].contains("mouse.copy_on_select"), "{warn:?}");
        assert!(
            !h.mouse.copy_on_select,
            "a rejected value left the old one alone"
        );
        let warn = apply(&mut h, "[mouse]\nspeed = 3\n");
        assert!(warn[0].contains("unknown key mouse.speed"), "{warn:?}");
        let warn = apply(&mut h, "[view]\nspeed = 3\n");
        assert!(warn[0].contains("unknown key view.speed"), "{warn:?}");
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
            assert!(
                text.contains(&format!("{name} = ")),
                "{name} missing from dump"
            );
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
        assert!(
            style("#ffffff wobbly").is_none(),
            "accepted an unknown word"
        );
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
