use gitten_app::env;
use gitten_core::*;
use std::collections::BTreeMap;

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

    let raw = read_fixture("fixtures/log.txt", json);
    let commits = parse_log(&raw);
    let rows = assign_lanes(&commits);

    let mut parents: BTreeMap<usize, usize> = BTreeMap::new();
    for c in &commits {
        *parents.entry(c.parents.len()).or_default() += 1;
    }

    let mut width: BTreeMap<usize, usize> = BTreeMap::new();
    for r in &rows {
        *width.entry(r.through.len() + 1).or_default() += 1;
    }

    let n = commits.len() as f64;
    if !json {
        println!(
            "  parents/commit: {}",
            parents
                .iter()
                .map(|(k, v)| format!("{k}p={:.1}%", *v as f64 / n * 100.0))
                .collect::<Vec<_>>()
                .join("  ")
        );
    }
    let mut cum = 0usize;
    let mut p50 = 0;
    let mut p99 = 0;
    for (w, c) in &width {
        cum += c;
        if p50 == 0 && cum as f64 >= n * 0.5 {
            p50 = *w;
        }
        if p99 == 0 && cum as f64 >= n * 0.99 {
            p99 = *w;
        }
    }
    if !json {
        println!(
            "  lanes active:   p50={p50}  p99={p99}  max={}",
            width.keys().last().unwrap()
        );
        println!(
            "  rows at 1 lane: {:.1}%",
            *width.get(&1).unwrap_or(&0) as f64 / n * 100.0
        );
        return;
    }

    let mut out = String::from("{");
    let mut first = true;
    sfield(&mut out, &mut first, "schema", "gitten.shape/1");
    sfield(&mut out, &mut first, "tool", "shape");
    nfield(&mut out, &mut first, "version", 1);
    nfield(&mut out, &mut first, "commits", commits.len());
    // Parent counts keyed by count: {"0": 7, "1": 60512, ...}.
    key(&mut out, &mut first, "parents");
    out.push('{');
    let mut pfirst = true;
    for (k, v) in &parents {
        key(&mut out, &mut pfirst, &k.to_string());
        out.push_str(&v.to_string());
    }
    out.push('}');
    nfield(&mut out, &mut first, "lanesP50", p50);
    nfield(&mut out, &mut first, "lanesP99", p99);
    nfield(
        &mut out,
        &mut first,
        "lanesMax",
        width.keys().last().unwrap_or(&0),
    );
    nfield(
        &mut out,
        &mut first,
        "rowsAtOneLanePct",
        format!("{:.3}", *width.get(&1).unwrap_or(&0) as f64 / n * 100.0),
    );
    out.push('}');
    println!("{out}");
}
