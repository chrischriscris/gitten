//! `plait-web` — acquire in the terminal, read in a browser.
//!
//! The arguments are every other client's, because they come from the same
//! place: see `plait_app::cli`. What this adds is `--port`.

use plait_app::acquire::Data as Loaded;
use plait_app::cli::{self, View};
use plait_app::{Startup, MAX_LINE_CHARS};
use plait_core::prepared::prepare;
use plait_web::log::Log;
use plait_web::rows::Doc;
use plait_web::{http, Data, State};
use std::rc::Rc;
use std::sync::Mutex;

const EXTRA: &str = "  --port N       listen on N instead of 7423

  Acquisition, the differ, the intraline pass, the highlighter and the wrap all
  run here, in this process, on this machine. The browser draws.
";

/// Not 8080: a port collision with whatever else is being developed is a
/// confusing failure, and this one is unassigned and easy to recognise.
const DEFAULT_PORT: u16 = 7423;

fn main() {
    let mut start = Startup::new("plait-web", View::Diff)
        .blurb("plait in a browser tab, served from this terminal")
        .extra(EXTRA);

    // Taken before the shared parse sees it, so `--port` may appear anywhere on
    // the line rather than only where a positional parser looks for it.
    let port = match cli::take_value(start.take(), "--port") {
        Ok(Some(v)) => match v.parse::<u16>() {
            Ok(p) => p,
            Err(_) => fail(&start, &format!("--port wants a number, not {v:?}")),
        },
        Ok(None) => DEFAULT_PORT,
        Err(e) => fail(&start, &e),
    };

    let started = match start.go() {
        Ok(started) => started,
        Err(exit) => exit.finish(),
    };
    let which = started.view;
    let label = started.loaded.label.clone();

    // One `prepare` pass, the same call every other client makes. The browser
    // gets its output and redoes none of it.
    let data = match started.loaded.data {
        Loaded::Diff(files) => Data::Diff(Mutex::new(Doc::build(prepare(
            &files,
            &started.host.syntax,
            MAX_LINE_CHARS,
        )))),
        Loaded::Commits(commits) => Data::Commits(Log::build(commits)),
    };
    // `Rc`, not `Arc`: requests run on the serving thread (see http.rs) and the
    // host behind this state is deliberately not Send. An Arc here is the same
    // type with a promise nothing keeps.
    let state = Rc::new(State {
        label: label.clone(),
        host: started.host,
        data,
    });

    // Printed, never opened. A browser window appearing on its own interrupts
    // whoever is at the keyboard — the same reason `./dev` is a rebuild and not
    // a launch.
    //
    // **The URL is the last thing printed**, and that is not a layout
    // preference. A terminal that turns URLs into links takes everything up to
    // whitespace, and a note on the line *after* it gets swallowed: printing
    // the build warning second produced `127.0.0.1:7423/DEBUG`, which the
    // server answered honestly with "no such route".
    let build = match cfg!(debug_assertions) {
        true => "  ·  debug build, timings meaningless — use --release",
        false => "",
    };
    println!("plait · {} · {label}{build}", which.name());
    println!("  http://127.0.0.1:{port}/");

    let serving = state.clone();
    if let Err(e) = http::serve(port, move |req| serving.route(req)) {
        eprintln!("plait-web: could not listen on 127.0.0.1:{port}: {e}");
        std::process::exit(1);
    }
}

fn fail(start: &Startup, message: &str) -> ! {
    eprintln!("plait-web: {message}\n\n{}", start.usage());
    std::process::exit(1)
}
