//! Checks the lane assignment's contract against real topology.
//!
//! The graph is only "correct" if these hold; eyeballing pixels cannot tell you.
use plait_core::*;
use std::collections::HashMap;

fn main() {
    let raw = String::from_utf8_lossy(&std::fs::read("fixtures/log.txt").unwrap()).into_owned();
    let commits = parse_log(&raw);
    let rows = assign_lanes(&commits);
    let index: HashMap<&str, usize> =
        commits.iter().enumerate().map(|(i, c)| (c.sha.as_str(), i)).collect();

    // How many children each commit has, in this window.
    let mut children: HashMap<&str, usize> = HashMap::new();
    for c in &commits {
        for p in &c.parents {
            *children.entry(p.as_str()).or_default() += 1;
        }
    }

    let (mut checked, mut inherit_ok, mut inherit_shared, mut inherit_bad) = (0, 0, 0, 0);
    let mut second_parent_same_lane = 0;

    for (i, c) in commits.iter().enumerate() {
        // 1. A commit's FIRST parent must continue on the same lane.
        if let Some(p) = c.parents.first() {
            if let Some(&j) = index.get(p.as_str()) {
                checked += 1;
                if rows[j].lane == rows[i].lane {
                    inherit_ok += 1;
                } else if children.get(p.as_str()).copied().unwrap_or(0) > 1 {
                    // Legitimate: an earlier commit already claimed that lane
                    // for the same parent, so this one collapses into it.
                    inherit_shared += 1;
                } else {
                    inherit_bad += 1;
                }
            }
        }
        // 2. Extra parents of a merge must NOT land on the merge's own lane.
        for p in c.parents.iter().skip(1) {
            if let Some(&j) = index.get(p.as_str()) {
                if rows[j].lane == rows[i].lane && children.get(p.as_str()).copied().unwrap_or(0) == 1 {
                    second_parent_same_lane += 1;
                }
            }
        }
    }

    println!("first-parent inherits the lane");
    println!("   checked          {checked}");
    println!("   same lane        {inherit_ok}  ({:.2}%)", inherit_ok as f64 / checked as f64 * 100.0);
    println!("   shared parent    {inherit_shared}  (legit: parent has >1 child)");
    println!("   UNEXPLAINED      {inherit_bad}");
    println!();
    println!("merge second parents on the merge's own lane (should be 0): {second_parent_same_lane}");
    assert_eq!(inherit_bad, 0, "first-parent lane continuity violated");
    assert_eq!(second_parent_same_lane, 0, "a fork landed on its own lane");
    println!("\nOK — lane contract holds across {} commits", commits.len());
}
