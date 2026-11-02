//! The window, driven and photographed with no window on screen.
//!
//! `AGENTS.md` forbids launching a client unannounced, and for a person that
//! rule is right: a window appearing takes the keyboard from whoever is
//! typing. It also meant nobody but a person could check the window, so
//! every desktop change ended in a handover. This is the way out that keeps
//! the rule: the same `open_main_window` a launch runs, on GPUI's test
//! platform with its headless Metal renderer behind it. There is no
//! `NSWindow`, no Dock icon and no activation, so there is nothing to take
//! focus *with*. Screen Recording permission isn't needed either: a frame
//! is rendered into a texture and read back, not captured off the display.
//!
//! ```sh
//! ./dev verify -s 'key j' -s 'shot after-j' -s state -- diff . HEAD~1
//! ```
//!
//! What this is not, honestly:
//!
//! - **Not a timing source.** The executor is GPUI's deterministic test
//!   dispatcher and the clock is simulated: frame times from here mean
//!   nothing. `./dev dump`, the bench and `tti` are the numbers.
//! - **Not the platform's input stack.** Keys and clicks enter at
//!   `Window::dispatch_keystroke` / `dispatch_event`, which is where the
//!   platform hands them over, so every binding, hitbox and handler is the
//!   real one; IME composition and the system's own key equivalents are not.
//! - **Not portable yet.** The renderer is GPUI's Metal one; on another
//!   platform `shot` says it has none, and every other step still runs.
//!
//! One run is one launch: the steps run in order, each followed by a settle,
//! and the process exits. Nothing persists — `./dev verify` points
//! `GITTEN_SESSION` and `GITTEN_PROJECTS` at scratch files — so two runs of
//! the same steps start from the same window.

use crate::{DevShell, Launch};
use gpui::{
    point, px, AnyWindowHandle, App, AppContext as _, Entity, Global, HeadlessAppContext,
    Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput,
    ScrollDelta, ScrollWheelEvent, TouchPhase, WeakEntity,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Where a run is asked for. Its presence is the request.
pub(crate) const STEPS: &str = "GITTEN_VERIFY_STEPS";
/// Where `shot` writes. Defaults to `target/verify`.
const OUT: &str = "GITTEN_VERIFY_OUT";

/// Whether this launch is a verify run rather than a window.
pub(crate) fn requested() -> bool {
    std::env::var_os(STEPS).is_some()
}

/// The shell the window built, kept where a step can reach it. Set once, from
/// the window's own constructor, and only in a verify build.
struct Adopted(WeakEntity<DevShell>);
impl Global for Adopted {}

pub(crate) fn adopt(shell: &Entity<DevShell>, cx: &mut App) {
    cx.set_global(Adopted(shell.downgrade()));
}

/// One thing to do to the window. Spelled as a verb and its arguments, one per
/// line or `;`-separated, so a script is something a person can read.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    /// A named command, through the one dispatch path a key reaches.
    Cmd(String),
    /// Space-separated keystrokes, GPUI spelling: `j`, `shift-tab`, `cmd-k`.
    Key(String),
    /// Text, a character at a time, into whatever holds the keyboard.
    Type(String),
    /// A primary click at a point, in logical pixels from the top-left.
    Click(f32, f32),
    /// A wheel at a point, by a pixel delta.
    Scroll(f32, f32, f32, f32),
    /// Simulated time passing, in milliseconds.
    Wait(u64),
    /// The frame as a PNG, named.
    Shot(String),
    /// The shell's state, as data.
    State,
}

impl Step {
    fn spell(&self) -> String {
        match self {
            Step::Cmd(c) => format!("cmd {c}"),
            Step::Key(k) => format!("key {k}"),
            Step::Type(t) => format!("type {t}"),
            Step::Click(x, y) => format!("click {x} {y}"),
            Step::Scroll(x, y, dx, dy) => format!("scroll {x} {y} {dx} {dy}"),
            Step::Wait(ms) => format!("wait {ms}"),
            Step::Shot(n) => format!("shot {n}"),
            Step::State => "state".into(),
        }
    }
}

fn parse(script: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for raw in script.split(['\n', ';']) {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        let nums = |want: usize| -> Result<Vec<f32>, String> {
            let n: Vec<f32> = rest
                .split_whitespace()
                .map(|w| w.parse::<f32>())
                .collect::<Result<_, _>>()
                .map_err(|_| format!("`{line}`: wanted {want} numbers"))?;
            if n.len() == want {
                Ok(n)
            } else {
                Err(format!("`{line}`: wanted {want} numbers, got {}", n.len()))
            }
        };
        let need = |what: &str| -> Result<String, String> {
            if rest.is_empty() {
                Err(format!("`{line}`: `{verb}` needs {what}"))
            } else {
                Ok(rest.to_string())
            }
        };
        steps.push(match verb {
            "cmd" => Step::Cmd(need("a command name, e.g. `cmd workspace.history`")?),
            "key" => Step::Key(need("keystrokes, e.g. `key j j enter`")?),
            "type" => Step::Type(need("text")?),
            "click" => {
                let n = nums(2)?;
                Step::Click(n[0], n[1])
            }
            "scroll" => {
                let n = nums(4)?;
                Step::Scroll(n[0], n[1], n[2], n[3])
            }
            "wait" => Step::Wait(
                rest.parse()
                    .map_err(|_| format!("`{line}`: wanted milliseconds, e.g. `wait 500`"))?,
            ),
            "shot" => {
                let name = need("a name, e.g. `shot history`")?;
                if name.contains(['/', '\\']) || name.starts_with('.') {
                    return Err(format!("`{line}`: a shot name is a file stem, not a path"));
                }
                Step::Shot(name)
            }
            "state" => Step::State,
            _ => {
                return Err(format!(
                    "`{line}`: `{verb}` is not a step — one of cmd, key, type, click, \
                     scroll, wait, shot, state"
                ))
            }
        });
    }
    Ok(steps)
}

/// What one step left behind, for the report.
struct Outcome {
    step: String,
    /// `false` when the step could not be carried out at all — a keystroke
    /// that does not parse, a shot with no renderer. A key nothing answered
    /// is not a failure: that is the window's answer, and `handled` says it.
    ok: bool,
    /// Keys and text only — see [`input`] for why a pointer cannot say.
    handled: Option<bool>,
    error: Option<String>,
    shot: Option<PathBuf>,
    state: Option<String>,
    /// Whether the settle after it ran out of time with work still in flight.
    unsettled: bool,
}

impl Outcome {
    fn new(step: &Step) -> Self {
        Self {
            step: step.spell(),
            ok: true,
            handled: None,
            error: None,
            shot: None,
            state: None,
            unsettled: false,
        }
    }
    fn fail(mut self, error: impl Into<String>) -> Self {
        self.ok = false;
        self.error = Some(error.into());
        self
    }
}

/// Runs the launch headless, then the steps, then exits.
pub(crate) fn run(launch: Launch) -> ! {
    let json = gitten_app::env::format_is_json();
    let steps = match parse(&std::env::var(STEPS).unwrap_or_default()) {
        Ok(steps) => steps,
        Err(e) => fail(json, "bad-step", &e, "see `./dev verify --help`"),
    };
    let out = std::env::var_os(OUT)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/verify"));
    if let Err(e) = std::fs::create_dir_all(&out) {
        fail(
            json,
            "out-dir",
            &format!("cannot create {}: {e}", out.display()),
            "set GITTEN_VERIFY_OUT to a writable directory",
        );
    }

    // A test window has no screen to size itself against, so a run that did
    // not ask for a size gets the one the guide's captures use. Set before
    // the launch reads the doors, so it goes through the same door a person's
    // `GITTEN_QA_GEOM` does.
    if std::env::var_os("GITTEN_QA_GEOM").is_none() {
        std::env::set_var("GITTEN_QA_GEOM", "1280x800");
    }

    // The platform's text system — real shaping, real fonts — under the test
    // platform's executor and windows. Constructing the platform does not
    // start it: no application object is run and nothing is shown.
    let text = gpui_platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(
        text,
        Arc::new(crate::assets::Assets::default()),
        gpui_platform::current_headless_renderer,
    );
    // Acquisition spawns `git` and reads files: real I/O the deterministic
    // dispatcher would otherwise refuse to wait on.
    cx.allow_parking();
    let began = Instant::now();
    cx.update(|cx| crate::open_main_window(launch, cx));
    let launch_settled = settle(&mut cx, Duration::from_millis(400));

    let Some(window) = cx.update(|cx| cx.windows().first().copied()) else {
        fail(
            json,
            "no-window",
            "the launch opened no window",
            "run the same arguments with `./dev gui` to see why",
        )
    };
    let Some(shell) = cx.update(|cx| cx.try_global::<Adopted>().and_then(|a| a.0.upgrade())) else {
        fail(
            json,
            "no-shell",
            "the window has no shell to drive",
            "this build lost `verify::adopt` in open_main_window",
        )
    };

    let mut outcomes = Vec::new();
    for step in &steps {
        let mut outcome = Outcome::new(step);
        match step {
            Step::Cmd(name) => {
                cx.update(|cx| shell.update(cx, |shell, cx| shell.run_command(name, cx)));
            }
            Step::Key(keys) => {
                let mut handled = true;
                for spelled in keys.split_whitespace() {
                    match Keystroke::parse(spelled) {
                        Ok(k) => handled &= keystroke(&mut cx, window, k),
                        Err(e) => {
                            outcome = outcome.fail(format!("`{spelled}` is not a keystroke: {e}"));
                            break;
                        }
                    }
                }
                if outcome.ok {
                    outcome.handled = Some(handled);
                }
            }
            Step::Type(text) => {
                let mut handled = true;
                for ch in text.chars() {
                    let key = match ch {
                        ' ' => "space".to_string(),
                        c => c.to_lowercase().to_string(),
                    };
                    let k = Keystroke {
                        modifiers: Modifiers {
                            shift: ch.is_uppercase(),
                            ..Default::default()
                        },
                        key,
                        key_char: Some(ch.to_string()),
                    };
                    handled &= keystroke(&mut cx, window, k);
                }
                outcome.handled = Some(handled);
            }
            Step::Click(x, y) => {
                let at = point(px(*x), px(*y));
                input(
                    &mut cx,
                    window,
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position: at,
                        pressed_button: None,
                        modifiers: Modifiers::default(),
                    }),
                );
                input(
                    &mut cx,
                    window,
                    PlatformInput::MouseDown(MouseDownEvent {
                        button: MouseButton::Left,
                        position: at,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    }),
                );
                input(
                    &mut cx,
                    window,
                    PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Left,
                        position: at,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                    }),
                );
            }
            Step::Scroll(x, y, dx, dy) => {
                let at = point(px(*x), px(*y));
                input(
                    &mut cx,
                    window,
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position: at,
                        pressed_button: None,
                        modifiers: Modifiers::default(),
                    }),
                );
                // One gesture, three phases: the axis lock the window keeps
                // (`OngoingScroll`) is decided at the start of a gesture and
                // held until its end, the way a trackpad delivers it.
                for (phase, delta) in [
                    (TouchPhase::Started, point(px(0.), px(0.))),
                    (TouchPhase::Moved, point(px(*dx), px(*dy))),
                    (TouchPhase::Ended, point(px(0.), px(0.))),
                ] {
                    input(
                        &mut cx,
                        window,
                        PlatformInput::ScrollWheel(ScrollWheelEvent {
                            position: at,
                            delta: ScrollDelta::Pixels(delta),
                            modifiers: Modifiers::default(),
                            touch_phase: phase,
                        }),
                    );
                }
            }
            Step::Wait(ms) => {
                outcome.unsettled = !settle(&mut cx, Duration::from_millis(*ms));
            }
            Step::Shot(name) => {
                let path = out.join(format!("{name}.png"));
                outcome = match shot(&mut cx, window, &path) {
                    Ok(()) => {
                        outcome.shot = Some(path);
                        outcome
                    }
                    Err(e) => outcome.fail(e),
                };
            }
            Step::State => {
                outcome.state = Some(cx.update(|cx| state(&shell, window, cx)));
            }
        }
        if !matches!(step, Step::Wait(_) | Step::Shot(_) | Step::State) {
            outcome.unsettled = !settle(&mut cx, Duration::from_millis(250));
        }
        outcomes.push(outcome);
    }

    let last = cx.update(|cx| state(&shell, window, cx));
    let failed = outcomes.iter().any(|o| !o.ok);
    if json {
        println!(
            "{}",
            report_json(&out, launch_settled, began, &outcomes, &last)
        );
    } else {
        report_human(&out, launch_settled, began, &outcomes, &last);
    }
    // Leave without unwinding the app: dropping a headless context closes
    // its windows through the platform, and there is nothing to save.
    std::process::exit(if failed { 1 } else { 0 })
}

fn keystroke(cx: &mut HeadlessAppContext, window: AnyWindowHandle, k: Keystroke) -> bool {
    cx.update_window(window, |_, window, cx| window.dispatch_keystroke(k, cx))
        .unwrap_or(false)
}

/// A pointer event, delivered where the platform delivers one. Nothing is
/// reported back: GPUI's click handlers do not stop propagation, so whether
/// one ran is not in the dispatch result. `state` or `shot` after it is how a
/// click says what it did.
fn input(cx: &mut HeadlessAppContext, window: AnyWindowHandle, event: PlatformInput) {
    _ = cx.update_window(window, |_, window, cx| window.dispatch_event(event, cx));
}

/// Lets the window finish what the last step started, and says whether it did.
///
/// Two clocks, because the window runs on both. GPUI's timers — the job pump,
/// the repository watcher's tick, debounces — run on the simulated clock,
/// which only moves when it is told to. A write runs on the job worker's real
/// thread and the watcher's callback on notify's, which no simulated clock
/// reaches. So: tick simulated time in 25 ms steps, give real time a moment
/// each tick, and call it settled once the shell has been idle for both
/// `quiet` of simulated time and 150 ms of real time. Ten seconds of real time
/// is the ceiling; past it the step is reported as unsettled, not hidden.
fn settle(cx: &mut HeadlessAppContext, quiet: Duration) -> bool {
    const TICK: Duration = Duration::from_millis(25);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut idle_sim = Duration::ZERO;
    let mut idle_since: Option<Instant> = None;
    loop {
        cx.run_until_parked();
        cx.advance_clock(TICK);
        cx.run_until_parked();
        let busy = cx.update(|cx| {
            cx.try_global::<Adopted>()
                .and_then(|a| a.0.upgrade())
                .is_some_and(|shell| shell.read(cx).busy())
        });
        if busy {
            idle_sim = Duration::ZERO;
            idle_since = None;
        } else {
            idle_sim += TICK;
            let since = *idle_since.get_or_insert_with(Instant::now);
            if idle_sim >= quiet && since.elapsed() >= Duration::from_millis(150) {
                return true;
            }
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn shot(cx: &mut HeadlessAppContext, window: AnyWindowHandle, path: &Path) -> Result<(), String> {
    // A frame drawn now, not the last one: a step's own refresh may still be
    // marked dirty, and a capture of the frame before it is the capture that
    // lies.
    cx.update_window(window, |_, window, _| window.refresh())
        .map_err(|e| e.to_string())?;
    cx.run_until_parked();
    let image = cx
        .capture_screenshot(window)
        .map_err(|e| format!("no frame to capture: {e}"))?;
    image
        .save(path)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

impl DevShell {
    /// Whether anything the window started is still running — a write, a
    /// refresh wave, a main-view load — or is owed: a change the repository
    /// watcher stamped that no wave has read yet. The last one is what a
    /// person waits out after a write too, so a settle that ignored it would
    /// photograph a window nobody ever sees.
    fn busy(&self) -> bool {
        self.running.is_some()
            || self.refresh_pending > 0
            || self.loading.get()
            || self.repo_changed.load(std::sync::atomic::Ordering::Relaxed) > self.repo_seen
    }
}

/// The shell as one JSON object: what a screenshot shows, as data, for the
/// questions a picture answers badly — which pane has the keyboard, what the
/// band says, whether a write is still running.
fn state(shell: &Entity<DevShell>, window: AnyWindowHandle, cx: &mut App) -> String {
    let size = cx
        .update_window(window, |_, window, _| {
            let s = window.viewport_size();
            (
                f32::from(s.width),
                f32::from(s.height),
                window.scale_factor(),
            )
        })
        .unwrap_or((0.0, 0.0, 1.0));
    let s = shell.read(cx);
    let mut o = Obj::default();
    o.num("width", size.0 as f64);
    o.num("height", size.1 as f64);
    o.num("scale", size.2 as f64);
    o.str(
        "repo",
        s.repo
            .as_ref()
            .map(|(p, _)| p.display().to_string())
            .as_deref(),
    );
    o.str(
        "destination",
        Some(match s.workspace.destination {
            crate::views::workspace::Destination::Changes => "changes",
            crate::views::workspace::Destination::History => "history",
        }),
    );
    o.str(
        "keyboard",
        Some(match s.spot {
            crate::Spot::List => "list",
            crate::Spot::Main => "main",
        }),
    );
    o.str("focusedPane", Some(s.panes.focused_name()));
    o.raw(
        "panes",
        &format!(
            "[{}]",
            s.panes
                .names()
                .zip(s.panes.iter())
                .map(|(name, screen)| {
                    let mut p = Obj::default();
                    p.str("name", Some(name));
                    p.str("mode", Some(screen.mode()));
                    p.str("label", Some(&screen.label(cx)));
                    p.finish()
                })
                .collect::<Vec<_>>()
                .join(",")
        ),
    );
    o.str("mainLabel", Some(&s.main.label(cx)));
    o.str("notice", s.notice.as_ref().map(|n| n.text()));
    o.str("error", s.error.as_ref().map(|e| e.summary.as_ref()));
    o.str("running", s.running.as_ref().map(|(name, _)| name.as_str()));
    o.bool("loading", s.loading.get());
    o.num("generation", s.generation.get() as f64);
    o.num("refreshPending", s.refresh_pending as f64);
    let mut open = Vec::new();
    if s.palette_open {
        open.push("commands");
    }
    if s.theme_picker_open {
        open.push("themes");
    }
    if s.commit_confirm {
        open.push("commit");
    }
    if s.input.is_some() {
        open.push("input");
    }
    if s.show_message {
        open.push("message");
    }
    if s.open.is_some() {
        open.push("menu");
    }
    o.raw(
        "open",
        &format!(
            "[{}]",
            open.iter().map(|n| quote(n)).collect::<Vec<_>>().join(",")
        ),
    );
    o.finish()
}

fn report_json(
    out: &Path,
    settled: bool,
    began: Instant,
    outcomes: &[Outcome],
    last: &str,
) -> String {
    let mut o = Obj::default();
    o.str("out", Some(&out.display().to_string()));
    o.bool("launchSettled", settled);
    o.num(
        "wallMs",
        (began.elapsed().as_secs_f64() * 1e6).round() / 1e3,
    );
    let steps = outcomes
        .iter()
        .map(|s| {
            let mut p = Obj::default();
            p.str("step", Some(&s.step));
            p.bool("ok", s.ok);
            if let Some(h) = s.handled {
                p.bool("handled", h);
            }
            if s.unsettled {
                p.bool("unsettled", true);
            }
            p.str("error", s.error.as_deref());
            p.str(
                "shot",
                s.shot.as_ref().map(|p| p.display().to_string()).as_deref(),
            );
            if let Some(state) = &s.state {
                p.raw("state", state);
            }
            p.finish()
        })
        .collect::<Vec<_>>();
    o.raw("steps", &format!("[{}]", steps.join(",")));
    o.raw("final", last);
    o.finish()
}

fn report_human(out: &Path, settled: bool, began: Instant, outcomes: &[Outcome], last: &str) {
    if !settled {
        println!("launch: still busy after 10 s — the first steps ran against a loading window");
    }
    for s in outcomes {
        let mut line = s.step.clone();
        match (s.ok, s.handled) {
            (false, _) => line.push_str(&format!("  ✗ {}", s.error.as_deref().unwrap_or(""))),
            (true, Some(false)) => line.push_str("  · nothing answered it"),
            _ => {}
        }
        if let Some(p) = &s.shot {
            line.push_str(&format!("  → {}", p.display()));
        }
        if s.unsettled {
            line.push_str("  · still busy after 10 s");
        }
        println!("{line}");
        if let Some(state) = &s.state {
            println!("  {state}");
        }
    }
    println!("final {last}");
    eprintln!(
        "verify: {} steps in {:.1}s, shots in {}",
        outcomes.len(),
        began.elapsed().as_secs_f64(),
        out.display()
    );
}

/// A failure before any step ran: `{error, code, hint}` on stderr, exit 2.
fn fail(json: bool, code: &str, error: &str, hint: &str) -> ! {
    if json {
        let mut o = Obj::default();
        o.str("error", Some(error));
        o.str("code", Some(code));
        o.str("hint", Some(hint));
        eprintln!("{}", o.finish());
    } else {
        eprintln!("verify: {error}\n  {hint}");
    }
    std::process::exit(2)
}

/// A JSON object written by hand, in the house style — see
/// `docs/agent-json.md`. Absent values are omitted rather than `null`.
#[derive(Default)]
struct Obj(Vec<String>);

impl Obj {
    fn str(&mut self, k: &str, v: Option<&str>) {
        if let Some(v) = v {
            self.0.push(format!("{}:{}", quote(k), quote(v)));
        }
    }
    fn num(&mut self, k: &str, v: f64) {
        self.0.push(format!("{}:{}", quote(k), v));
    }
    fn bool(&mut self, k: &str, v: bool) {
        self.0.push(format!("{}:{}", quote(k), v));
    }
    fn raw(&mut self, k: &str, v: &str) {
        self.0.push(format!("{}:{}", quote(k), v));
    }
    fn finish(self) -> String {
        format!("{{{}}}", self.0.join(","))
    }
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
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
    out
}

#[cfg(test)]
mod tests {
    use super::{parse, quote, Step};

    #[test]
    fn a_script_reads_as_lines_or_semicolons_and_skips_comments() {
        let steps = parse("cmd workspace.history; key j j\n# a note\nshot after\nstate").unwrap();
        assert_eq!(
            steps,
            vec![
                Step::Cmd("workspace.history".into()),
                Step::Key("j j".into()),
                Step::Shot("after".into()),
                Step::State,
            ]
        );
    }

    #[test]
    fn a_bad_step_is_said_with_what_was_wanted() {
        let e = parse("clack 1 2").unwrap_err();
        assert!(e.contains("one of cmd, key"), "{e}");
        assert!(parse("click 1").unwrap_err().contains("wanted 2 numbers"));
        assert!(parse("wait soon").unwrap_err().contains("milliseconds"));
        assert!(parse("shot ../escape").unwrap_err().contains("file stem"));
        assert!(parse("cmd").unwrap_err().contains("needs"));
    }

    #[test]
    fn quoting_escapes_what_json_requires() {
        assert_eq!(quote("a\"b\\c\nd\u{1}"), "\"a\\\"b\\\\c\\nd\\u0001\"");
    }
}
