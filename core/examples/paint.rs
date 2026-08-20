//! What the diff view will actually show, in a terminal.
//!
//! Highlighting and theming are the two things in here whose output is visual,
//! and a window is a slow way to look at either. This paints
//! `fixtures/big.diff` with the same highlighters and the same theme the GPUI
//! view uses — 24-bit ANSI instead of `Hsla`, and that is the only difference.
//! If a colour looks wrong here it is wrong there.
//!
//!   cargo run -q -p plait-core --example paint --release [ROWS] [PATH-FILTER]
use plait_core::host::Host;
use plait_core::prepared::prepare;
use plait_core::syntax::Kind;
use plait_core::theme::{Rgb, Style, Surface};
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

fn main() {
    let mut args = std::env::args().skip(1);
    let budget: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(60);
    let filter = args.next().unwrap_or_default();

    let raw = String::from_utf8_lossy(&std::fs::read("fixtures/big.diff").unwrap()).into_owned();

    // Exactly what the shell builds, and exactly the same call: the host, then
    // one prepare pass. Nothing about the assembly is re-implemented here.
    let host = Host::new();
    let theme = &host.theme;
    let p = prepare(&parse_unified_diff(&raw), &host.syntax, 2000);
    let mut left = budget;

    for f in &p.files {
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
        for h in &f.hunks {
            if left == 0 {
                break;
            }
            for l in &h.lines {
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
                print!("{}{}{sign} ", bg(row_bg), fg(row_fg));
                // Syntax colours the text; the intraline spans underline the
                // words that changed, standing in for the background the window
                // uses — a terminal cell cannot hold two backgrounds at once.
                let mut cursor = 0;
                for t in &l.tokens {
                    print!("{}", underlined(&l, cursor, t.start, &l.text[cursor..t.start]));
                    let on_word = l.spans.iter().any(|s| s.start < t.end && s.end > t.start);
                    let style = theme.syntax_on(t.kind, if on_word { word } else { surface });
                    let piece = styled(style, &l.text[t.range()]);
                    print!("{}{}", underlined(&l, t.start, t.end, &piece), fg(row_fg));
                    cursor = t.end;
                }
                println!("{}\x1b[0m", underlined(&l, cursor, l.text.len(), &l.text[cursor..]));
            }
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
