---
name: diff-rules
description: The rules for differs, alignment, intraline highlighting, move detection, layouts (unified/side-by-side registry) and wrapping that are wrong in the plausible direction. Load before changing core/src/{differ,align,wrap,markdown,rows}.rs, a diff view in gui/ or tui/, git/'s diff acquisition, or a Layout/Wrap registry entry.
---

# Diff rules

Read [docs/diff-rules.md](../../../docs/diff-rules.md) in full before the
change. It is the single copy; this skill only exists so it loads when you're
on the diff path without every other task paying for it.

Every rule there produces output that still looks like a working diff when
broken, so "the diff renders" is not evidence. Check a differ change with
`diffcheck` against git, and compare the hunk positions, not the totals:

```sh
cargo run -q -p gitten-git --example diffcheck --release [REPO] [REVSPEC]
```

For how the pipeline fits together, read
[docs/diff-pipeline.md](../../../docs/diff-pipeline.md).
