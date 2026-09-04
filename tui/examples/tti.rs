//! Time to interactive, measured headlessly.
//!
//! ```sh
//! cargo run -q -p gitten-tui --example tti --release [REPO]
//! GITTEN_BASELINE=~/old/target/release/gitten-tui \
//!   cargo run -q -p gitten-tui --example tti --release [REPO]
//! ```
//!
//! Two roads, one number each:
//!
//! - The **terminal** runs on a private pty — `Term::enter` refuses anything
//!   else — with `GITTEN_START_LOG=1`, so its stderr carries the same
//!   `gitten-start:` stages the window prints. `spawn → first frame flushed`
//!   is the terminal's TTI: the stage the list the launch asked for has been
//!   interactive since. `spawn → startup frame flushed` adds the deferred
//!   sidebars and the preview; a binary older than the deferral never prints
//!   it, and absence is reported, not an error. `q` on the pty master ends
//!   the run.
//! - The **desktop** is the wall clock around `target/release/gitten-shell`
//!   under `GITTEN_START_QUIT=1` — the client ends itself at the first rows
//!   (`app/src/env.rs`), and the clock around the process is the number. A
//!   window does appear, for however long the road takes; that is the
//!   measurement, not a side effect. The side runs only when the binary
//!   exists (skipped with a note otherwise) and `GITTEN_TTI_SHELL` is on;
//!   `check.sh` turns it off because it opens no windows.
//!
//! With a baseline (`GITTEN_BASELINE`, optionally `GITTEN_BASELINE_SHELL`)
//! the rounds are ABBA-interleaved, the starting side flips every round and
//! the figure is the median — `docs/measurements.md` has the why; naive
//! back-to-back A/B has swung +25–95% on this codebase. One warmup per side
//! is run and discarded. **The suite advises; it never gates.** `deltaPct`
//! against the baseline is the entire conclusion. The only enforcement is
//! opt-in: set `GITTEN_TTI_MAX_FIRST_FRAME_MS` (and `..._FILLED_MS`,
//! `..._SHELL_MS`) and a median past its ceiling exits non-zero; unset, every
//! run exits 0.
//!
//! `--json` (or `GITTEN_FORMAT=json`) prints one object, schema
//! `gitten.tti/1`, with the same conventions as `dump`: null where a side was
//! skipped, sample arrays beside each median. Human mode is grep-friendly
//! medians.
//!
//! No crate gains a dependency for this: the pty is `openpty` and the slave
//! fds are `dup`, both declared `extern "C"` below and both already linked —
//! libSystem on the Mac, glibc on Linux (≥ 2.34, which folded `openpty` in
//! from libutil). `script -q` was measured as a std-only replacement and
//! cannot work: it buffers its typescript and flushes at exit, which erases
//! exactly the timestamps the measurement exists for.

use gitten_app::env;
use std::fs::File;
use std::io::ErrorKind;
use std::os::raw::{c_char, c_int};
use std::os::unix::io::FromRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The stage the launch's own list is interactive at. `GITTEN_START_LOG`
/// prints it as `gitten-start: first frame flushed in …`.
const FIRST: &str = "first frame flushed";
/// The stage the deferred sidebars and preview land on.
const FILLED: &str = "startup frame flushed";
/// How long past the first marker we wait for the filled one before deciding
/// it is absent — an old binary's shape, not a hang. The marker rides the
/// frame after the first, so it is there in milliseconds when it is coming.
const FILLED_GRACE: Duration = Duration::from_secs(2);
/// What a wedged child gets. The pty read below blocks, so something outside
/// it has to hold the stick; 60 s is well past any honest first frame.
const KILL_AFTER: Duration = Duration::from_secs(60);

/// The JSON string escaper, the same one `dump` and the core examples carry:
/// structural characters and C0 escaped, everything else passed through.
fn jstr(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
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

/// A sample array, numbers at one decimal — the medians' raw material.
fn narray(out: &mut String, first: &mut bool, k: &str, xs: &[f64]) {
    key(out, first, k);
    out.push('[');
    for (i, x) in xs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("{x:.1}"));
    }
    out.push(']');
}

/// A fatal failure: `{error, code, hint}` on stderr, then out with status 1.
fn fail(json: bool, human: &str, code: &str, error: &str, hint: &str) -> ! {
    if json {
        let mut out = String::from("{");
        let mut first = true;
        for (k, v) in [("error", error), ("code", code), ("hint", hint)] {
            key(&mut out, &mut first, k);
            jstr(&mut out, v);
        }
        out.push('}');
        eprintln!("{out}");
    } else {
        eprintln!("{human}");
    }
    std::process::exit(1);
}

extern "C" {
    /// The pty pair. Linked by `std` itself on every target this repo builds
    /// for; see the module comment for why there is no crate behind it.
    fn openpty(
        master: *mut c_int,
        slave: *mut c_int,
        name: *mut c_char,
        term: *const std::os::raw::c_void,
        win: *const Winsize,
    ) -> c_int;
    fn dup(fd: c_int) -> c_int;
}

/// The window size a pty is born with — `COLS`/`ROWS`, the same defaults
/// `dump` draws at, so both clients launch into a comparable screen.
#[repr(C)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

/// A second handle on a child, for the watchdog: the pty read blocks, so the
/// stick has to be held from outside the read.
struct Guard {
    child: Arc<Mutex<Child>>,
    done: Arc<AtomicBool>,
}

impl Guard {
    fn over(child: Child) -> Self {
        let child = Arc::new(Mutex::new(child));
        let done = Arc::new(AtomicBool::new(false));
        let (c, d) = (child.clone(), done.clone());
        std::thread::spawn(move || {
            let step = Duration::from_millis(200);
            let mut waited = Duration::ZERO;
            while waited < KILL_AFTER && !d.load(Ordering::Relaxed) {
                std::thread::sleep(step);
                waited += step;
            }
            if !d.load(Ordering::Relaxed) {
                if let Ok(mut c) = c.lock() {
                    let _ = c.kill();
                }
            }
        });
        Self { child, done }
    }

    /// The exit status if it has landed, `None` while it has not.
    fn status(&self) -> Option<std::process::ExitStatus> {
        if let Ok(mut c) = self.child.lock() {
            return c.try_wait().ok().flatten();
        }
        None
    }

    /// Releases the watchdog. Every path through a run calls this; a wedged
    /// child was already killed by it.
    fn finish(&self) {
        self.done.store(true, Ordering::Relaxed);
    }
}

/// One terminal run's timings, both relative to the spawn.
#[derive(Clone, Copy)]
struct TuiRun {
    first: f64,
    filled: Option<f64>,
}

/// The desktop run's timing, one number.
type ShellRun = f64;

/// A side of the comparison: the binaries it owns and the samples it earns.
/// `current` and `baseline` are the only two there are.
#[derive(Clone, Copy)]
struct Side<'a> {
    label: &'a str,
    tui: &'a Path,
    shell: Option<&'a Path>,
}

impl<'a> Side<'a> {
    /// The prefix this side's figures carry in JSON: `""` for the current
    /// side, `baseline` for the other — `baselineTuiFirstFrameMs` and friends.
    fn key_prefix(&self) -> &'a str {
        match self.label {
            "current" => "",
            _ => "baseline",
        }
    }
}

/// Runs `binary commits <repo>` on a fresh pty and times its road to the
/// first frame. The whole stderr rides the pty too — the terminal owns it
/// behind the alternate screen as much as it owns stdout — so the markers
/// arrive interleaved with the frames and are read out of the same stream.
fn tui_run(binary: &Path, repo: &str) -> Result<TuiRun, String> {
    use std::io::{Read, Write};
    let mut master_fd: c_int = 0;
    let mut slave_fd: c_int = 0;
    let win = Winsize {
        ws_row: env::rows() as u16,
        ws_col: env::cols() as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe {
        openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null(),
            &win,
        )
    } != 0
    {
        return Err("openpty failed".into());
    }
    // Three handles on the slave, one per stdio — `dup` twice so every fd is
    // closed exactly once, by the `File` that owns it.
    let out_fd = unsafe { dup(slave_fd) };
    let err_fd = unsafe { dup(slave_fd) };
    if out_fd < 0 || err_fd < 0 {
        return Err("dup failed".into());
    }
    let t0 = Instant::now();
    let child = Command::new(binary)
        .arg("commits")
        .arg(repo)
        .stdin(Stdio::from(unsafe { File::from_raw_fd(slave_fd) }))
        .stdout(Stdio::from(unsafe { File::from_raw_fd(out_fd) }))
        .stderr(Stdio::from(unsafe { File::from_raw_fd(err_fd) }))
        .env("GITTEN_START_LOG", "1")
        .spawn()
        .map_err(|e| format!("could not spawn {}: {e}", binary.display()))?;
    let guard = Guard::over(child);

    let mut master = unsafe { File::from_raw_fd(master_fd) };
    let mut chunk = [0u8; 4096];
    let mut seen = String::new();
    let (mut first, mut filled, mut first_at) = (None, None, None);
    loop {
        match master.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let now = t0.elapsed().as_secs_f64() * 1000.0;
                seen.push_str(&String::from_utf8_lossy(&chunk[..n]));
                if first.is_none() && seen.contains(FIRST) {
                    first = Some(now);
                    first_at = Some(Instant::now());
                } else if first.is_some() && filled.is_none() {
                    if seen.contains(FILLED) {
                        filled = Some(now);
                        break;
                    }
                    // An old binary never prints the filled marker; the grace
                    // window decides absence. The marker rides the frame
                    // after the first, so absence is decidable, not a wait.
                    if first_at.is_some_and(|at| at.elapsed() > FILLED_GRACE) {
                        break;
                    }
                }
                // The stream also carries every repaint; keep a tail wide
                // enough for a marker split across chunks, and no wider.
                if seen.len() > 8192 {
                    let cut = seen.len() - 256;
                    seen.replace_range(..cut, "");
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            // The child is gone; a pty master reads EIO for it.
            Err(_) => break,
        }
    }
    let Some(first) = first else {
        guard.finish();
        let tail = seen.lines().rev().find(|l| !l.trim().is_empty());
        let tail = tail
            .map(|l| l.trim())
            .unwrap_or("")
            .chars()
            .take(120)
            .collect::<String>();
        return Err(format!(
            "no `{FIRST}` marker in {:.0?}; last output: {tail:?}",
            t0.elapsed()
        ));
    };

    // Quit, then keep draining until the exit: a client that keeps drawing
    // must never block on a full pty with `q` unread behind it.
    let _ = master.write_all(b"q");
    loop {
        match master.read(&mut chunk) {
            Ok(0) => break,
            Ok(_) => {
                if let Some(status) = guard.status() {
                    if !status.success() {
                        return Err(format!("{} exited {status}", binary.display()));
                    }
                    break;
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    guard.finish();
    Ok(TuiRun { first, filled })
}

/// The desktop: `GITTEN_START_QUIT=1` around the shell binary, wall clock.
/// The client quits itself at the first rows; nothing here waits for a human.
fn shell_run(binary: &Path, repo: &str) -> Result<ShellRun, String> {
    let t0 = Instant::now();
    let mut child = Command::new(binary)
        .arg("commits")
        .arg(repo)
        .env("GITTEN_START_QUIT", "1")
        .env("GITTEN_START_LOG", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not spawn {}: {e}", binary.display()))?;
    let end = Instant::now() + KILL_AFTER;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(format!("{} exited {status}", binary.display()));
                }
                return Ok(t0.elapsed().as_secs_f64() * 1000.0);
            }
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "the window never reached its first rows; killed after {:?}",
                    KILL_AFTER
                ));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// The middle sample, the median for an odd count — which is what `ROUNDS`
/// defaults to — and the upper of the two middle ones for an even count.
/// Same answer `statistics.median` gives whenever the count is odd.
fn median(mut xs: Vec<f64>) -> Option<f64> {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if xs.is_empty() {
        None
    } else {
        Some(xs[xs.len() / 2])
    }
}

/// The binary beside this example: `target/release/examples/tti` sits next to
/// `target/release/gitten-tui`, wherever `CARGO_TARGET_DIR` put the tree.
fn sibling_bin(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.parent()?.to_path_buf();
    let bin = dir.join(name);
    bin.is_file().then_some(bin)
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let json = env::wants_json(&raw);
    let args = env::strip_json_arg(&raw);
    let repo = args.first().map(String::as_str).unwrap_or(".");
    let rounds = env::rounds(7);
    let settle = env::settle(1.0);
    if rounds == 0 {
        fail(
            json,
            "gitten: ROUNDS must be >= 1",
            "usage",
            "ROUNDS must be >= 1",
            "set ROUNDS to the runs per side you want the median over",
        );
    }

    // The current side's binaries, both release siblings of this example.
    // This example does not build them: it times what exists, and the run
    // command above says which build produces them.
    let tui = match sibling_bin("gitten-tui") {
        Some(p) => p,
        None => fail(
            json,
            "gitten: no gitten-tui beside this example",
            "build",
            "no release gitten-tui beside the example",
            "cargo build -q --release -p gitten-tui, then run this again",
        ),
    };
    let shell = sibling_bin("gitten-shell");

    // The baseline side, whole or not at all: a named binary that is missing
    // is a caller's mistake, not a side to quietly drop.
    let baseline_tui = match env::baseline() {
        Some(p) if p.is_file() => Some(p),
        Some(p) => fail(
            json,
            &format!("gitten: GITTEN_BASELINE={} is not a file", p.display()),
            "usage",
            &format!("GITTEN_BASELINE={:?} is not a file", p),
            "point GITTEN_BASELINE at a built gitten-tui of the vintage to compare",
        ),
        None => None,
    };
    let baseline_shell = match env::baseline_shell() {
        Some(p) if p.is_file() => Some(p),
        Some(p) => fail(
            json,
            &format!(
                "gitten: GITTEN_BASELINE_SHELL={} is not a file",
                p.display()
            ),
            "usage",
            &format!("GITTEN_BASELINE_SHELL={:?} is not a file", p),
            "point GITTEN_BASELINE_SHELL at a built gitten-shell of the vintage to compare",
        ),
        None => None,
    };

    let measure_shell = env::tti_shell();
    let mut notes: Vec<String> = Vec::new();
    let shell = match (&shell, measure_shell) {
        (Some(p), true) => Some(p),
        (None, true) => {
            notes.push(
                "no target/release/gitten-shell beside the example; the desktop side is skipped \
                 (build it: cargo build -q --release -p gitten-shell)"
                    .into(),
            );
            None
        }
        (_, false) => {
            notes.push("desktop side off (GITTEN_TTI_SHELL=0)".into());
            None
        }
    };

    let current = Side {
        label: "current",
        tui: &tui,
        shell: shell.map(PathBuf::as_path),
    };
    let sides: Vec<Side> = match &baseline_tui {
        Some(b) => vec![
            current,
            Side {
                label: "baseline",
                tui: b,
                shell: baseline_shell.as_deref().map(Path::new),
            },
        ],
        None => vec![current],
    };

    // One warmup per measured figure, run and discarded: the first spawn of a
    // binary pays page cache and dyld, and measurements.md's discipline is
    // that the timed rounds start warm. The desktop side warms too — a window
    // appears for it, as it does for every shell run here.
    for side in &sides {
        let name = side_name(side);
        tui_run(side.tui, repo)
            .map_err(|e| {
                fail(json, &format!("gitten: {name}: {e}"), "run", &e,
                "the launch itself failed; check the repository path and that both binaries build")
            })
            .unwrap();
        if let Some(sh) = side.shell {
            let name = format!("{name} shell");
            shell_run(sh, repo)
                .map_err(|e| fail(json, &format!("gitten: {name}: {e}"), "run", &e,
                    "the window never came up or exited non-zero; run the shell binary once by hand"))
                .unwrap();
        }
    }

    // The rounds. The starting side flips every round — ABBA — so VM-reclaim
    // noise from one run does not always land on the same other run; the
    // settle gap goes between every two consecutive runs.
    let mut samples: Vec<Vec<Vec<f64>>> = sides
        .iter()
        .map(|_| vec![Vec::new(), Vec::new(), Vec::new()])
        .collect();
    for r in 1..=rounds {
        let order: Vec<Side> = match r % 2 == 1 {
            true => sides.clone(),
            false => sides.iter().rev().copied().collect(),
        };
        let last = r == rounds;
        for (i, side) in order.iter().enumerate() {
            let idx = sides.iter().position(|s| s.label == side.label).unwrap();
            let name = side_name(side);
            let run = tui_run(side.tui, repo)
                .map_err(|e| {
                    fail(
                        json,
                        &format!("gitten: {name}: {e}"),
                        "run",
                        &e,
                        "the launch itself failed mid-run; check the repository path",
                    )
                })
                .unwrap();
            samples[idx][0].push(run.first);
            if let Some(f) = run.filled {
                samples[idx][1].push(f);
            }
            if let Some(sh) = side.shell {
                let name = format!("{name} shell");
                let wall = shell_run(sh, repo)
                    .map_err(|e| {
                        fail(
                            json,
                            &format!("gitten: {name}: {e}"),
                            "run",
                            &e,
                            "the window never came up mid-run",
                        )
                    })
                    .unwrap();
                samples[idx][2].push(wall);
            }
            let more = !(last && i + 1 == order.len());
            if more {
                std::thread::sleep(Duration::from_secs_f64(settle));
            }
        }
    }

    let medians: Vec<[Option<f64>; 3]> = samples
        .iter()
        .map(|s| {
            [
                median(s[0].clone()),
                median(s[1].clone()),
                median(s[2].clone()),
            ]
        })
        .collect();
    let delta = match (
        medians.first().and_then(|m| m[0]),
        medians.get(1).and_then(|m| m[0]),
    ) {
        (Some(a), Some(b)) if b > 0.0 => Some((a - b) / b * 100.0),
        _ => None,
    };

    if json {
        let mut out = String::from("{");
        let mut first = true;
        sfield(&mut out, &mut first, "schema", "gitten.tti/1");
        sfield(&mut out, &mut first, "repo", repo);
        nfield(&mut out, &mut first, "rounds", rounds);
        nfield(&mut out, &mut first, "settleSec", format!("{settle:.1}"));
        sfield(&mut out, &mut first, "profile", "release");
        let figures = ["tuiFirstFrameMs", "tuiFilledMs", "shellWallMs"];
        for (i, side) in sides.iter().enumerate() {
            for (f, m) in figures.iter().copied().zip(medians[i]) {
                // `baseline` glues on with a capital: baselineTuiFirstFrameMs.
                let name = match side.key_prefix() {
                    "" => f.to_string(),
                    prefix => {
                        let (head, rest) = f.split_at(1);
                        format!("{prefix}{}{rest}", head.to_uppercase())
                    }
                };
                match m {
                    Some(m) => {
                        nfield(&mut out, &mut first, &name, format!("{m:.1}"));
                        narray(
                            &mut out,
                            &mut first,
                            &format!("{name}Samples"),
                            &samples[i][f_idx(f)],
                        );
                    }
                    None => {
                        key(&mut out, &mut first, &name);
                        out.push_str("null");
                    }
                }
            }
        }
        match delta {
            Some(d) => nfield(&mut out, &mut first, "deltaPct", format!("{d:+.1}")),
            None => {
                key(&mut out, &mut first, "deltaPct");
                out.push_str("null");
            }
        }
        if !notes.is_empty() {
            let arr: Vec<String> = notes.clone();
            key(&mut out, &mut first, "notes");
            out.push('[');
            for (i, n) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                jstr(&mut out, n);
            }
            out.push(']');
        }
        out.push('}');
        println!("{out}");
    } else {
        println!("tti {repo} ({rounds} rounds, median)");
        for note in &notes {
            println!("  note: {note}");
        }
        let figures = ["tui first frame", "tui filled frame", "shell wall"];
        for (i, side) in sides.iter().enumerate() {
            for (f, m) in figures.iter().copied().zip(medians[i]) {
                match m {
                    Some(m) => println!("  {}{f:<17} {m:9.1}ms", side_prefix(side)),
                    None => {
                        let absent = match f {
                            "tui filled frame" => "absent (no marker; older binary?)",
                            _ => "skipped",
                        };
                        println!("  {}{f:<17} {absent}", side_prefix(side));
                    }
                }
            }
        }
        if let Some(d) = delta {
            println!("  delta first frame {d:+.1}% (current vs baseline)");
        }
    }

    // The one enforcement there is, and only where a caller pinned it: a
    // ceiling passed, the run says so and exits non-zero. Unset ceilings
    // never fire, which is what keeps the suite advisory by default.
    let ceilings = [
        (
            "tui first frame",
            "GITTEN_TTI_MAX_FIRST_FRAME_MS",
            medians[0][0],
        ),
        (
            "tui filled frame",
            "GITTEN_TTI_MAX_FILLED_MS",
            medians[0][1],
        ),
        ("shell wall", "GITTEN_TTI_MAX_SHELL_MS", medians[0][2]),
    ];
    for (what, name, m) in ceilings {
        let Some(limit) = env::ceiling(name) else {
            continue;
        };
        match m {
            Some(m) if m > limit => {
                eprintln!(
                    "gitten: {what} median {m:.1}ms exceeds {name}={limit:.1} — set by the caller, so this run fails"
                );
                std::process::exit(1);
            }
            _ => {}
        }
    }
}

/// The label human lines carry for a side — the current side's lines are
/// bare, the baseline's are prefixed.
fn side_prefix(side: &Side) -> &'static str {
    match side.label {
        "current" => "",
        _ => "baseline ",
    }
}

/// The same in a failure message, where "side" alone says nothing.
fn side_name(side: &Side) -> String {
    match side.label {
        "current" => "current binary".into(),
        _ => "baseline binary".into(),
    }
}

/// The samples slot a figure name sits in — the order of `figures` above.
fn f_idx(name: &str) -> usize {
    match name {
        "tuiFirstFrameMs" => 0,
        "tuiFilledMs" => 1,
        _ => 2,
    }
}
