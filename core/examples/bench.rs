use gitten_app::env;
use gitten_core::*;
use std::time::{Duration, Instant};

/// Milliseconds, the unit every JSON number in here is in.
fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn jstr(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn fail(json: bool, code: &str, error: &str, hint: &str) -> ! {
    if json {
        let mut out = String::from("{");
        let mut first = true;
        for (k, v) in [("error", error), ("code", code), ("hint", hint)] {
            if !first {
                out.push(',');
            }
            first = false;
            jstr(&mut out, k);
            out.push(':');
            jstr(&mut out, v);
        }
        out.push('}');
        eprintln!("{out}");
        std::process::exit(1);
    }
    panic!("{error}");
}

fn key(out: &mut String, first: &mut bool, k: &str) {
    if !*first {
        out.push(',');
    }
    *first = false;
    jstr(out, k);
    out.push(':');
}

fn sfield(out: &mut String, first: &mut bool, k: &str, v: &str) {
    key(out, first, k);
    jstr(out, v);
}

fn nfield(out: &mut String, first: &mut bool, k: &str, v: impl std::fmt::Display) {
    key(out, first, k);
    out.push_str(&v.to_string());
}

fn read_fixture(path: &str, json: bool) -> String {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => fail(
            json,
            "io",
            &format!("{path}: {e}"),
            "run from the repository root so fixtures/ resolves",
        ),
    }
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let json = env::wants_json(&raw);

    let t = Instant::now();
    let raw = read_fixture("fixtures/log.txt", json);
    let read = t.elapsed();
    let t = Instant::now();
    let commits = parse_log(&raw);
    let parse = t.elapsed();
    let t = Instant::now();
    let rows = assign_lanes(&commits);
    let lanes = t.elapsed();
    let widest = rows.iter().map(|r| r.through.len() + 1).max().unwrap_or(0);
    if !json {
        println!("COMMITS  {:>9}  widest {:>2} lanes", commits.len(), widest);
        println!(
            "  read {:>9.1?}   parse {:>9.1?}   lanes {:>9.1?}",
            read, parse, lanes
        );
    }

    let t = Instant::now();
    let raw = read_fixture("fixtures/big.diff", json);
    let dread = t.elapsed();
    let t = Instant::now();
    let files = parse_unified_diff(&raw);
    let dparse = t.elapsed();
    let nlines: usize = files
        .iter()
        .flat_map(|f| &f.hunks)
        .map(|h| h.lines.len())
        .sum();

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
            paired += (s.left().is_some() && s.right().is_some()) as usize;
        }
    }
    let aligned = t.elapsed();

    // The wrap pass, which is what a resize costs. Over the prepared rows, at a
    // budget a real window gives: 1440px of text in a 14px monospaced face is
    // about 150 columns. This is the number a drag pays per column crossed, so it
    // is the one that decides whether reflowing on resize is viable at all.
    // `WRAP_COLS=80` for a narrow window: the budget is what decides how many
    // rows wrapping adds, and at a normal width on code that is almost none.
    let cols: usize = env::wrap_cols(150);
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

    if !json {
        println!(
            "DIFF     {:>9} lines  {:>5} files  {} replace-pairs",
            nlines,
            files.len(),
            pairs
        );
        println!(
            "  read {:>9.1?}   parse {:>9.1?}   intraline {:>9.1?}",
            dread, dparse, intra
        );
        // `prepare` is wall clock and the two beside it are CPU summed across
        // workers, so above one worker they deliberately do not add up. Printing the
        // count is what keeps that from reading as a broken measurement — and the
        // MB/s figure is throughput per core for the same reason.
        println!(
            "  prepare {:>6.1?}   intraline {:>7.1?}  syntax {:>7.1?}  ×{} cpu  {} tokens  {:.1} MB scanned ({:.0} MB/s/core)",
            build,
            p.intraline,
            p.syntax,
            p.threads,
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
        return;
    }

    let mb = bytes as f64 / 1e6;
    let mut out = String::new();
    out.push('{');
    // Numbers are {:.3} ms, ratios full precision — a machine does its own
    // rounding.
    let mut first = true;
    sfield(&mut out, &mut first, "schema", "gitten.bench/1");
    sfield(&mut out, &mut first, "tool", "bench");
    nfield(&mut out, &mut first, "version", 1);
    nfield(&mut out, &mut first, "commits", commits.len());
    nfield(&mut out, &mut first, "widestLanes", widest);
    nfield(
        &mut out,
        &mut first,
        "commitReadMs",
        format!("{:.3}", ms(read)),
    );
    nfield(
        &mut out,
        &mut first,
        "commitParseMs",
        format!("{:.3}", ms(parse)),
    );
    nfield(&mut out, &mut first, "lanesMs", format!("{:.3}", ms(lanes)));
    nfield(&mut out, &mut first, "diffLines", nlines);
    nfield(&mut out, &mut first, "diffFiles", files.len());
    nfield(&mut out, &mut first, "replacePairs", pairs);
    nfield(
        &mut out,
        &mut first,
        "diffReadMs",
        format!("{:.3}", ms(dread)),
    );
    nfield(
        &mut out,
        &mut first,
        "diffParseMs",
        format!("{:.3}", ms(dparse)),
    );
    nfield(
        &mut out,
        &mut first,
        "intralineMs",
        format!("{:.3}", ms(intra)),
    );
    nfield(
        &mut out,
        &mut first,
        "prepareMs",
        format!("{:.3}", ms(build)),
    );
    nfield(
        &mut out,
        &mut first,
        "prepareIntralineCpuMs",
        format!("{:.3}", ms(p.intraline)),
    );
    nfield(
        &mut out,
        &mut first,
        "prepareSyntaxCpuMs",
        format!("{:.3}", ms(p.syntax)),
    );
    nfield(&mut out, &mut first, "prepareThreads", p.threads);
    nfield(&mut out, &mut first, "tokens", tokens);
    nfield(&mut out, &mut first, "bytes", bytes);
    nfield(
        &mut out,
        &mut first,
        "mbPerSecPerCore",
        format!("{:.3}", mb / p.syntax.as_secs_f64().max(1e-9)),
    );
    nfield(
        &mut out,
        &mut first,
        "alignMs",
        format!("{:.3}", ms(aligned)),
    );
    nfield(&mut out, &mut first, "alignRows", slots);
    nfield(
        &mut out,
        &mut first,
        "alignPctOfLines",
        format!("{:.3}", 100.0 * slots as f64 / nlines.max(1) as f64),
    );
    nfield(&mut out, &mut first, "alignPaired", paired);
    nfield(
        &mut out,
        &mut first,
        "alignNsPerRow",
        format!("{:.1}", aligned.as_secs_f64() * 1e9 / slots.max(1) as f64),
    );
    nfield(
        &mut out,
        &mut first,
        "wrapMs",
        format!("{:.3}", ms(wrapping)),
    );
    nfield(&mut out, &mut first, "wrapRows", wrapped.total());
    nfield(&mut out, &mut first, "wrapCols", cols);
    nfield(
        &mut out,
        &mut first,
        "wrapXLines",
        format!("{:.4}", wrapped.total() as f64 / nlines.max(1) as f64),
    );
    nfield(
        &mut out,
        &mut first,
        "wrapNsPerLine",
        format!("{:.1}", wrapping.as_secs_f64() * 1e9 / nlines.max(1) as f64),
    );
    nfield(&mut out, &mut first, "wrapRejected", wrapped.rejected());
    if md_files > 0 {
        nfield(
            &mut out,
            &mut first,
            "markdownMs",
            format!("{:.3}", ms(layout)),
        );
        nfield(&mut out, &mut first, "markdownRows", md_rows);
        nfield(&mut out, &mut first, "markdownFiles", md_files);
        nfield(
            &mut out,
            &mut first,
            "markdownNsPerRow",
            format!("{:.1}", layout.as_secs_f64() * 1e9 / md_rows.max(1) as f64),
        );
        nfield(
            &mut out,
            &mut first,
            "markdownPctOfPrepare",
            format!("{:.3}", 100.0 * layout.as_secs_f64() / build.as_secs_f64()),
        );
    } else {
        // Null, not absent: a machine should not have to guess whether the
        // fixture held Markdown.
        key(&mut out, &mut first, "markdownMs");
        out.push_str("null");
    }
    out.push('}');
    println!("{out}");
}

fn is_markdown(path: &str) -> bool {
    matches!(path.rsplit('.').next(), Some("md" | "markdown" | "mdx"))
}
