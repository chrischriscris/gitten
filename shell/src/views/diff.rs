//! The diff view.
//!
//! Everything is flattened to a uniform row list up front — file headers, hunk
//! headers and lines all the same height — so the whole thing virtualizes
//! through one `uniform_list` regardless of how large the diff is.
//!
//! Word-level spans come from `plait_core::intraline`, computed once at load.
//! Nothing here re-diffs during render or scroll.

use gpui::*;
use gpui_component::scroll::Scrollbar;
use plait_core::{intraline, replace_pairs, FileDiff, LineKind, Span};
use std::cell::Cell;
use std::rc::Rc;

const ROW_H: f32 = 22.0;
const GUTTER_W: f32 = 52.0;

/// Real repos contain minified bundles and base64 blobs; a single line of 9.6
/// million characters was measured in the wild. Text layout is linear in
/// length, so one such line stalls the frame. Nobody reads past column 2000.
const MAX_LINE_CHARS: usize = 2000;

fn clip(s: &str) -> String {
    let n = s.chars().count();
    if n <= MAX_LINE_CHARS {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX_LINE_CHARS).collect();
    format!("{head}  … {} more chars", n - MAX_LINE_CHARS)
}

enum Row {
    File { path: String, adds: usize, dels: usize },
    Hunk(String),
    Line { kind: LineKind, old: String, new: String, text: String, spans: Vec<Span> },
}

pub struct Diff {
    rows: Rc<Vec<Row>>,
    /// See the note in the commits view: uniform_list sizes its scrollable
    /// width from a single measured row, defaulting to row 0.
    widest: usize,
    scroll: UniformListScrollHandle,
    pub rendered: Rc<Cell<usize>>,
    pub load: String,
}

impl Diff {
    pub fn total(&self) -> usize {
        self.rows.len()
    }
}

impl Diff {
    pub fn new(files: Vec<FileDiff>) -> Self {
        let t = std::time::Instant::now();
        let mut intraline_time = std::time::Duration::ZERO;
        let mut rows = Vec::new();

        for f in &files {
            let adds = f.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == LineKind::Added).count();
            let dels = f.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == LineKind::Removed).count();
            rows.push(Row::File { path: f.path.clone(), adds, dels });

            for h in &f.hunks {
                rows.push(Row::Hunk(h.header.clone()));

                // Second pass: only the removed/added pairs a line diff already
                // matched get word-level spans.
                // Clip first, then diff the clipped text, so spans can never
                // point past what is rendered.
                let mut texts: Vec<String> = h.lines.iter().map(|l| clip(&l.text)).collect();
                let mut spans: Vec<Vec<Span>> = vec![Vec::new(); h.lines.len()];
                let ti = std::time::Instant::now();
                for (d, a) in replace_pairs(h) {
                    let (o, n) = intraline(&texts[d], &texts[a]);
                    spans[d] = o;
                    spans[a] = n;
                }
                intraline_time += ti.elapsed();

                for (i, l) in h.lines.iter().enumerate() {
                    rows.push(Row::Line {
                        kind: l.kind,
                        old: l.old_no.map(|n| n.to_string()).unwrap_or_default(),
                        new: l.new_no.map(|n| n.to_string()).unwrap_or_default(),
                        text: std::mem::take(&mut texts[i]),
                        spans: std::mem::take(&mut spans[i]),
                    });
                }
            }
        }
        let widest = rows
            .iter()
            .enumerate()
            .max_by_key(|(_, r)| match r {
                Row::Line { text, .. } => text.len(),
                Row::Hunk(h) => h.len(),
                Row::File { path, .. } => path.len(),
            })
            .map(|(i, _)| i)
            .unwrap_or(0);

        let t_build = t.elapsed();
        let load = format!(
            "{} rows · {} files · build {:.0?} (intraline {:.0?})",
            rows.len(), files.len(), t_build, intraline_time
        );
        eprintln!("{load}");
        Self {
            rows: Rc::new(rows),
            widest,
            scroll: UniformListScrollHandle::new(),
            rendered: Rc::new(Cell::new(0)),
            load,
        }
    }
}

impl Render for Diff {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows.clone();
        let rendered = self.rendered.clone();
        let list = uniform_list("diff", rows.len(), move |range, _, _| {
            rendered.set(range.len());
            range.map(|i| render_row(&rows[i])).collect()
        })
        .with_width_from_item(Some(self.widest))
        .track_scroll(&self.scroll)
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .size_full();

        div()
            .relative()
            .size_full()
            .child(list)
            .child(Scrollbar::vertical(&self.scroll))
            .child(Scrollbar::horizontal(&self.scroll))
    }
}

fn render_row(row: &Row) -> AnyElement {
    match row {
        Row::File { path, adds, dels } => div()
            .flex()
            .items_center()
            .gap_3()
            .h(px(ROW_H))
            .px_4()
            .bg(rgb(0x151312))
            .child(div().text_color(rgb(0xe8e3dc)).child(path.clone()))
            .child(div().text_color(rgb(0x6fbf73)).child(format!("+{adds}")))
            .child(div().text_color(rgb(0xd4736b)).child(format!("-{dels}")))
            .into_any_element(),

        Row::Hunk(header) => div()
            .flex()
            .items_center()
            .h(px(ROW_H))
            .px_4()
            .bg(rgb(0x14181d))
            .text_color(rgb(0x7d8fa8))
            .child(header.clone())
            .into_any_element(),

        Row::Line { kind, old, new, text, spans } => {
            let (bg, fg, sign, hl_bg, hl_fg) = match kind {
                LineKind::Added => (0x16241a, 0x9dc79b, "+", 0x2c5c33, 0xd6f0d4),
                LineKind::Removed => (0x2a1917, 0xd4a09a, "-", 0x6b2f2a, 0xf0d6d4),
                LineKind::Context => (0x0e0d0c, 0xa39c93, " ", 0x0e0d0c, 0xa39c93),
            };
            div()
                .flex()
                .items_center()
                .h(px(ROW_H))
                .px_4()
                .bg(rgb(bg))
                .child(num(old))
                .child(num(new))
                .child(div().flex_none().w(px(16.)).text_color(rgb(fg)).child(sign))
                .child(spanned(text, spans, fg, hl_bg, hl_fg))
                .into_any_element()
        }
    }
}

fn num(n: &str) -> Div {
    div()
        .flex_none()
        .w(px(GUTTER_W))
        .text_color(rgb(0x4a4540))
        .child(n.to_string())
}

/// Splits the line at span boundaries so changed words carry their own
/// background. Unchanged text and changed text are siblings in one flex row.
fn spanned(text: &str, spans: &[Span], fg: u32, hl_bg: u32, hl_fg: u32) -> Div {
    let mut row = div().flex().flex_none();
    let mut cursor = 0;
    for s in spans {
        if s.start > cursor {
            row = row.child(plain(&text[cursor..s.start], fg));
        }
        row = row.child(
            div()
                .flex_none()
                .bg(rgb(hl_bg))
                .text_color(rgb(hl_fg))
                .child(text[s.start..s.end].to_string()),
        );
        cursor = s.end;
    }
    if cursor < text.len() {
        row = row.child(plain(&text[cursor..], fg));
    }
    row
}

fn plain(text: &str, fg: u32) -> Div {
    div().flex_none().text_color(rgb(fg)).child(text.to_string())
}
