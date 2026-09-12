//! The page-1 rasterizer for PDFs, and nothing else about blobs.
//!
//! A PDF is not an image and cannot be handed to an image decoder as one: it
//! is a document with a page tree, fonts and a content stream, and drawing one
//! means running a renderer. There is no PDF renderer in this tree and there
//! will not be one — `pdfium`, `mupdf` and `poppler` are all C or C++ and the
//! dependency rule is not worth a document viewer — so a page comes out of
//! whichever tool this machine already has.
//!
//! **Found by name on `PATH`, never by `cfg(target_os)`.** That is plans/003's
//! rule for external tools and it is what makes this honest: a Mac with poppler
//! installed gets poppler's page, a Mac without one gets Quick Look, and a
//! Linux box with ghostscript gets ghostscript. The order is by quality, and
//! the tools are all asked for the same thing — page `n`, at most
//! [`PAGE_PX`] pixels wide, as PNG bytes.
//!
//! Nothing here runs during a frame. A rasterization is one process and a few
//! hundred milliseconds, so it belongs on a job thread — `app::jobs` — and its
//! answer belongs in a cache keyed by the document and the page, which is the
//! caller's half of this contract.
//!
//! Two shapes, because the tools come in two: the ones that read a PDF on
//! standard input and write a PNG on standard output (`mutool`, `gs`), and the
//! ones that only work on paths (Quick Look, `pdftoppm`'s output prefix). The
//! path-shaped half keeps its scratch under the temporary directory — one
//! directory per document, one uniquely-named file per call, so a scratch
//! write can never be steered through a link somebody planted.

use gitten_core::source::DiffSource;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// How wide a rasterized page is asked to be.
///
/// Wide enough that a page fills its pane on a retina display without being
/// asked for again when the window is dragged, and small enough that the PNG
/// is a few hundred kilobytes. A fixed number rather than the pane's width on
/// purpose: a raster that depends on the window would be re-made on every
/// resize, and a resize is a frame, not a load.
pub const PAGE_PX: u32 = 1400;

/// The nominal resolution asked of the tools that take one, in dots per inch.
/// A4 at 150 dpi is 1240 pixels wide, which is [`PAGE_PX`] in document terms.
const DPI: u32 = 150;

/// How long a renderer is given before it is killed.
///
/// Not a performance budget — a *liveness* one, and it is not optional. These
/// are system services and command-line tools run on a caller's job thread, and
/// a single-threaded job queue is blocked behind whatever does not return:
/// Quick Look on a malformed document is a process that never exits, measured
/// on the development machine, and without a deadline it takes every later
/// write in the process with it. Twenty seconds is a page nobody was going to
/// read anyway.
pub const PAGE_TIMEOUT: Duration = Duration::from_secs(20);

/// The most a renderer may answer with. A page at [`PAGE_PX`] is a few
/// hundred kilobytes of PNG; anything past this is not a page somebody asked
/// for but a document attacking its reader, so it is refused rather than
/// held.
const OUT_CAP: u64 = 64 << 20;

/// A renderer found on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rasterizer {
    tool: Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    PdfToPpm,
    Mutool,
    Ghostscript,
    QuickLook,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Tool::PdfToPpm => "pdftoppm",
            Tool::Mutool => "mutool",
            Tool::Ghostscript => "gs",
            Tool::QuickLook => "qlmanage",
        }
    }
}

/// The tools worth having, best first.
///
/// Poppler is the reference implementation of "render this page"; `mutool` is
/// the same quality from mupdf; ghostscript is everywhere and slower; Quick
/// Look is the one every Mac has and is a *thumbnail* at [`PAGE_PX`] square,
/// so a page fitted into it is smaller than the number — which is still a page
/// somebody can read the top of, and better than a sentence saying no renderer
/// was found.
const CANDIDATES: [Tool; 4] = [
    Tool::PdfToPpm,
    Tool::Mutool,
    Tool::Ghostscript,
    Tool::QuickLook,
];

impl Rasterizer {
    /// The renderer this machine has, or `None` when it has none.
    ///
    /// Probed once per process: `PATH` does not change under a running app,
    /// and a lookup is a `stat` per directory per candidate.
    pub fn found() -> Option<Rasterizer> {
        static FOUND: OnceLock<Option<Rasterizer>> = OnceLock::new();
        *FOUND.get_or_init(|| {
            CANDIDATES
                .into_iter()
                .find(|tool| on_path(tool.name()).is_some())
                .map(|tool| Rasterizer { tool })
        })
    }

    pub fn name(&self) -> &'static str {
        self.tool.name()
    }

    /// Page `page` (0-based) of `pdf`, as PNG bytes.
    ///
    /// `key` names the *document* for the scratch half — the blob's object id
    /// where there is one, and anything stable where there is not. The file
    /// inside it is a fresh name per call ([`scratch_stem`]), because the
    /// directory is the document's and the write must be nobody else's.
    pub fn page(&self, pdf: &[u8], key: &str, page: usize) -> Result<Vec<u8>, String> {
        self.page_within(pdf, key, page, PAGE_TIMEOUT)
    }

    /// [`Rasterizer::page`] under a deadline of the caller's choosing. A test
    /// proves the kill path with a short one; nothing else should pass one.
    pub fn page_within(
        &self,
        pdf: &[u8],
        key: &str,
        page: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, String> {
        let first = (page + 1).to_string();
        match self.tool {
            Tool::Mutool => {
                // `-o -` is standard output, `-p n` is one page, `-q` keeps its
                // progress off our stderr.
                let out = run(
                    Command::new("mutool").args([
                        "draw",
                        "-q",
                        "-o",
                        "-",
                        "-r",
                        &DPI.to_string(),
                        "-p",
                        &first,
                        "-",
                    ]),
                    Some(pdf.to_vec()),
                    timeout,
                    OUT_CAP,
                )?;
                png(out)
            }
            Tool::Ghostscript => {
                let out = run(
                    Command::new("gs").args([
                        "-q",
                        "-dNOPAUSE",
                        "-dBATCH",
                        "-dSAFER",
                        "-sDEVICE=png16m",
                        "-dFirstPage",
                        &first,
                        "-dLastPage",
                        &first,
                        "-r",
                        &DPI.to_string(),
                        "-sOutputFile=-",
                        "-",
                    ]),
                    Some(pdf.to_vec()),
                    timeout,
                    OUT_CAP,
                )?;
                png(out)
            }
            Tool::PdfToPpm => {
                // Poppler writes files and nothing else, named after the prefix
                // it was given: `<dir>/<stem>.png` with `-singlefile`.
                let dir = scratch(key)?;
                let stem = scratch_stem();
                let pdf_path = dir.join(format!("{stem}.pdf"));
                let png_path = dir.join(format!("{stem}.png"));
                let result = (|| {
                    write_new(&pdf_path, pdf)?;
                    run(
                        Command::new("pdftoppm")
                            .args([
                                "-png",
                                "-singlefile",
                                "-r",
                                &DPI.to_string(),
                                "-f",
                                &first,
                                "-l",
                                &first,
                            ])
                            .arg(&pdf_path)
                            .arg(dir.join(&stem)),
                        None,
                        timeout,
                        OUT_CAP,
                    )?;
                    let bytes = std::fs::read(&png_path)
                        .map_err(|e| format!("{}: {e}", png_path.display()))?;
                    png(bytes)
                })();
                let _ = std::fs::remove_file(&png_path);
                let _ = std::fs::remove_file(&pdf_path);
                result
            }
            Tool::QuickLook => {
                // The one every Mac has. A thumbnail, sized square, written
                // into a directory beside the document.
                let dir = scratch(key)?;
                let stem = scratch_stem();
                let pdf_path = dir.join(format!("{stem}.pdf"));
                // Quick Look names its answer after the document it read.
                let png_path = dir.join(format!("{stem}.pdf.png"));
                let result = (|| {
                    write_new(&pdf_path, pdf)?;
                    let size = PAGE_PX.to_string();
                    run(
                        Command::new("qlmanage")
                            .args(["-t", "-s", &size, "-o"])
                            .arg(&dir)
                            .arg(&pdf_path),
                        None,
                        timeout,
                        OUT_CAP,
                    )?;
                    // Quick Look answers zero even when it draws nothing, so
                    // the file it names is the check, not the status.
                    let bytes = std::fs::read(&png_path)
                        .map_err(|_| "qlmanage drew no page".to_string())?;
                    png(bytes)
                })();
                let _ = std::fs::remove_file(&png_path);
                let _ = std::fs::remove_file(&pdf_path);
                result
            }
        }
    }
}

/// `Some(path)` when `name` is an executable file on `PATH`.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// A command with the document on its standard input, slurping its standard
/// output — the shape `mutool` and `gs` both take.
/// Runs a renderer: optional document on its standard input, its standard
/// output in hand, and **killed if it does not answer within `timeout`**.
///
/// Both pipes are drained on their own threads, which is the rule every other
/// child in this tree follows: a tool writing its page into a full stdout pipe
/// while we are still writing the document into a full stdin pipe is a
/// deadlock, and a large PDF is exactly the shape that fills both.
///
/// The deadline is what keeps a job queue alive. A short sleep loop around
/// `try_wait` rather than a watchdog thread and a channel: the wait happens on
/// a job thread that has nothing else to do, the poll costs nothing beside the
/// process it is watching, and the alternative is a second thread per
/// rasterization to say one thing.
///
/// The answer is bounded the way the document was: `out_cap` bytes are kept
/// and the rest are drained and dropped — still read, because a child with a
/// full pipe cannot exit — so a pathological page (a point-wide MediaBox at
/// 150 dpi is a megapixel tall) cannot grow this process's memory without
/// limit. stderr keeps a message's worth only; it is read for the error
/// string, not collected.
fn run(
    cmd: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    out_cap: u64,
) -> Result<Vec<u8>, String> {
    use std::io::{Read as _, Write as _};
    let name = cmd.get_program().to_string_lossy().into_owned();
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = cmd.spawn().map_err(|e| format!("{name}: {e}"))?;

    let writer = input.and_then(|bytes| {
        child.stdin.take().map(|mut stdin| {
            std::thread::spawn(move || {
                let _ = stdin.write_all(&bytes);
                // Dropping it closes the pipe, which is what tells a reader on
                // the far end that the document ended.
            })
        })
    });
    // (kept bytes, total seen) — the pipe is always emptied so the child is
    // never blocked on a write, but no more than `keep` is held onto.
    let drain = |mut pipe: Option<Box<dyn std::io::Read + Send>>, keep: usize| {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            let mut total = 0u64;
            let mut chunk = [0u8; 16 * 1024];
            if let Some(pipe) = pipe.as_mut() {
                loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            total += n as u64;
                            let room = keep.saturating_sub(kept.len());
                            if room > 0 {
                                kept.extend_from_slice(&chunk[..n.min(room)]);
                            }
                        }
                    }
                }
            }
            (kept, total)
        })
    };
    let stdout = drain(
        child.stdout.take().map(|p| Box::new(p) as _),
        out_cap as usize,
    );
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as _), 64 * 1024);

    let status = wait_within(&mut child, timeout, &name)?;
    let _ = writer.map(|w| w.join());
    let (out, out_total) = stdout.join().unwrap_or_default();
    let (err, _) = stderr.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("{name}: {}", String::from_utf8_lossy(&err).trim()));
    }
    if out_total > out_cap {
        return Err(format!(
            "{name} answered with more than {out_cap} bytes — a page that large is a document attacking its reader, so it was refused"
        ));
    }
    Ok(out)
}

/// Waits for a child, killing it past `timeout` and reporting that as the
/// failure it is: a renderer that did not answer is not a renderer that
/// answered with nothing.
fn wait_within(child: &mut Child, timeout: Duration, name: &str) -> Result<ExitStatus, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().map_err(|e| format!("{name}: {e}"))? {
            Some(status) => return Ok(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{name} did not answer within {timeout:?} and was killed"
                ));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// The scratch directory for one document, made on demand.
fn scratch(key: &str) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("gitten-blobs").join(key);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// A file stem nobody could have planted.
///
/// On a shared `/tmp` the scratch directory can be one a hostile local user
/// made first, so a predictable name inside it is a symlink we would write
/// through. A per-process counter behind the pid makes the name a guess that
/// cannot be won in advance — and [`write_new`] refuses to follow whatever
/// *is* already there, so a lost race is an error and not an overwrite of
/// somebody else's file.
fn scratch_stem() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NONCE: AtomicU64 = AtomicU64::new(0);
    format!(
        "page-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    )
}

/// Writes a file that did not exist a moment ago, and fails rather than
/// follow a link somebody left where it was going to be.
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut f| f.write_all(bytes))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Checks the renderer's answer really is a PNG, so a tool that "succeeded"
/// with an empty file or an error message is a failure with a reason rather
/// than a blob that decodes to nothing.
fn png(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    const MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.starts_with(MAGIC) {
        Ok(bytes)
    } else {
        Err(format!(
            "the renderer did not answer with a PNG ({} bytes)",
            bytes.len()
        ))
    }
}

/// What a viewer draws for one blob pair: the sides, and their pages.
///
/// Pure data on the way out of this module — a client's job thread builds it
/// and its view draws it, which is the same split every other read in this
/// crate has.
#[derive(Debug)]
pub struct Loaded {
    pub pair: gitten_core::blob::Pair,
    /// A rasterized page one per PDF side, each under its [`page_key`]. The
    /// *new* side's page is what a viewer opens on, and a flip draws the old
    /// side's own page — a flip that redrew the new one's would be showing
    /// the after under the before's name.
    pub pages: Vec<(String, Vec<u8>)>,
    /// Why a page is missing: no renderer on `PATH`, or one that refused. A
    /// sentence for the pane rather than an error, because the bytes are
    /// still there to describe.
    pub note: Option<String>,
}

/// The key a rasterized page is stored and decoded under.
///
/// The document's own identity plus the page number: a page never changes
/// either, so the second visit is a cache hit — and a page of a *different*
/// document can never collide with this one, however similar the bytes.
pub fn page_key(document: &str, page: usize) -> String {
    format!("{document}-page{page}")
}

/// Reads one blob pair, and the first page of it when it is a PDF.
///
/// `cap` is the caller's limit in bytes and travels to the read, where it is
/// enforced before any body is loaded. `path` is raw bytes and required: a
/// blob is one file's content, and the sources that name a whole tree answer
/// for the path asked about.
///
/// The rasterization happens **here**, on whatever thread called this — which
/// is a job thread in every client that has one — and never during a frame.
/// A document with no renderer on the machine is not an error: the pair comes
/// back with a `note` saying so, and the pane draws what it can.
pub fn load(
    source: &DiffSource,
    path: &[u8],
    repo: &dyn gitten_git::Repo,
    cap: u64,
) -> Result<Loaded, String> {
    let pair = repo.blob_pair(source, path, cap)?;
    let mut loaded = Loaded {
        pair,
        pages: Vec::new(),
        note: None,
    };
    // Every held side that is a document gets its own page — a modified PDF
    // flipped to "before" draws *its* raster, not the after one's again. The
    // sides are cloned out of the pair first so the loop can write the pages
    // list while it reads them; a clone is a refcount, not a copy.
    let documents: Vec<gitten_core::blob::Blob> = [&loaded.pair.new, &loaded.pair.old]
        .into_iter()
        .filter_map(|side| side.blob().cloned())
        .filter(|blob| blob.kind == gitten_core::blob::Kind::Pdf)
        .collect();
    if documents.is_empty() {
        return Ok(loaded);
    }
    let Some(rasterizer) = Rasterizer::found() else {
        loaded.note = Some(format!(
            "no PDF renderer on PATH (looked for {}) — the bytes are still here",
            CANDIDATES
                .iter()
                .map(|t| t.name())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        return Ok(loaded);
    };
    // Identical sides (a mode-only change) rasterize once: the key is the
    // document's own identity, so a second pass would buy the same page.
    let mut seen = std::collections::HashSet::new();
    for blob in documents {
        if !seen.insert(blob.key().to_owned()) {
            continue;
        }
        match rasterizer.page(&blob.bytes, blob.key(), 0) {
            Ok(png) => loaded.pages.push((page_key(blob.key(), 0), png)),
            Err(e) => {
                loaded.note = Some(format!("{} could not draw a page: {e}", rasterizer.name()))
            }
        }
    }
    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page of a real document, from bytes this process made: a one-page PDF
    /// with a single line of text on it, written out by hand because the whole
    /// point of this file is not to depend on a PDF library.
    fn one_page_pdf() -> Vec<u8> {
        let content = b"BT /F1 24 Tf 72 700 Td (gitten) Tj ET";
        let mut pdf = Vec::new();
        let mut offsets = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R \
               /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_vec(),
            {
                let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
                stream.extend_from_slice(content);
                stream.extend_from_slice(b"\nendstream");
                stream
            },
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        ];
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    /// The document really is one, which is what makes the test below about
    /// the renderer rather than about a fixture nobody validated.
    #[test]
    fn the_hand_written_page_is_a_pdf() {
        let pdf = one_page_pdf();
        assert_eq!(
            gitten_core::blob::Kind::of(&pdf),
            gitten_core::blob::Kind::Pdf
        );
        assert!(String::from_utf8_lossy(&pdf).contains("/MediaBox [0 0 595 842]"));
    }

    /// Whatever renderer this machine has draws the page, and what comes back
    /// is a PNG. Skipped — loudly — where there is no renderer at all, because
    /// a machine without poppler is not a bug in this file.
    #[test]
    fn a_page_comes_back_as_a_png() {
        let Some(rasterizer) = Rasterizer::found() else {
            eprintln!("no PDF renderer on PATH: the rasterizer test has nothing to run");
            return;
        };
        let pdf = one_page_pdf();
        let page = rasterizer
            .page(&pdf, "gitten-test-page", 0)
            .unwrap_or_else(|e| panic!("{} could not draw the page: {e}", rasterizer.name()));
        assert!(
            page.starts_with(b"\x89PNG\r\n\x1a\n"),
            "{} answered {} bytes that are not a PNG",
            rasterizer.name(),
            page.len()
        );
        // And the scratch half left nothing behind — the names are per-call,
        // so the check is that the directory holds nothing, whichever tool ran.
        let scratch = std::env::temp_dir()
            .join("gitten-blobs")
            .join("gitten-test-page");
        assert!(
            scratch
                .read_dir()
                .map(|mut d| d.next().is_none())
                .unwrap_or(true),
            "a rasterization left its scratch behind"
        );
    }

    /// A renderer that floods its output is refused, not held: the kept half
    /// of the pipe is capped, and the whole of it is still drained so the
    /// child can exit. `cat` is the stand-in — it answers with whatever it is
    /// fed, unboundedly.
    #[test]
    fn an_answer_over_the_cap_is_refused() {
        let big = vec![b'x'; 256 * 1024];
        let err = run(
            &mut Command::new("cat"),
            Some(big.clone()),
            Duration::from_secs(5),
            1024,
        )
        .expect_err("an answer over the cap is not a page");
        assert!(err.contains("more than"), "{err}");

        // And under the cap it is the bytes, whole.
        let out = run(
            &mut Command::new("cat"),
            Some(b"a small honest answer".to_vec()),
            Duration::from_secs(5),
            1024,
        )
        .expect("a small answer");
        assert_eq!(out, b"a small honest answer");
    }

    /// A scratch write lands in a file that did not exist before it — on a
    /// shared temporary directory the predictable name is the attack, and
    /// `create_new` is the refusal of it.
    #[test]
    fn a_scratch_write_never_follows_what_was_planted() {
        let dir = std::env::temp_dir()
            .join("gitten-blobs")
            .join("gitten-test-planted");
        std::fs::create_dir_all(&dir).expect("a scratch dir");
        let planted = dir.join("page-0-0.pdf");
        std::fs::write(&planted, b"planted").expect("planted");
        assert!(
            write_new(&planted, b"ours").is_err(),
            "an existing file is not written through"
        );
        assert_eq!(
            std::fs::read(&planted).expect("read").as_slice(),
            b"planted",
            "and what was there is still there"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Garbage in is a failure with a sentence, not a panic and not a blob:
    /// a document that does not parse is the case every viewer meets first.
    ///
    /// The deadline is short here and not because the answer is expected to be
    /// slow — it is that *one* of these tools answers a document it cannot
    /// read by sitting there, which is the same hang the deadline exists for.
    #[test]
    fn something_that_is_not_a_document_is_an_error() {
        let Some(rasterizer) = Rasterizer::found() else {
            return;
        };
        let err = rasterizer
            .page_within(
                b"this is not a PDF at all",
                "gitten-test-garbage",
                0,
                Duration::from_secs(2),
            )
            .expect_err("garbage is not a page");
        assert!(!err.is_empty(), "the refusal says something: {err}");
    }

    /// A renderer that never answers is killed, and the failure says so — the
    /// liveness rule, tested against a process that really does hang rather
    /// than against whichever tool this machine happens to have.
    #[test]
    fn a_renderer_that_never_answers_is_killed() {
        let mut child = Command::new("sleep")
            .arg("60")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sleep runs");
        let started = Instant::now();
        let err = wait_within(&mut child, Duration::from_millis(300), "sleep")
            .expect_err("a process that never exits is not a page");
        assert!(err.contains("did not answer"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the deadline did not hold: {:?}",
            started.elapsed()
        );
        assert!(
            child.try_wait().expect("reaped").is_some(),
            "the child was killed and left unreaped"
        );
    }

    #[test]
    fn a_renderer_is_found_by_name_and_is_one_of_the_candidates() {
        if let Some(rasterizer) = Rasterizer::found() {
            assert!(
                CANDIDATES.iter().any(|t| t.name() == rasterizer.name()),
                "{} is not a candidate",
                rasterizer.name()
            );
            assert!(
                on_path(rasterizer.name()).is_some(),
                "a renderer was reported that is not on PATH"
            );
        }
    }

    /// The lookup is a lookup: a name nothing installs is not found, and the
    /// non-executable case is refused rather than run.
    #[test]
    fn a_lookup_finds_what_is_there_and_nothing_else() {
        assert!(on_path("gitten-no-such-tool").is_none());
        let dir = std::env::temp_dir().join(format!("gitten-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let plain = dir.join("gitten-not-executable");
        std::fs::write(&plain, b"#!").expect("written");
        assert!(!is_executable(&plain), "a mode without x is not a tool");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            !is_executable(Path::new("/")),
            "a directory is not an executable"
        );
    }
}
