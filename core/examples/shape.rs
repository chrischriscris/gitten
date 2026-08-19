

use plait_core::*;
use std::collections::BTreeMap;

fn main() {
    let raw = String::from_utf8_lossy(&std::fs::read("fixtures/log.txt").unwrap()).into_owned();
    let commits = parse_log(&raw);
    let rows = assign_lanes(&commits);

    let mut parents: BTreeMap<usize, usize> = BTreeMap::new();
    for c in &commits { *parents.entry(c.parents.len()).or_default() += 1; }

    let mut width: BTreeMap<usize, usize> = BTreeMap::new();
    for r in &rows { *width.entry(r.through.len() + 1).or_default() += 1; }

    let n = commits.len() as f64;
    println!("  parents/commit: {}", parents.iter()
        .map(|(k,v)| format!("{k}p={:.1}%", *v as f64/n*100.0)).collect::<Vec<_>>().join("  "));
    let mut cum = 0usize; let mut p50 = 0; let mut p99 = 0;
    for (w, c) in &width {
        cum += c;
        if p50 == 0 && cum as f64 >= n*0.5 { p50 = *w; }
        if p99 == 0 && cum as f64 >= n*0.99 { p99 = *w; }
    }
    println!("  lanes active:   p50={p50}  p99={p99}  max={}", width.keys().last().unwrap());
    println!("  rows at 1 lane: {:.1}%", *width.get(&1).unwrap_or(&0) as f64/n*100.0);
}
