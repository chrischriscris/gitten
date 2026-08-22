//! What the diff view will actually show, in a terminal.
//!
//! Highlighting and theming are the two things in here whose output is visual,
//! and a window is a slow way to look at either. This paints
//! `fixtures/big.diff` with the same highlighters and the same theme the GPUI
//! view uses — 24-bit ANSI instead of `Hsla`, and that is the only difference.
//! If a colour looks wrong here it is wrong there.
//!
//!   cargo run -q -p plait-core --example paint --release [ROWS] [PATH-FILTER]
//!
//! `THEME=name` paints it in another registered palette — `dark`, `light`,
//! `slate`, or one `plait.toml` defined. `WRAP_COLS=n` sets where a long line
//! breaks, and `WRAP_COLS=0` turns wrapping off. That is the same [`Wrap`] the
//! window uses, reached the same way: a break point is a property of text, and
//! this is the check that nothing about the seam is shaped like GPUI. What a
//! terminal supplies is the column count — the one thing `core` cannot know.
use plait_core::host::Host;
use plait_core::markdown::{lay_out, Block, Layout};
use plait_core::prepared::prepare;
use plait_core::syntax::Kind;
use plait_core::theme::{MarkdownPalette, Rgb, Style, Surface};
use plait_core::wrap::Wrapped;
use plait_core::{parse_unified_diff, LineKind};

fn fg(c: Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", c >> 16 & 0xff, c >> 8 & 0xff, c & 0xff)
}

fn bg(c: Rgb) -> String {
    format!("\x1b[48;2;{};{};{}m", c >> 16 & 0xff, c >> 8 & 0xff, c & 0xff)
}

/// Underline a piece if any intraline span covers it. Cheap and approximate —
/// the point is to see that the spans and the tokens agree.
fn underlined(l: &plait_core::prepared::Line, start: usize, end: usize, piece: &str) -> String {
    let hit = l.spans.iter().any(|s| s.start < end.max(start + 1) && s.end > start);
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
            format!("{indent}{}{} ", fg(p.marker), if first { glyph } else { " " })
        }
        Block::Quote(_) => format!("{indent}{}│ ", fg(p.quote_bar)),
        Block::Fence | Block::Code => format!("{}│ ", fg(p.code_bar)),
        Block::Rule => format!("{}{}", fg(p.rule), "─".repeat(40)),
        // A table draws its own grid inside its text; anything added in front of
        // it would shift one row of the grid relative to the next.
        Block::Table | Block::TableRule => format!("{}", fg(p.marker)),
        _ => format!("{indent}"),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let budget: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(60);
    let filter = args.next().unwrap_or_default();

    let raw = String::from_utf8_lossy(&std::fs::read("fixtures/big.diff").unwrap()).into_owned();

    // Exactly what the shell builds, and exactly the same call: the host, then
    // one prepare pass. Nothing about the assembly is re-implemented here.
    let mut host = Host::new();
    // `THEME=light`, off the same registry the window's picker lists. A palette
    // is the one thing in here whose only real test is looking at it, and a
    // frame on stdout interrupts nobody.
    if let Ok(name) = std::env::var("THEME") {
        if !host.select_theme(&name) {
            eprintln!(
                "paint: no theme {name:?}; registered: {}",
                host.themes.names().join(", ")
            );
        }
    }
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
                .map(|h| if md { lay_out(&mut h.lines, &layout) } else { Vec::new() })
                .collect(),
        );
    }
    // Where every line breaks, from the host's own registry. The sign column and
    // one space are all this draws in front of the text, so that is the chrome.
    let cols: usize =
        std::env::var("WRAP_COLS").ok().and_then(|v| v.parse().ok()).unwrap_or(100);
    let wrapped: Vec<Wrapped> = p
        .files
        .iter()
        .map(|f| {
            Wrapped::build(
                f.hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .map(|l| (l.text.as_str(), cols.saturating_sub(2))),
                host.wrap.current(),
            )
        })
        .collect();

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
                    LineKind::Added => {
                        ("+", theme.diff.added_bg, theme.diff.added_fg, Surface::Added, Surface::AddedWord)
                    }
                    LineKind::Removed => {
                        ("-", theme.diff.removed_bg, theme.diff.removed_fg, Surface::Removed, Surface::RemovedWord)
                    }
                    LineKind::Context => {
                        (" ", theme.diff.context_bg, theme.diff.context_fg, Surface::Context, Surface::Context)
                    }
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
                        let (s, e) = (t.start.max(span.start), t.end.min(span.end));
                        if e <= s {
                            continue;
                        }
                        print!("{}", underlined(&l, cursor, s, &l.text[cursor..s]));
                        let on_word = l.spans.iter().any(|w| w.start < e && w.end > s);
                        let style = theme.syntax_on(t.kind, if on_word { word } else { surface });
                        let piece = styled(style, &l.text[s..e]);
                        print!("{}{}", underlined(&l, s, e, &piece), fg(row_fg));
                        cursor = e;
                    }
                    println!(
                        "{}\x1b[0m",
                        underlined(&l, cursor, span.end, &l.text[cursor..span.end])
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
