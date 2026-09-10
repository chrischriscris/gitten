//! The History destination: the guide's branch timeline beside the selected
//! commit's diff.
//!
//! The timeline is a projection of the commits pane, not a second list with
//! its own cursor: it reads [`Commits`]' visible rows and tints the row the
//! keyboard is on, and a click routes back through the pane's own
//! `select_row`. Its gutter is the same commit graph the pane draws — each row
//! carries the plan `core` computed for it, drawn by [`crate::graph`] at the
//! timeline's taller row height — so branches fork and merge here exactly as
//! they do in the commits list. The detail is the window's one diff view — the
//! same entity the stacked History used — so a commit is loaded exactly once
//! however it was reached. Nothing here holds repository data or makes a
//! selection decision; it is geometry and one named dispatch.

use super::commits::Commits;
use super::diff::Diff;
use super::{vertical_scrollbar, DeferredScrollbar};
use crate::chrome;
use crate::graph;
use gitten_core::theme::Surface;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use std::rc::Rc;

/// The reference's timeline column: 310px at ordinary desktop widths, with
/// the commit detail taking the rest.
pub const TIMELINE_W: f32 = 310.0;

/// One timeline row. Fixed, because `uniform_list` virtualizes one height and
/// the reference's rows are uniform enough that the ref-tag air can live
/// inside the slot rather than opening a variable-height list.
pub const ROW_H: f32 = 76.0;

/// A click on a timeline row: select that commit in the commits pane and
/// re-aim the diff at it. The shell owns both halves; the view only names the
/// row, the same way every other mouse control names a command.
pub(crate) type Select = Rc<dyn Fn(usize, &mut App)>;

/// Everything the History destination draws that it does not own.
pub(crate) struct HistoryDeps {
    /// The timeline's data and cursor — the same entity the old stack drew.
    pub commits: Entity<Commits>,
    /// The selected commit's diff, re-aimed by the shell when the cursor
    /// moves. Shared with the stacked History so one load serves both.
    pub diff: Entity<Diff>,
    /// The timeline's own scroll handle; grouped rows have their own
    /// addresses, so the pane's list handle cannot serve the timeline.
    pub scroll: UniformListScrollHandle,
    pub select: Select,
    /// Back to the working tree, for the timeline's `Uncommitted changes`
    /// row — the one entry that is not a commit.
    pub changes: Rc<dyn Fn(&mut App)>,
    /// The working tree's changed-path count, for that row's dim half.
    pub changed: usize,
    /// HEAD's spelling, for the detail's footer.
    pub branch: SharedString,
}

/// The whole destination: the 310px timeline over a hairline, then the
/// detail's heading, diff and footer.
pub(crate) fn render_history(deps: &HistoryDeps, cx: &mut App) -> AnyElement {
    let host = crate::config::host(cx);
    let c = host.theme.chrome;
    let dim = host.theme.dim_on(Surface::Context);
    let cursor = deps.commits.read(cx).cursor();
    let rows = deps.commits.read(cx).rows();
    // The detail's presentation registry, read once per frame: what the picker
    // in the heading lists and which entry is loaded.
    let (layout_names, layout_index) = {
        let v = deps.diff.read(cx);
        (v.layout_names(), v.layout_index())
    };

    let timeline = {
        let commits = deps.commits.clone();
        let select = deps.select.clone();
        uniform_list("history-timeline", rows, move |range, _, cx| {
            let host = crate::config::host(cx);
            let view = commits.read(cx);
            range
                .map(|i| {
                    let (subject, author, age, short) = match view.commit_at(i) {
                        Some(commit) => (
                            SharedString::from(commit.subject.clone()),
                            SharedString::from(commit.author.to_string()),
                            view.age_at(i).cloned().unwrap_or_default(),
                            SharedString::from(commit.short.clone()),
                        ),
                        None => (
                            SharedString::default(),
                            SharedString::default(),
                            SharedString::default(),
                            SharedString::default(),
                        ),
                    };
                    let selected = i == cursor;
                    let draw = view.draw_at(i).cloned();
                    let select = select.clone();
                    let row = div()
                        .flex()
                        .flex_none()
                        .w_full()
                        .h(px(ROW_H))
                        .rounded(px(5.0))
                        .bg(rgb(match selected {
                            true => c.selection_bg,
                            false => c.bg,
                        }));
                    // The graph gutter is the plan `core` computed for this
                    // row, drawn at the timeline's own row height: a lane's
                    // two halves meet on the row boundary, so the whole column
                    // reads as one continuous graph. Per-row width, like the
                    // commits list — a row alone on the trunk spends its
                    // columns on its subject rather than reserving the widest
                    // merge's.
                    let row = match draw {
                        Some(d) => row.child(graph::row_canvas_h(d, host.clone(), ROW_H)),
                        None => row.child(div().flex_none().w(px(0.0))),
                    };
                    row.child(
                        div()
                            .min_w_0()
                            .flex_grow(1.0)
                            .flex()
                            .flex_col()
                            .justify_center()
                            .h_full()
                            .gap_y(px(5.0))
                            .pr(px(12.0))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(11.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(c.fg))
                                    .child(subject),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(9.0))
                                    .text_color(rgb(dim))
                                    .child(SharedString::from(format!(
                                        "{author} \u{00b7} {age}  {short}"
                                    ))),
                            ),
                    )
                    .id(("history-row", i))
                    .cursor_pointer()
                    .when(!selected, |r| {
                        r.hover(|s| s.bg(rgb(host.theme.chrome.fg).alpha(0.03)))
                    })
                    .on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, _, cx| {
                        select(i, cx);
                    })
                    .into_any_element()
                })
                .collect()
        })
        .track_scroll(&deps.scroll)
        .size_full()
    };

    let (subject, author, age, short, parent) = {
        let view = deps.commits.read(cx);
        match view.current() {
            Some(commit) => (
                SharedString::from(commit.subject.clone()),
                SharedString::from(commit.author.to_string()),
                view.age_at(view.cursor()).cloned().unwrap_or_default(),
                SharedString::from(commit.short.clone()),
                commit
                    .parents
                    .first()
                    .map(|p| SharedString::from(p.chars().take(7).collect::<String>()))
                    .unwrap_or_else(|| SharedString::from("\u{2014}")),
            ),
            None => (
                SharedString::from("No commit selected"),
                SharedString::default(),
                SharedString::default(),
                SharedString::default(),
                SharedString::from("\u{2014}"),
            ),
        }
    };
    let avatar = author.chars().next().map(|c| c.to_uppercase().to_string());

    let heading = div()
        .flex_none()
        // Chrome type, and only this part of the pane: the diff below keeps
        // whatever font the shell root hands it, which is the code font.
        .font_family(host.chrome_family.clone())
        .px(px(28.0))
        .pt(px(28.0))
        .pb(px(22.0))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(10.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(dim))
                        .child(SharedString::from(format!("COMMIT {short}"))),
                )
                // The commit's diff is the same component the Changes center
                // shows, so it carries the same presentation picker — acting
                // on *this* view, which is the one on screen here.
                .child(super::diff::layout_toggle(
                    &deps.diff,
                    layout_names,
                    layout_index,
                    &host,
                    Surface::Context,
                )),
        )
        .child(
            div()
                .mt(px(12.0))
                .mb(px(12.0))
                .text_size(px(22.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(c.fg))
                .child(subject),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .text_size(px(11.0))
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(22.0))
                        .h(px(22.0))
                        .rounded_full()
                        .bg(rgb(c.raised))
                        .text_size(px(9.0))
                        .text_color(rgb(c.fg))
                        .children(avatar),
                )
                .child(div().text_color(rgb(c.fg)).child(author))
                .child(
                    div()
                        .text_color(rgb(dim))
                        .child(SharedString::from(format!("committed {age}"))),
                ),
        );

    let footer = div()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .font_family(host.chrome_family.clone())
        .px(px(16.0))
        .py(px(14.0))
        .border_t_1()
        .border_color(rgb(c.border))
        .text_size(px(10.0))
        .text_color(rgb(dim))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child("Parent")
                .child(div().text_color(rgb(c.fg)).child(parent)),
        )
        .child(SharedString::from(format!("Authored on {}", deps.branch)));

    let detail = div()
        .min_w_0()
        .min_h_0()
        .flex_grow(1.0)
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(heading)
        .child(
            div()
                .min_h_0()
                .flex_grow(1.0)
                .overflow_hidden()
                .child(deps.diff.clone()),
        )
        .child(footer);

    let uncommitted = {
        let changes = deps.changes.clone();
        div()
            .id("history-uncommitted")
            .relative()
            .flex_none()
            .w_full()
            .h(px(64.0))
            .pl(px(31.0))
            .pr(px(12.0))
            .rounded(px(5.0))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(host.theme.chrome.fg).alpha(0.03)))
            .on_click(move |_, _, cx| changes(cx))
            // The connector leaves below the node and runs to the list, so
            // the eye reads the working tree as the newest entry in the same
            // column.
            .child(
                div()
                    .absolute()
                    .left(px(17.0))
                    .top(px(28.0))
                    .bottom_0()
                    .w(px(1.0))
                    .bg(rgb(c.accent).alpha(0.35)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(13.0))
                    .top(px(21.0))
                    .w(px(11.0))
                    .h(px(11.0))
                    .rounded_full()
                    .border_2()
                    .border_dashed()
                    .border_color(rgb(c.accent))
                    .bg(rgb(c.bg)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .h_full()
                    .gap_y(px(4.0))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(c.fg))
                            .child("Uncommitted changes"),
                    )
                    .child(div().text_size(px(9.0)).text_color(rgb(dim)).child(
                        SharedString::from(format!("Working tree \u{00b7} {} files", deps.changed)),
                    )),
            )
    };

    div()
        .id("workspace-history")
        .debug_selector(|| "workspace-history".to_string())
        .size_full()
        .flex()
        // No family here: the code is this pane's main content and the shell
        // root already draws it in the code font. The chrome around it — the
        // timeline, the commit's heading, the footer — names the chrome face
        // for itself, the way every other pane's furniture does.
        .child(
            div()
                .flex_none()
                .w(px(TIMELINE_W))
                .min_h_0()
                .flex()
                .flex_col()
                .overflow_hidden()
                .font_family(host.chrome_family.clone())
                .border_r_1()
                .border_color(rgb(c.border))
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px(px(18.0))
                        .pt(px(16.0))
                        .pb(px(12.0))
                        .text_size(px(11.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(c.fg))
                        .child("Branch history")
                        .child(chrome::icon(
                            "gitten/branch.svg",
                            13.0,
                            host.theme.dim_on(Surface::Context),
                        )),
                )
                .child(div().flex_none().px(px(10.0)).child(uncommitted))
                .child(
                    div()
                        // The bar overlays the list, so the container is the
                        // positioned ancestor — the same shape the rail and
                        // every pane's strip container use.
                        .relative()
                        .min_h_0()
                        .flex_grow(1.0)
                        .overflow_hidden()
                        .px(px(10.0))
                        .child(timeline)
                        .when(host.view.scrollbar, |d| {
                            // `direct`: the timeline's wheel writes the handle's
                            // own offset in the platform's pixels, so nothing
                            // is banked for a thumb drag to cancel — only the
                            // strict request the cursor-follow scroll parks.
                            d.child(vertical_scrollbar(&DeferredScrollbar::direct(&deps.scroll)))
                        }),
                ),
        )
        .child(detail)
        .into_any_element()
}
