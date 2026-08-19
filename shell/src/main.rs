mod graph;
mod stats;
mod views;

use gpui::*;
use gpui_component::*;
use std::cell::Cell;
use std::rc::Rc;
use stats::Stats;

#[global_allocator]
static ALLOC: stats::Counting = stats::Counting;

actions!(plait, [Quit]);

/// The dev harness: a title strip, one view, and an optional stats overlay.
/// Deliberately one-way — no view depends on anything in here, so each drops
/// into a real pane unchanged when the layout gets assembled.
struct DevShell {
    title: SharedString,
    view: AnyView,
    stats: Option<Stats>,
}

impl Render for DevShell {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let overlay = self.stats.as_mut().map(|s| {
            s.tick();
            (s.frames(), s.rows(), s.heap(), s.load.clone())
        });

        // Force a continuous redraw loop so the frame numbers mean something.
        // Only while the overlay is on: at rest GPUI draws nothing at all.
        if overlay.is_some() {
            window.request_animation_frame();
        }

        div()
            .size_full()
            .v_flex()
            .bg(rgb(0x0e0d0c))
            .text_color(rgb(0xe8e3dc))
            .text_sm()
            .font_family("Menlo")
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .h(px(32.))
                    .px_4()
                    .bg(rgb(0x151312))
                    .text_color(rgb(0x6e6862))
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
                    .bg(rgb(0x131211))
                    .text_color(rgb(0x6e6862))
                    .child(
                        div()
                            .flex()
                            .gap_6()
                            .child(div().text_color(rgb(0xdfa851)).child(frames))
                            .child(rows)
                            .child(heap),
                    )
                    .child(div().text_color(rgb(0x4a4540)).child(load))
            }))
    }
}

fn main() {
    let which = std::env::args().nth(1).unwrap_or_else(|| "commits".into());
    let title = SharedString::from(format!(
        "plait · {which}   —  arg: commits | diff   ·   PLAIT_STATS=1 for the overlay"
    ));

    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);
    app.run(move |cx| {
        gpui_component::init(cx);

        // A non-bundled binary has no application menu, so nothing is wired to
        // the standard Quit. Register the action, the keystroke and a menu.
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.set_menus(vec![Menu {
            name: "plait".into(),
            items: vec![MenuItem::action("Quit", Quit)],
            disabled: false,
        }]);

        // Closing the last window should end the process. macOS keeps appless
        // processes alive by convention; for a dev binary that just leaves
        // orphans behind every time you hit the red button.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                // Each view owns its own counters; we only read them.
                let (view, rendered, total, load): (AnyView, Rc<Cell<usize>>, usize, String) =
                    match which.as_str() {
                        "diff" => {
                            let e = cx.new(|_| views::diff::Diff::from_fixtures());
                            let v = e.read(cx);
                            (e.clone().into(), v.rendered.clone(), v.total(), v.load.clone())
                        }
                        _ => {
                            let e = cx.new(|_| views::commits::Commits::from_fixtures());
                            let v = e.read(cx);
                            (e.clone().into(), v.rendered.clone(), v.total(), v.load.clone())
                        }
                    };

                let stats = stats::enabled().then(|| Stats::new(rendered, total, load));
                let shell = cx.new(|_| DevShell { title, view, stats });
                cx.new(|cx| Root::new(shell, window, cx))
            })
            .expect("failed to open window");

            // A bare binary launched from a terminal is not an .app bundle, so
            // macOS treats it as a background process and the window opens
            // behind whatever you were doing. Ask for the foreground explicitly.
            cx.update(|cx| cx.activate(true));
        })
        .detach();
    });
}
