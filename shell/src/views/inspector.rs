//! The workspace's commit inspector: staged summary, drafts, commit.
//!
//! The guide's right rail, drawn from the files pane's staged rows and the
//! shell's draft store — never from a second read of the repository. The two
//! fields are persistent [`crate::input::Input`] entities owned by the
//! workspace state: Summary single-line, Description multiline. Every edit
//! writes through to the draft; the Commit button's gate reads the draft
//! plus the staged-file list; the confirmation dialog and the write itself
//! live shell-side, through the same `files.commit` machinery the prompt
//! path calls — one commit implementation, two doors.

use super::files::StagedFile;
use crate::chrome::{self, section_label};
use crate::graph::ROW_H;
use crate::input::Input;
use gitten_core::theme::Surface;
#[allow(unused_imports)]
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use std::rc::Rc;

/// The shell's named dispatch, as the inspector's controls call it: one
/// command name at a time, through the same path the keyboard resolves to.
#[allow(dead_code)] // STUB(phase3-resume): the inspector's dispatch handle, unwired.
pub(crate) type Dispatch = Rc<dyn Fn(&str, &mut App)>;

/// Everything the inspector draws that it does not own. Counts and rows
/// arrive spelled per refresh; the fields arrive as entities; the strings
/// arrive spelled shell-side once per frame, not per row.
#[allow(dead_code)] // STUB(phase3-resume): constructed by the workspace once wired.
pub(crate) struct InspectorDeps {
    pub summary: Option<Entity<Input>>,
    pub description: Option<Entity<Input>>,
    pub staged: Vec<StagedFile>,
    pub staged_hunks: u32,
    /// Whether the Commit button may fire: staged content *and* a
    /// non-whitespace summary. `None` also carries the reason, said aloud
    /// when the button is pressed anyway — a button that silently eats a
    /// click is a control that no longer exists.
    pub can_commit: bool,
    pub commit_note: SharedString,
    pub dispatch: Dispatch,
}

/// The whole rail, sized by its parent. Reads entities and refcounts once
/// per frame; regrouping and side reads happen on refresh, never here.
#[allow(dead_code)] // STUB(phase3-resume): called from the workspace's inspector slot.
pub(crate) fn render_inspector(deps: &InspectorDeps, cx: &mut App) -> AnyElement {
    let host = crate::config::host(cx);
    let dim = rgb(host.theme.dim_on(Surface::Context));

    let staged_rows = match deps.staged.is_empty() {
        true => div()
            .text_color(dim)
            .child("No staged changes")
            .into_any_element(),
        false => div()
            .flex()
            .flex_col()
            .gap_y(px(2.0))
            .children(deps.staged.iter().enumerate().map(|(i, f)| {
                let hunks = match f.hunks {
                    (0, 0) => SharedString::from("staged"),
                    (staged, total) => SharedString::from(format!("{staged}/{total} hunks")),
                };
                div()
                    .id(("ws-staged", i))
                    .flex()
                    .items_center()
                    .gap(chrome::gap_s(&host.font))
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(host.theme.diff.adds_fg))
                            .child("+"),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_shrink(1.0)
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_color(rgb(host.theme.chrome.fg))
                                    .child(f.name.clone()),
                            )
                            .child(div().truncate().text_color(dim).child(f.dir.clone())),
                    )
                    .child(div().flex_none().text_color(dim).child(hunks))
                    .into_any_element()
            }))
            .into_any_element(),
    };

    // A field or its honest absence: entities exist from first workspace
    // entry; before that the rail says what stands here rather than
    // drawing a dead box.
    let field = |entity: &Option<Entity<Input>>, missing: &'static str| match entity {
        Some(field) => field.clone().into_any_element(),
        None => div().text_color(dim).child(missing).into_any_element(),
    };

    let commit = deps.dispatch.clone();
    let commit_row = div()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .gap(chrome::gap_m(&host.font))
        .child(div().text_color(dim).child(SharedString::from(format!(
            "{} hunks staged",
            deps.staged_hunks
        ))))
        .child(
            div()
                .id("ws-commit")
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .h(px(28.0))
                .px(chrome::gap_l(&host.font))
                .rounded(px(chrome::RADIUS))
                .cursor_pointer()
                // Lit when it may fire, furniture when it may not — and
                // still clickable then, because the press says *why* not
                // rather than swallowing the click.
                .bg(rgb(match deps.can_commit {
                    true => host.theme.chrome.accent,
                    false => host.theme.chrome.raised,
                }))
                .text_color(rgb(match deps.can_commit {
                    true => host.theme.chrome.status_bg,
                    false => host.theme.dim_on(Surface::Context),
                }))
                .child("Commit")
                .on_click(move |_, _, cx| commit("workspace.commit", cx)),
        );

    div()
        .flex()
        .flex_col()
        .size_full()
        .overflow_hidden()
        .bg(rgb(host.theme.chrome.bg))
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .px(px(chrome::ROW_PAD))
                .h(px(ROW_H + 8.0))
                .child(div().text_color(rgb(host.theme.chrome.fg)).child("Commit"))
                .child(div().text_color(dim).child(SharedString::from(format!(
                    "{} files staged",
                    deps.staged.len()
                )))),
        )
        .child(
            div()
                .flex_none()
                .px(px(chrome::ROW_PAD))
                .pb(px(4.0))
                .child(section_label(
                    &host,
                    SharedString::from("STAGED FILES"),
                    Some(SharedString::from(deps.staged.len().to_string())),
                    ROW_H,
                )),
        )
        .child(
            div()
                .flex_none()
                .px(px(chrome::ROW_PAD))
                .pb(px(8.0))
                .child(staged_rows),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap_y(px(2.0))
                .px(px(chrome::ROW_PAD))
                .pb(px(6.0))
                .child(div().text_color(rgb(host.theme.chrome.fg)).child("Summary"))
                .child(field(&deps.summary, "Summary arrives with the workspace.")),
        )
        .child(
            div()
                .min_h_0()
                .flex_shrink(1.0)
                .flex()
                .flex_col()
                .gap_y(px(2.0))
                .px(px(chrome::ROW_PAD))
                .pb(px(6.0))
                .overflow_hidden()
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .gap(chrome::gap_s(&host.font))
                        .text_color(rgb(host.theme.chrome.fg))
                        .child("Description")
                        .child(div().text_color(dim).child("Optional")),
                )
                .child(
                    div()
                        .min_h_0()
                        .flex_shrink(1.0)
                        .overflow_hidden()
                        .child(field(
                            &deps.description,
                            "Description arrives with the workspace.",
                        )),
                ),
        )
        .child(
            div()
                .flex_none()
                .px(px(chrome::ROW_PAD))
                .py(px(8.0))
                .border_t_1()
                .border_color(rgb(host.theme.chrome.border))
                .child(commit_row),
        )
        .into_any_element()
}
