//! gitten in a browser tab, served from the terminal you started it in.
//!
//! Everything above drawing runs here, natively: acquisition spawns `git` the
//! way it always has, and `core` runs the differ, the intraline pass, the
//! highlighter and the wrap. What crosses to the browser is
//! [`gitten_core::prepared`] cut into windows of rows — which is what
//! `Prepared`'s own docs describe as "ready for whatever is going to draw them",
//! and this is the third thing to take them up on after the GPUI view and
//! `core/examples/paint.rs`.
//!
//! It follows that nothing in here needs a wasm target and nothing in `core`
//! had to change. The cost is on the other side: the browser reimplements the
//! drawing, because a `Rows` returns a UI element and that registry is a
//! client's. See `docs/clients.md` for exactly where that line falls.
//!
//! # Two views, one page
//!
//! `/` serves the same document for a diff and for a commit list; which one it
//! is arrives in `meta` and the script branches on it. A second page would be a
//! second copy of the virtual list, the theme and the keys.
//!
//! The commit graph crosses the wire as `gitten_core::graph`'s **plan** — which
//! halves of which lanes exist, which curve pairs with which, which branch is
//! which colour — and the browser turns each half into one SVG path. It is the
//! same plan the window paints as Bézier curves and the terminal paints as box
//! characters, so all three agree about the shape of history and only the
//! arithmetic differs. See [`log::Log`].

pub mod api;
pub mod http;
pub mod json;
pub mod log;
pub mod rows;

use gitten_app::MIN_WRAP_COLS;
use gitten_core::host::Host;
use gitten_core::view::Viewport;
use http::{Request, Response};
use log::Log;
use rows::Doc;
use std::sync::Mutex;

const INDEX: &str = include_str!("../ui/index.html");
const CSS: &str = include_str!("../ui/app.css");
const JS: &str = include_str!("../ui/app.js");

/// The narrowest and widest column budgets a *client* may be asked for.
///
/// [`MIN_WRAP_COLS`] is the shared floor; the ceiling is this crate's own,
/// because only this client takes its budget from a URL. Well past any window —
/// the point is that the number came from outside the process, so a client
/// cannot be asked for a break table the size of the diff squared.
const MAX_WRAP_COLS: usize = 10_000;

pub enum Data {
    /// Behind a `Mutex` because a request can reflow it: the column budget is
    /// the client's to set, and rebuilding the break table mutates.
    Diff(Mutex<Doc>),
    /// Not behind one: a commit list has no width to be cut for, so every
    /// request reads the same resolved graph and nothing mutates.
    Commits(Log),
}

pub struct State {
    pub label: String,
    pub host: Host,
    pub data: Data,
    /// The agent's cursor: what `POST /api/dispatch` moves.
    ///
    /// The browser has none of this — it keeps its own scroll position and asks
    /// for windows of rows — so one viewport per process is enough for the same
    /// reason one `Doc` is: this serves a single reader. Behind a `Mutex`
    /// because a dispatch mutates it, like a reflow mutates the `Doc`.
    view: Mutex<Viewport>,
}

/// How many rows one screenful is before the agent says otherwise.
///
/// The browser never asks — it draws its own height — so this is only the
/// opening value for `POST /api/dispatch`, replaceable per request with
/// `args.height`. Forty rows is a terminal screen, the unit every `page`
/// command in `core` already thinks in.
const DISPATCH_HEIGHT: usize = 40;

/// What a dispatch that names a real command but no headless meaning is told.
///
/// The registry's own one-liner is the `error`; this is the `hint` — where to
/// do it instead, because an agent with nobody at the keyboard needs a next
/// step rather than a refusal.
const WRITES_HINT: &str =
    "this API never changes the repository — run the verb in the terminal that started this server";
const PANES_HINT: &str =
    "this server holds one view — start gitten-web with the other subcommand for the others";
const KEYS_HINT: &str = "GET /api/keys lists the commands this view answers to";

impl State {
    /// One viewport over whichever view was loaded, sized for an agent that
    /// has not named its own height yet.
    pub fn new(label: String, host: Host, data: Data) -> Self {
        let len = match &data {
            Data::Diff(doc) => doc.lock().unwrap_or_else(|p| p.into_inner()).total(),
            Data::Commits(log) => log.len(),
        };
        let mut view = Viewport::new();
        view.set_len(len);
        view.set_height(DISPATCH_HEIGHT);
        view.set_scrolloff(host.view.scrolloff);
        Self {
            label,
            host,
            data,
            view: Mutex::new(view),
        }
    }
}

impl State {
    /// Applies a client's column budget and wrap choice, if they changed.
    ///
    /// Taken on every request that reads rows rather than only on `meta`, so a
    /// window of rows is always consistent with the budget the client asked for
    /// — a client that resized between the two would otherwise be handed rows
    /// cut for the old width and have no way to tell.
    ///
    /// One `Doc` for the whole process, which is the one place this assumes a
    /// single reader: two tabs at different widths would take turns rebuilding
    /// the break table. Sharpening that means a `Doc` per client, and a client
    /// is a concept this does not have yet.
    fn reflow(&self, doc: &mut Doc, req: &Request) {
        let selected = req
            .param("wrap")
            .and_then(|n| self.host.wrap.position(&n))
            .map(|i| self.host.wrap.at(i))
            .unwrap_or_else(|| self.host.wrap.current());
        // Zero is meaningful — it is how `Wrapped` is told never to break this
        // line — so it is let through and only the range above it is clamped.
        let cols = match req.param("cols").and_then(|v| v.parse::<usize>().ok()) {
            Some(0) | None => 0,
            Some(n) => n.clamp(MIN_WRAP_COLS, MAX_WRAP_COLS),
        };
        doc.reflow(cols, selected);
    }

    pub fn route(&self, req: &Request) -> Response {
        // The read routes are `GET`; the cursor is `POST`. A `POST` anywhere
        // else is not a route that exists under another method — it is a
        // client guessing at a write API this deliberately does not have.
        if req.method == "POST" && req.path != "/api/dispatch" {
            return err_json(
                405,
                "only /api/dispatch answers POST",
                "method-not-allowed",
                "GET the rows and meta routes; POST a {\"command\":...} body only to /api/dispatch",
            );
        }
        match (req.path.as_str(), &self.data) {
            // One page for both views. Which one it is arrives in `meta`, and
            // the script branches on it — a second page would be a second copy
            // of the virtual list, the theme and the keys.
            ("/", _) => Response::html(INDEX),
            ("/app.css", _) => Response::css(CSS),
            ("/app.js", _) => Response::js(JS),

            ("/api/meta", Data::Diff(doc)) => {
                let mut doc = doc.lock().unwrap_or_else(|p| p.into_inner());
                self.reflow(&mut doc, req);
                let mut out = String::new();
                api::meta(&mut out, &doc, &self.host, &self.label);
                Response::json(out)
            }
            ("/api/rows", Data::Diff(doc)) => {
                let mut doc = doc.lock().unwrap_or_else(|p| p.into_inner());
                self.reflow(&mut doc, req);
                // Capped: `count` comes from a URL, and a client asking for the
                // whole of a 714k-row diff in one response is a request that
                // should be answered with a window, not with 400 MB.
                let count = req.number("count", 200).min(2000);
                let mut out = String::with_capacity(count * 256);
                api::rows(&mut out, &doc, req.number("from", 0), count);
                Response::json(out)
            }
            ("/api/commits", Data::Commits(log)) => {
                let count = req.number("count", 200).min(2000);
                let mut out = String::with_capacity(count * 256);
                api::commits(&mut out, log, &self.host, req.number("from", 0), count);
                Response::json(out)
            }
            ("/api/meta", Data::Commits(log)) => {
                let mut out = String::new();
                api::commits_meta(&mut out, log, &self.host, &self.label);
                Response::json(out)
            }

            // The agent's door: what is bound, what is configured, whether the
            // server is up. View-independent — a keymap and a catalogue belong
            // to the host, not to the view — so both views answer all three.
            ("/api/keys", _) => {
                let mut out = String::new();
                api::keys(&mut out, &self.host);
                Response::json(out)
            }
            ("/api/config", _) => {
                let mut out = String::new();
                api::config(&mut out, &self.host, &self.label);
                Response::json(out)
            }
            ("/api/health", _) => {
                let mut out = String::new();
                api::health(&mut out);
                Response::json(out)
            }
            ("/api/dispatch", _) => self.dispatch(req),

            // A route that exists for the other view is a different mistake from
            // one that does not exist at all, and saying so is what stops a
            // wrong subcommand looking like a broken build.
            ("/api/rows", _) | ("/api/commits", _) => Response::status(
                404,
                "not this view — start gitten-web with the other subcommand",
            ),
            _ => Response::status(404, "no such route"),
        }
    }

    /// Runs one named command against the loaded view.
    ///
    /// Resolution is [`Commands`](gitten_core::command::Commands)-first: an
    /// unknown name is `unknown-command`, a name from the other view is
    /// `wrong-view`, and a name nothing headless can do is `unavailable` with
    /// somewhere to do it instead. What *runs* is the cursor verbs — `view.*`
    /// and the file walk — because moving a cursor is the whole of what an
    /// agent needs a server for, and everything else is a `hint`.
    fn dispatch(&self, req: &Request) -> Response {
        if req.method != "POST" {
            return err_json(
                405,
                "dispatch takes POST",
                "method-not-allowed",
                "POST {\"command\":\"view.down\"} here; the read routes are GET",
            );
        }
        let (command, args) = match api::parse_dispatch(&req.body) {
            Ok(parsed) => parsed,
            Err(error) => {
                return err_json(
                    400,
                    &error,
                    "bad-request",
                    "POST {\"command\":\"view.down\",\"args\":{\"by\":1}} — the commands are at GET /api/keys",
                );
            }
        };
        if !self.host.commands.known(&command) {
            return err_json(
                404,
                &format!("no such command {command:?}"),
                "unknown-command",
                KEYS_HINT,
            );
        }

        let mut view = self.view.lock().unwrap_or_else(|p| p.into_inner());
        view.set_scrolloff(self.host.view.scrolloff);
        match &self.data {
            Data::Diff(doc) => {
                let doc = doc.lock().unwrap_or_else(|p| p.into_inner());
                view.set_len(doc.total());
                if let Some(height) = args.height {
                    view.set_height(height.clamp(1, 10_000));
                }
                if let Some(row) = args.row {
                    view.go_to(row);
                }
                match command.as_str() {
                    "view.down" => view.move_by(args.by.unwrap_or(1)),
                    "view.up" => view.move_by(-args.by.unwrap_or(1)),
                    "view.page-down" => view.page(args.pages.unwrap_or(1)),
                    "view.page-up" => view.page(-args.pages.unwrap_or(1)),
                    "view.top" => view.to_top(),
                    "view.bottom" => view.to_bottom(),
                    "view.scroll-down" => {
                        view.scroll_by(args.by.unwrap_or(self.host.view.rows as isize));
                    }
                    "view.scroll-up" => {
                        view.scroll_by(-args.by.unwrap_or(self.host.view.rows as isize));
                    }
                    "diff.next-file" => {
                        let at = next_file(&doc, view.cursor(), true);
                        let status = file_status(&doc, at);
                        view.go_to(at);
                        return ok_json(&command, &view, &status);
                    }
                    "diff.prev-file" => {
                        let at = next_file(&doc, view.cursor(), false);
                        let status = file_status(&doc, at);
                        view.go_to(at);
                        return ok_json(&command, &view, &status);
                    }
                    name if name.starts_with("commits.") || name.starts_with("reset") => {
                        return err_json(
                            404,
                            &format!("{command} needs the commits view"),
                            "wrong-view",
                            PANES_HINT,
                        );
                    }
                    _ => return unavailable(&command),
                }
            }
            Data::Commits(log) => {
                view.set_len(log.len());
                if let Some(height) = args.height {
                    view.set_height(height.clamp(1, 10_000));
                }
                if let Some(row) = args.row {
                    view.go_to(row);
                }
                match command.as_str() {
                    "view.down" => view.move_by(args.by.unwrap_or(1)),
                    "view.up" => view.move_by(-args.by.unwrap_or(1)),
                    "view.page-down" => view.page(args.pages.unwrap_or(1)),
                    "view.page-up" => view.page(-args.pages.unwrap_or(1)),
                    "view.top" => view.to_top(),
                    "view.bottom" => view.to_bottom(),
                    "view.scroll-down" => {
                        view.scroll_by(args.by.unwrap_or(self.host.view.rows as isize));
                    }
                    "view.scroll-up" => {
                        view.scroll_by(-args.by.unwrap_or(self.host.view.rows as isize));
                    }
                    name if name.starts_with("diff.") => {
                        return err_json(
                            404,
                            &format!("{command} needs the diff view"),
                            "wrong-view",
                            PANES_HINT,
                        );
                    }
                    _ => return unavailable(&command),
                }
            }
        }
        ok_json(
            &command,
            &view,
            &format!(
                "row {} of {} · top {}",
                view.cursor(),
                view.len(),
                view.top()
            ),
        )
    }
}

/// The first file header after `cursor` — or before it — in visual rows.
///
/// Clamps rather than wraps, like [`Viewport::move_by`](gitten_core::view::Viewport::move_by):
/// past the last file stays on the last file, because a list that jumps from
/// the last file to the first loses the agent's place by the whole diff.
fn next_file(doc: &Doc, cursor: usize, forward: bool) -> usize {
    let mut rows: Vec<usize> = doc.files().iter().map(|e| doc.visual(e.row)).collect();
    rows.sort();
    match forward {
        true => rows.into_iter().find(|&r| r > cursor).unwrap_or(cursor),
        false => rows
            .into_iter()
            .rev()
            .find(|&r| r < cursor)
            .unwrap_or(cursor),
    }
}

/// The status line a file walk answers with: which file the cursor landed on.
fn file_status(doc: &Doc, at: usize) -> String {
    let found = doc
        .files()
        .iter()
        .find(|e| doc.visual(e.row) == at)
        .map(|e| e.path.as_str());
    match found {
        Some(path) => format!("file {path} · row {at} of {}", doc.total()),
        None => format!("row {at} of {}", doc.total()),
    }
}

/// A command that names something real but does nothing headless: which kind
/// of nothing, and where to do it instead.
fn unavailable(command: &str) -> Response {
    let hint = match command {
        "quit" => "close the tab; Ctrl-C the terminal that started this server",
        "help" => "GET /api/keys is the help screen, as data",
        "view.left" | "view.right" => {
            "the web view wraps text instead of scrolling it sideways — see ?wrap= and ?cols="
        }
        "diff.cycle-wrap" => {
            "pass ?wrap= and ?cols= on the rows request instead — the names are at GET /api/config"
        }
        "diff.cycle-layout" => {
            "the browser draws its own presentation; there is no server layout to cycle"
        }
        "theme.cycle" => {
            "the palette rides in every meta payload; pick client-side or restart with another gitten.toml theme"
        }
        "commits.open-diff" => {
            "one server holds one view — start a second gitten-web on that commit"
        }
        name
            if name.starts_with("repo.")
                || name.starts_with("rebase.")
                || name.starts_with("files.stage")
                || name.starts_with("files.commit")
                || name.starts_with("files.amend")
                || name.starts_with("files.discard")
                || name.starts_with("files.ignore")
                || name.starts_with("files.stash")
                || name.starts_with("stashes.")
                || name.starts_with("branches.")
                || name.starts_with("commits.reset-")
                || name.starts_with("commits.revert")
                || name.starts_with("commits.squash")
                || name.starts_with("commits.fixup")
                || name.starts_with("commits.drop")
                || name.starts_with("commits.rebase")
                || name.starts_with("commits.cherry-pick")
                || name.starts_with("commits.new-")
                || name.starts_with("commits.checkout")
                || name.starts_with("diff.stage-")
                || name.starts_with("diff.unstage-")
                || name.starts_with("diff.discard-") =>
        {
            WRITES_HINT
        }
        name if name.ends_with(".focus") || name.ends_with(".search") => PANES_HINT,
        _ => KEYS_HINT,
    };
    err_json(
        422,
        &format!("{command} is not actionable over HTTP"),
        "unavailable",
        hint,
    )
}

/// A dispatch that ran, with the viewport it left behind.
fn ok_json(command: &str, view: &Viewport, status: &str) -> Response {
    let mut out = String::new();
    api::dispatch_ok(&mut out, command, view, status);
    Response::json(out)
}

/// A dispatch that did not run, at the HTTP status its `code` maps to.
fn err_json(status: u16, error: &str, code: &str, hint: &str) -> Response {
    let mut out = String::new();
    api::dispatch_err(&mut out, error, code, hint);
    let mut response = Response::json(out);
    response.status = status;
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitten_core::prepared::prepare;
    use gitten_core::{parse_log, parse_unified_diff};

    fn diff_state() -> State {
        let host = Host::new();
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n-let a = 1;\n+let a = 2;\n fn b() {}\n";
        let doc = Doc::build(prepare(
            &parse_unified_diff(raw),
            &host.syntax,
            gitten_app::MAX_LINE_CHARS,
        ));
        State::new("test".into(), host, Data::Diff(Mutex::new(doc)))
    }

    /// `Request` builds from a target the way the server does, so a test asks
    /// for exactly what a browser would.
    fn get(state: &State, target: &str) -> Response {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        state.route(&Request::new(path, query))
    }

    /// What an agent posts: the command, as JSON, the way the server reads it.
    fn post(state: &State, command: &str, args: &str) -> (u16, String) {
        let r = state.route(&Request::post(
            "/api/dispatch",
            "",
            &format!("{{\"command\":\"{command}\",\"args\":{{{args}}}}}"),
        ));
        (
            r.status,
            String::from_utf8(r.body).expect("a response is UTF-8"),
        )
    }

    fn body(r: Response) -> String {
        String::from_utf8(r.body).expect("a response is UTF-8")
    }

    #[test]
    fn the_page_and_its_assets_are_served() {
        let s = diff_state();
        for path in ["/", "/app.css", "/app.js"] {
            let r = get(&s, path);
            assert_eq!(r.status, 200, "{path}");
            assert!(!r.body.is_empty(), "{path} is empty");
        }
    }

    #[test]
    fn an_unknown_route_is_a_404_and_not_a_panic() {
        assert_eq!(get(&diff_state(), "/nope").status, 404);
    }

    #[test]
    fn asking_the_diff_view_for_commits_says_which_view_it_is() {
        let r = get(&diff_state(), "/api/commits");
        assert_eq!(r.status, 404);
        assert!(body(r).contains("subcommand"));
    }

    #[test]
    fn a_column_budget_from_the_client_reaches_the_wrap() {
        let s = diff_state();
        let wide = body(get(&s, "/api/meta?cols=0"));
        let narrow = body(get(&s, "/api/meta?cols=8&wrap=word"));
        assert!(wide.contains("\"cols\":0"));
        assert!(narrow.contains("\"cols\":8"));
        assert!(narrow.contains("\"selected\":\"word\""));
        // Wrapping at eight columns turns three rows into more than three.
        let rows_of = |s: &str| {
            let at = s.find("\"rows\":").expect("meta reports a row count") + 7;
            s[at..].split(',').next().unwrap().parse::<usize>().unwrap()
        };
        assert!(rows_of(&narrow) > rows_of(&wide), "{narrow}");
    }

    #[test]
    fn an_absurd_column_budget_is_clamped_rather_than_honoured() {
        let s = diff_state();
        assert!(body(get(&s, "/api/meta?cols=1&wrap=char"))
            .contains(&format!("\"cols\":{MIN_WRAP_COLS}")));
        assert!(body(get(&s, "/api/meta?cols=99999999&wrap=char"))
            .contains(&format!("\"cols\":{MAX_WRAP_COLS}")));
    }

    #[test]
    fn an_unknown_wrap_falls_back_to_the_configured_one_rather_than_failing() {
        let s = diff_state();
        let out = body(get(&s, "/api/meta?wrap=nonsense"));
        assert!(out.contains(&format!("\"selected\":\"{}\"", s.host.wrap.selected())));
    }

    #[test]
    fn a_row_window_is_capped_however_much_is_asked_for() {
        let s = diff_state();
        let out = body(get(&s, "/api/rows?from=0&count=999999"));
        assert!(out.starts_with("{\"from\":0,"));
    }

    #[test]
    fn the_commits_view_serves_its_own_rows() {
        let host = Host::new();
        let log = parse_log("aaaa1111\x1faaaa111\x1f\x1fAda Lovelace\x1f1700000000\x1froot\x1e");
        let s = State::new("t".into(), host, Data::Commits(Log::build(log)));
        let out = body(get(&s, "/api/commits?from=0&count=10"));
        assert!(out.contains("\"subject\":\"root\""));
        assert!(out.contains("\"initials\":\"AL\""));
        assert_eq!(get(&s, "/api/rows").status, 404);
        // The page is the same page: which view it is arrives in `meta`.
        assert_eq!(get(&s, "/").status, 200);
        let meta = body(get(&s, "/api/meta"));
        assert!(meta.contains("\"kind\":\"commits\""), "{meta}");
        assert!(meta.contains("\"theme\":"), "the commits view got no theme");
    }

    #[test]
    fn a_poisoned_doc_is_still_served_rather_than_re_panicked() {
        use std::panic::{catch_unwind, AssertUnwindSafe};
        let s = diff_state();
        // Poison the doc mutex the way a handler panicking mid-reflow would: a
        // panic unwinding out of a held guard marks it poisoned. Once the lock
        // was `.expect(...)`, every request after this one panicked in turn.
        if let Data::Diff(m) = &s.data {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let _guard = m.lock().unwrap();
                panic!("a handler panicked while holding the doc");
            }));
            assert!(m.is_poisoned(), "the test did not actually poison the doc");
        } else {
            panic!("diff_state built the wrong Data");
        }
        // The next request recovers the inner `Doc` and answers as normal.
        let r = get(&s, "/api/rows?from=0&count=10");
        assert_eq!(r.status, 200);
        assert!(body(r).starts_with("{\"from\":0,"));
    }

    #[test]
    fn the_agent_routes_are_served_on_both_views() {
        for s in [
            diff_state(),
            State::new(
                "t".into(),
                Host::new(),
                Data::Commits(Log::build(parse_log(
                    "aaaa1111\x1faaaa111\x1f\x1fAda Lovelace\x1f1700000000\x1froot\x1e",
                ))),
            ),
        ] {
            let keys = body(get(&s, "/api/keys"));
            assert!(keys.contains("\"kind\":\"keys\""), "{keys}");
            assert!(keys.contains("\"command\":\"view.down\""), "{keys}");
            let config = body(get(&s, "/api/config"));
            assert!(config.contains("\"kind\":\"config\""), "{config}");
            assert!(config.contains("\"selected\":\"histogram\""), "{config}");
            let health = body(get(&s, "/api/health"));
            assert!(health.contains("\"ok\":true"), "{health}");
            assert!(health.contains("\"service\":\"gitten-web\""), "{health}");
        }
    }

    #[test]
    fn a_dispatch_moves_the_cursor_and_says_where_it_landed() {
        let s = diff_state();
        let (status, out) = post(&s, "view.down", "\"by\":2");
        assert_eq!(status, 200, "{out}");
        assert!(out.contains("\"ok\":true"), "{out}");
        assert!(out.contains("\"command\":\"view.down\""), "{out}");
        assert!(out.contains("\"cursor\":2"), "{out}");
        assert!(out.contains("\"viewport\":"), "{out}");
        assert!(out.contains("\"status\":"), "{out}");
        // And the cursor stays moved: the next dispatch starts from row 2.
        let (status, out) = post(&s, "view.up", "");
        assert_eq!(status, 200, "{out}");
        assert!(out.contains("\"cursor\":1"), "{out}");
        // An absolute row beats a relative one for an agent that just read
        // `?from=` addresses.
        let (status, out) = post(&s, "view.down", "\"row\":0,\"by\":0");
        assert_eq!(status, 200, "{out}");
        assert!(out.contains("\"cursor\":0"), "{out}");
    }

    #[test]
    fn a_dispatch_of_a_file_walk_names_the_file_it_landed_on() {
        let s = diff_state();
        let (status, out) = post(&s, "diff.next-file", "");
        assert_eq!(status, 200, "{out}");
        // One file in the fixture and the cursor opens on it: clamping keeps
        // it there rather than inventing a second file.
        assert!(out.contains("a.rs"), "{out}");
        assert!(out.contains("\"cursor\":0"), "{out}");
    }

    #[test]
    fn a_dispatch_that_cannot_run_names_its_code_and_a_hint() {
        let s = diff_state();
        let (status, out) = post(&s, "frobnicate", "");
        assert_eq!(status, 404, "{out}");
        assert!(out.contains("\"code\":\"unknown-command\""), "{out}");
        assert!(out.contains("\"hint\":"), "{out}");

        let (status, out) = post(&s, "commits.search", "");
        assert_eq!(status, 404, "{out}");
        assert!(out.contains("\"code\":\"wrong-view\""), "{out}");

        let (status, out) = post(&s, "repo.push", "");
        assert_eq!(status, 422, "{out}");
        assert!(out.contains("\"code\":\"unavailable\""), "{out}");
        assert!(out.contains("never changes the repository"), "{out}");

        // A body with no command is a 400, not a guess.
        let r = s.route(&Request::post("/api/dispatch", "", "{}"));
        assert_eq!(r.status, 400);
        assert!(body(r).contains("\"code\":\"bad-request\""));
    }

    #[test]
    fn dispatch_is_post_only_and_post_is_dispatch_only() {
        let s = diff_state();
        // Reading the cursor is not a `GET`: a URL that moves state belongs in
        // no history, prefetch or log.
        let r = get(&s, "/api/dispatch?command=view.down");
        assert_eq!(r.status, 405);
        assert!(body(r).contains("\"code\":\"method-not-allowed\""));
        // And a `POST` to a read route is not a second way to read it.
        let r = s.route(&Request::post("/api/rows", "from=0", ""));
        assert_eq!(r.status, 405);
    }
}
