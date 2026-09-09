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
use crate::chrome;
use crate::input::Input;
use gitten_core::theme::Surface;
#[allow(unused_imports)]
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use std::rc::Rc;

/// The shell's named dispatch, as the inspector's controls call it: one
/// command name at a time, through the same path the keyboard resolves to.
pub(crate) type Dispatch = Rc<dyn Fn(&str, &mut App)>;

/// Everything the inspector draws that it does not own. Counts and rows
/// arrive spelled per refresh; the fields arrive as entities; the strings
/// arrive spelled shell-side once per frame, not per row.
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
    let commit_button = div()
        .id("ws-commit")
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w_full()
        .h(px(34.0))
        .rounded(px(5.0))
        .cursor_pointer()
        .bg(rgb(host.theme.chrome.accent).alpha(if deps.can_commit { 1.0 } else { 0.45 }))
        .text_color(rgb(host.theme.chrome.title_bg))
        .font_weight(FontWeight::SEMIBOLD)
        .child("Commit  ›")
        .on_click(move |_, _, cx| commit("workspace.commit", cx));

    div()
        .flex()
        .flex_col()
        .size_full()
        .overflow_hidden()
        .bg(rgb(host.theme.chrome.title_bg))
        .font_family(host.chrome_family.clone())
        .text_size(px(11.0))
        .child(
            div()
                .flex_none()
                .px(px(18.0))
                .pt(px(30.0))
                .pb(px(26.0))
                .text_size(px(17.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(host.theme.chrome.fg))
                .child("Commit"),
        )
        .child(
            div()
                .flex_none()
                .mx(px(18.0))
                .pt(px(14.0))
                .border_t_1()
                .border_color(rgb(host.theme.chrome.border))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .text_size(px(9.0))
                        .text_color(dim)
                        .child("STAGED FILES")
                        .child(
                            div()
                                .text_color(rgb(host.theme.chrome.accent))
                                .child(SharedString::from(deps.staged.len().to_string())),
                        ),
                ),
        )
        .child(
            div()
                .id("workspace-staged-files")
                .min_h_0()
                .flex_grow(1.0)
                .overflow_y_scroll()
                .px(px(18.0))
                .py(px(26.0))
                .child(staged_rows),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap_y(px(7.0))
                .p(px(18.0))
                .border_t_1()
                .border_color(rgb(host.theme.chrome.border))
                .child(div().text_color(rgb(host.theme.chrome.fg)).child("Summary"))
                .child(field(&deps.summary, "Summary arrives with the workspace."))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .mt(px(8.0))
                        .text_color(rgb(host.theme.chrome.fg))
                        .child("Description")
                        .child(div().text_size(px(10.0)).text_color(dim).child("Optional")),
                )
                .child(field(
                    &deps.description,
                    "Description arrives with the workspace.",
                ))
                .child(div().my(px(5.0)).text_size(px(10.0)).text_color(dim).child(
                    if deps.can_commit {
                        SharedString::from(format!("{} hunks staged", deps.staged_hunks))
                    } else {
                        deps.commit_note.clone()
                    },
                ))
                .child(commit_button),
        )
        .into_any_element()
}
