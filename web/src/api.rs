//! What crosses the wire.
//!
//! Two payloads, split by how often they change. `meta` is the theme, the font,
//! the file list and the row count — everything a repaint needs and a scroll
//! does not, fetched once and again on reflow. `rows` is a window of the diff,
//! fetched constantly and therefore carrying nothing derivable from `meta`.
//!
//! The colours are resolved here rather than named, because
//! [`Theme::syntax_on`](gitten_core::theme::Theme::syntax_on) is where the
//! contrast floor is applied and a client that picked its own colours would
//! quietly not have one.

use crate::json::*;
use crate::log::Log;
use crate::rows::Doc;
use gitten_core::command::{HelpRow, Modes};
use gitten_core::graph::{Draw, MAX_LANES};
use gitten_core::host::Host;
use gitten_core::rows::Row;
use gitten_core::runs::{runs, Run};
use gitten_core::syntax::Kind;
use gitten_core::theme::{Surface, Theme};
use gitten_core::view::Viewport;
use gitten_core::LineKind;

/// The name a client uses for a syntax class.
///
/// A match and not an array indexed by [`Kind::index`], so that adding a class
/// to `core` is a compile error in here rather than a stylesheet that silently
/// colours strings like comments.
fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Comment => "comment",
        Kind::Str => "string",
        Kind::Number => "number",
        Kind::Keyword => "keyword",
        Kind::Type => "type",
        Kind::Constant => "constant",
        Kind::Func => "func",
        Kind::Property => "property",
        Kind::Heading => "heading",
        Kind::Strong => "strong",
        Kind::Emphasis => "emphasis",
        Kind::Link => "link",
    }
}

/// Same contract as [`kind_name`], and the same reason.
fn surface_name(s: Surface) -> &'static str {
    match s {
        Surface::Context => "context",
        Surface::Added => "added",
        Surface::Removed => "removed",
        Surface::AddedWord => "addedWord",
        Surface::RemovedWord => "removedWord",
        Surface::MovedRemoved => "movedRemoved",
        Surface::MovedAdded => "movedAdded",
        Surface::Selected => "selected",
        Surface::Cursor => "cursor",
        Surface::Title => "title",
        Surface::Status => "status",
    }
}

fn line_kind_name(k: LineKind) -> &'static str {
    match k {
        LineKind::Context => "context",
        LineKind::Added => "added",
        LineKind::Removed => "removed",
    }
}

/// The face, for both views. A client measures its own advance from the font
/// the browser actually resolved — see `app.js` — and takes the rest of it.
fn font(out: &mut String, host: &Host) {
    object(out, |o, f| {
        field_str(o, f, "family", &host.font.family);
        field_num(o, f, "size", host.font.size);
        field_bool(o, f, "monospaced", host.font.monospaced);
        field_num(o, f, "advance", host.font.advance);
        field_num(o, f, "charWidth", host.font.char_width());
    });
}

fn theme(out: &mut String, t: &Theme) {
    object(out, |o, f| {
        field_str(o, f, "name", &t.name);
        field_num(o, f, "minContrast", t.min_contrast);

        key(o, f, "diff");
        let d = &t.diff;
        object(o, |o, f| {
            field_rgb(o, f, "fileBg", d.file_bg);
            field_rgb(o, f, "fileFg", d.file_fg);
            field_rgb(o, f, "addsFg", d.adds_fg);
            field_rgb(o, f, "delsFg", d.dels_fg);
            field_rgb(o, f, "hunkBg", d.hunk_bg);
            field_rgb(o, f, "hunkFg", d.hunk_fg);
            field_rgb(o, f, "gutterFg", d.gutter_fg);
            field_rgb(o, f, "contextBg", d.context_bg);
            field_rgb(o, f, "contextFg", d.context_fg);
            field_rgb(o, f, "addedBg", d.added_bg);
            field_rgb(o, f, "addedFg", d.added_fg);
            field_rgb(o, f, "addedWordBg", d.added_word_bg);
            field_rgb(o, f, "removedBg", d.removed_bg);
            field_rgb(o, f, "removedFg", d.removed_fg);
            field_rgb(o, f, "removedWordBg", d.removed_word_bg);
            field_rgb(o, f, "movedRemovedBg", d.moved_removed_bg);
            field_rgb(o, f, "movedAddedBg", d.moved_added_bg);
            field_rgb(o, f, "absentBg", d.absent_bg);
        });

        key(o, f, "chrome");
        let c = &t.chrome;
        object(o, |o, f| {
            field_rgb(o, f, "bg", c.bg);
            field_rgb(o, f, "fg", c.fg);
            field_rgb(o, f, "dim", c.dim);
            field_rgb(o, f, "faint", c.faint);
            field_rgb(o, f, "accent", c.accent);
            field_rgb(o, f, "titleBg", c.title_bg);
            field_rgb(o, f, "statusBg", c.status_bg);
            field_rgb(o, f, "selectionBg", c.selection_bg);
            field_rgb(o, f, "selectedBg", c.selected_bg);
            field_rgb(o, f, "error", c.error);
        });

        key(o, f, "markdown");
        let m = &t.markdown;
        object(o, |o, f| {
            field_rgb(o, f, "codeBar", m.code_bar);
            field_rgb(o, f, "quoteBar", m.quote_bar);
            field_rgb(o, f, "marker", m.marker);
            field_rgb(o, f, "rule", m.rule);
        });

        key(o, f, "lanes");
        rgb_list(o, &t.lanes);
        field_rgb(o, f, "laneOverflow", t.lane_overflow);
        key(o, f, "authors");
        rgb_list(o, &t.authors);

        // Every syntax class resolved against every background it can land on:
        // 8 surfaces by 12 classes, computed once here because the contrast
        // resolution is a handful of `powf` and the client would otherwise be
        // doing it per visible row per frame — which is the reason `Theme`
        // caches it in the first place.
        key(o, f, "syntax");
        object(o, |o, f| {
            for s in Surface::ALL {
                key(o, f, surface_name(s));
                object(o, |o, f| {
                    for k in Kind::ALL {
                        key(o, f, kind_name(k));
                        let st = t.syntax_on(k, s);
                        object(o, |o, f| {
                            field_rgb(o, f, "fg", st.fg);
                            field_bool(o, f, "bold", st.bold);
                            field_bool(o, f, "italic", st.italic);
                        });
                    }
                });
            }
        });

        key(o, f, "background");
        object(o, |o, f| {
            for s in Surface::ALL {
                field_rgb(o, f, surface_name(s), t.background(s));
            }
        });
    });
}

/// Everything a repaint needs and a scroll does not.
pub fn meta(out: &mut String, doc: &Doc, host: &Host, label: &str) {
    object(out, |o, f| {
        field_str(o, f, "label", label);
        field_str(o, f, "kind", "diff");
        field_str(o, f, "layout", &host.layout);
        field_num(o, f, "rows", doc.total());
        field_num(o, f, "lines", doc.rows().len());
        field_num(o, f, "moved", doc.moved());
        field_num(o, f, "intralineMs", doc.intraline.as_secs_f64() * 1000.0);
        field_num(o, f, "syntaxMs", doc.syntax.as_secs_f64() * 1000.0);

        key(o, f, "wrap");
        object(o, |o, f| {
            key(o, f, "names");
            list(o, host.wrap.names(), string);
            field_str(o, f, "selected", doc.wrap_name());
            field_num(o, f, "cols", doc.cols());
            // Surfaced, never swallowed: an extension's wrap whose breaks were
            // all thrown away looks exactly like one with nothing to do.
            field_num(o, f, "rejected", doc.rejected());
        });

        key(o, f, "font");
        font(o, host);

        // Served in full, read only for its length.
        //
        // The client counts these for the status line and looks at nothing else
        // in them. The paths, the counts and the rows are here for a jump list
        // or a finder, and neither is built — which is worth knowing before
        // trusting the shape: on a 1375-file diff this array is 109 KB of a
        // 111 KB payload, so it is the whole cost of `meta` and currently buys
        // one integer.
        key(o, f, "files");
        list(o, doc.files(), |o, e| {
            object(o, |o, f| {
                field_str(o, f, "path", &e.path);
                field_num(o, f, "adds", e.adds);
                field_num(o, f, "dels", e.dels);
                // The visual row, not the logical one: this is what a jump
                // scrolls to, and after a reflow they are different numbers.
                field_num(o, f, "row", doc.visual(e.row));
            });
        });

        key(o, f, "theme");
        theme(o, &host.theme);
    });
}

/// One run of a line, as the client draws it.
///
/// The text is sliced here and not handed over as byte offsets, on purpose.
/// [`Run::at`] is in bytes and a JavaScript string is UTF-16, so offsets mean
/// every consumer converts and the one that forgets breaks on exactly the lines
/// a diff of anything non-English is made of. Slicing also means the browser
/// does a `for` over pieces instead of a sweep per row per frame.
///
/// The `surface` a [`Run`] carries is dropped: the row already said its kind
/// and `w` says which words changed, so the client resolves the background from
/// the two — the one thing a frontend still owns after
/// [`runs`](gitten_core::runs).
fn piece(out: &mut String, (text, r): (&str, &Run)) {
    object(out, |o, f| {
        field_str(o, f, "t", text);
        if let Some(k) = r.kind {
            field_str(o, f, "k", kind_name(k));
        }
        if r.word {
            field_bool(o, f, "w", true);
        }
    });
}

/// A window of the diff, in visual rows.
///
/// `from` and `count` address rows *after* wrapping, because that is the space
/// a scrollbar lives in. A row past the end is not an error — the client can ask
/// while a reflow is in flight — it just ends the array early, and `from` in the
/// reply is what lets the client notice.
pub fn rows(out: &mut String, doc: &Doc, from: usize, count: usize) {
    let mut scratch = Vec::new();
    object(out, |o, f| {
        field_num(o, f, "from", from);
        field_num(o, f, "total", doc.total());
        key(o, f, "rows");
        o.push('[');
        let mut first = true;
        for v in from..from.saturating_add(count) {
            let Some((i, seg)) = doc.at(v) else { break };
            if !first {
                o.push(',');
            }
            first = false;
            match &doc.rows()[i] {
                Row::File { path, adds, dels } => object(o, |o, f| {
                    field_str(o, f, "type", "file");
                    field_str(o, f, "path", path);
                    field_num(o, f, "adds", adds);
                    field_num(o, f, "dels", dels);
                }),
                Row::Hunk(h) => object(o, |o, f| {
                    field_str(o, f, "type", "hunk");
                    field_str(o, f, "header", h);
                }),
                Row::Line(l) => {
                    // Line coordinates in, line coordinates out: tokens and
                    // spans belong to the line and not to one of the rows it
                    // wrapped onto, so `at` clips them and `Run::at` indexes
                    // the same string this slices.
                    let at = doc.range(i, seg);
                    runs(at, &l.tokens, &l.spans, l.kind, l.moved, &mut scratch);
                    object(o, |o, f| {
                        field_str(o, f, "type", "line");
                        field_str(o, f, "kind", line_kind_name(l.kind));
                        if l.moved {
                            field_bool(o, f, "moved", true);
                        }
                        // A continuation row carries no number and no sign: the
                        // background says which line it belongs to, and an empty
                        // gutter says it is not a line of its own.
                        if seg > 0 {
                            field_bool(o, f, "cont", true);
                        } else {
                            if let Some(n) = l.old_no {
                                field_num(o, f, "old", n);
                            }
                            if let Some(n) = l.new_no {
                                field_num(o, f, "new", n);
                            }
                        }
                        key(o, f, "x");
                        list(o, scratch.iter().map(|r| (&l.text[r.at.clone()], r)), piece);
                    });
                }
            }
        }
        o.push(']');
    });
}

/// The commit graph. Lanes come from
/// [`assign_lanes`](gitten_core::assign_lanes) — the geometry is `core`'s, and
/// what a frontend adds is a curve.
/// Everything about the commit list that a scroll does not change: the theme,
/// how wide the gutter is, and how many lanes there really are.
///
/// Split from [`commits`] for the reason [`meta`] is split from [`rows`] — the
/// syntax table alone is 7 surfaces by 12 classes, and sending it with every
/// page of a 82,000-commit log is a megabyte of colours nobody asked for twice.
pub fn commits_meta(out: &mut String, log: &Log, host: &Host, label: &str) {
    object(out, |o, f| {
        field_str(o, f, "kind", "commits");
        field_str(o, f, "label", label);
        field_num(o, f, "total", log.len());
        // One width for the whole list — see `Log::lanes` for why this client
        // and the terminal agree, and the window does not.
        field_num(o, f, "lanes", log.lanes);
        // The honest count, against the drawn one. "280 lanes · 12 drawn" is
        // worth knowing; silently drawing twelve is not.
        field_num(o, f, "concurrent", log.concurrent);
        field_num(o, f, "maxLanes", MAX_LANES);
        key(o, f, "font");
        font(o, host);
        key(o, f, "theme");
        theme(o, &host.theme);
    });
}

/// A window of the commit list, with each row's graph already resolved.
///
/// The **shape** of a row — which halves of which lanes exist, which curve pairs
/// with which — is [`gitten_core::graph::plan`], the same plan the window paints
/// as Bézier curves and the terminal paints as box characters. What crosses the
/// wire is that plan, not a drawing of it: turning a half-curve into an SVG path
/// is arithmetic, and it is the client's.
///
/// Hues are **indices** into `theme.lanes` rather than colours, unlike the
/// syntax table: a lane colour has no contrast floor to resolve against, so
/// there is nothing the server knows that the client does not. `capped` is the
/// exception it exists for — a row hiding lanes past the cap draws its last
/// column in `laneOverflow`, and only the server can know it is hiding any.
pub fn commits(out: &mut String, log: &Log, host: &Host, from: usize, count: usize) {
    let end = from.saturating_add(count).min(log.len());
    object(out, |o, f| {
        field_num(o, f, "from", from);
        field_num(o, f, "total", log.len());
        field_str(o, f, "kind", "commits");
        key(o, f, "rows");
        list(o, from..end, |o, i| {
            let (c, d) = (&log.commits[i], &log.plan[i]);
            object(o, |o, f| {
                field_str(o, f, "sha", &c.short);
                field_str(o, f, "author", &c.author);
                field_str(o, f, "initials", &gitten_core::initials(&c.author));
                // Resolved here because the hash that picks it is `Theme`'s,
                // and a client reimplementing it would drift the moment the
                // palette changed length.
                field_rgb(o, f, "authorFg", host.theme.author(&c.author));
                field_num(o, f, "timestamp", c.timestamp);
                field_str(o, f, "subject", &c.subject);
                field_num(o, f, "parents", c.parents.len());
                draw(o, f, d);
            });
        });
    });
}

/// Every key an agent can press, as data.
///
/// Two lists and not one, because a key that moves a diff and a key that opens
/// one never share a screen: `diff` runs [`Modes`] `[global, diff]`, `commits`
/// runs `[global, commits, reset]` — each through the same
/// [`Keymap::help`](gitten_core::command::Keymap::help) the window's help panel
/// reads, so the three can never disagree about what a key does. Flat rows with
/// the mode on each, rather than headings with rows under them: an agent filters
/// these, it does not lay them out.
pub fn keys(out: &mut String, host: &Host) {
    let mut diff = Modes::new();
    diff.push("diff");
    let mut commits = Modes::new();
    commits.push("commits");
    commits.push("reset");
    object(out, |o, f| {
        field_str(o, f, "kind", "keys");
        key(o, f, "diff");
        help_list(o, host, &diff);
        key(o, f, "commits");
        help_list(o, host, &commits);
    });
}

/// One [`Keymap::help`](gitten_core::command::Keymap::help) projection as a
/// flat array. The blank separators are presentation — air between two modes —
/// so they are dropped, and the mode travels on each row instead of in a
/// heading above it.
fn help_list(out: &mut String, host: &Host, modes: &Modes) {
    out.push('[');
    let mut first_item = true;
    let rows = host.keys.help(&host.commands, modes);
    let mut mode = "";
    for row in &rows {
        match row {
            HelpRow::Mode(m) => mode = m,
            HelpRow::Command { name, keys, doc } => {
                if !first_item {
                    out.push(',');
                }
                first_item = false;
                object(out, |o, f| {
                    field_str(o, f, "mode", mode);
                    field_str(o, f, "command", name);
                    field_str(o, f, "keys", keys);
                    field_str(o, f, "doc", doc);
                });
            }
            HelpRow::Blank => {}
        }
    }
    out.push(']');
}

/// Everything an agent may assume about the configuration, and the names it
/// may pass back.
///
/// `meta` carries the palette in full; this carries the *catalogues* — every
/// wrap, differ and theme name registered, with which one is selected — so an
/// agent asking for `?wrap=word` knows `word` exists before it asks. Read-only:
/// changing any of these is a `gitten.toml` edit and a restart, not a request.
pub fn config(out: &mut String, host: &Host, label: &str) {
    object(out, |o, f| {
        field_str(o, f, "kind", "config");
        field_str(o, f, "label", label);
        field_str(o, f, "layout", &host.layout);
        key(o, f, "wrap");
        object(o, |o, f| {
            key(o, f, "names");
            list(o, host.wrap.names(), string);
            field_str(o, f, "selected", host.wrap.selected());
        });
        key(o, f, "differ");
        object(o, |o, f| {
            key(o, f, "names");
            list(o, host.differ.names(), string);
            field_str(o, f, "selected", host.differ.selected());
            field_num(o, f, "context", host.differ.context);
        });
        key(o, f, "themes");
        object(o, |o, f| {
            key(o, f, "names");
            list(o, host.themes.names(), string);
            field_str(o, f, "selected", &host.theme.name);
        });
        key(o, f, "view");
        object(o, |o, f| {
            field_num(o, f, "scrollRows", host.view.rows);
            field_num(o, f, "scrolloff", host.view.scrolloff);
            field_bool(o, f, "scrollbar", host.view.scrollbar);
        });
    });
}

/// Whether the server is up. Nothing view-specific — an agent polls this before
/// trusting any other route, and after a restart to learn the process is new.
pub fn health(out: &mut String) {
    object(out, |o, f| {
        field_bool(o, f, "ok", true);
        field_str(o, f, "service", "gitten-web");
        field_str(o, f, "version", env!("CARGO_PKG_VERSION"));
    });
}

/// Where the agent's cursor is, as `/api/rows?from=` addresses it: visual rows
/// for a diff, list rows for commits.
pub fn viewport(out: &mut String, first: &mut bool, v: &Viewport) {
    key(out, first, "viewport");
    object(out, |o, f| {
        field_num(o, f, "cursor", v.cursor());
        field_num(o, f, "top", v.top());
        field_num(o, f, "len", v.len());
        field_num(o, f, "height", v.height());
    });
}

/// A dispatch that ran: what it did, where the cursor landed, and one line an
/// agent can quote back.
pub fn dispatch_ok(out: &mut String, command: &str, v: &Viewport, status: &str) {
    object(out, |o, f| {
        field_bool(o, f, "ok", true);
        field_str(o, f, "command", command);
        viewport(o, f, v);
        field_str(o, f, "status", status);
    });
}

/// A dispatch that did not run. `code` is the machine word — `bad-request`,
/// `unknown-command`, `wrong-view`, `method-not-allowed`, `unavailable` — and
/// `hint` says what to do instead, because an error an agent cannot act on is
/// a dead end in the one client that has nobody at the keyboard.
pub fn dispatch_err(out: &mut String, error: &str, code: &str, hint: &str) {
    object(out, |o, f| {
        field_bool(o, f, "ok", false);
        field_str(o, f, "error", error);
        field_str(o, f, "code", code);
        field_str(o, f, "hint", hint);
    });
}

/// The arguments a dispatch body may carry, beside its command.
///
/// All optional. `row` moves the cursor absolutely first — `?from=` addressing,
/// visual rows for a diff — `height` sets how many rows a screenful is before
/// anything else runs, and `by`/`pages` scale the one command they accompany.
/// Unknown fields are ignored, so a newer agent's body still runs here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DispatchArgs {
    pub row: Option<usize>,
    pub by: Option<isize>,
    pub pages: Option<isize>,
    pub height: Option<usize>,
}

/// Parses `{"command":"view.down","args":{"by":3}}` without a JSON dependency.
///
/// There is deliberately no JSON reader in this crate — see `json.rs` — and a
/// four-field body does not justify one. What this understands is a string
/// valued `command` and a flat `args` object of integers; anything else is a
/// `bad-request` naming what was wanted rather than a guess.
pub fn parse_dispatch(body: &str) -> Result<(String, DispatchArgs), String> {
    const SHAPE: &str = "{\"command\":\"view.down\",\"args\":{\"by\":1}}";
    let command = extract_str(body, "command")
        .ok_or_else(|| format!("a dispatch needs a \"command\" string, e.g. {SHAPE}"))?;
    if command.is_empty() {
        return Err(format!(
            "a dispatch needs a \"command\" string, e.g. {SHAPE}"
        ));
    }
    let args = extract_object(body, "args").unwrap_or_default();
    let num = |name: &str| extract_num(&args, name);
    let non_negative = |name: &str| match num(name) {
        Some(n) if n < 0 => Err(format!("\"{name}\" wants zero or more, not {n}")),
        Some(n) => Ok(Some(n as usize)),
        None => Ok(None),
    };
    Ok((
        command,
        DispatchArgs {
            row: non_negative("row")?,
            by: num("by").map(|n| n as isize),
            pages: num("pages").map(|n| n as isize),
            height: non_negative("height")?,
        },
    ))
}

/// The value of `"key":"..."` in a flat object, with the escapes a command
/// name could plausibly carry handled and the rest left to the caller to
/// reject. The first match wins; bodies are machine-made and nesting `command`
/// inside `args` is not a shape this reads.
fn extract_str(src: &str, name: &str) -> Option<String> {
    let mut rest = src;
    let quoted = format!("\"{name}\"");
    loop {
        let at = rest.find(quoted.as_str())?;
        let after = rest[at + quoted.len()..].trim_start();
        let value = after.strip_prefix(':').map(str::trim_start)?;
        rest = value;
        if let Some(lit) = value.strip_prefix('"') {
            let mut out = String::new();
            let mut chars = lit.chars();
            loop {
                match chars.next()? {
                    '"' => return Some(out),
                    '\\' => match chars.next()? {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        c => {
                            out.push('\\');
                            out.push(c);
                        }
                    },
                    c => out.push(c),
                }
            }
        }
    }
}

/// The value of `"key":123` in a flat object. Negative for the two arguments
/// that take a direction; the caller decides which those are.
fn extract_num(src: &str, name: &str) -> Option<i64> {
    let mut rest = src;
    let quoted = format!("\"{name}\"");
    loop {
        let at = rest.find(quoted.as_str())?;
        let after = rest[at + quoted.len()..].trim_start();
        let value = after.strip_prefix(':').map(str::trim_start)?;
        rest = value;
        let digits = value
            .strip_prefix('-')
            .unwrap_or(value)
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        if digits.is_empty() {
            continue;
        }
        let negative = value.starts_with('-');
        let n: i64 = digits.parse().ok()?;
        return Some(match negative {
            true => n.saturating_neg(),
            false => n,
        });
    }
}

/// The `{...}` valued `"key"` of an object, braces included, or `None` when it
/// is absent or not an object. Balanced and string-aware, so a `}` inside a
/// string does not end it early.
fn extract_object(src: &str, name: &str) -> Option<String> {
    let quoted = format!("\"{name}\"");
    let at = src.find(quoted.as_str())?;
    let mut after = src[at + quoted.len()..].trim_start();
    after = after.strip_prefix(':')?.trim_start();
    after = after.strip_prefix('{')?;
    let mut depth = 1i32;
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in after.char_indices() {
        if in_str {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(format!("{{{}}}", &after[..i]));
                }
            }
            _ => {}
        }
    }
    None
}

/// One row's plan: where the dot is, and every half that reaches it.
fn draw(out: &mut String, first: &mut bool, d: &Draw) {
    field_num(out, first, "lane", d.lane);
    field_num(out, first, "hue", d.hue);
    field_num(out, first, "lanes", d.lanes);
    if d.merge {
        field_bool(out, first, "merge", true);
    }
    if d.capped {
        field_bool(out, first, "capped", true);
    }
    key(out, first, "lines");
    list(out, &d.lines, |o, l| {
        object(o, |o, f| {
            field_num(o, f, "lane", l.lane);
            field_num(o, f, "hue", l.hue);
            // Omitted when false: a straight lane through the middle of a busy
            // repository is most of the payload, and `up`/`down` are true far
            // more often than not.
            if l.up {
                field_bool(o, f, "up", true);
            }
            if l.down {
                field_bool(o, f, "down", true);
            }
        });
    });
    key(out, first, "curves");
    list(out, &d.curves, |o, c| {
        object(o, |o, f| {
            field_num(o, f, "lane", c.lane);
            field_num(o, f, "partner", c.partner);
            field_num(o, f, "hue", c.hue);
            if c.down {
                field_bool(o, f, "down", true);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitten_core::parse_unified_diff;
    use gitten_core::prepared::prepare;
    use gitten_core::wrap::Off;

    const DIFF: &str = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,3 +1,3 @@
 fn one() {}
-let x = 1;
+let x = 2;
 fn two() {}
";

    fn doc(host: &Host) -> Doc {
        let mut d = Doc::build(prepare(&parse_unified_diff(DIFF), &host.syntax, 2000));
        d.reflow(0, &Off);
        d
    }

    /// The one property that matters and the one this cannot check by reading:
    /// the payload has to parse. `JSON.parse` is not available here, so this is
    /// a structural check — balanced, and no bare control bytes.
    fn well_formed(s: &str) {
        let (mut depth, mut in_str, mut escaped) = (0i32, false, false);
        for c in s.chars() {
            if in_str {
                match c {
                    _ if escaped => escaped = false,
                    '\\' => escaped = true,
                    '"' => in_str = false,
                    c if (c as u32) < 0x20 => panic!("raw control byte inside a string"),
                    _ => {}
                }
                continue;
            }
            match c {
                '"' => in_str = true,
                '{' | '[' => depth += 1,
                '}' | ']' => depth -= 1,
                _ => {}
            }
            assert!(depth >= 0, "closed more than was opened");
        }
        assert_eq!(depth, 0, "unbalanced");
        assert!(!in_str, "unterminated string");
        assert!(!s.contains(",}") && !s.contains(",]"), "trailing comma");
        assert!(!s.contains("{,") && !s.contains("[,"), "leading comma");
    }

    #[test]
    fn the_meta_payload_is_well_formed_and_carries_the_whole_theme() {
        let host = Host::new();
        let mut out = String::new();
        meta(&mut out, &doc(&host), &host, "test");
        well_formed(&out);
        // One surface and one class from each end, so a truncated table fails.
        assert!(out.contains("\"context\":{\"comment\""));
        assert!(out.contains("\"movedAdded\""));
        assert!(out.contains("\"link\""));
        assert!(out.contains("\"laneOverflow\""));
    }

    #[test]
    fn a_row_window_is_well_formed_and_stops_at_the_end() {
        let host = Host::new();
        let d = doc(&host);
        let mut out = String::new();
        rows(&mut out, &d, 0, 1000);
        well_formed(&out);
        assert_eq!(out.matches("\"type\":\"line\"").count(), 4);
        assert_eq!(out.matches("\"type\":\"file\"").count(), 1);
        assert_eq!(out.matches("\"type\":\"hunk\"").count(), 1);
    }

    #[test]
    fn a_window_past_the_end_is_an_empty_list_and_not_an_error() {
        let host = Host::new();
        let mut out = String::new();
        rows(&mut out, &doc(&host), 9999, 40);
        well_formed(&out);
        assert!(out.contains("\"rows\":[]"));
    }

    #[test]
    fn a_diff_line_with_a_tab_stays_well_formed() {
        let host = Host::new();
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,1 @@\n-\tlet a = \"x\";\n+\tlet a = \"y\";\n";
        let mut d = Doc::build(prepare(&parse_unified_diff(raw), &host.syntax, 2000));
        d.reflow(0, &Off);
        let mut out = String::new();
        rows(&mut out, &d, 0, 40);
        well_formed(&out);
        assert!(out.contains("\\t"), "the tab is escaped rather than raw");
    }

    #[test]
    fn the_commit_payload_is_well_formed() {
        // sha, short, parents, author, timestamp, subject — see `parse_log`.
        let log = gitten_core::parse_log(
            "aaaa1111\x1faaaa111\x1fbbbb2222\x1fAda Lovelace\x1f1700000000\x1ffirst\x1e\
             bbbb2222\x1fbbbb222\x1f\x1fAda Lovelace\x1f1699999999\x1froot\x1e",
        );
        assert_eq!(log.len(), 2, "the fixture parses");
        let host = Host::new();
        let log = Log::build(log);
        let mut out = String::new();
        commits(&mut out, &log, &host, 0, 10);
        well_formed(&out);
        // The graph crosses the wire as a plan, not as a drawing.
        assert!(out.contains("\"lines\":"), "{out}");
        assert!(out.contains("\"curves\":"), "{out}");
        assert!(out.contains("\"authorFg\":\"#"), "{out}");

        let mut meta = String::new();
        commits_meta(&mut meta, &log, &host, "test");
        well_formed(&meta);
        assert!(meta.contains("\"lanes\":1"), "{meta}");
        assert!(meta.contains("\"maxLanes\":12"), "{meta}");
        assert!(
            meta.contains("\"laneOverflow\":"),
            "the palette did not ride along"
        );
    }

    #[test]
    fn the_keys_payload_is_well_formed_and_both_views_agree_with_help() {
        let host = Host::new();
        let mut out = String::new();
        keys(&mut out, &host);
        well_formed(&out);
        assert!(out.contains("\"kind\":\"keys\""), "{out}");
        // One row from each end of the map, each carrying its mode.
        assert!(
            out.contains("\"command\":\"view.down\",\"keys\":\"j / down\""),
            "{out}"
        );
        assert!(out.contains("\"command\":\"diff.next-file\""), "{out}");
        assert!(out.contains("\"command\":\"commits.search\""), "{out}");
        // The same projection the window's help panel reads — spot-check one
        // doc line rather than re-asserting the whole registry here.
        assert!(out.contains("\"doc\":\"one row down\""), "{out}");
        // No blank separators leak into the wire: air between modes is layout.
        assert!(!out.contains("Blank"), "{out}");
    }

    #[test]
    fn the_config_payload_names_every_catalogue_and_its_selection() {
        let host = Host::new();
        let mut out = String::new();
        config(&mut out, &host, "test");
        well_formed(&out);
        assert!(out.contains("\"kind\":\"config\""), "{out}");
        assert!(out.contains("\"label\":\"test\""), "{out}");
        assert!(out.contains("\"layout\":\"unified\""), "{out}");
        for name in host.wrap.names() {
            assert!(out.contains(&format!("\"{name}\"")), "wrap {name} missing");
        }
        assert!(out.contains("\"selected\":\"word\""), "{out}");
        for name in host.differ.names() {
            assert!(
                out.contains(&format!("\"{name}\"")),
                "differ {name} missing"
            );
        }
        assert!(out.contains("\"selected\":\"histogram\""), "{out}");
        assert!(out.contains("\"context\":3"), "{out}");
        for name in host.themes.names() {
            assert!(out.contains(&format!("\"{name}\"")), "theme {name} missing");
        }
        assert!(out.contains("\"scrolloff\":3"), "{out}");
    }

    #[test]
    fn the_health_payload_is_well_formed_and_affirmative() {
        let mut out = String::new();
        health(&mut out);
        well_formed(&out);
        assert!(out.contains("\"ok\":true"), "{out}");
        assert!(out.contains("\"service\":\"gitten-web\""), "{out}");
        assert!(out.contains("\"version\":"), "{out}");
    }

    #[test]
    fn a_dispatch_outcome_is_well_formed_either_way() {
        let mut v = gitten_core::view::Viewport::new();
        v.set_len(100);
        v.set_height(20);
        v.move_by(12);
        let mut ok = String::new();
        dispatch_ok(&mut ok, "view.down", &v, "row 12 of 100");
        well_formed(&ok);
        assert!(ok.contains("\"ok\":true"), "{ok}");
        assert!(ok.contains("\"command\":\"view.down\""), "{ok}");
        assert!(ok.contains("\"cursor\":12"), "{ok}");
        assert!(ok.contains("\"status\":\"row 12 of 100\""), "{ok}");

        let mut err = String::new();
        dispatch_err(
            &mut err,
            "no such command \"frobnicate\"",
            "unknown-command",
            "see GET /api/keys",
        );
        well_formed(&err);
        assert!(err.contains("\"ok\":false"), "{err}");
        assert!(err.contains("\"code\":\"unknown-command\""), "{err}");
        assert!(err.contains("\"hint\":"), "{err}");
    }

    #[test]
    fn a_dispatch_body_parses_the_command_and_its_integers() {
        let (command, args) =
            parse_dispatch("{\"command\":\"view.down\",\"args\":{\"by\":3}}").unwrap();
        assert_eq!(command, "view.down");
        assert_eq!(
            args,
            DispatchArgs {
                by: Some(3),
                ..DispatchArgs::default()
            }
        );
        let (command, args) =
            parse_dispatch("{\"command\": \"view.page-down\", \"args\": {\"pages\": -2, \"height\": 50, \"row\": 7}}")
                .unwrap();
        assert_eq!(command, "view.page-down");
        assert_eq!(args.pages, Some(-2));
        assert_eq!(args.height, Some(50));
        assert_eq!(args.row, Some(7));
        // No args is a body with just a command — everything is optional.
        let (command, args) = parse_dispatch("{\"command\":\"view.top\"}").unwrap();
        assert_eq!(command, "view.top");
        assert_eq!(args, DispatchArgs::default());
    }

    #[test]
    fn a_dispatch_body_with_no_command_is_a_named_error_and_not_a_guess() {
        assert!(parse_dispatch("{}").is_err());
        assert!(parse_dispatch("{\"command\":\"\"}").is_err());
        assert!(parse_dispatch("not json at all").is_err());
        assert!(parse_dispatch("{\"command\":\"view.down\",\"args\":{\"row\":-1}}").is_err());
        // Extra fields are ignored, so a newer agent's body still runs here.
        let (command, _) =
            parse_dispatch("{\"command\":\"view.down\",\"args\":{\"by\":1,\"frob\":\"x\"}}")
                .unwrap();
        assert_eq!(command, "view.down");
    }
}
