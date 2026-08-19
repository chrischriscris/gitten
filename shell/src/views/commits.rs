use crate::graph;
use gpui::*;
use gpui_component::scroll::Scrollbar;
use plait_core::{assign_lanes, parse_log, Commit};
use std::cell::Cell;
use std::rc::Rc;

struct Data {
    commits: Vec<Commit>,
    draws: Vec<graph::RowDraw>,
    lanes: usize,
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
    pub load: String,
}

impl Commits {
    pub fn total(&self) -> usize {
        self.data.commits.len()
    }
}

impl Commits {
    pub fn from_fixtures() -> Self {
        let t = std::time::Instant::now();
        // Git does not guarantee UTF-8 in commit metadata or file contents —
        // real history carries Latin-1 author names and worse. Lossy decoding
        // is correct here: never fail to show a repo over one bad byte.
        let raw = String::from_utf8_lossy(&std::fs::read("fixtures/log.txt").unwrap_or_default()).into_owned();
        let t_read = t.elapsed();

        let t = std::time::Instant::now();
        let commits = parse_log(&raw);
        let t_parse = t.elapsed();

        let t = std::time::Instant::now();
        let rows = assign_lanes(&commits);
        let t_lanes = t.elapsed();

        let t = std::time::Instant::now();
        let draws = graph::row_draws(&commits, &rows);
        let lanes = graph::lane_count(&draws);
        let t_draws = t.elapsed();

        let widest = commits
            .iter()
            .enumerate()
            .max_by_key(|(_, c)| c.subject.len())
            .map(|(i, _)| i)
            .unwrap_or(0);

        let load = format!(
            "{} commits · {} lanes · read {:.0?} parse {:.0?} lanes {:.0?} draws {:.0?}",
            commits.len(), lanes, t_read, t_parse, t_lanes, t_draws
        );
        eprintln!("{load}");
        Self {
            data: Rc::new(Data { commits, draws, lanes, widest }),
            scroll: UniformListScrollHandle::new(),
            rendered: Rc::new(Cell::new(0)),
            load,
        }
    }
}

impl Render for Commits {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let data = self.data.clone();
        let rendered = self.rendered.clone();
        let list = uniform_list("commits", data.commits.len(), move |range, _, _| {
            rendered.set(range.len());
            range
                .map(|i| row(&data.commits[i], &data.draws[i], data.lanes))
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

fn row(c: &Commit, d: &graph::RowDraw, lanes: usize) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_3()
        .h(px(graph::ROW_H))
        .child(graph::row_canvas(d.clone(), lanes))
        .child(div().flex_none().w(px(96.)).text_color(rgb(0x6e6862)).child(c.short.clone()))
        .child(div().flex_none().child(c.subject.clone()))
        .into_any_element()
}
