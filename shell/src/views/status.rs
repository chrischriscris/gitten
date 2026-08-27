//! The status pane: where HEAD sits, in one row.
//!
//! `⎇ full/full · ↑1 ↓0` — the branch, and how far it has drifted from its
//! upstream, or a ✓ when it has not. This is the fact lazygit's [1] Status
//! exists to say, and the one fact this window otherwise buries: the title
//! strip names the branch only while no filter is live, and the branches list
//! puts it among sixteen others. A ✓ is furniture ink, not a success green —
//! there is no green in [`ChromePalette`], and inventing one for a tick is a
//! colour that means one thing in one pane.
//!
//! It acquires nothing. The branches pane already reads `head()` on its
//! refresh wave — the only read of it this window pays for — so this pane
//! reads that model per frame and never disagrees with the list beside it.
//! An extension that took the branches pane over would starve this one of
//! its answer, which is the honest failure: an absent branch, not a stale
//! one.

use crate::chrome;
use crate::graph;
use crate::views::branches::Branches;
use gpui::*;

/// The status pane. `repo` is the repository's own name — the bright half of
/// the row — solved once at construction, the way every other pane solves its
/// label. `branches` is who owns the HEAD answer.
pub struct Status {
    repo: SharedString,
    branches: Option<Entity<Branches>>,
}

impl Status {
    pub fn new(repo: impl Into<SharedString>, branches: Option<Entity<Branches>>) -> Self {
        Self {
            repo: repo.into(),
            branches,
        }
    }
}

/// The one row's height, shared with every other list row.
const ROW_H: f32 = graph::ROW_H;

impl Render for Status {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = crate::config::host(cx);
        let c = host.theme.chrome;
        let ch = host.font.char_width();
        let info = self.branches.as_ref().and_then(|b| b.read(cx).head_info());
        let row = div()
            .flex()
            .items_center()
            .min_w_full()
            .h(px(ROW_H))
            .pl(px(chrome::ROW_PAD))
            .pr_2()
            .child(
                div()
                    .flex_none()
                    .text_color(rgb(c.fg))
                    .child(self.repo.clone()),
            );
        match info {
            Some(info) => row
                .child(
                    div()
                        .flex_none()
                        .px(px(ch * 0.5))
                        .text_color(rgb(c.faint))
                        .child("→"),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(rgb(c.dim))
                        .child(info.chip.clone()),
                )
                .children(match (info.ahead, info.behind) {
                    (Some(0), Some(0)) => Some(
                        div()
                            .flex_none()
                            .pl(px(ch))
                            .text_color(rgb(c.faint))
                            .child("✓"),
                    ),
                    (ahead, behind) => super::branches::drift(ahead, behind).map(|drift| {
                        div()
                            .flex_none()
                            .pl(px(ch))
                            .text_color(rgb(c.faint))
                            .child(SharedString::from(drift))
                    }),
                }),
            None => row.child(
                div()
                    .flex_none()
                    .pl(px(ch))
                    .text_color(rgb(c.faint))
                    .child("no branch"),
            ),
        }
    }
}

// The pane holds no cursor, so it has no view model to test — the facts it
// draws are `branches`' own, tested there. What is ours is the rendering
// rule: one row, quiet inks, and the in-sync tick exactly when both counts
// are zero-and-known.
#[cfg(test)]
mod tests {
    use super::Status;
    use gpui::{AppContext as _, TestAppContext};

    #[gpui::test]
    fn it_renders_without_a_branches_pane(cx: &mut TestAppContext) {
        cx.new(|_| Status::new("repo", None));
    }
}
