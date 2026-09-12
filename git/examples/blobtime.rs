//! What one blob pair read costs, on a repository with a modified binary file.
//!
//! The number quoted in [docs/blobs.md](../../docs/blobs.md#what-this-costs)
//! and in [decisions/0032](../../docs/decisions/0032-blobs-are-shown-by-the-renderer.md):
//!
//! ```sh
//! cargo run -q --release -p gitten-git --example blobtime -- pic.png
//! ```
//!
//! Run it *inside* the repository you mean — the handle opens `.` — and make
//! sure the named path really has an unstaged change, or the read answers
//! empty for the honest reason that nothing moved.
//!
//! Release and not debug: AGENTS.md's rule, and here it matters little — the
//! cost is two `git` process spawns rather than anything this process does —
//! but a debug number quoted in a doc is still a number nobody should trust.
use gitten_core::source::DiffSource;
use std::time::Instant;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: blobtime <path-with-an-unstaged-change>");
    let repo = gitten_git::open(std::path::Path::new("."));
    let source = DiffSource::Unstaged {
        path: path.as_str().into(),
    };
    let cap = 64 << 20;
    // One read before the clock starts: the first `git` on a cold cache pays a
    // page-in that no later round does, and it is not what a viewer pays.
    let warmed = repo
        .blob_pair(&source, path.as_bytes(), cap)
        .expect("a pair");
    assert!(!warmed.is_empty(), "{path} has no unstaged change to read");

    let rounds = 10;
    let start = Instant::now();
    for _ in 0..rounds {
        let pair = repo
            .blob_pair(&source, path.as_bytes(), cap)
            .expect("a pair");
        assert!(!pair.is_empty());
    }
    let each = start.elapsed() / rounds;
    let bytes = warmed.new.blob().map_or(0, |blob| blob.bytes.len());
    println!("{path}: {each:?} a read ({bytes} bytes behind it)");
}
