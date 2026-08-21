//! `plait-web` — acquire in the terminal, read in a browser.
//!
//! The arguments are the shell's, deliberately: whatever you type at
//! `plait-shell diff` works here, so the two doors are not two things to
//! remember. What it adds is `--port`.

use plait_core::differ::Overrides;
use plait_core::host::Host;
use plait_core::prepared::prepare;
use plait_web::rows::Doc;
use plait_web::{http, Data, State, MAX_LINE_CHARS};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const USAGE: &str = "\
plait-web — plait in a browser tab, served from this terminal

  plait-web diff    [REPO] [REVSPEC]   a diff   (default: . , working tree)
  plait-web commits [REPO] [LIMIT]     history  (default: . , 5000)

  --fixtures     read fixtures/ instead of a repository
  --port N       listen on N instead of 7423

Acquisition, the differ, the intraline pass, the highlighter and the wrap all
run here, in this process, on this machine. The browser draws.
";

/// Not 8080: a port collision with whatever else is being developed is a
/// confusing failure, and this one is unassigned and easy to recognise.
const DEFAULT_PORT: u16 = 7423;

enum Source {
    Repo(PathBuf, String),
    Fixtures,
}

fn main() {
    let all: Vec<String> = std::env::args().skip(1).collect();
    if all.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return;
    }

    // `--port` pulled out first so it can appear anywhere, then the rest read
    // positionally the way the shell reads them.
    let mut port = DEFAULT_PORT;
    let mut args: Vec<String> = Vec::new();
    let mut it = all.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--port" => match it.next().and_then(|v| v.parse().ok()) {
                Some(p) => port = p,
                None => fail("--port wants a number"),
            },
            _ => args.push(a),
        }
    }

    let which = args.first().cloned().unwrap_or_else(|| "diff".into());
    if which != "diff" && which != "commits" {
        fail(&format!("no such view: {which}"));
    }
    let source = match args.get(1).map(String::as_str) {
        Some("--fixtures") => Source::Fixtures,
        Some(path) => Source::Repo(PathBuf::from(path), args.get(2).cloned().unwrap_or_default()),
        None => Source::Repo(PathBuf::from("."), args.get(2).cloned().unwrap_or_default()),
    };

    // The shipped host, and not the one `plait.toml` describes.
    //
    // Config parsing lives in `shell/src/config.rs`, which is behind GPUI, so
    // reaching it from here means depending on the window. That is the same
    // latent duplication the unbuilt `cli/` door has, and the fix is the same
    // for both: lift the toml half of that file into a crate `shell`, `cli` and
    // this all depend on. Until then the theme here is `default_dark` and
    // `[diff] algorithm` is not read.
    let host = Host::new();

    let (label, data) = match acquire(&which, &source, &host) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("plait: {e}\n\n{USAGE}");
            std::process::exit(1);
        }
    };

    let state = Arc::new(State { label: label.clone(), host, data });

    // Printed, never opened. A browser window appearing on its own interrupts
    // whoever is at the keyboard — the same reason `./dev.sh` is a rebuild and
    // not a launch.
    println!("plait · {which} · {label}");
    println!("  http://127.0.0.1:{port}/");
    if cfg!(debug_assertions) {
        println!("  DEBUG BUILD — timings meaningless, use --release");
    }

    let serving = state.clone();
    if let Err(e) = http::serve(port, move |req| serving.route(req)) {
        eprintln!("plait: could not listen on 127.0.0.1:{port}: {e}");
        std::process::exit(1);
    }
}

fn fail(message: &str) -> ! {
    eprintln!("plait: {message}\n\n{USAGE}");
    std::process::exit(1);
}

/// Git guarantees no encoding; never fail to show something over one bad byte.
fn read_fixture(path: &str) -> String {
    String::from_utf8_lossy(&std::fs::read(path).unwrap_or_default()).into_owned()
}

fn acquire(which: &str, source: &Source, host: &Host) -> Result<(String, Data), String> {
    match (which, source) {
        ("diff", Source::Repo(repo, revspec)) => {
            let files = plait_git::diff(repo, revspec, &host.differ, &Overrides::default())?;
            if files.is_empty() {
                let what = if revspec.is_empty() { "(working tree)" } else { revspec };
                return Err(format!("no changes for {} {what}", repo.display()));
            }
            Ok((describe(repo, revspec), diff_data(&files, host)))
        }
        ("diff", Source::Fixtures) => {
            let files = plait_core::parse_unified_diff(&read_fixture("fixtures/big.diff"));
            if files.is_empty() {
                return Err("fixtures/big.diff is missing or empty".into());
            }
            Ok(("fixtures".into(), diff_data(&files, host)))
        }
        (_, Source::Repo(repo, limit)) => {
            let n = limit.parse().unwrap_or(5000);
            let commits = plait_git::log(repo, n)?;
            if commits.is_empty() {
                return Err(format!("no commits in {}", repo.display()));
            }
            Ok((plait_git::describe(repo), Data::Commits(commits)))
        }
        (_, Source::Fixtures) => {
            Ok(("fixtures".into(), Data::Commits(plait_core::parse_log(&read_fixture("fixtures/log.txt")))))
        }
    }
}

/// One `prepare` pass, the same call the window and `paint` make. The browser
/// gets its output and redoes none of it.
fn diff_data(files: &[plait_core::FileDiff], host: &Host) -> Data {
    Data::Diff(Mutex::new(Doc::build(prepare(files, &host.syntax, MAX_LINE_CHARS))))
}

fn describe(repo: &Path, revspec: &str) -> String {
    format!("{} {revspec}", plait_git::describe(repo)).trim().into()
}
