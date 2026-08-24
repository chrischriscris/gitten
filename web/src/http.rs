//! A web server for one reader on loopback.
//!
//! Blocking, a thread per connection, `GET` only, no body parsing. That is not
//! minimalism for its own sake: the whole traffic pattern is one browser asking
//! for windows of rows over localhost, which is neither concurrent nor slow, and
//! an async runtime would be the largest dependency in the repository by two
//! orders of magnitude to serve it. `shell` has three dependencies and `core`
//! has none; this has none either.
//!
//! What it is *not* is a server for the internet. It binds loopback, and the
//! things it therefore does not do — TLS, auth, request limits beyond a header
//! cap, any write method at all — are the reason that is not a knob.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;

/// Longest request head accepted, headers included. A browser sends about 600
/// bytes; anything past this is not a browser and gets a `431` rather than a
/// buffer that grows until the process dies.
const MAX_HEAD: usize = 16 * 1024;

/// Idle timeout on a kept-alive connection. Long enough that scrolling never
/// pays a reconnect, short enough that closing a tab does not leave threads.
const IDLE: Duration = Duration::from_secs(90);

pub struct Request {
    pub path: String,
    query: String,
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
        404 => "Not Found",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    }
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
                let _ = connection(stream, &post);
            });
        }
    });

    // The handler's whole life, on one thread. Ends when every connection
    // thread and the accept loop have dropped their sender, which is to say
    // never — the process is what ends this.
    for job in jobs {
        let response = handler(&job.request);
        let _ = job.reply.send(response);
    }
    Ok(())
}

/// One request, and where its answer goes.
struct Job {
    request: Request,
    reply: mpsc::Sender<Response>,
}

fn connection(stream: TcpStream, post: &mpsc::Sender<Job>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(IDLE))?;
    stream.set_nodelay(true)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream;
    let (reply, answers) = mpsc::channel::<Response>();

    loop {
        let Some(head) = read_head(&mut reader)? else {
            return Ok(());
        };
        let mut lines = head.lines();
        let Some(start) = lines.next() else {
            return Ok(());
        };
        let mut parts = start.split(' ');
        let method = parts.next().unwrap_or("");
        let target = parts.next().unwrap_or("/");

        let response = if method != "GET" {
            Response::status(405, "GET only")
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

/// Reads up to the blank line that ends the request head. `None` on a clean
/// close, which is what a browser does to an idle keep-alive connection and is
/// not an error.
fn read_head(reader: &mut BufReader<TcpStream>) -> std::io::Result<Option<String>> {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        // `read_line` on non-UTF-8 is an error, and a request head that is not
        // UTF-8 is not one we serve.
        let n = match reader.read_line(&mut line) {
            Ok(n) => n,
            Err(_) => return Ok(None),
        };
        if n == 0 {
            return Ok(if head.is_empty() { None } else { Some(head) });
        }
        if head.len() + n > MAX_HEAD {
            return Ok(Some("GET /-too-large HTTP/1.1".into()));
        }
        let blank = line.trim_end_matches(['\r', '\n']).is_empty();
        head.push_str(&line);
        if blank {
            return Ok(Some(head));
        }
    }
}

fn write(out: &mut TcpStream, r: &Response) -> std::io::Result<()> {
    let cache = if r.cache {
        "Cache-Control: max-age=3600\r\n"
    } else {
        "Cache-Control: no-store\r\n"
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{cache}\r\n",
        r.status,
        reason(r.status),
        r.content_type,
        r.body.len(),
    );
    out.write_all(head.as_bytes())?;
    out.write_all(&r.body)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(query: &str) -> Request {
        Request {
            path: "/api/rows".into(),
            query: query.into(),
        }
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
}
