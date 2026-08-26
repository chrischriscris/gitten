use crate::graph;
use gitten_core::host::Host;
use gitten_core::{assign_lanes, initials, Commit};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::scroll::Scrollbar;
use std::cell::Cell;
use std::rc::Rc;

/// The commit column between the sha and the graph, resolved once at load: two
/// letters. Not the colour: that follows the live theme like everything else
/// on the row.
struct Who {
    initials: SharedString,
}

struct Data {
    commits: Vec<Commit>,
    draws: Vec<graph::Draw>,
    who: Vec<Who>,
    /// uniform_list measures exactly ONE row to decide how wide the content is,
    /// and by default that is row 0. If row 0 is short there is nothing to
    /// scroll to, however long the rest are. Point it at the real widest row.
    widest: usize,
}

pub struct Commits {
    data: Rc<Data>,
    scroll: UniformListScrollHandle,
    /// Instrumentation the view owns and anyone may read. The view does not
    /// know the stats overlay exists.
    pub rendered: Rc<Cell<usize>>,
    /// First visible row, for the session — see the note in the diff view.
    pub top: Rc<Cell<usize>>,
    pub load: String,
}

impl Commits {
    /// Puts a saved row back at the top of the viewport. Clamped — see the diff
    /// view's note.
    pub fn scroll_to(&self, row: usize) {
        if self.data.commits.is_empty() {
            return;
        }
        self.scroll
            .scroll_to_item(row.min(self.data.commits.len() - 1), ScrollStrategy::Top);
    }

    pub fn total(&self) -> usize {
        self.data.commits.len()
    }
}

/// One row's reach, roughly: the graph gutter plus the subject. An estimate,
/// because this number only decides which single row `uniform_list` measures
/// to learn the true scrollable width — see the note in `Data`.
///
/// Characters, never `.len()`: a byte-lengthed CJK subject counted itself
/// three times too wide and could dethrone a genuinely wider ASCII row,
/// leaving that row clipped past the last reachable column forever.
///
/// Characters shrink the error; they move it rather than close it. Common
/// scripts measure strictly closer — each CJK glyph counts once instead of
/// three times, and the count is exact for Cyrillic — while a pure-ASCII
/// repository ranks identically either way. The comparison is still a sum of
/// counts rather than of columns, so rankings that mix scripts can invert
/// where their ASCII:CJK ratios fall inside a miss-window. That residual
/// window belongs to the approximation the first paragraph already disclaims.
/// Completing it would mean a display-width table in `core`, dependency-free
/// like the differs.
fn estimated_row_width(gutter: &graph::Draw, subject: &str, char_w: f32) -> f32 {
    graph::row_width(gutter) + subject.chars().count() as f32 * char_w
}

impl Commits {
    pub fn new(commits: Vec<Commit>, host: Rc<Host>) -> Self {
        let t = std::time::Instant::now();
        let rows = assign_lanes(&commits);
        let t_lanes = t.elapsed();

        let t = std::time::Instant::now();
        let draws = graph::row_draws(&commits, &rows);
        let lanes = graph::lane_count(&rows);
        let t_draws = t.elapsed();

        let who: Vec<Who> = commits
            .iter()
            .map(|c| Who {
                initials: initials(&c.author).into(),
            })
            .collect();

        // The widest row is no longer just the longest subject: every row's
        // graph is only as wide as its own lanes, so a short message behind a
        // wide graph can still out-reach a long one on the trunk.
        //
        // One character's width comes from the host's font rather than a constant
        // measured on whatever the font used to be. It only picks which row
        // `uniform_list` measures, so an approximation is fine — and it is
        // meaningless for a proportional face, which is the honest reason a
        // long subject may then win over a wide graph.
        let char_w = host.font.char_width();
        let widest = draws
            .iter()
            .zip(&commits)
            .map(|(d, c)| estimated_row_width(d, &c.subject, char_w))
            .enumerate()
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(i, _)| i)
            .unwrap_or(0);

        let load = format!(
            "{} commits · {} lanes · lanes {:.0?} draws {:.0?}",
            commits.len(),
            lanes,
            t_lanes,
            t_draws
        );
        eprintln!("{load}");

        Self {
            data: Rc::new(Data {
                commits,
                draws,
                who,
                widest,
            }),
            scroll: UniformListScrollHandle::new(),
            rendered: Rc::new(Cell::new(0)),
            top: Rc::new(Cell::new(0)),
            load,
        }
    }
}

impl Render for Commits {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let data = self.data.clone();
        let rendered = self.rendered.clone();
        let top = self.top.clone();
        // Read per batch, not captured at construction — see the note in the
        // diff view: this is what makes a saved config apply on the next frame.
        let list = uniform_list("commits", data.commits.len(), move |range, _, cx| {
            rendered.set(range.len());
            top.set(range.start);
            let host = crate::config::host(cx);
            range
                .map(|i| row(&data.commits[i], &data.who[i], &data.draws[i], &host))
                .collect()
        })
        .with_width_from_item(Some(self.data.widest))
        .track_scroll(&self.scroll)
        // Let rows exceed the viewport width instead of being clipped; this is
        // what turns on horizontal scrolling.
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .size_full()
        .p_4();

        // The scrollbar overlays the list, so the container must be positioned.
        // `[view] scrollbar` is read per frame like every other setting: the
        // terminal draws its own bar from the same flag, and a knob that means
        // two things in two clients is a knob nobody trusts.
        let bars = crate::config::host(cx).view.scrollbar;
        div().relative().size_full().child(list).when(bars, |d| {
            d.child(Scrollbar::vertical(&self.scroll))
                .child(Scrollbar::horizontal(&self.scroll))
        })
    }
}

/// The sha and the initials columns, in *characters*.
///
/// Twelve, because `%h` is seven in a young repository and eleven in git/git,
/// plus the air after it. In pixels rather than characters these were 90 and 26,
/// which is 10.7 and 3.1 in the shipped face — so an eleven-character sha
/// overflowed its own column by two pixels while the comment above it said
/// eleven — and 5 and 1.4 at the 18px `font.size` the config file will happily
/// give you. Fixed columns, unlike the graph: the eye scans these vertically, so
/// they have to *be* columns.
const SHA_CHARS: f32 = 12.0;
const WHO_CHARS: f32 = 3.0;

/// lazygit's order — sha, author, graph, subject — and lazygit's spacing: the
/// subject follows its own row's graph immediately, so a commit on the trunk
/// reads from the left instead of starting behind the widest merge in the
/// repository.
fn row(c: &Commit, who: &Who, d: &graph::Draw, host: &Rc<Host>) -> AnyElement {
    let ch = host.font.char_width();
    div()
        .flex()
        .items_center()
        .h(px(graph::ROW_H))
        .child(
            div()
                .flex_none()
                .w(px(SHA_CHARS * ch))
                .text_color(rgb(host.theme.chrome.dim))
                .child(c.short.clone()),
        )
        .child(
            div()
                .flex_none()
                .w(px(WHO_CHARS * ch))
                // The colour resolves here, not at construction, so it reads
                // the live theme like the dim sha and the character width
                // beside it. Deliberate cost: one byte-fold hash of the author
                // name per visible row per frame. A memo HashMap arrives only
                // if profiling ever demands one.
                .text_color(rgb(host.theme.author(&c.author)))
                .child(who.initials.clone()),
        )
        .child(graph::row_canvas(d.clone(), host.clone()))
        .child(div().flex_none().child(c.subject.clone()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]`.
    use super::estimated_row_width;
    use crate::graph;
    use gitten_core::{assign_lanes, Commit};

    #[test]
    fn widest_row_ranks_subjects_by_characters_not_bytes() {
        // Forty-five ASCII characters are also forty-five bytes.
        let ascii = Commit {
            sha: "aaaa".into(),
            short: "aaaa".into(),
            parents: Vec::new().into(),
            author: "Ann Author".into(),
            timestamp: 0,
            subject: "a".repeat(45),
        };
        // Twenty CJK characters are sixty UTF-8 bytes: ranked by byte length
        // this row wins and the wider ASCII row clips past the last column
        // `uniform_list` will ever scroll to.
        let cjk = Commit {
            sha: "bbbb".into(),
            short: "bbbb".into(),
            parents: Vec::new().into(),
            author: "Bob Blob".into(),
            timestamp: 0,
            subject: "日".repeat(20),
        };

        let commits = vec![ascii, cjk];
        let rows = assign_lanes(&commits);
        let draws = graph::row_draws(&commits, &rows);
        let char_w = 12.0;

        let widest = draws
            .iter()
            .zip(&commits)
            .map(|(d, c)| estimated_row_width(d, &c.subject, char_w))
            .enumerate()
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(i, _)| i)
            .unwrap_or(0);
        assert_eq!(widest, 0, "the ASCII subject must outrank the CJK one");

        // The inversion itself: the same rows ranked by bytes, as this code
        // once was, pick the CJK row. This keeps the test from passing
        // vacuously should both estimates ever drift together.
        //
        // The one `str::len` left standing in this file, on purpose: clippy
        // wants it (needless_as_bytes, bytes_count_to_len reject every other
        // spelling of byte length), and here it stands in for the regression
        // being asserted against.
        let by_bytes = draws
            .iter()
            .zip(&commits)
            .map(|(d, c)| graph::row_width(d) + c.subject.len() as f32 * char_w)
            .enumerate()
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(i, _)| i)
            .unwrap_or(0);
        assert_eq!(by_bytes, 1, "the byte-lengthed rank picks the CJK row");
    }
}
