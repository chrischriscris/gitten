mod config;
mod graph;
mod stats;
mod views;

use gpui::*;
use gpui_component::*;
use plait_core::host::Host;
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



const USAGE: &str = "\
plait — a git client

  plait commits [REPO] [LIMIT]     history graph      (default: . , 5000)
  plait diff    [REPO] [REVSPEC]   a diff             (default: . , working tree)
  plait config                     print the current theme and font as TOML

  REVSPEC is anything git takes:  HEAD~50..HEAD   main..feature   <sha>
  Pass --fixtures instead of REPO to read fixtures/ instead of a repository.

  plait.toml next to the binary (or $PLAIT_CONFIG) is re-read every time it is
  saved, and colours and font apply on the next frame — no rebuild, no relaunch.
  Start one with:  plait config > plait.toml

  PLAIT_STATS=1   frame, row and heap overlay
";

struct DevShell {
    title: SharedString,
    view: AnyView,
    stats: Option<Stats>,
    host: Rc<Host>,
}

impl Render for DevShell {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let overlay = self.stats.as_mut().map(|s| {
            s.tick();
            (s.frames(), s.rows(), s.heap(), s.load.clone())
        });
        if overlay.is_some() {
            window.request_animation_frame();
        }

        let c = self.host.theme.chrome;
        let f = &self.host.font;
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
                    .h(px(32.))
                    .px_4()
                    .bg(rgb(c.title_bg))
                    .text_color(rgb(c.dim))
                    .child(self.title.clone()),
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

    // Acquire before opening a window: a git error should print and exit, not
    // flash an empty window and leave you guessing.
    let loaded = load(&which, &source);
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

        // One host, built before any view exists: the highlighters, the theme,
        // the font, and whatever else becomes swappable later. An extension
        // registering itself does it here, and every view reads the same struct.
        let mut built = Host::new();
        let config_path = config::path();
        for w in config::load(&mut built, &config_path) {
            eprintln!("plait: {w}");
        }
        let host = Rc::new(built);
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
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
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
                let (view, rendered, total, load): (AnyView, Rc<Cell<usize>>, usize, String) =
                    match data {
                        Data::Commits(commits) => {
                            let h = host.clone();
                            let e = cx.new(|_| views::commits::Commits::new(commits, h));
                            let v = e.read(cx);
                            (e.clone().into(), v.rendered.clone(), v.total(), v.load.clone())
                        }
                        Data::Diff(files) => {
                            let h = host.clone();
                            let e = cx.new(|_| views::diff::Diff::new(files, h));
                            let v = e.read(cx);
                            (e.clone().into(), v.rendered.clone(), v.total(), v.load.clone())
                        }
                    };
                let stats = stats::enabled().then(|| Stats::new(rendered, total, load));
                let shell = cx.new(|_| DevShell { title, view, stats, host });
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

fn load(which: &str, source: &Source) -> Result<(String, Data), String> {
    match (which, source) {
        ("diff", Source::Repo(repo, revspec)) => {
            let files = plait_git::diff(repo, revspec)?;
            if files.is_empty() {
                return Err(format!(
                    "no changes for {:?} {}",
                    repo,
                    if revspec.is_empty() { "(working tree)" } else { revspec }
                ));
            }
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
