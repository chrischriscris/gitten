//! What the diff view will actually show, in a terminal.
//!
//! Highlighting and theming are the two things in here whose output is visual,
//! and a window is a slow way to look at either. This paints
//! `fixtures/big.diff` with the same highlighters and the same theme the GPUI
//! view uses — 24-bit ANSI instead of `Hsla`, and that is the only difference.
//! If a colour looks wrong here it is wrong there.
//!
//!   cargo run -q -p gitten-core --example paint --release [ROWS] [PATH-FILTER]
//!
//! `--json` (or `GITTEN_FORMAT=json`) prints one object to stdout instead of
//! the ANSI frame — the schema is `gitten.paint/1`, documented in
//! `docs/agent-json.md`.
//!
//! `THEME=name` paints it in another registered palette — `dark`, `light`,
//! `slate`, or one `gitten.toml` defined. `WRAP_COLS=n` sets where a long line
//! breaks, and `WRAP_COLS=0` turns wrapping off. That is the same [`Wrap`] the
//! window uses, reached the same way: a break point is a property of text, and
//! this is the check that nothing about the seam is shaped like GPUI. What a
//! terminal supplies is the column count — the one thing `core` cannot know.
use gitten_app::env;
use gitten_core::host::Host;
use gitten_core::markdown::{lay_out, Block, Layout};
use gitten_core::prepared::prepare;
use gitten_core::syntax::Kind;
use gitten_core::theme::{MarkdownPalette, Rgb, Style, Surface};
use gitten_core::wrap::Wrapped;
use gitten_core::{parse_unified_diff, LineKind};

fn fg(c: Rgb) -> String {
    format!(
        "\x1b[38;2;{};{};{}m",
        c >> 16 & 0xff,
        c >> 8 & 0xff,
        c & 0xff
    )
}

fn bg(c: Rgb) -> String {
    format!(
        "\x1b[48;2;{};{};{}m",
        c >> 16 & 0xff,
        c >> 8 & 0xff,
        c & 0xff
    )
}

/// Underline a piece if any intraline span covers it. Cheap and approximate —
/// the point is to see that the spans and the tokens agree.
fn underlined(l: &gitten_core::prepared::Line, start: usize, end: usize, piece: &str) -> String {
    let hit = l
        .spans
        .iter()
        .any(|s| s.start < (end.max(start + 1)) as u32 && s.end > start as u32);
    if hit && start < end {
        format!("\x1b[4m{piece}\x1b[24m")
    } else {
        piece.to_string()
    }
}

fn styled(s: Style, text: &str) -> String {
    let mut out = fg(s.fg);
    if s.bold {
        out.push_str("\x1b[1m");
    }
    if s.italic {
        out.push_str("\x1b[3m");
    }
    out.push_str(text);
    out.push_str("\x1b[22;23;39m");
    out
}

/// What a rendered Markdown row puts *before* its text, in place of the markers
/// that are no longer in it. The window draws a coloured div and a font size;
/// a terminal draws a glyph and a bold escape. Same [`Block`], same decision,
/// two frontends — which is the whole point of the block living in `core`.
///
/// The one thing a terminal cannot do is the size, so a heading here is bold and
/// the level shows as its own depth of indent instead.
///
/// `first` is false on a wrapped line's continuation rows. A bar repeats down all
/// of them — it is the block, and the block continues — but the bullet is drawn
/// once and its width kept, so the text of a wrapped item lines up under itself
/// rather than under a column of glyphs. The window does the same thing with the
/// same distinction.
fn furniture(block: Block, p: &MarkdownPalette, first: bool) -> String {
    let indent = "  ".repeat(block.depth() as usize);
    match block {
        Block::Heading(l) => format!("{}{}", fg(p.marker), "  ".repeat(l as usize - 1)),
        Block::Bullet(d) => {
            let glyph = ["•", "◦", "▪", "·"][(d as usize).min(3)];
            format!(
                "{indent}{}{} ",
                fg(p.marker),
                if first { glyph } else { " " }
            )
        }
        Block::Quote(_) => format!("{indent}{}│ ", fg(p.quote_bar)),
        Block::Fence | Block::Code => format!("{}│ ", fg(p.code_bar)),
        Block::Rule => format!("{}{}", fg(p.rule), "─".repeat(40)),
        // A table draws its own grid inside its text; anything added in front of
        // it would shift one row of the grid relative to the next.
        Block::Table | Block::TableRule => fg(p.marker).to_string(),
        _ => indent.to_string(),
    }
}

fn jstr(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn key(out: &mut String, first: &mut bool, k: &str) {
    if !*first {
        out.push(',');
    }
    *first = false;
    jstr(out, k);
    out.push(':');
}

fn sfield(out: &mut String, first: &mut bool, k: &str, v: &str) {
    key(out, first, k);
    jstr(out, v);
}

fn nfield(out: &mut String, first: &mut bool, k: &str, v: impl std::fmt::Display) {
    key(out, first, k);
    out.push_str(&v.to_string());
}

fn fail(json: bool, code: &str, error: &str, hint: &str) -> ! {
    if json {
        let mut out = String::from("{");
        let mut first = true;
        for (k, v) in [("error", error), ("code", code), ("hint", hint)] {
            if !first {
                out.push(',');
            }
            first = false;
            jstr(&mut out, k);
            out.push(':');
            jstr(&mut out, v);
        }
        out.push('}');
        eprintln!("{out}");
        std::process::exit(1);
    }
    panic!("{error}");
}

fn read_fixture(path: &str, json: bool) -> String {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => fail(
            json,
            "io",
            &format!("{path}: {e}"),
            "run from the repository root so fixtures/ resolves",
        ),
    }
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let json = env::wants_json(&raw);
    let args = env::strip_json_arg(&raw);
    let mut args = args.into_iter();
    let budget: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(60);
    let filter = args.next().unwrap_or_default();

    let raw = read_fixture("fixtures/big.diff", json);

    // Exactly what the shell builds, and exactly the same call: the host, then
    // one prepare pass. Nothing about the assembly is re-implemented here.
    let mut host = Host::new();
    // `THEME=light`, off the same registry the window's picker lists. A palette
    // is the one thing in here whose only real test is looking at it, and a
    // frame on stdout interrupts nobody.
    if let Some(name) = env::theme() {
        if !host.select_theme(&name) {
            eprintln!(
                "paint: no theme {name:?}; registered: {}",
                host.themes.names().join(", ")
            );
        }
    }
    let theme_name = host.theme.name.clone();
    let theme = &host.theme;
    let mut p = prepare(&parse_unified_diff(&raw), &host.syntax, 2000);

    // A `.md` file is laid out as a document, the same call the shell's
    // `MarkdownRows` makes and with no markdown logic re-implemented here. What
    // this example adds is escape codes.
    // A terminal is monospaced by definition, so tables get the grid treatment
    // here for free — the one assumption `core` cannot make for itself.
    let layout = Layout::monospaced();
    let mut blocks: Vec<Vec<Vec<Block>>> = Vec::with_capacity(p.files.len());
    for f in &mut p.files {
        let md = matches!(f.path.rsplit('.').next(), Some("md" | "markdown" | "mdx"));
        blocks.push(
            f.hunks
                .iter_mut()
                .map(|h| {
                    if md {
                        lay_out(&mut h.lines, &layout)
                    } else {
                        Vec::new()
                    }
                })
                .collect(),
        );
    }
    // Where every line breaks, from the host's own registry. The sign column and
    // one space are all this draws in front of the text, so that is the chrome.
    let cols: usize = env::wrap_cols(100);
    let wrapped: Vec<Wrapped> = p
        .files
        .iter()
        .map(|f| {
            Wrapped::build(
                f.hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .map(|l| (l.text.as_ref(), cols.saturating_sub(2))),
                host.wrap.current(),
            )
        })
        .collect();

    if json {
        // The same walk the frame below does — budget, filter, one line at a
        // time — counting rows instead of printing them. A rule or a blank
        // draws one row however many the wrap table gives it, exactly as the
        // frame does.
        let mut left = budget;
        let mut rows_printed = 0usize;
        let mut out = String::from("{");
        let mut first = true;
        sfield(&mut out, &mut first, "schema", "gitten.paint/1");
        sfield(&mut out, &mut first, "tool", "paint");
        nfield(&mut out, &mut first, "version", 1);
        sfield(&mut out, &mut first, "theme", &theme_name);
        nfield(&mut out, &mut first, "wrapCols", cols);
        nfield(&mut out, &mut first, "budget", budget);
        sfield(&mut out, &mut first, "filter", &filter);
        key(&mut out, &mut first, "files");
        out.push('[');
        let mut ffirst = true;
        for ((f, fb), fw) in p.files.iter().zip(&blocks).zip(&wrapped) {
            let lines: usize = f.hunks.iter().map(|h| h.lines.len()).sum();
            if !ffirst {
                out.push(',');
            }
            ffirst = false;
            out.push('{');
            let mut ifirst = true;
            sfield(&mut out, &mut ifirst, "path", &f.path);
            nfield(&mut out, &mut ifirst, "adds", f.adds);
            nfield(&mut out, &mut ifirst, "dels", f.dels);
            nfield(&mut out, &mut ifirst, "lines", lines);
            out.push('}');
            if left == 0 {
                continue;
            }
            if !filter.is_empty() && !f.path.contains(&filter) {
                continue;
            }
            let mut line_no = 0usize;
            'hunks: for (h, hb) in f.hunks.iter().zip(fb) {
                if left == 0 {
                    break 'hunks;
                }
                for (i, _) in h.lines.iter().enumerate() {
                    if left == 0 {
                        break 'hunks;
                    }
                    left -= 1;
                    let at = line_no + i;
                    rows_printed += match hb.get(i).copied() {
                        Some(Block::Rule | Block::Blank) => 1,
                        _ => fw.rows(at),
                    };
                }
                line_no += h.lines.len();
            }
        }
        out.push(']');
        nfield(&mut out, &mut first, "rowsPrinted", rows_printed);
        out.push('}');
        println!("{out}");
        return;
    }

    let mut left = budget;

    for ((f, fb), fw) in p.files.iter().zip(&blocks).zip(&wrapped) {
        if left == 0 {
            break;
        }
        if !filter.is_empty() && !f.path.contains(&filter) {
            continue;
        }
        println!(
            "{}{}{}  {}+{} -{}\x1b[0m",
            bg(theme.diff.file_bg),
            fg(theme.diff.file_fg),
            f.path,
            fg(theme.diff.adds_fg),
            f.adds,
            f.dels,
        );
        // The wrap table is per *file*, so a hunk's lines have to be found in it
        // at their offset from the start of the file rather than from the start
        // of the hunk.
        let mut line_no = 0usize;
        for (h, hb) in f.hunks.iter().zip(fb) {
            if left == 0 {
                break;
            }
            for (i, l) in h.lines.iter().enumerate() {
                let at = line_no + i;
                if left == 0 {
                    break;
                }
                left -= 1;
                let (sign, row_bg, row_fg, surface, word) = match l.kind {
                    LineKind::Added => (
                        "+",
                        theme.diff.added_bg,
                        theme.diff.added_fg,
                        Surface::Added,
                        Surface::AddedWord,
                    ),
                    LineKind::Removed => (
                        "-",
                        theme.diff.removed_bg,
                        theme.diff.removed_fg,
                        Surface::Removed,
                        Surface::RemovedWord,
                    ),
                    LineKind::Context => (
                        " ",
                        theme.diff.context_bg,
                        theme.diff.context_fg,
                        Surface::Context,
                        Surface::Context,
                    ),
                };
                let block = hb.get(i).copied();
                // One pass per row the line takes. A continuation gets a blank
                // sign, exactly as it does in the window: the background says
                // which line it belongs to and there is nothing else to add.
                for row in 0..fw.rows(at) {
                    let span = fw.range(at, row, &l.text);
                    print!(
                        "{}{}{} ",
                        bg(row_bg),
                        fg(row_fg),
                        if row == 0 { sign } else { " " }
                    );
                    if let Some(b) = block {
                        print!("{}{}", furniture(b, &theme.markdown, row == 0), fg(row_fg));
                        // A rule and a blank draw no text: the punctuation *was*
                        // the drawing, and the furniture has replaced it.
                        if matches!(b, Block::Rule | Block::Blank) {
                            println!("\x1b[0m");
                            break;
                        }
                        if matches!(b, Block::Heading(_)) {
                            print!("\x1b[1m");
                        }
                    }
                    // Syntax colours the text; the intraline spans underline the
                    // words that changed, standing in for the background the
                    // window uses — a terminal cell cannot hold two backgrounds
                    // at once. Tokens are in *line* coordinates and clamped into
                    // this row, which is what `runs` does in the shell.
                    let mut cursor = span.start;
                    for t in &l.tokens {
                        let (s, e) = (
                            (t.start as usize).max(span.start),
                            (t.end as usize).min(span.end),
                        );
                        if e <= s {
                            continue;
                        }
                        print!("{}", underlined(l, cursor, s, &l.text[cursor..s]));
                        let on_word = l
                            .spans
                            .iter()
                            .any(|w| (w.start as usize) < e && (w.end as usize) > s);
                        let style = theme.syntax_on(t.kind, if on_word { word } else { surface });
                        let piece = styled(style, &l.text[s..e]);
                        print!("{}{}", underlined(l, s, e, &piece), fg(row_fg));
                        cursor = e;
                    }
                    println!(
                        "{}\x1b[0m",
                        underlined(l, cursor, span.end, &l.text[cursor..span.end])
                    );
                }
            }
            line_no += h.lines.len();
        }
    }

    // The palette itself, since the other half of what this example is for is
    // looking at a theme rather than at a diff.
    let legend: Vec<String> = Kind::ALL
        .iter()
        .map(|k| styled(theme.syntax_on(*k, Surface::Context), &format!("{k:?}")))
        .collect();
    println!("\n{}  {}", theme.name, legend.join(" "));
}
