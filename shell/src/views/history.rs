//! The History destination: the guide's branch timeline beside the selected
//! commit's diff.
//!
//! The timeline is a projection of the commits pane, not a second list with
//! its own cursor: it reads [`Commits`]' visible rows and tints the row the
//! keyboard is on, and a click routes back through the pane's own
//! `select_row`. The detail is the window's one diff view — the same entity
//! the stacked History used — so a commit is loaded exactly once however it
//! was reached. Nothing here holds repository data or makes a selection
//! decision; it is geometry and one named dispatch.

use super::commits::Commits;
use super::diff::Diff;
use crate::chrome;
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
                    let head = i == 0;
                    let node = if head {
                        host.theme.chrome.accent
                    } else {
                        host.theme.chrome.faint
                    };
                    let select = select.clone();
                    div()
                        .relative()
                        .flex_none()
                        .w_full()
                        .h(px(ROW_H))
                        .pl(px(31.0))
                        .pr(px(12.0))
                        .rounded(px(5.0))
                        .bg(rgb(match selected {
                            true => c.selection_bg,
                            false => c.bg,
                        }))
                        // The connector: one pixel down the row's own
                        // height, so consecutive rows draw one line. The
                        // reference tints it between the node and the rail.
                        .child(
                            div()
                                .absolute()
                                .left(px(17.0))
                                .top_0()
                                .bottom_0()
                                .w(px(1.0))
                                .bg(rgb(c.accent).alpha(0.35)),
                        )
                        // The node: HEAD is the accent ring, the rest the
                        // faint one — the reference's current/other cut,
                        // without a second lane model.
                        .child(
                            div()
                                .absolute()
                                .left(px(13.0))
                                .top(px(21.0))
                                .w(px(9.0))
                                .h(px(9.0))
                                .rounded_full()
                                .border_2()
                                .border_color(rgb(node))
                                .bg(rgb(c.bg)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .justify_center()
                                .h_full()
                                .gap_y(px(5.0))
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
        .px(px(28.0))
        .pt(px(28.0))
        .pb(px(22.0))
        .child(
            div()
                .text_size(px(10.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(dim))
                .child(SharedString::from(format!("COMMIT {short}"))),
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
        .font_family(host.chrome_family.clone())
        .child(
            div()
                .flex_none()
                .w(px(TIMELINE_W))
                .min_h_0()
                .flex()
                .flex_col()
                .overflow_hidden()
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
                        .min_h_0()
                        .flex_grow(1.0)
                        .overflow_hidden()
                        .px(px(10.0))
                        .child(timeline),
                ),
        )
        .child(detail)
        .into_any_element()
}
