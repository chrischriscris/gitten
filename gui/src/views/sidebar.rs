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
use crate::chrome::{self, empty_line};
use gitten_core::font::Font;
use gitten_core::groups::StageFraction;
use gitten_core::theme::Surface;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use std::rc::Rc;

/// Compact single-line rows; directory names belong to group headings.
pub(crate) fn ws_row_h(font: &Font) -> f32 {
    (font.size * 1.3).ceil() + 10.0
}

/// Sidebar chrome insets, per the reference: 18px rails against the old
/// stack's 10px `ROW_PAD`. Workspace-only — the numbered stack keeps its
/// own axis until Phase 5 removes it.
const RAIL_PX: f32 = 18.0;

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
    /// The working tree's distinct changed paths, for the `Changed files N`
    /// heading. A count and not a sentence: the heading's own words are
    /// static and the number is acquisition's.
    pub changed: usize,
    /// Shown-over-loaded when a filter narrows the list, appended to the
    /// heading so the rail says it is showing a subset.
    pub filter_note: Option<SharedString>,
    pub destination: Destination,
    /// `Some(branch)` in the History destination: the rail replaces the whole
    /// file list — heading, filter, rows and footer — with the reference's
    /// CURRENT BRANCH note. `None` in Changes.
    pub history_note: Option<SharedString>,
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
    let row_h = ws_row_h(&host.font);
    let face = host.chrome_family.clone();
    // The staged/total hunk sentence for the rail's footer, summed from
    // the refresh's own per-path map — the same numbers the inspector
    // spells, read here as refcounts and never recomputed.
    let (hunks_staged, hunks_total) = {
        let f = deps.files.read(cx);
        f.counts()
            .values()
            .fold((0u32, 0u32), |(s, t), (a, b)| (s + a, t + b))
    };

    // One file row: leading stage checkbox, selectable filename, trailing
    // status. The checkbox and the filename are separate hit targets sharing
    // one keyboard move — the box stages, the name only selects.
    let files = deps.files.clone();
    let dispatch = deps.dispatch.clone();
    // The root chrome's face, cloned before the row closure moves `face`.
    let root_face = face.clone();
    let list = uniform_list(
        "workspace-files",
        grouped.rows.len(),
        move |range, _, cx| {
            let host = crate::config::host(cx);
            range
                .map(|i| match &grouped.rows[i] {
                    GroupedRow::Heading { dir, count } => {
                        // A directory label, once, in the reference's voice:
                        // small muted sentence-case over a right-edge count —
                        // not the numbered stack's caps `section_label`.
                        // Centered in the file-row slot: `uniform_list`
                        // virtualizes one height, and the mock's group gaps
                        // absorb the extra air.
                        let label = match dir.as_ref().is_empty() {
                            true => SharedString::from("repository root"),
                            false => dir.clone(),
                        };
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_between()
                            .w_full()
                            .h(px(row_h))
                            .mx(px(0.0))
                            .px(px(8.0))
                            .font_family(face.clone())
                            .text_size(px((host.font.size * 0.72).round()))
                            .text_color(rgb(host.theme.dim_on(Surface::Context)))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(5.0))
                                    .min_w_0()
                                    // The reference's decorative chevron,
                                    // pointing down over its group. Kept a
                                    // glyph rather than a collapse control:
                                    // the contract says groups do not fold.
                                    .child(chrome::icon(
                                        "gitten/chevron.svg",
                                        11.0,
                                        host.theme.dim_on(Surface::Context),
                                    ))
                                    .child(div().truncate().child(label)),
                            )
                            .child(div().flex_none().child(count.clone()))
                            .into_any_element()
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
                        // The box is the fraction, not the section: a staged
                        // twin reads Full and draws checked, an unstaged twin
                        // reads Partial and draws the remainder mark, anything
                        // else draws empty. All three dispatch the same
                        // `files.stage` name — the act behind it stages a
                        // twin's remainder and unstages a whole — so the box
                        // and the keyboard never disagree about one row.
                        let box_fill = matches!(f.fraction, StageFraction::Full { .. });
                        let partial = matches!(f.fraction, StageFraction::Partial { .. });
                        let fraction_note: Option<SharedString> = match f.fraction {
                            StageFraction::Partial { staged, total } => {
                                Some(SharedString::from(format!("{staged}/{total}")))
                            }
                            _ => None,
                        };
                        let files_click = files.clone();
                        let files_stage = files.clone();
                        let stage_cmd = dispatch.clone();
                        let preview_after_stage = dispatch.clone();
                        let preview_row = dispatch.clone();
                        // A compact file row: a 14px stage box, filename and a
                        // trailing status — selected in the green tint with
                        // rounded ends and no keyboard bar, unselected plain.
                        let row = div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(9.0))
                            .w_full()
                            .h(px(row_h))
                            .mx(px(0.0))
                            .px(px(7.0))
                            .rounded(px(6.0))
                            .font_family(face.clone())
                            .bg(rgb(match (current, focused) {
                                (true, true) => host.theme.chrome.selection_bg,
                                (true, false) => host.theme.chrome.raised,
                                (false, _) => host.theme.chrome.bg,
                            }))
                            .child(
                                // The stage checkbox: checked when the row's
                                // side is fully staged, a remainder mark over
                                // an accent `staged/total` fraction when
                                // partially staged, an empty box otherwise.
                                // Plain text marks only — no icon-font
                                // codepoint the configured face may not carry
                                // (the checked state reads from the accent
                                // fill alone; see the deviation note in
                                // PLAN.md). A partial box spends the accent
                                // border on the reference's tinted ground.
                                div()
                                    .id(("ws-stage", i))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(14.0))
                                    .h(px(14.0))
                                    .rounded(px(4.0))
                                    .border_1()
                                    .border_color(rgb(match (box_fill, partial) {
                                        (true, _) => host.theme.chrome.accent,
                                        (false, true) => host.theme.chrome.accent,
                                        (false, false) => host.theme.chrome.faint,
                                    }))
                                    .when(box_fill, |d| d.bg(rgb(host.theme.chrome.accent)))
                                    .when(partial, |d| {
                                        d.text_color(rgb(host.theme.chrome.accent))
                                            .child("\u{2212}")
                                    })
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
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_grow(1.0)
                                    .flex()
                                    .flex_col()
                                    .justify_center()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(12.0))
                                            .text_color(rgb(host.theme.chrome.fg))
                                            .child(f.name.clone()),
                                    ),
                            )
                            .children(fraction_note.map(|note| {
                                div()
                                    .flex_none()
                                    .text_size(px((host.font.size * 0.75).round()))
                                    .text_color(rgb(host.theme.chrome.accent))
                                    .child(note)
                                    .into_any_element()
                            }))
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px((host.font.size * 0.75).round()))
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

    let nav = |n: usize,
               glyph: &'static str,
               label: &'static str,
               active: bool,
               command: &'static str| {
        // The reference's destinations: 35px rows, active in the green
        // tint with accent type, idle in dim on the rail ground. The glyph
        // takes the row's own ink, so the pair reads as one control.
        let dispatch = deps.dispatch.clone();
        let ink = match active {
            true => host.theme.chrome.accent,
            false => host.theme.dim_on(Surface::Context),
        };
        div()
            .id(("ws-nav", n))
            .flex_grow(1.0)
            .flex()
            .items_center()
            .justify_center()
            .gap(px(10.0))
            .h(px(35.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .bg(rgb(match active {
                true => host.theme.chrome.selection_bg,
                false => host.theme.chrome.bg,
            }))
            .text_color(rgb(ink))
            .child(chrome::icon(glyph, 15.0, ink))
            .child(label)
            .on_click(move |_, _, cx| dispatch(command, cx))
    };
    let util = |n: usize,
                glyph: &'static str,
                label: &'static str,
                command: &'static str,
                and_then: Option<&'static str>| {
        let dispatch = deps.dispatch.clone();
        let ink = host.theme.dim_on(Surface::Context);
        div()
            .id(("ws-util", n))
            .flex()
            .items_center()
            .gap(px(7.0))
            .cursor_pointer()
            .text_color(rgb(ink))
            .hover(|s| s.text_color(rgb(host.theme.chrome.fg)))
            .child(chrome::icon(glyph, 13.0, ink))
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
    let switch_project = deps.dispatch.clone();

    div()
        .flex()
        .flex_col()
        .size_full()
        .overflow_hidden()
        .bg(rgb(host.theme.chrome.bg))
        .font_family(root_face)
        .text_size(px(12.0))
        .child(
            // Repository identity, spelled by acquisition — never by the
            // view: the reference's 34px mark (branch glyph in accent on
            // the surface, hairline ring) over name and dim path.
            div()
                .id("workspace-project")
                .cursor_pointer()
                .on_click(move |_, _, cx| switch_project("project.switch", cx))
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(chrome::gap_m(&host.font))
                .px(px(RAIL_PX))
                .pt(px(23.0))
                .pb(px(22.0))
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(34.0))
                        .h(px(34.0))
                        .rounded(px(9.0))
                        .border_1()
                        .border_color(rgb(host.theme.chrome.border))
                        .bg(rgb(host.theme.chrome.title_bg))
                        .child(chrome::icon(
                            "gitten/branch.svg",
                            17.0,
                            host.theme.chrome.accent,
                        )),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_y(px(2.0))
                        .child(
                            div()
                                .truncate()
                                .text_color(rgb(host.theme.chrome.fg))
                                .child(deps.title.clone()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px((host.font.size * 0.72).round()))
                                .text_color(rgb(host.theme.dim_on(Surface::Context)))
                                .child(deps.subtitle.clone()),
                        ),
                ),
        )
        .child(
            // Changes / History: the destinations, in a 4px-gapped pair
            // over the utilities' hairline — the reference's segmented
            // pair, centered labels and all.
            div()
                .flex_none()
                .flex()
                .flex_row()
                .gap(px(4.0))
                .px(px(12.0))
                .pb(px(8.0))
                .child(nav(
                    0,
                    "gitten/changes.svg",
                    "Changes",
                    deps.destination == Destination::Changes,
                    "workspace.changes",
                ))
                .child(nav(
                    1,
                    "gitten/history.svg",
                    "History",
                    deps.destination == Destination::History,
                    "workspace.history",
                )),
        )
        .child(
            // Branches / Stashes: dim utilities spread edge to edge under
            // their own hairline, per the reference's sidebar-utilities.
            div()
                .flex_none()
                .flex()
                .flex_row()
                .justify_between()
                .px(px(20.0))
                .pt(px(14.0))
                .pb(px(19.0))
                .border_b_1()
                .border_color(rgb(host.theme.chrome.border))
                .child(util(
                    0,
                    "gitten/branch.svg",
                    "Branches",
                    "workspace.history",
                    Some("branches.focus"),
                ))
                .child(util(
                    1,
                    "gitten/stash.svg",
                    "Stashes",
                    "workspace.history",
                    Some("stashes.focus"),
                )),
        )
        .when(deps.history_note.is_none(), |d| {
            d.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(RAIL_PX))
                    .pt(px(20.0))
                    .pb(px(12.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.0))
                            .text_size(px((host.font.size * 0.8).round()))
                            .text_color(rgb(host.theme.chrome.fg))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Changed files"),
                            )
                            .child(
                                div()
                                    .text_color(rgb(host.theme.dim_on(Surface::Context)))
                                    .child(SharedString::from(deps.changed.to_string())),
                            )
                            // The filter's shown-over-loaded note rides after
                            // the count in the same muted ink, so a narrowed
                            // rail never looks like a repository that shrank.
                            .children(deps.filter_note.clone().map(|note| {
                                div()
                                    .text_color(rgb(host.theme.dim_on(Surface::Context)))
                                    .child(note)
                            })),
                    )
                    .child(
                        div()
                            .id("ws-stage-all")
                            .cursor_pointer()
                            // The reference's text button: the one accent word
                            // in the heading, not furniture.
                            .text_color(rgb(host.theme.chrome.accent))
                            .hover(|s| s.text_color(rgb(host.theme.chrome.fg)))
                            .child("Stage all")
                            .on_click(move |_, _, cx| stage_all("files.stage-all", cx)),
                    ),
            )
        })
        .when(deps.history_note.is_none(), |d| {
            d.child(
                // The filter, as a bordered surface box with its `/` hint —
                // a click opens the existing `files.search` prompt, whose
                // query already survives refresh.
                div()
                    .id("ws-filter")
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .mx(px(12.0))
                    .mb(px(10.0))
                    .px(px(8.0))
                    .h(px(30.0))
                    .rounded(px(5.0))
                    .border_1()
                    .border_color(rgb(host.theme.chrome.border))
                    .bg(rgb(host.theme.chrome.title_bg))
                    .cursor_pointer()
                    .text_size(px((host.font.size * 0.72).round()))
                    .text_color(rgb(host.theme.dim_on(Surface::Context)))
                    .child(chrome::icon(
                        "gitten/search.svg",
                        13.0,
                        host.theme.dim_on(Surface::Context),
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .flex_grow(1.0)
                            .truncate()
                            .child(filter_label),
                    )
                    .child(div().flex_none().child("/"))
                    .on_click(move |_, _, cx| filter_dispatch("files.search", cx)),
            )
        })
        .when(deps.history_note.is_none(), |d| {
            d.child(
                div()
                    .flex_grow(1.0)
                    .min_h_0()
                    .overflow_hidden()
                    .pt(px(5.0))
                    .px(px(10.0))
                    .child(list),
            )
        })
        .when(deps.history_note.is_none(), |d| {
            d.child(
                // The rail's footer: the staged/total hunk sentence over a
                // hairline — the reference's list-foot, reading the same map
                // the inspector spells.
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(px(RAIL_PX))
                    .py(px(12.0))
                    .border_t_1()
                    .border_color(rgb(host.theme.chrome.border))
                    .text_size(px((host.font.size * 0.72).round()))
                    .text_color(rgb(host.theme.dim_on(Surface::Context)))
                    .child(SharedString::from(format!(
                        "{hunks_staged} of {hunks_total} hunks staged"
                    ))),
            )
        })
        .children(deps.history_note.clone().map(|branch| {
            div()
                .flex_none()
                .flex()
                .flex_col()
                .px(px(RAIL_PX))
                .pt(px(24.0))
                .child(
                    div()
                        .text_size(px(10.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(host.theme.dim_on(Surface::Context)))
                        .child("CURRENT BRANCH"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .mt(px(12.0))
                        .text_size(px(12.0))
                        .text_color(rgb(host.theme.chrome.fg))
                        .child(chrome::icon(
                            "gitten/branch.svg",
                            15.0,
                            host.theme.chrome.accent,
                        ))
                        .child(branch),
                )
        }))
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .px(px(19.0))
                .py(px(16.0))
                .border_t_1()
                .border_color(rgb(host.theme.chrome.border))
                .text_size(px(9.0))
                .text_color(rgb(host.theme.dim_on(Surface::Context)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(
                            div()
                                .w(px(5.0))
                                .h(px(5.0))
                                .rounded_full()
                                .bg(rgb(host.theme.chrome.accent)),
                        )
                        .child("Local workspace"),
                )
                .child("gitten"),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::ws_row_h;
    use gitten_core::font::Font;

    /// One filename line plus padding, shared by every virtualized row.
    #[test]
    fn workspace_rows_are_compact() {
        let font = Font {
            family: String::from("test"),
            size: 15.0,
            advance: 0.6,
            monospaced: true,
        };
        assert_eq!(ws_row_h(&font), 30.0);
    }
}
