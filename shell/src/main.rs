mod config;
mod controls;
mod graph;
mod session;
mod stats;
mod views;

use gpui::*;
use gpui_component::*;
use plait_core::host::Host;
use plait_core::differ::{Overrides, Whitespace};
use plait_core::FileDiff;
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use stats::Stats;

#[global_allocator]
static ALLOC: stats::Counting = stats::Counting;

actions!(plait, [Quit]);

use views::diff::{CycleLayout, CycleWrap};



const USAGE: &str = "\
plait — a git client

  plait commits [REPO] [LIMIT]     history graph      (default: . , 5000)
  plait diff    [REPO] [REVSPEC]   a diff             (default: . , working tree)
  plait config                     print the current theme and font as TOML

  REVSPEC is anything git takes:  HEAD~50..HEAD   main..feature   <sha>
  Pass --fixtures instead of REPO to read fixtures/ instead of a repository.

  The title bar carries four pickers: the presentation (unified, side-by-side),
  where a line too wide for the window breaks (off, word, char), the diff
  algorithm (histogram, patience, myers) and how much whitespace has to match
  (exact, trailing, change, all — git's default, --ignore-space-at-eol, -b and
  -w). `s` cycles the presentation and `w` the wrap. [diff] in plait.toml sets
  what they open on, plus `context`, `moves` and `indent_heuristic`.

  plait.toml next to the binary (or $PLAIT_CONFIG) is re-read every time it is
  saved, and colours and font apply on the next frame — no rebuild, no relaunch.
  Start one with:  plait config > plait.toml

  ./dev.sh <args>  rebuild and relaunch on every source change, landing back
                   on the row you were reading. Debug build and the overlay by
                   default; pass --release before trusting a timing.

  PLAIT_STATS=1   frame, row and heap overlay
";

/// How to acquire the diff again with a different algorithm.
///
/// A closure and not a repository, because the shell does no I/O and must not
/// learn what one is: `main` captures the source and hands over the single
/// operation the control needs. The live [`Host`] is passed *in* rather than
/// captured, so a config reload cannot leave a stale registry behind it.
///
/// `None` on `DevShell` means the source cannot be re-diffed at all — a `.diff`
/// fixture was diffed by somebody else — and the control is drawn inert.
type Rediff = Rc<dyn Fn(&Host, &Overrides) -> Result<Vec<FileDiff>, String>>;

/// Which picker is open. At most one, because two open menus over a diff is two
/// things to dismiss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Open {
    Layout,
    Wrap,
    Algorithm,
    Whitespace,
}

struct DevShell {
    title: SharedString,
    view: AnyView,
    stats: Option<Stats>,
    /// The diff view, when that is what is on screen. Held so the control strip
    /// can drive it — the view itself stays a data-in view and never learns the
    /// strip exists.
    diff: Option<Entity<views::diff::Diff>>,
    rediff: Option<Rediff>,
    /// The live picks. Every field `None` means "whatever the config selected",
    /// which is what the controls show until somebody changes one — so the strip
    /// agrees with `plait.toml` rather than with a copy of it taken at startup.
    over: Overrides,
    open: Option<Open>,
    /// A failed re-diff. Shown, not swallowed: the usual cause is a repository
    /// that moved under the window, and silently keeping the old rows would be a
    /// diff labelled with an algorithm that did not produce it.
    error: Option<SharedString>,
}

impl DevShell {
    /// Re-acquires the diff under `next` and swaps it in.
    ///
    /// The whole cost is one acquisition plus one `prepare` — 40–120 ms and
    /// 8–250 ms respectively, on a click. Cheap enough not to need a spinner and
    /// not cheap enough to do on a keystroke repeat, which is why these are menus
    /// and only the layout is bound to a key.
    fn set_overrides(&mut self, next: Overrides, cx: &mut Context<Self>) {
        let (Some(rediff), Some(diff)) = (self.rediff.clone(), self.diff.clone()) else {
            return;
        };
        if next == self.over {
            return;
        }
        let host = config::host(cx);
        match rediff(&host, &next) {
            Ok(files) => {
                self.over = next;
                self.error = None;
                diff.update(cx, |d, cx| d.replace(files, &host, cx));
                let load = diff.read(cx).load.clone();
                if let Some(stats) = &mut self.stats {
                    stats.reloaded(load);
                }
            }
            // The old rows stay on screen, which is the right failure: they are
            // still a true diff, just not the one that was asked for.
            Err(e) => self.error = Some(e.into()),
        }
        cx.notify();
    }

    /// Costs no re-diff and no `prepare` — only where the lines break moves —
    /// which is why this one needs none of `set_overrides`' machinery.
    fn set_wrap(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(diff) = self.diff.clone() else { return };
        let host = config::host(cx);
        diff.update(cx, |d, cx| d.set_wrap(index, &host, cx));
        cx.notify();
    }

    fn set_layout(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(diff) = self.diff.clone() else { return };
        // A layout change is a fresh look at the same diff, so a message about
        // an algorithm that failed to load is no longer describing the screen.
        self.error = None;
        let host = config::host(cx);
        diff.update(cx, |d, cx| d.set_layout(index, &host, cx));
        let load = diff.read(cx).load.clone();
        if let Some(stats) = &mut self.stats {
            stats.reloaded(load);
        }
        cx.notify();
    }

    /// The pickers, right-aligned in the title bar. Nothing when the diff view is
    /// not what is on screen — the commit graph has none of these to choose.
    ///
    /// Each one is the same shape: a list of names from a registry or an enum,
    /// and an index into it. That is why adding a presentation or an algorithm
    /// needs no work here.
    fn strip(&self, host: &Host, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(diff) = &self.diff else { return Vec::new() };
        let me = cx.entity().downgrade();

        // `Fn(bool)` per picker rather than one shared handler: which menu is
        // open is one field, and the closure is what knows which one it is.
        let toggle = |which: Open| {
            let me = me.clone();
            move |next: bool, _: &mut Window, cx: &mut App| {
                _ = me.update(cx, |this, cx| {
                    this.open = next.then_some(which);
                    cx.notify();
                });
            }
        };
        // Every override-driven pick is the same two lines: close, then re-diff.
        let pick_over = |build: fn(&Overrides, usize) -> Overrides| {
            let me = me.clone();
            move |i: usize, _: &mut Window, cx: &mut App| {
                _ = me.update(cx, |this, cx| {
                    this.open = None;
                    let next = build(&this.over, i);
                    this.set_overrides(next, cx);
                });
            }
        };

        let names = diff.read(cx).layout_names();
        let layouts = controls::Picker::new("layout", &names, diff.read(cx).layout_index());

        // Straight off the registry in `core`, so a wrap an extension registers
        // is in this menu the day it exists.
        let wrap_names = diff.read(cx).wrap_names(host);
        let wrap = controls::Picker::new("wrap", &wrap_names, diff.read(cx).wrap_index());

        let algorithms = host.differ.names();
        let selected = self.over.algorithm.as_deref().unwrap_or(host.differ.selected());
        let algorithm = controls::Picker::new(
            "algorithm",
            &algorithms,
            algorithms.iter().position(|n| *n == selected).unwrap_or(0),
        )
        .enabled(self.rediff.is_some());

        let ws_names: Vec<&str> = Whitespace::ALL.iter().map(|w| w.name()).collect();
        let ws = self.over.whitespace.unwrap_or(host.differ.whitespace);
        let whitespace = controls::Picker::new(
            "whitespace",
            &ws_names,
            Whitespace::ALL.iter().position(|w| *w == ws).unwrap_or(0),
        )
        .enabled(self.rediff.is_some());

        vec![
            controls::picker(
                "layout-picker",
                &layouts,
                self.open == Some(Open::Layout),
                &host.theme,
                &host.font,
                toggle(Open::Layout),
                {
                    let me = me.clone();
                    move |i, _, cx| {
                        _ = me.update(cx, |this, cx| {
                            this.open = None;
                            this.set_layout(i, cx);
                        });
                    }
                },
            ),
            controls::picker(
                "wrap-picker",
                &wrap,
                self.open == Some(Open::Wrap),
                &host.theme,
                &host.font,
                toggle(Open::Wrap),
                {
                    let me = me.clone();
                    move |i, _, cx| {
                        _ = me.update(cx, |this, cx| {
                            this.open = None;
                            this.set_wrap(i, cx);
                        });
                    }
                },
            ),
            controls::picker(
                "algorithm-picker",
                &algorithm,
                self.open == Some(Open::Algorithm),
                &host.theme,
                &host.font,
                toggle(Open::Algorithm),
                {
                    // The registry's own order, so an extension's differ is
                    // reachable here the day it is registered.
                    let names: Vec<String> = algorithms.iter().map(|s| s.to_string()).collect();
                    let me = me.clone();
                    move |i, _, cx| {
                        let Some(name) = names.get(i).cloned() else { return };
                        _ = me.update(cx, |this, cx| {
                            this.open = None;
                            let next =
                                Overrides { algorithm: Some(name), ..this.over.clone() };
                            this.set_overrides(next, cx);
                        });
                    }
                },
            ),
            controls::picker(
                "whitespace-picker",
                &whitespace,
                self.open == Some(Open::Whitespace),
                &host.theme,
                &host.font,
                toggle(Open::Whitespace),
                pick_over(|over, i| Overrides {
                    whitespace: Whitespace::ALL.get(i).copied(),
                    ..over.clone()
                }),
            ),
        ]
    }
}

impl Render for DevShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let overlay = self.stats.as_mut().map(|s| {
            s.tick();
            (s.frames(), s.rows(), s.heap(), s.load.clone())
        });
        if overlay.is_some() {
            window.request_animation_frame();
        }

        // The live host, read per frame, not the one captured when this was
        // built. It was the captured one, which meant the window chrome and the
        // *font for the whole window* silently did not hot-reload while every
        // view inside it did — the exact trap `docs/extending.md` warns about,
        // in the one place nobody looked.
        let host = config::host(cx);
        let c = host.theme.chrome;
        let f = &host.font;
        let strip = self.strip(&host, cx);
        let error = self.error.clone();

        div()
            .size_full()
            .v_flex()
            .bg(rgb(c.bg))
            .text_color(rgb(c.fg))
            // From the host, not a constant: `text_sm` was `rems(0.875)` — 14px —
            // and the family was hardcoded here while three other things
            // depended on which font it was.
            .text_size(px(f.size))
            .font_family(f.family.clone())
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(32.))
                    .px_4()
                    .bg(rgb(c.title_bg))
                    .text_color(rgb(c.dim))
                    .child(div().flex_none().child(self.title.clone()))
                    .children(
                        error.map(|e| div().flex_none().text_color(rgb(c.error)).child(e)),
                    )
                    // Pushes the controls to the right edge and takes the clicks
                    // that land between them, so a stray click on the title bar
                    // does not fall through to whatever is under it.
                    .child(div().flex_grow(1.0))
                    .children(strip),
            )
            .child(div().flex_grow(1.0).overflow_hidden().child(self.view.clone()))
            .children(overlay.map(|(frames, rows, heap, load)| {
                div()
                    .flex_none()
                    .v_flex()
                    .px_4()
                    .py_2()
                    .gap_1()
                    .bg(rgb(c.status_bg))
                    .text_color(rgb(c.dim))
                    .child(
                        div()
                            .flex()
                            .gap_6()
                            .child(div().text_color(rgb(c.accent)).child(frames))
                            .child(rows)
                            .child(heap),
                    )
                    .child(div().text_color(rgb(c.faint)).child(load))
            }))
    }
}

enum Source {
    Repo(PathBuf, String),
    Fixtures,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return;
    }
    let which = args.first().cloned().unwrap_or_else(|| "commits".into());

    // `plait config > plait.toml` — a complete, correct starting file rather than
    // a page of documentation to copy out of. No window, so it comes first.
    if which == "config" {
        let mut h = Host::new();
        let path = config::path();
        for w in config::load(&mut h, &path) {
            eprintln!("plait: {w}");
        }
        print!("{}", config::dump(&h));
        return;
    }

    let source = match args.get(1).map(String::as_str) {
        Some("--fixtures") => Source::Fixtures,
        Some(path) => Source::Repo(PathBuf::from(path), args.get(2).cloned().unwrap_or_default()),
        None => Source::Repo(PathBuf::from("."), args.get(2).cloned().unwrap_or_default()),
    };

    // Names this exact view, so a saved scroll position is only ever restored
    // into the diff it was taken in — see `session.rs`.
    let session_key = match &source {
        Source::Repo(repo, revspec) => {
            session::Session::key(&which, &repo.to_string_lossy(), revspec)
        }
        Source::Fixtures => session::Session::key(&which, "--fixtures", ""),
    };
    let session_path = session::path();

    // One host, built before anything reads it: the highlighters, the differs,
    // the theme, the font. An extension registering itself does it here, and
    // every view reads the same struct.
    //
    // Before acquisition and not inside `app.run`, because the host is what
    // chooses the diff algorithm now — building it after the diff had been
    // fetched would leave `[diff] algorithm` describing nothing.
    let config_path = config::path();
    let mut built = Host::new();
    for w in config::load(&mut built, &config_path) {
        eprintln!("plait: {w}");
    }
    let host = Rc::new(built);

    // How to fetch the diff again with a different algorithm. Built here, where
    // the source is known, so nothing downstream has to learn what a repository
    // is. `None` for a `.diff` fixture and for the commit graph — neither has an
    // algorithm to choose, and the control says so by being inert.
    let rediff: Option<Rediff> = match (which.as_str(), &source) {
        ("diff", Source::Repo(repo, revspec)) => {
            let (repo, revspec) = (repo.clone(), revspec.clone());
            Some(Rc::new(move |host: &Host, over: &Overrides| {
                plait_git::diff(&repo, &revspec, &host.differ, over)
            }))
        }
        _ => None,
    };

    // Acquire before opening a window: a git error should print and exit, not
    // flash an empty window and leave you guessing.
    let loaded = load(&which, &source, &host);
    let (label, data) = match loaded {
        Ok(v) => v,
        Err(e) => {
            eprintln!("plait: {e}\n\n{USAGE}");
            std::process::exit(1);
        }
    };

    let build = if cfg!(debug_assertions) {
        "  ·  DEBUG BUILD — timings meaningless, use --release"
    } else {
        ""
    };
    let title = SharedString::from(format!("plait · {which} · {label}{build}"));

    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);
    app.run(move |cx| {
        gpui_component::init(cx);
        cx.set_global(config::Active(host.clone()));

        // Re-read the file whenever it is written, and hand the result to every
        // window. The watcher's callback runs on its own thread, so it only sets
        // a flag; the task below is what touches the app.
        //
        // Polling a flag rather than plumbing an async channel through: a save is
        // a human action, 120 ms of latency is imperceptible, and this is five
        // lines with nothing to get wrong about wakeups.
        let dirty = Arc::new(AtomicBool::new(false));
        let watcher = {
            let dirty = dirty.clone();
            config::watch(&config_path, move || dirty.store(true, Ordering::Relaxed)).ok()
        };
        if watcher.is_none() {
            eprintln!("plait: could not watch {}; config reload is off", config_path.display());
        }
        cx.spawn(async move |cx: &mut AsyncApp| {
            // Held for as long as the task lives: dropping a `notify` watcher
            // stops it watching, silently.
            let _watcher = watcher;
            loop {
                cx.background_executor().timer(Duration::from_millis(120)).await;
                if !dirty.swap(false, Ordering::Relaxed) {
                    continue;
                }
                let warnings = cx.update(|cx| {
                    // From defaults every time, not from the live host: otherwise
                    // deleting a line from the file would leave the old value in
                    // place and the file would stop describing what you see.
                    let mut next = Host::new();
                    let warnings = config::load(&mut next, &config_path);
                    cx.set_global(config::Active(Rc::new(next)));
                    cx.refresh_windows();
                    warnings
                });
                for w in warnings {
                    eprintln!("plait: {w}");
                }
            }
        })
        .detach();
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        // No context predicate: there is one focusable view and no mode stack
        // yet, so a binding is global and the view that has focus gets it. The
        // day there is a second pane, this is what a keymap replaces.
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("s", CycleLayout, None),
            KeyBinding::new("w", CycleWrap, None),
        ]);
        cx.set_menus(vec![Menu {
            name: "plait".into(),
            items: vec![MenuItem::action("Quit", Quit)],
            disabled: false,
        }]);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), move |window, cx| {
                // Where the last run of this exact command left off. Restored
                // before the first frame so you never see row 0 flash past.
                let resume = session::restore(&session_key, &session_path);
                let mut diff_entity: Option<Entity<views::diff::Diff>> = None;
                #[allow(clippy::type_complexity)]
                let (view, rendered, top, total, note, load): (
                    AnyView,
                    Rc<Cell<usize>>,
                    Rc<Cell<usize>>,
                    Rc<Cell<usize>>,
                    Rc<std::cell::RefCell<SharedString>>,
                    String,
                ) = match data {
                    Data::Commits(commits) => {
                        let h = host.clone();
                        let e = cx.new(|_| views::commits::Commits::new(commits, h));
                        let v = e.read(cx);
                        if let Some(r) = &resume {
                            v.scroll_to(r.top);
                        }
                        (
                            e.clone().into(),
                            v.rendered.clone(),
                            v.top.clone(),
                            // The commit graph has a fixed row count: one per
                            // commit, and nothing reflows it.
                            Rc::new(Cell::new(v.total())),
                            Rc::new(std::cell::RefCell::new(SharedString::default())),
                            v.load.clone(),
                        )
                    }
                    Data::Diff(files) => {
                        let h = host.clone();
                        let e = cx.new(|cx| views::diff::Diff::new(files, h, cx));
                        diff_entity = Some(e.clone());
                        let v = e.read(cx);
                        if let Some(r) = &resume {
                            v.scroll_to(r.top);
                        }
                        (
                            e.clone().into(),
                            v.rendered.clone(),
                            v.top.clone(),
                            v.total.clone(),
                            v.note.clone(),
                            v.load.clone(),
                        )
                    }
                };

                // Persist as you scroll, so any kind of death keeps the position:
                // `dev.sh` kills the process, and nothing runs on the way out.
                // Only on change, so an idle window writes nothing at all.
                {
                    let (key, path) = (session_key.clone(), session_path.clone());
                    let start = resume.map(|r| r.top).unwrap_or(0);
                    cx.spawn(async move |cx: &mut AsyncApp| {
                        let mut last = start;
                        loop {
                            cx.background_executor().timer(Duration::from_millis(400)).await;
                            let now = top.get();
                            if now != last {
                                last = now;
                                session::save(&session::Session { key: key.clone(), top: now }, &path);
                            }
                        }
                    })
                    .detach();
                }
                let stats = stats::enabled().then(|| Stats::new(rendered, total, note, load));
                let shell = cx.new(|_| DevShell {
                    title,
                    view,
                    stats,
                    // Only where there is something to drive: the commit graph
                    // gets no strip rather than a strip of dead controls.
                    rediff: diff_entity.as_ref().and(rediff),
                    diff: diff_entity,
                    over: Overrides::default(),
                    open: None,
                    error: None,
                });
                cx.new(|cx| Root::new(shell, window, cx))
            })
            .expect("failed to open window");
            cx.update(|cx| cx.activate(true));
        })
        .detach();
    });
}

enum Data {
    Commits(Vec<plait_core::Commit>),
    Diff(Vec<plait_core::FileDiff>),
}

fn read_fixture(path: &str) -> String {
    // Git guarantees no encoding; never fail over one bad byte.
    String::from_utf8_lossy(&std::fs::read(path).unwrap_or_default()).into_owned()
}

fn load(which: &str, source: &Source, host: &Host) -> Result<(String, Data), String> {
    match (which, source) {
        ("diff", Source::Repo(repo, revspec)) => {
            // The host's differs, not a default: which algorithm ran is a
            // configured choice, and this is the one place it is made.
            let files = plait_git::diff(repo, revspec, &host.differ, &Overrides::default())?;
            if files.is_empty() {
                return Err(format!(
                    "no changes for {:?} {}",
                    repo,
                    if revspec.is_empty() { "(working tree)" } else { revspec }
                ));
            }
            // No algorithm in the label: there is a control in the title bar
            // that says which one, and it stays true when you change it.
            let label = format!("{} {}", plait_git::describe(repo), revspec);
            Ok((label.trim().into(), Data::Diff(files)))
        }
        ("diff", Source::Fixtures) => Ok((
            "fixtures".into(),
            Data::Diff(plait_core::parse_unified_diff(&read_fixture("fixtures/big.diff"))),
        )),
        (_, Source::Repo(repo, limit)) => {
            let n = limit.parse().unwrap_or(5000);
            let commits = plait_git::log(repo, n)?;
            if commits.is_empty() {
                return Err(format!("no commits in {repo:?}"));
            }
            Ok((plait_git::describe(repo), Data::Commits(commits)))
        }
        (_, Source::Fixtures) => Ok((
            "fixtures".into(),
            Data::Commits(plait_core::parse_log(&read_fixture("fixtures/log.txt"))),
        )),
    }
}
