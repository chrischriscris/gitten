//! A web server for one reader on loopback.
//!
//! Blocking, a thread per connection, `GET` plus one `POST`, no body parsing
//! beyond the dispatch command. That is not minimalism for its own sake: the
//! whole traffic pattern is one browser asking for windows of rows over
//! localhost plus an agent moving a cursor, which is neither concurrent nor
//! slow, and an async runtime would be the largest dependency in the repository
//! by two orders of magnitude to serve it. `shell` has three dependencies and
//! `core` has none; this has none either.
//!
//! What it is *not* is a server for the internet. It binds loopback, and the
//! things it therefore does not do — TLS, auth, request limits beyond a header
//! cap, any method besides `GET` and the one `POST` — are the reason that is
//! not a knob. The `POST` moves only the server's cursor: nothing reachable
//! through it changes the repository, which is what keeps a rebound request
//! from writing as well as reading — see [`addressed_to_us`].
//!
//! # Loopback is not the boundary it looks like
//!
//! Binding `127.0.0.1` keeps the *network* out. It does not keep a *browser*
//! out, and a browser is the one client this has. Any page the person is
//! visiting can point its own hostname at 127.0.0.1 and come back through the
//! user's own browser, at which point the same-origin policy is on the
//! attacker's side and every route here answers with the contents of a working
//! tree. That is DNS rebinding, it needs no privileged position on the network,
//! and the only thing that stops it is refusing a request whose `Host` is not
//! one this server could legitimately have been reached by — see
//! [`addressed_to_us`].
//!
//! Two headers do the rest, and they are on every response rather than on the
//! HTML because the cost of forgetting one is the whole working tree:
//! `Content-Security-Policy` keeps injected markup from reaching a third-party
//! origin even if something downstream is escaped wrong, and `nosniff` stops a
//! diff of an HTML file being re-interpreted as one.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::time::Duration;

/// Longest request head accepted, headers included. A browser sends about 600
/// bytes; anything past this is not a browser and gets a `431` rather than a
/// buffer that grows until the process dies.
///
/// The same cap bounds a `POST` body: a dispatch is a command name and four
/// integers, and anything past 16 KB of it is not one.
const MAX_HEAD: usize = 16 * 1024;

/// Idle timeout on a kept-alive connection. Long enough that scrolling never
/// pays a reconnect, short enough that closing a tab does not leave threads.
const IDLE: Duration = Duration::from_secs(90);

pub struct Request {
    pub path: String,
    query: String,
    /// `GET`, `POST`, ... The rows and meta routes answer `GET`;
    /// `/api/dispatch` answers `POST`. Anything else is a `405`.
    pub method: String,
    /// The `POST` body, if any. Empty on every `GET`.
    pub body: String,
}

impl Request {
    /// The server builds these from a request target; a test builds one
    /// directly, which is the whole reason this is not just a struct literal —
    /// `query` stays private so nothing outside can hand itself a half-parsed
    /// one.
    pub fn new(path: &str, query: &str) -> Self {
        Self {
            path: decode(path),
            query: query.to_string(),
            method: "GET".into(),
            body: String::new(),
        }
    }

    /// A `POST`, built the way the server builds one: from the target and the
    /// bounded body. A test's stand-in for `curl -d`.
    pub fn post(path: &str, query: &str, body: &str) -> Self {
        Self {
            path: decode(path),
            query: query.to_string(),
            method: "POST".into(),
            body: body.to_string(),
        }
    }

    /// A query parameter, percent-decoded. `None` when it is absent, `Some("")`
    /// when it is present and empty — the caller usually wants those to mean
    /// different things.
    pub fn param(&self, name: &str) -> Option<String> {
        self.query.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (k == name).then(|| decode(v))
        })
    }

    /// A numeric parameter, or `fallback` when it is missing or not a number.
    /// Garbage from a client is not worth a `400`: it means a stale script, and
    /// a sensible window of rows is a more useful reply than an error.
    pub fn number(&self, name: &str, fallback: usize) -> usize {
        self.param(name)
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
    }
}

/// `%20` and `+`. Invalid escapes pass through as themselves rather than
/// erroring: this decodes parameters, not a protocol.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    /// Whether the client may cache this.
    ///
    /// Nothing does. The stylesheet and the script are `include_str!`d into the
    /// binary, so they change exactly when the binary does — and a cached copy
    /// then survives the rebuild that was meant to replace it, which reads as
    /// "the fix did nothing" and costs an hour. Two small files over loopback
    /// per page load is not a cost worth that.
    pub cache: bool,
}

impl Response {
    pub fn json(body: String) -> Self {
        Self {
            status: 200,
            content_type: "application/json; charset=utf-8",
            body: body.into_bytes(),
            cache: false,
        }
    }

    pub fn html(body: &str) -> Self {
        Self {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: body.as_bytes().to_vec(),
            cache: false,
        }
    }

    pub fn css(body: &str) -> Self {
        Self {
            status: 200,
            content_type: "text/css; charset=utf-8",
            body: body.as_bytes().to_vec(),
            cache: false,
        }
    }

    pub fn js(body: &str) -> Self {
        Self {
            status: 200,
            content_type: "text/javascript; charset=utf-8",
            body: body.as_bytes().to_vec(),
            cache: false,
        }
    }

    pub fn status(status: u16, message: &str) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: message.as_bytes().to_vec(),
            cache: false,
        }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Content Too Large",
        422 => "Unprocessable Entity",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    }
}

/// What the page is allowed to do, which is very little.
///
/// `default-src 'none'` and then back only what the document actually uses: its
/// own script and stylesheet, `fetch` to its own origin, and inline `style=`
/// attributes — which every row carries, because a token's colour is resolved
/// per surface and arrives as data. No inline *script* is needed; the page has
/// none.
///
/// `connect-src 'self'` is the one that matters most. Injected script that
/// cannot reach another origin cannot post a working tree to it, so this is what
/// keeps an escaping bug from being an exfiltration bug.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
                   connect-src 'self'; base-uri 'none'; form-action 'none'";

/// Whether a request's `Host` is one this server could have been reached by.
///
/// The defence against DNS rebinding, and it is a whitelist rather than a
/// blocklist because the attacker picks the name: `evil.example` resolving to
/// 127.0.0.1 arrives here indistinguishable from a legitimate request in every
/// respect *except* this header, which carries whatever the browser was asked
/// for. So: the loopback literals and `localhost`, with our own port or no port
/// at all, and nothing else.
///
/// A request with no `Host` at all is refused too. HTTP/1.1 requires one, every
/// browser sends one, and something that does not is not the client this serves.
fn addressed_to_us(head: &str, port: u16) -> bool {
    let Some(host) = header(head, "host") else {
        return false;
    };
    // An IPv6 literal is bracketed, so the last colon outside the brackets is
    // the port separator — splitting on the first colon would cut `[::1]` up.
    let (name, given) = match host.rfind(':') {
        Some(i) if !host[i..].contains(']') => (&host[..i], Some(&host[i + 1..])),
        _ => (host, None),
    };
    if let Some(given) = given {
        if given.parse::<u16>() != Ok(port) {
            return false;
        }
    }
    matches!(name, "127.0.0.1" | "localhost" | "[::1]" | "::1")
}

/// One header's value, matched case-insensitively on the name because a header
/// name is case-insensitive and a client may send `HOST:`.
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// Binds and serves until the process ends.
///
/// `127.0.0.1` and never `0.0.0.0`: this hands out the contents of a working
/// tree with no authentication of any kind, and the difference between those two
/// strings is whether it hands it to the network. Not configurable for that
/// reason.
///
/// # Why the handler never moves
///
/// Connections get a thread each — a browser opens several and keeps them alive,
/// so serving them one after another would have the second wait out the first's
/// idle timeout. But `handler` stays on the thread that called `serve` and every
/// request is posted to it over a channel.
///
/// That is not caution, it is a fact about [`gitten_core::host::Host`]: its three
/// registries are `Box<dyn Differ>`, `Box<dyn Wrap>` and `Box<dyn Highlighter>`
/// with no `Send` bound, because the shell holds an `Rc<Host>` and has never
/// needed one. Sharing it across threads means putting `Send + Sync` on three
/// public extension seams. Posting the work to one thread instead costs a
/// channel hop per request on loopback and keeps `core` exactly as it is.
pub fn serve<H>(port: u16, handler: H) -> std::io::Result<()>
where
    H: Fn(&Request) -> Response,
{
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let (post, jobs) = mpsc::channel::<Job>();

    // Accepting is the only thing that happens off this thread besides reading
    // and writing sockets.
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let post = post.clone();
            // Detached, and a panic in one connection is that connection's
            // problem: a malformed request should not take the server down
            // while someone is reading a diff in another tab.
            std::thread::spawn(move || {
                let _ = connection(stream, &post, port);
            });
        }
    });

    // The handler's whole life, on one thread. Ends when every connection
    // thread and the accept loop have dropped their sender, which is to say
    // never — the process is what ends this.
    //
    // A panic reachable from routing — a third-party `Wrap` or `Highlighter`
    // bug, an index slip — would otherwise unwind out of `serve` and end the
    // process, stalling every browser tab. Catch it here and answer 500 with no
    // internal detail: this thread is the whole server, so its death is the
    // server's, and one bad request is not the others' to pay for.
    for job in jobs {
        let response = catch_unwind(AssertUnwindSafe(|| handler(&job.request)))
            .unwrap_or_else(|_| Response::status(500, "internal error"));
        let _ = job.reply.send(response);
    }
    Ok(())
}

/// One request, and where its answer goes.
struct Job {
    request: Request,
    reply: mpsc::Sender<Response>,
}

fn connection(stream: TcpStream, post: &mpsc::Sender<Job>, port: u16) -> std::io::Result<()> {
    stream.set_read_timeout(Some(IDLE))?;
    stream.set_nodelay(true)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream;
    let (reply, answers) = mpsc::channel::<Response>();

    loop {
        let head = match read_head(&mut reader)? {
            Head::Got(head) => head,
            Head::Closed => return Ok(()),
            // Answered and then *closed*: whatever else is in that oversized
            // head is still in the pipe, and reading it as the next request
            // would parse the tail of one message as the start of another.
            Head::TooLarge => {
                let _ = write(&mut out, &Response::status(431, "request head too large"));
                return Ok(());
            }
        };
        let mut lines = head.lines();
        let Some(start) = lines.next() else {
            return Ok(());
        };
        let mut parts = start.split(' ');
        let method = parts.next().unwrap_or("");
        let target = parts.next().unwrap_or("/");

        let response = if !addressed_to_us(&head, port) {
            // Before the method check and before the route: a rebound request
            // must not learn which routes exist either.
            Response::status(403, "not addressed to this server")
        } else if method == "POST" {
            // Framed by `Content-Length`, which is the only framing this reads:
            // a body without one cannot be told apart from the next request, so
            // it is refused rather than guessed at.
            match read_body(&mut reader, &head)? {
                Body::Got(body) => {
                    let (path, query) = target.split_once('?').unwrap_or((target, ""));
                    let job = Job {
                        request: Request::post(path, query, &body),
                        reply: reply.clone(),
                    };
                    if post.send(job).is_err() {
                        return Ok(());
                    }
                    match answers.recv() {
                        Ok(r) => r,
                        Err(_) => return Ok(()),
                    }
                }
                // Answered and then *closed*: with no trustworthy length the
                // pipe cannot be re-framed, and after an oversized claim the
                // rest of the flood is still arriving.
                Body::NoLength => {
                    let _ = write(
                        &mut out,
                        &Response::status(400, "POST needs a Content-Length and a body"),
                    );
                    return Ok(());
                }
                Body::TooLarge => {
                    let _ = write(
                        &mut out,
                        &Response::status(413, "POST body past 16 KB is not a dispatch"),
                    );
                    return Ok(());
                }
                Body::NotUtf8 => {
                    let _ = write(&mut out, &Response::status(400, "POST body is not UTF-8"));
                    return Ok(());
                }
            }
        } else if method != "GET" {
            Response::status(405, "GET and POST only")
        } else {
            let (path, query) = target.split_once('?').unwrap_or((target, ""));
            let job = Job {
                request: Request::new(path, query),
                reply: reply.clone(),
            };
            // A closed channel means the handler thread is gone, which means the
            // process is going. Nothing useful to answer with.
            if post.send(job).is_err() {
                return Ok(());
            }
            match answers.recv() {
                Ok(r) => r,
                Err(_) => return Ok(()),
            }
        };
        write(&mut out, &response)?;

        // A client that asked to close gets closed. Everything else stays open,
        // which is what keeps a scroll from paying a handshake per window.
        if head.to_ascii_lowercase().contains("connection: close") {
            return Ok(());
        }
    }
}

/// How a read of a request head ended.
///
/// Three outcomes and not an `Option`, because the oversized one is neither a
/// head nor a close: it used to be reported as a synthetic `GET /-too-large`,
/// which got a 404 while the rest of the oversized head stayed in the pipe to be
/// read as the *next* request. The cap has to end the connection, not the
/// message.
enum Head {
    /// A complete request head, up to and including the blank line.
    Got(String),
    /// A clean close, which is what a browser does to an idle keep-alive
    /// connection and is not an error.
    Closed,
    /// Past [`MAX_HEAD`]. Not a browser.
    TooLarge,
}

/// Reads up to the blank line that ends the request head.
///
/// Generic over the reader so a test can drive it from a byte slice rather than
/// a live socket; `connection` still hands it a `BufReader<TcpStream>`.
fn read_head<R: Read>(reader: &mut BufReader<R>) -> std::io::Result<Head> {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        // `read_line` on non-UTF-8 is an error, and a request head that is not
        // UTF-8 is not one we serve.
        //
        // Bounded at `MAX_HEAD`: `read_line` on its own grows `line` until a
        // newline arrives, so one connection sending bytes with no `\n` is an
        // unbounded allocation on loopback that ends in an OOM kill. `take`
        // stops the read at the cap, and a line that hits it is over-long by the
        // check just below — a whole request head fits in far less than one
        // `MAX_HEAD` line.
        let n = match (&mut *reader).take(MAX_HEAD as u64).read_line(&mut line) {
            Ok(n) => n,
            Err(_) => return Ok(Head::Closed),
        };
        if n == 0 {
            return Ok(match head.is_empty() {
                true => Head::Closed,
                false => Head::Got(head),
            });
        }
        if head.len() + n > MAX_HEAD {
            return Ok(Head::TooLarge);
        }
        let blank = line.trim_end_matches(['\r', '\n']).is_empty();
        head.push_str(&line);
        if blank {
            return Ok(Head::Got(head));
        }
    }
}

/// How a `POST` body read ended.
///
/// Four outcomes and not an `Option`, for the same reason [`Head`] is not one:
/// each failure answers differently, and two of them end the connection because
/// the pipe past them cannot be re-framed.
enum Body {
    /// Exactly `Content-Length` bytes, as text.
    Got(String),
    /// No usable `Content-Length`: absent, unparsable, or chunked — the one
    /// framing this does not read.
    NoLength,
    /// Past [`MAX_HEAD`]. Not a dispatch.
    TooLarge,
    /// The framed bytes, but not text.
    NotUtf8,
}

/// Reads exactly `Content-Length` bytes past the head.
///
/// Generic over the reader for the same reason [`read_head`] is: a test drives
/// it from a byte slice rather than a live socket.
fn read_body<R: Read>(reader: &mut BufReader<R>, head: &str) -> std::io::Result<Body> {
    let Some(len) = header(head, "content-length") else {
        return Ok(Body::NoLength);
    };
    // Chunked is a second framing and this reads one. A request carrying both
    // per the spec prefers the length; one carrying only the encoding has no
    // length to read by, which is `NoLength` above.
    let Ok(len) = len.trim().parse::<usize>() else {
        return Ok(Body::NoLength);
    };
    if len > MAX_HEAD {
        return Ok(Body::TooLarge);
    }
    let mut buf = vec![0u8; len];
    if let Err(e) = reader.read_exact(&mut buf) {
        // An EOF mid-body is a client gone away, not an error worth answering:
        // there may be nobody left to read it. Other I/O errors end the
        // connection the same way a closed socket does.
        return match e.kind() {
            std::io::ErrorKind::UnexpectedEof => Ok(Body::NoLength),
            _ => Err(e),
        };
    }
    Ok(match String::from_utf8(buf) {
        Ok(body) => Body::Got(body),
        Err(_) => Body::NotUtf8,
    })
}
/// The response head, as its own function so a test asserts on the bytes that
/// actually go out rather than on a second copy of this format string.
fn head_of(r: &Response) -> String {
    let cache = if r.cache {
        "Cache-Control: max-age=3600\r\n"
    } else {
        "Cache-Control: no-store\r\n"
    };
    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Content-Security-Policy: {CSP}\r\nX-Content-Type-Options: nosniff\r\n{cache}\r\n",
        r.status,
        reason(r.status),
        r.content_type,
        r.body.len(),
    )
}

fn write(out: &mut TcpStream, r: &Response) -> std::io::Result<()> {
    out.write_all(head_of(r).as_bytes())?;
    out.write_all(&r.body)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(query: &str) -> Request {
        Request::new("/api/rows", query)
    }

    #[test]
    fn a_missing_number_falls_back_and_a_present_one_does_not() {
        assert_eq!(req("from=12&count=40").number("from", 0), 12);
        assert_eq!(req("count=40").number("from", 7), 7);
        assert_eq!(req("from=nonsense").number("from", 7), 7);
    }

    #[test]
    fn an_absent_parameter_and_an_empty_one_are_different() {
        assert_eq!(req("wrap=").param("wrap"), Some(String::new()));
        assert_eq!(req("cols=80").param("wrap"), None);
    }

    #[test]
    fn a_percent_escape_and_a_plus_both_decode() {
        assert_eq!(decode("a%20b+c"), "a b c");
        assert_eq!(decode("src%2Fmain.rs"), "src/main.rs");
    }

    #[test]
    fn a_truncated_escape_is_left_alone_rather_than_dropped() {
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
    }

    #[test]
    fn a_parameter_is_matched_whole_and_not_by_prefix() {
        assert_eq!(req("count=40").param("cou"), None);
        assert_eq!(req("recount=1&count=40").param("count"), Some("40".into()));
    }

    fn head(host: &str) -> String {
        format!("GET / HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\n\r\n")
    }

    #[test]
    fn only_a_host_we_could_have_been_reached_by_is_served() {
        for host in [
            "127.0.0.1:7423",
            "localhost:7423",
            "[::1]:7423",
            "127.0.0.1",
        ] {
            assert!(addressed_to_us(&head(host), 7423), "{host} was refused");
        }
        // The rebinding case: the attacker's own name, resolved to loopback. The
        // request is identical to a real one in every way but this.
        for host in [
            "evil.example:7423",
            "evil.example",
            "gitten.attacker.test:7423",
        ] {
            assert!(!addressed_to_us(&head(host), 7423), "{host} was served");
        }
        // A port that is not ours is somebody else's server being proxied at us.
        assert!(!addressed_to_us(&head("127.0.0.1:9999"), 7423));
        // HTTP/1.1 requires a Host; something without one is not a browser.
        assert!(!addressed_to_us("GET / HTTP/1.0\r\n\r\n", 7423));
    }

    #[test]
    fn a_host_header_is_matched_case_insensitively() {
        let raw = "GET / HTTP/1.1\r\nHOST: LocalHost:7423\r\n\r\n";
        // The name is case-insensitive; the value's host part is too, per URL
        // rules, but we only ever produce the lowercase form ourselves.
        assert_eq!(header(raw, "host"), Some("LocalHost:7423"));
    }

    #[test]
    fn the_request_line_is_not_mistaken_for_a_header() {
        // `header` skips line one, or a target containing a colon would read as
        // one — `GET /a:b HTTP/1.1` has a perfectly good `:` in it.
        assert_eq!(header("GET /a:b HTTP/1.1\r\nHost: x\r\n\r\n", "a"), None);
    }

    #[test]
    fn every_response_carries_the_policy_and_nosniff() {
        // On all of them, not just the HTML: a JSON route that forgets is the
        // one an injected script would use.
        for r in [
            Response::json("{}".into()),
            Response::html("<p>"),
            Response::css("a{}"),
            Response::js("0"),
            Response::status(404, "no"),
        ] {
            let head = head_of(&r);
            assert!(
                head.contains("X-Content-Type-Options: nosniff"),
                "{}: no nosniff",
                r.content_type
            );
            assert!(
                head.contains("connect-src 'self'"),
                "{}: exfiltration is not fenced",
                r.content_type
            );
            assert!(head.contains("default-src 'none'"), "{}", r.content_type);
        }
        // No inline script anywhere in the page, so none is allowed.
        assert!(!CSP.contains("script-src 'self' 'unsafe-inline'"));
    }

    #[test]
    fn a_post_request_carries_its_method_and_its_body() {
        let r = Request::post("/api/dispatch", "", "{\"command\":\"view.down\"}");
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/api/dispatch");
        assert!(r.body.contains("view.down"));
        let g = Request::new("/api/rows", "from=0");
        assert_eq!(g.method, "GET");
        assert!(g.body.is_empty());
    }

    fn post_head(len: &str) -> String {
        format!(
            "POST /api/dispatch HTTP/1.1\r\nHost: 127.0.0.1:7423\r\nContent-Length: {len}\r\n\r\n"
        )
    }

    #[test]
    fn a_framed_body_is_read_exactly_and_a_missing_one_is_named() {
        let body = "{\"command\":\"view.down\"}";
        let mut reader = BufReader::new(body.as_bytes());
        match read_body(&mut reader, &post_head(&body.len().to_string())) {
            Ok(Body::Got(got)) => assert_eq!(got, body),
            _ => panic!("a well-framed body did not read"),
        }
        let mut reader = BufReader::new(&b""[..]);
        assert!(matches!(
            read_body(&mut reader, "POST /x HTTP/1.1\r\nHost: y\r\n\r\n"),
            Ok(Body::NoLength)
        ));
        assert!(matches!(
            read_body(&mut reader, &post_head("not-a-number")),
            Ok(Body::NoLength)
        ));
    }

    #[test]
    fn a_body_past_the_cap_is_refused_before_it_is_read() {
        // `read_body` never allocates the claimed length without checking it:
        // the flood below is never sent, only claimed.
        let mut reader = BufReader::new(&b""[..]);
        assert!(matches!(
            read_body(&mut reader, &post_head(&(MAX_HEAD + 1).to_string())),
            Ok(Body::TooLarge)
        ));
        // Exactly the cap is still a body, not a flood.
        let flood = vec![b'{'; MAX_HEAD];
        let mut reader = BufReader::new(&flood[..]);
        assert!(matches!(
            read_body(&mut reader, &post_head(&MAX_HEAD.to_string())),
            Ok(Body::Got(_))
        ));
    }

    #[test]
    fn a_body_that_is_not_text_is_named_rather_than_served() {
        let latin1: &[u8] = b"{\"command\":\"caf\xe9\"}";
        let mut reader = BufReader::new(latin1);
        assert!(matches!(
            read_body(&mut reader, &post_head(&latin1.len().to_string())),
            Ok(Body::NotUtf8)
        ));
    }
    #[test]
    fn a_head_line_with_no_newline_is_bounded_not_grown_forever() {
        // A single line longer than the cap and with no `\n`: the `take` stops
        // the read at `MAX_HEAD` so the buffer never grows without bound, and
        // the accumulated-head check then reports it as too large rather than
        // parsing a flood as a request.
        let flood = vec![b'a'; MAX_HEAD + 100];
        let mut reader = BufReader::new(&flood[..]);
        assert!(matches!(read_head(&mut reader), Ok(Head::TooLarge)));
    }
}
