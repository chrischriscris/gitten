//! One frame of the terminal views, printed to stdout.
//!
//! ```sh
//! cargo run -q -p gitten-tui --example dump -- diff    [REPO] [REVSPEC]
//! cargo run -q -p gitten-tui --example dump -- diff    --fixtures
//! cargo run -q -p gitten-tui --example dump -- diff    --patch FILE   (- = stdin)
//! cargo run -q -p gitten-tui --example dump -- commits [REPO] [LIMIT]
//! cargo run -q -p gitten-tui --example dump -- commits --fixtures
//! ```
//!
//! `COLS` and `ROWS` override the size; `LAYOUT`, `WRAP`, `THEME` and `AT` pick
//! the presentation, the wrap, the palette and how far down the diff to start. `FRAMES` is how
//! many times to repaint before printing — the average lands on stderr, so a
//! per-frame cost is measurable without a terminal to watch. **Build with
//! `--release` before believing it**, exactly as the window's overlay says.
//!
//! No raw mode, no alternate screen, no window: it builds the same
//! [`Screen`](gitten_tui::screen::Screen) the app would and
//! [`print`](gitten_tui::screen::Screen::print)s it, so it can be piped into
//! `less -R` or diffed against yesterday's output. That is what `core`'s
//! `paint` example is for the pipeline, and it exists for the same reason
//! `AGENTS.md` says never to launch the app unannounced — a window appearing
//! interrupts whoever is at the keyboard, and a frame on stdout does not.

use gitten_core::host::Host;
use gitten_tui::commits::Commits;
use gitten_tui::diff::Diff;
use gitten_tui::screen::{Ink, Screen};
use std::path::PathBuf;
use std::time::Instant;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn number(name: &str, default: usize) -> usize {
    env(name).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn read(path: &str) -> String {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => {
            eprintln!("gitten: {path}: {e}");
            std::process::exit(1);
        }
    }
}

/// A patch from `-`: lossy, like everything that came out of git.
fn read_stdin() -> String {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut bytes)
        .expect("standard input");
    String::from_utf8_lossy(&bytes).into_owned()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let which = args.next().unwrap_or_else(|| "diff".into());
    let where_ = args.next().unwrap_or_else(|| ".".into());
    let rest = args.next();

    let (cols, rows) = (number("COLS", 100), number("ROWS", 40));
    let mut host = Host::new();
    if let Some(name) = env("LAYOUT") {
        host.layout = name;
    }
    if let Some(name) = env("WRAP") {
        if !host.wrap.select(&name) {
            eprintln!(
                "gitten: unknown wrap {name:?}; have {}",
                host.wrap.names().join(", ")
            );
        }
    }
    if let Some(name) = env("THEME") {
        if !host.select_theme(&name) {
            eprintln!(
                "gitten: unknown theme {name:?}; have {}",
                host.themes.names().join(", ")
            );
        }
    }

    let mut screen = Screen::new(cols, rows);
    screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));

    let status = match which.as_str() {
        "diff" => {
            let files = match where_.as_str() {
                "--fixtures" => gitten_core::parse_unified_diff(&read("fixtures/big.diff")),
                "--patch" => match rest.as_deref() {
                    Some("-") => gitten_core::parse_unified_diff(&read_stdin()),
                    Some(path) => gitten_core::parse_unified_diff(&read(path)),
                    None => {
                        eprintln!("gitten: --patch wants a file, or - for standard input");
                        std::process::exit(1);
                    }
                },
                "-" => gitten_core::parse_unified_diff(&read_stdin()),
                repo => {
                    let spec = rest.unwrap_or_default();
                    match gitten_git::diff(
                        gitten_git::open(&PathBuf::from(repo)).as_ref(),
                        &spec,
                        &host.differ,
                        &Default::default(),
                    ) {
                        Ok(f) => f,
                        Err(e) => {
                            eprintln!("gitten: {e}");
                            std::process::exit(1);
                        }
                    }
                }
            };
            let load = Instant::now();
            let mut view = Diff::new(files, &host);
            view.resize(cols, rows - 1, &host);
            view.move_by(number("AT", 0) as isize);
            let load = load.elapsed();
            // One buffer across every repaint, which is the contract `Rows`
            // has with `runs`: drawing a frame allocates nothing.
            let mut out = Vec::new();
            let frames = number("FRAMES", 50);
            let t = Instant::now();
            for _ in 0..frames {
                view.paint(&mut screen, 0, &host, &mut out);
            }
            eprintln!(
                "load {load:.0?} · frame {:.0?} · {} rows",
                t.elapsed() / frames.max(1) as u32,
                view.rows()
            );
            view.status(&host)
        }
        "commits" => {
            let commits = match where_.as_str() {
                "--fixtures" => gitten_core::parse_log(&read("fixtures/log.txt")),
                // Same answer acquisition gives, because the example mirrors
                // the CLI rather than re-deciding it.
                "--patch" | "-" => {
                    eprintln!("gitten: a patch is one diff and has no history — open it with diff");
                    std::process::exit(1);
                }
                repo => {
                    let limit = rest.and_then(|n| n.parse().ok()).unwrap_or(5000);
                    match gitten_git::open(&PathBuf::from(repo)).log(limit) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("gitten: {e}");
                            std::process::exit(1);
                        }
                    }
                }
            };
            let load = Instant::now();
            let mut view = Commits::new(commits);
            view.resize(cols, rows - 1);
            view.move_by(number("AT", 0) as isize);
            let load = load.elapsed();
            let frames = number("FRAMES", 50);
            let t = Instant::now();
            for _ in 0..frames {
                view.paint(&mut screen, 0, &host);
            }
            eprintln!(
                "load {load:.0?} · frame {:.0?} · {} commits",
                t.elapsed() / frames.max(1) as u32,
                view.len()
            );
            view.status()
        }
        other => {
            eprintln!("gitten: no such view {other:?}; try `diff` or `commits`");
            std::process::exit(1);
        }
    };

    // The status line, in the last row: the same string the app's own bar would
    // show, so a dump says which layout and which wrap produced it.
    let ink = Ink::new(host.theme.chrome.dim, host.theme.chrome.status_bg);
    let mut pen = screen.row(rows - 1);
    pen.put(" ", ink);
    pen.put(&status, ink);
    pen.wash(ink);

    let mut out = std::io::stdout().lock();
    if let Err(e) = screen.print(&mut out) {
        eprintln!("gitten: {e}");
    }
}
