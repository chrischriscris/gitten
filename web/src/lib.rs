//! plait in a browser tab, served from the terminal you started it in.
//!
//! Everything above drawing runs here, natively: acquisition spawns `git` the
//! way it always has, and `core` runs the differ, the intraline pass, the
//! highlighter and the wrap. What crosses to the browser is
//! [`plait_core::prepared`] cut into windows of rows — which is what
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
//! The commit graph crosses the wire as `plait_core::graph`'s **plan** — which
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

use http::{Request, Response};
use plait_app::MIN_WRAP_COLS;
use plait_core::host::Host;
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
        match (req.path.as_str(), &self.data) {
            // One page for both views. Which one it is arrives in `meta`, and
            // the script branches on it — a second page would be a second copy
            // of the virtual list, the theme and the keys.
            ("/", _) => Response::html(INDEX),
            ("/app.css", _) => Response::css(CSS),
            ("/app.js", _) => Response::js(JS),

            ("/api/meta", Data::Diff(doc)) => {
                let mut doc = doc.lock().expect("no request panics while holding the doc");
                self.reflow(&mut doc, req);
                let mut out = String::new();
                api::meta(&mut out, &doc, &self.host, &self.label);
                Response::json(out)
            }
            ("/api/rows", Data::Diff(doc)) => {
                let mut doc = doc.lock().expect("no request panics while holding the doc");
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

            // A route that exists for the other view is a different mistake from
            // one that does not exist at all, and saying so is what stops a
            // wrong subcommand looking like a broken build.
            ("/api/rows", _) | ("/api/commits", _) => {
                Response::status(404, "not this view — start plait-web with the other subcommand")
            }
            _ => Response::status(404, "no such route"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plait_core::prepared::prepare;
    use plait_core::{parse_log, parse_unified_diff};

    fn diff_state() -> State {
        let host = Host::new();
        let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n-let a = 1;\n+let a = 2;\n fn b() {}\n";
        let doc =
            Doc::build(prepare(&parse_unified_diff(raw), &host.syntax, plait_app::MAX_LINE_CHARS));
        State { label: "test".into(), host, data: Data::Diff(Mutex::new(doc)) }
    }

    /// `Request` builds from a target the way the server does, so a test asks
    /// for exactly what a browser would.
    fn get(state: &State, target: &str) -> Response {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        state.route(&Request::new(path, query))
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
        assert!(body(get(&s, "/api/meta?cols=1&wrap=char")).contains(&format!("\"cols\":{MIN_WRAP_COLS}")));
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
        let s = State { label: "t".into(), host, data: Data::Commits(Log::build(log)) };
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
}
