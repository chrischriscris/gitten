//! The workspace sidebar: directory-grouped files under repo identity,
//! destination nav and filter — the guide's left rail, drawn from the files
//! pane's grouped projection under the files pane's cursor.
//!
//! No state of its own: rows come from [`super::files::Files::grouped`],
//! selection is [`super::files::Files::select_row`], staging is the existing
//! `files.stage` / `files.stage-all` commands, the filter is the existing
//! `files.search` prompt (its query survives refresh already, and the grouped
//! cache rebuilds inside [`super::files::Files::apply_query`]). Every mouse
//! control resolves through the shell's named dispatch — a click moves the
//! keyboard first (the files list's own rule: a click is the keyboard coming
//! back), then dispatches, then asks for a preview re-aim.
//!
//! TODO(Phase 3/4, see plans/desktop-v2/PLAN.md): mixed checkbox + `n/m`
//! hunk fractions, stage-remainder semantics on partial rows, staged summary
//! counts in the footer.

use super::files::{Entry, Files, GroupedRow};
use super::workspace::Destination;
use crate::chrome::{self, empty_line, list_row, path_spans, section_label};
use crate::graph::ROW_H;
use gitten_core::theme::Surface;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use std::rc::Rc;

/// The shell's named dispatch, as the sidebar's controls call it: one
/// command name at a time, through the same path the keyboard resolves to.
/// A button is an adapter, never a second implementation.
pub(crate) type Dispatch = Rc<dyn Fn(&str, &mut App)>;

/// Everything the sidebar draws that it does not own. The files entity
/// carries the rows, the cursor and the filter; `dispatch` carries the
/// shell's named commands; the strings are spelled shell-side once per
/// frame, not per row.
pub(crate) struct SidebarDeps {
    pub files: Entity<Files>,
    pub dispatch: Dispatch,
    /// Repository identity: the repo name over the files label
    /// (`"{describe} · {n} changed"`), both spelled by acquisition.
    pub title: SharedString,
    pub subtitle: SharedString,
    /// `"N files changed"` plus the filter note when filtered.
    pub status_line: SharedString,
    pub destination: Destination,
    /// The sidebar list's scroll handle, owned by the workspace state —
    /// grouped space has its own addresses, so the stack list's handle
    /// cannot serve it.
    pub scroll: UniformListScrollHandle,
}

/// The whole rail, sized by its parent. Reads the files entity once per
/// frame (refcount bumps, like every other per-frame read of that pane);
/// regrouping happens on refresh/filter change, never here.
pub(crate) fn render_sidebar(deps: &SidebarDeps, cx: &mut App) -> AnyElement {
    let host = crate::config::host(cx);
    // One borrow, owned clones out: everything below reads refcounts.
    let (data, visible, grouped, cursor, focused, query) = {
        let f = deps.files.read(cx);
        (
            f.data().clone(),
            f.visible().clone(),
            f.grouped().clone(),
            f.cursor_visible(),
            f.focused(),
            f.query().map(str::to_string),
        )
    };
    let ch = host.font.char_width();

    // One file row: leading stage checkbox, selectable filename, trailing
    // status. The checkbox and the filename are separate hit targets sharing
    // one keyboard move — the box stages, the name only selects.
    let files = deps.files.clone();
    let dispatch = deps.dispatch.clone();
    let list = uniform_list(
        "workspace-files",
        grouped.rows.len(),
        move |range, _, cx| {
            let host = crate::config::host(cx);
            range
                .map(|i| match &grouped.rows[i] {
                    GroupedRow::Heading { dir, count } => {
                        let label = match dir.as_ref().is_empty() {
                            true => SharedString::from("repository root"),
                            false => dir.clone(),
                        };
                        section_label(&host, label, Some(count.clone()), ROW_H).into_any_element()
                    }
                    GroupedRow::File { visible: at } => {
                        // Owned before any handler below: the row's handlers
                        // must be 'static, and a borrow of the outer closure's
                        // grouped rows cannot travel with them.
                        let vp: usize = *at;
                        let d = visible[vp];
                        let Entry::File(f) = &data[d] else {
                            return empty_line(&host, SharedString::from(""));
                        };
                        let current = vp == cursor;
                        let staged = f.section == super::files::Section::Staged;
                        let files_click = files.clone();
                        let files_stage = files.clone();
                        let stage_cmd = dispatch.clone();
                        let preview_after_stage = dispatch.clone();
                        let preview_row = dispatch.clone();
                        let row = list_row(&host, current, focused, ROW_H)
                            .child(
                                // The stage checkbox: filled in accent when the
                                // row's side is staged, an empty box otherwise.
                                // Partial state and hunk fractions are Phase 3.
                                div()
                                    .id(("ws-stage", i))
                                    .flex_none()
                                    .w(px(14.0))
                                    .h(px(14.0))
                                    .rounded(px(3.0))
                                    .border_1()
                                    .border_color(rgb(match staged {
                                        true => host.theme.chrome.accent,
                                        false => host.theme.chrome.faint,
                                    }))
                                    .when(staged, |d| d.bg(rgb(host.theme.chrome.accent)))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        move |_: &MouseDownEvent, _, cx| {
                                            let host = crate::config::host(cx);
                                            files_stage.update(cx, |f, cx| {
                                                f.select_row(vp, &host);
                                                cx.notify();
                                            });
                                            stage_cmd("files.stage", cx);
                                            preview_after_stage("workspace.preview", cx);
                                        },
                                    ),
                            )
                            .child(div().min_w_0().flex_grow(1.0).child(path_spans(
                                &host,
                                f.dir.clone(),
                                f.name.clone(),
                                host.theme.chrome.fg,
                                match current {
                                    true => Surface::Cursor,
                                    false => Surface::Context,
                                },
                                false,
                            )))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(2.0 * ch))
                                    .text_color(rgb(f.section.ink(&host)))
                                    .child(SharedString::from(f.letters)),
                            );
                        row.id(("ws-row", i))
                            .cursor_pointer()
                            .when(!current, |r| {
                                r.hover(|s| s.bg(rgb(host.theme.chrome.fg).alpha(0.03)))
                            })
                            .on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, _, cx| {
                                let host = crate::config::host(cx);
                                files_click.update(cx, |f, cx| {
                                    f.select_row(vp, &host);
                                    cx.notify();
                                });
                                preview_row("workspace.preview", cx);
                            })
                            .into_any_element()
                    }
                })
                .collect()
        },
    )
    .track_scroll(&deps.scroll)
    .size_full();

    let nav = |n: usize, label: &'static str, active: bool, command: &'static str| {
        let dispatch = deps.dispatch.clone();
        div()
            .id(("ws-nav", n))
            .flex_grow(1.0)
            .flex()
            .items_center()
            .justify_center()
            .h(px(28.0))
            .rounded(px(chrome::RADIUS))
            .cursor_pointer()
            .bg(rgb(match active {
                true => host.theme.chrome.raised,
                false => host.theme.chrome.bg,
            }))
            .text_color(rgb(match active {
                true => host.theme.chrome.fg,
                false => host.theme.dim_on(Surface::Context),
            }))
            .child(label)
            .on_click(move |_, _, cx| dispatch(command, cx))
    };
    let util =
        |n: usize, label: &'static str, command: &'static str, and_then: Option<&'static str>| {
            let dispatch = deps.dispatch.clone();
            div()
                .id(("ws-util", n))
                .cursor_pointer()
                .text_color(rgb(host.theme.dim_on(Surface::Context)))
                .hover(|s| s.text_color(rgb(host.theme.chrome.fg)))
                .child(label)
                .on_click(move |_, _, cx| {
                    dispatch(command, cx);
                    if let Some(next) = and_then {
                        dispatch(next, cx);
                    }
                })
        };

    let filter_label = query
        .as_deref()
        .filter(|q| !q.is_empty())
        .map(SharedString::from)
        .unwrap_or_else(|| SharedString::from("Filter files…"));
    let filter_dispatch = deps.dispatch.clone();
    let stage_all = deps.dispatch.clone();

    div()
        .flex()
        .flex_col()
        .size_full()
        .overflow_hidden()
        .bg(rgb(host.theme.chrome.bg))
        .child(
            // Repository identity, spelled by acquisition — never by the view.
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap_y(px(2.0))
                .px(px(chrome::ROW_PAD))
                .pt(px(10.0))
                .pb(px(8.0))
                .child(
                    div()
                        .text_color(rgb(host.theme.chrome.fg))
                        .child(deps.title.clone()),
                )
                .child(
                    div()
                        .text_color(rgb(host.theme.dim_on(Surface::Context)))
                        .child(deps.subtitle.clone()),
                ),
        )
        .child(
            // Changes / History: the destinations. History leaves the
            // workspace for the full stack until the timeline moves in.
            div()
                .flex_none()
                .flex()
                .flex_row()
                .gap(chrome::gap_s(&host.font))
                .px(px(chrome::ROW_PAD))
                .pb(px(8.0))
                .child(nav(
                    0,
                    "Changes",
                    deps.destination == Destination::Changes,
                    "workspace.changes",
                ))
                .child(nav(
                    1,
                    "History",
                    deps.destination == Destination::History,
                    "workspace.history",
                )),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .gap(chrome::gap_m(&host.font))
                .px(px(chrome::ROW_PAD))
                .pb(px(8.0))
                .child(util(
                    0,
                    "Branches",
                    "workspace.history",
                    Some("branches.focus"),
                ))
                .child(util(
                    1,
                    "Stashes",
                    "workspace.history",
                    Some("stashes.focus"),
                )),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .px(px(chrome::ROW_PAD))
                .pb(px(4.0))
                .child(
                    div()
                        .text_color(rgb(host.theme.dim_on(Surface::Context)))
                        .child(deps.status_line.clone()),
                )
                .child(
                    div()
                        .id("ws-stage-all")
                        .cursor_pointer()
                        .text_color(rgb(host.theme.dim_on(Surface::Context)))
                        .hover(|s| s.text_color(rgb(host.theme.chrome.fg)))
                        .child("Stage all")
                        .on_click(move |_, _, cx| stage_all("files.stage-all", cx)),
                ),
        )
        .child(
            div()
                .id("ws-filter")
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .mx(px(chrome::ROW_PAD))
                .mb(px(4.0))
                .px(chrome::gap_s(&host.font))
                .h(px(26.0))
                .rounded(px(chrome::RADIUS))
                .border_1()
                .border_color(rgb(host.theme.chrome.border))
                .cursor_pointer()
                .text_color(rgb(host.theme.dim_on(Surface::Context)))
                .child(filter_label)
                .child(div().child("/"))
                .on_click(move |_, _, cx| filter_dispatch("files.search", cx)),
        )
        .child(div().flex_grow(1.0).min_h_0().overflow_hidden().child(list))
        .into_any_element()
}
