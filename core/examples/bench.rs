

use plait_core::*;
use std::time::Instant;

fn main() {
    let t = Instant::now();
    let raw = String::from_utf8_lossy(&std::fs::read("fixtures/log.txt").unwrap()).into_owned();
    let read = t.elapsed();
    let t = Instant::now();
    let commits = parse_log(&raw);
    let parse = t.elapsed();
    let t = Instant::now();
    let rows = assign_lanes(&commits);
    let lanes = t.elapsed();
    let widest = rows.iter().map(|r| r.through.len() + 1).max().unwrap_or(0);
    println!("COMMITS  {:>9}  widest {:>2} lanes", commits.len(), widest);
    println!("  read {:>9.1?}   parse {:>9.1?}   lanes {:>9.1?}", read, parse, lanes);

    let t = Instant::now();
    let raw = String::from_utf8_lossy(&std::fs::read("fixtures/big.diff").unwrap()).into_owned();
    let read = t.elapsed();
    let t = Instant::now();
    let files = parse_unified_diff(&raw);
    let parse = t.elapsed();
    let nlines: usize = files.iter().flat_map(|f| &f.hunks).map(|h| h.lines.len()).sum();

    let t = Instant::now();
    let mut pairs = 0usize;
    for f in &files {
        for h in &f.hunks {
            for (d, a) in replace_pairs(h) {
                let _ = intraline(&h.lines[d].text, &h.lines[a].text);
                pairs += 1;
            }
        }
    }
    let intra = t.elapsed();

    // The whole assembly the views use, in one call: clip, intraline, syntax.
    let host = host::Host::new();
    let t = Instant::now();
    let mut p = prepared::prepare(&files, &host.syntax, 2000);
    let build = t.elapsed();
    let (mut tokens, mut bytes) = (0usize, 0usize);
    for l in p.files.iter().flat_map(|f| &f.hunks).flat_map(|h| &h.lines) {
        tokens += l.tokens.len();
        bytes += l.text.len();
    }

    // The markdown pass, on whatever of this diff is markdown. It reuses the
    // tokens `prepare` just produced, so what is measured is the block pass, the
    // marker removal and the range remapping — nothing else.
    //
    // In place, on `p` itself, and deliberately not on a clone: duplicating a
    // 714k-line prepared diff to protect five markdown files put the first touch
    // of every one of those files inside the timer, and reported 610 µs for 44
    // rows. This is also what the view does — `lay_out` runs on the rows
    // `prepare` handed it, still warm.
    let layout = markdown::Layout::monospaced();
    let t = Instant::now();
    let (mut md_rows, mut md_files) = (0usize, 0usize);
    for f in p.files.iter_mut().filter(|f| is_markdown(&f.path)) {
        md_files += 1;
        for h in &mut f.hunks {
            md_rows += markdown::lay_out(&mut h.lines, &layout).len();
        }
    }
    let layout = t.elapsed();

    // The alignment pass, which is what a two-column presentation costs in
    // `core`. Over the prepared rows, which is where a renderer calls it from.
    let t = Instant::now();
    let (mut slots, mut paired) = (0usize, 0usize);
    for h in p.files.iter().flat_map(|f| &f.hunks) {
        let kinds: Vec<LineKind> = h.lines.iter().map(|l| l.kind).collect();
        for s in align::align(&kinds) {
            slots += 1;
            paired += (s.old().is_some() && s.new().is_some()) as usize;
        }
    }
    let aligned = t.elapsed();

    // The wrap pass, which is what a resize costs. Over the prepared rows, at a
    // budget a real window gives: 1440px of text in a 14px monospaced face is
    // about 150 columns. This is the number a drag pays per column crossed, so it
    // is the one that decides whether reflowing on resize is viable at all.
    // `WRAP_COLS=80` for a narrow window: the budget is what decides how many
    // rows wrapping adds, and at a normal width on code that is almost none.
    let cols: usize = std::env::var("WRAP_COLS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);
    let t = Instant::now();
    let wrapped = wrap::Wrapped::build(
        p.files
            .iter()
            .flat_map(|f| &f.hunks)
            .flat_map(|h| &h.lines)
            .map(|l| (l.text.as_ref(), cols)),
        &wrap::Word,
    );
    let wrapping = t.elapsed();

    println!("DIFF     {:>9} lines  {:>5} files  {} replace-pairs", nlines, files.len(), pairs);
    println!("  read {:>9.1?}   parse {:>9.1?}   intraline {:>9.1?}", read, parse, intra);
    println!(
        "  prepare {:>6.1?}   intraline {:>7.1?}  syntax {:>7.1?}  {} tokens  {:.1} MB scanned ({:.0} MB/s)",
        build,
        p.intraline,
        p.syntax,
        tokens,
        bytes as f64 / 1e6,
        (bytes as f64 / 1e6) / p.syntax.as_secs_f64()
    );
    println!(
        "  align {:>8.1?}   {} rows ({:.0}% of {} lines)  {} paired  {:.0} ns/row",
        aligned,
        slots,
        100.0 * slots as f64 / nlines.max(1) as f64,
        nlines,
        paired,
        aligned.as_secs_f64() * 1e9 / slots.max(1) as f64,
    );
    println!(
        "  wrap {:>9.1?}   {} rows at {cols} cols ({:.2}x {} lines)  {:.0} ns/line  {} rejected",
        wrapping,
        wrapped.total(),
        wrapped.total() as f64 / nlines.max(1) as f64,
        nlines,
        wrapping.as_secs_f64() * 1e9 / nlines.max(1) as f64,
        wrapped.rejected(),
    );
    if md_files > 0 {
        println!(
            "  markdown {:>5.1?}   {} rows  {} files  {:.0} ns/row  ({:.1}% of prepare)",
            layout,
            md_rows,
            md_files,
            layout.as_secs_f64() * 1e9 / md_rows.max(1) as f64,
            100.0 * layout.as_secs_f64() / build.as_secs_f64(),
        );
    }
}

fn is_markdown(path: &str) -> bool {
    matches!(path.rsplit('.').next(), Some("md" | "markdown" | "mdx"))
}
