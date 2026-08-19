

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
    println!("DIFF     {:>9} lines  {:>5} files  {} replace-pairs", nlines, files.len(), pairs);
    println!("  read {:>9.1?}   parse {:>9.1?}   intraline {:>9.1?}", read, parse, intra);
}
