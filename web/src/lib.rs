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
//! frontend's. See `docs/` for where that seam wants to move.

pub mod api;
pub mod http;
pub mod json;
pub mod rows;

use http::{Request, Response};
use plait_core::host::Host;
use plait_core::Commit;
use rows::Doc;
use std::sync::Mutex;

const INDEX: &str = include_str!("../ui/index.html");
const CSS: &str = include_str!("../ui/app.css");
const JS: &str = include_str!("../ui/app.js");

/// The commits view has an endpoint and no drawing.
///
/// Served instead of the diff page rather than letting the script fail on a
/// payload with no theme in it: a blank window that says nothing is the worst of
/// the three options, and claiming a view exists because its data does is the
/// second worst.
const NOT_DRAWN: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>plait</title>\
<style>body{background:#181614;color:#a39c93;font:14px ui-monospace,monospace;padding:3rem;line-height:1.6}\
code{color:#d9c98f}</style></head><body>\
<p>The commit graph is served but not drawn yet: <code>GET /api/commits?from=0&amp;count=200</code> \
returns the rows, lanes included, from <code>core</code>&rsquo;s own <code>assign_lanes</code>.</p>\
<p>What is missing is the gutter &mdash; the curves live in <code>shell/src/graph.rs</code> and want \
porting to SVG. For a diff instead: <code>plait-web diff [REPO] [REVSPEC]</code>.</p>\
</body></html>";

/// How wide a row may get before it is clipped. The shell's `MAX_LINE_CHARS`,
/// and the same reasoning: a rendering budget, owned by the frontend, applied by
/// `core`. A 9.6-million-character line was measured in the wild and nobody
/// reads past column 2000.
pub const MAX_LINE_CHARS: usize = 2000;

/// Narrowest budget a client can ask to wrap at, mirroring the shell's
/// `MIN_WRAP_COLS`. A budget of one character is a row per character and a row
/// count that grows without bound.
const MIN_WRAP_COLS: usize = 8;

/// Widest, so a client cannot ask for a `Wrapped` the size of the diff squared.
/// Well past any window; the point is that the number came from a URL.
const MAX_WRAP_COLS: usize = 10_000;

pub enum Data {
    /// Behind a `Mutex` because a request can reflow it: the column budget is
    /// the client's to set, and rebuilding the break table mutates.
    Diff(Mutex<Doc>),
    Commits(Vec<Commit>),
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
            ("/", Data::Diff(_)) => Response::html(INDEX),
            ("/", Data::Commits(_)) => Response::html(NOT_DRAWN),
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
            ("/api/commits", Data::Commits(all)) => {
                let count = req.number("count", 200).min(2000);
                let mut out = String::with_capacity(count * 128);
                api::commits(&mut out, all, req.number("from", 0), count);
                Response::json(out)
            }
            ("/api/meta", Data::Commits(all)) => {
                let mut out = String::new();
                api::commits(&mut out, all, 0, 0);
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
        let doc = Doc::build(prepare(&parse_unified_diff(raw), &host.syntax, MAX_LINE_CHARS));
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
        let s = State { label: "t".into(), host, data: Data::Commits(log) };
        let out = body(get(&s, "/api/commits?from=0&count=10"));
        assert!(out.contains("\"subject\":\"root\""));
        assert!(out.contains("\"initials\":\"AL\""));
        assert_eq!(get(&s, "/api/rows").status, 404);
    }
}
