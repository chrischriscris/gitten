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
//! `--json` (or `GITTEN_FORMAT=json`) prints one object to stdout instead of
//! the frame — the schema is `gitten.dump/1`, documented in
//! `docs/agent-json.md` — and failures become `{error, code, hint}` on stderr
//! with a nonzero exit. Stdout is the machine contract; stderr may still carry
//! human warnings (an unknown theme falls back and says so, in both modes).
//!
//! No raw mode, no alternate screen, no window: it builds the same
//! [`Screen`](gitten_tui::screen::Screen) the app would and
//! [`print`](gitten_tui::screen::Screen::print)s it, so it can be piped into
//! `less -R` or diffed against yesterday's output. That is what `core`'s
//! `paint` example is for the pipeline, and it exists for the same reason
//! `AGENTS.md` says never to launch the app unannounced — a window appearing
//! interrupts whoever is at the keyboard, and a frame on stdout does not.

use gitten_app::env;
use gitten_core::host::Host;
use gitten_tui::commits::Commits;
use gitten_tui::diff::Diff;
use gitten_tui::screen::{Ink, Screen};
use std::path::PathBuf;
use std::time::Instant;

/// Appends a JSON string literal, quotes included. The same escaping
/// `web/src/json.rs` documents: structural characters and C0 escaped,
/// everything else passed through as UTF-8.
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

/// A `"key":` prefix, comma-separated from whatever came before it.
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

/// A fatal failure: `{error, code, hint}` on stderr, then out with status 1.
/// Human mode keeps its own sentences; this is the JSON mode's equivalent.
fn fail(json: bool, human: &str, code: &str, error: &str, hint: &str) -> ! {
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
    } else {
        eprintln!("{human}");
    }
    std::process::exit(1);
}

fn read(path: &str, json: bool) -> String {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => fail(
            json,
            &format!("gitten: {path}: {e}"),
            "io",
            &format!("{path}: {e}"),
            "run from the repository root so fixtures/ resolves, or pass a path that exists",
        ),
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
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let json = env::wants_json(&raw);
    let args = env::strip_json_arg(&raw);
    let mut args = args.into_iter();
    let which = args.next().unwrap_or_else(|| "diff".into());
    let where_ = args.next().unwrap_or_else(|| ".".into());
    let rest = args.next();

    let (cols, rows) = (env::cols(), env::rows());
    let (at, frames) = (env::at(), env::frames());
    let mut host = Host::new();
    if let Some(name) = env::layout() {
        host.layout = name;
    }
    if let Some(name) = env::wrap() {
        if !host.wrap.select(&name) {
            eprintln!(
                "gitten: unknown wrap {name:?}; have {}",
                host.wrap.names().join(", ")
            );
        }
    }
    if let Some(name) = env::theme() {
        if !host.select_theme(&name) {
            eprintln!(
                "gitten: unknown theme {name:?}; have {}",
                host.themes.names().join(", ")
            );
        }
    }
    // What this frame is a frame *of*, in one string for the JSON `source`.
    // A repository plus its revspec, or the fixture/patch/stdin spelling that
    // chose the data instead.
    let source = match which.as_str() {
        "diff" => match where_.as_str() {
            "--fixtures" => "--fixtures".to_string(),
            "--patch" => format!("--patch {}", rest.clone().unwrap_or_default()),
            "-" => "stdin".to_string(),
            repo => format!("{} {}", repo, rest.clone().unwrap_or_default())
                .trim_end()
                .to_string(),
        },
        "commits" => match where_.as_str() {
            "--fixtures" => "--fixtures".to_string(),
            repo => format!("{} {}", repo, rest.clone().unwrap_or_default())
                .trim_end()
                .to_string(),
        },
        _ => String::new(),
    };

    let mut screen = Screen::new(cols, rows);
    screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));

    // The load and the repaint loop are the same in both modes — JSON reports
    // the numbers instead of printing the frame they came from.
    let (status, rows_total, load_ms, frame_ms) = match which.as_str() {
        "diff" => {
            let files = match where_.as_str() {
                "--fixtures" => gitten_core::parse_unified_diff(&read("fixtures/big.diff", json)),
                "--patch" => match rest.as_deref() {
                    Some("-") => gitten_core::parse_unified_diff(&read_stdin()),
                    Some(path) => gitten_core::parse_unified_diff(&read(path, json)),
                    None => fail(
                        json,
                        "gitten: --patch wants a file, or - for standard input",
                        "usage",
                        "--patch wants a file, or - for standard input",
                        "pass a patch file, or - to read it from standard input",
                    ),
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
                        Err(e) => fail(
                            json,
                            &format!("gitten: {e}"),
                            "acquire",
                            &e.to_string(),
                            "check the repository path and the revspec",
                        ),
                    }
                }
            };
            let load = Instant::now();
            let mut view = Diff::new(files, &host);
            view.resize(cols, rows - 1, &host);
            view.move_by(at as isize);
            let load = load.elapsed();
            // One buffer across every repaint, which is the contract `Rows`
            // has with `runs`: drawing a frame allocates nothing.
            let mut out = Vec::new();
            let t = Instant::now();
            for _ in 0..frames {
                view.paint(&mut screen, 0, 0, true, &host, &mut out);
            }
            let frame = t.elapsed() / frames.max(1) as u32;
            if !json {
                eprintln!("load {load:.0?} · frame {frame:.0?} · {} rows", view.rows());
            }
            let status = view.status(&host);
            let total = view.rows();
            (
                status,
                total,
                load.as_secs_f64() * 1000.0,
                frame.as_secs_f64() * 1000.0,
            )
        }
        "commits" => {
            let commits = match where_.as_str() {
                "--fixtures" => gitten_core::parse_log(&read("fixtures/log.txt", json)),
                // Same answer acquisition gives, because the example mirrors
                // the CLI rather than re-deciding it.
                "--patch" | "-" => fail(
                    json,
                    "gitten: a patch is one diff and has no history — open it with diff",
                    "usage",
                    "a patch is one diff and has no history",
                    "open it with diff",
                ),
                repo => {
                    let limit = rest.and_then(|n| n.parse().ok()).unwrap_or(5000);
                    match gitten_git::open(&PathBuf::from(repo)).log(limit) {
                        Ok(c) => c,
                        Err(e) => fail(
                            json,
                            &format!("gitten: {e}"),
                            "acquire",
                            &e.to_string(),
                            "check the repository path",
                        ),
                    }
                }
            };
            let load = Instant::now();
            let mut view = Commits::new(commits);
            view.resize(cols, rows - 1);
            view.move_by(at as isize);
            let load = load.elapsed();
            let t = Instant::now();
            for _ in 0..frames {
                view.paint(&mut screen, 0, 0, true, &host);
                // The bar's column is the app's, not the pane's: this example
                // is one pane over the whole body, so its boundary is the
                // screen's edge — the rail the paint loop would choose.
                view.paint_bar(&mut screen, cols - 1, None, 0, &host);
            }
            let frame = t.elapsed() / frames.max(1) as u32;
            if !json {
                eprintln!(
                    "load {load:.0?} · frame {frame:.0?} · {} commits",
                    view.len()
                );
            }
            let status = view.status();
            let total = view.len();
            (
                status,
                total,
                load.as_secs_f64() * 1000.0,
                frame.as_secs_f64() * 1000.0,
            )
        }
        other => fail(
            json,
            &format!("gitten: no such view {other:?}; try `diff` or `commits`"),
            "usage",
            &format!("no such view {other:?}"),
            "try `diff` or `commits`",
        ),
    };

    if json {
        let mut out = String::from("{");
        let mut first = true;
        sfield(&mut out, &mut first, "schema", "gitten.dump/1");
        sfield(&mut out, &mut first, "view", &which);
        sfield(&mut out, &mut first, "source", &source);
        nfield(&mut out, &mut first, "cols", cols);
        nfield(&mut out, &mut first, "rows", rows);
        sfield(&mut out, &mut first, "layout", &host.layout);
        sfield(&mut out, &mut first, "wrap", host.wrap.selected());
        sfield(&mut out, &mut first, "theme", &host.theme.name);
        nfield(&mut out, &mut first, "at", at);
        nfield(&mut out, &mut first, "frames", frames);
        nfield(&mut out, &mut first, "loadMs", format!("{load_ms:.3}"));
        nfield(&mut out, &mut first, "frameMs", format!("{frame_ms:.3}"));
        nfield(&mut out, &mut first, "rowsTotal", rows_total);
        sfield(&mut out, &mut first, "status", &status);
        out.push('}');
        println!("{out}");
        return;
    }

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
