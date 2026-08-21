use crate::graph;
use gpui::*;
use gpui_component::scroll::Scrollbar;
use plait_core::host::Host;
use plait_core::{assign_lanes, initials, Commit};
use std::cell::Cell;
use std::rc::Rc;

/// The commit column between the sha and the graph, resolved once at load: two
/// letters and the colour they are drawn in. Not a per-frame job.
struct Who {
    initials: SharedString,
    color: Rgba,
}

struct Data {
    commits: Vec<Commit>,
    draws: Vec<graph::RowDraw>,
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
        self.scroll.scroll_to_item(row.min(self.data.commits.len() - 1), ScrollStrategy::Top);
    }

    pub fn total(&self) -> usize {
        self.data.commits.len()
    }
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
                color: rgb(host.theme.author(&c.author)),
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
            .map(|(d, c)| d.width() + c.subject.len() as f32 * char_w)
            .enumerate()
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(i, _)| i)
            .unwrap_or(0);

        let load = format!(
            "{} commits · {} lanes · lanes {:.0?} draws {:.0?}",
            commits.len(), lanes, t_lanes, t_draws
        );
        eprintln!("{load}");

        Self {
            data: Rc::new(Data { commits, draws, who, widest }),
            scroll: UniformListScrollHandle::new(),
            rendered: Rc::new(Cell::new(0)),
            top: Rc::new(Cell::new(0)),
            load,
        }
    }
}

impl Render for Commits {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
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
        div()
            .relative()
            .size_full()
            .child(list)
            .child(Scrollbar::vertical(&self.scroll))
            .child(Scrollbar::horizontal(&self.scroll))
    }
}



/// Wide enough for an eleven-character sha and a space after it. Fixed, unlike
/// the graph: the eye scans this column vertically, so it has to be a column.
const SHA_W: f32 = 90.0;
const WHO_W: f32 = 26.0;

/// lazygit's order — sha, author, graph, subject — and lazygit's spacing: the
/// subject follows its own row's graph immediately, so a commit on the trunk
/// reads from the left instead of starting behind the widest merge in the
/// repository.
fn row(c: &Commit, who: &Who, d: &graph::RowDraw, host: &Rc<Host>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .h(px(graph::ROW_H))
        .child(
            div()
                .flex_none()
                .w(px(SHA_W))
                .text_color(rgb(host.theme.chrome.dim))
                .child(c.short.clone()),
        )
        .child(div().flex_none().w(px(WHO_W)).text_color(who.color).child(who.initials.clone()))
        .child(graph::row_canvas(d.clone(), host.clone()))
        .child(div().flex_none().child(c.subject.clone()))
        .into_any_element()
}

