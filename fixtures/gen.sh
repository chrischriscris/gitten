#!/usr/bin/env bash
# Synthetic fixtures at arbitrary scale, for stress testing the views.
# Real repos cap out; this doesn't.
#
#   ./fixtures/gen.sh 1000000 1000000     # 1M commits, 1M diff lines
#   ./fixtures/gen.sh 50000 20000
#
# Topology is realistic, not random: mostly linear with periodic merges that
# fork a lane and collapse it a few commits later, which is what actually
# exercises the lane allocator.
set -euo pipefail
COMMITS="${1:-100000}"
DIFFLINES="${2:-100000}"
# Hermetic override: point at a scratch dir and the committed fixtures are
# never touched. Defaults to the fixtures dir beside this script.
OUT="${GITTEN_FIXTURES:-$(cd "$(dirname "$0")" && pwd)}"
mkdir -p "$OUT"

python3 - "$COMMITS" "$DIFFLINES" "$OUT" <<'PY'
import os, sys, random
n, dl, out = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
random.seed(7)

sha = lambda i: f"{i:040x}"
VERBS = ["Fix","Add","Remove","Refactor","Guard","Cache","Inline","Hoist","Split","Rename"]
NOUNS = ["dispatch loop","lane allocator","diff parser","scroll handle","token budget",
         "extension host","commit graph","hunk renderer","keymap resolver","worktree scan"]

# Build newest-first. Commit i's first parent is i+1; every ~40th is a merge
# whose second parent is a few commits further back, forking a lane that
# collapses when the allocator reaches the shared ancestor.
# Written to temp files and renamed into place, so an interrupted run never
# leaves a half-written fixture behind.
with open(f"{out}/.log.txt.tmp", "w", buffering=1 << 20) as f:
    for i in range(n):
        parents = []
        if i + 1 < n:
            parents.append(sha(i + 1))
            # Long-lived branches: the second parent is far back, so the lane
            # stays open for hundreds of rows and many run concurrently. Merging
            # into the very next commit (the naive version) never exceeds 2
            # lanes and tests nothing.
            if i % 20 == 0:
                back = i + random.randint(150, 400)
                if back < n:
                    parents.append(sha(back))
        subj = f"{random.choice(VERBS)} {random.choice(NOUNS)}"
        if len(parents) > 1:
            subj = f"Merge branch 'feat/{i}' into main"
        f.write(f"{sha(i)}\x1f{sha(i)[:9]}\x1f{' '.join(parents)}\x1f"
                f"Dev {i%7}\x1f{1700000000 - i * 60}\x1f{subj}\x1e")

# Diff: files of hunks, with replace pairs carrying small word-level edits so
# the intraline pass has real work to do.
with open(f"{out}/.big.diff.tmp", "w", buffering=1 << 20) as f:
    written, fi = 0, 0
    while written < dl:
        f.write(f"diff --git a/pkg/mod{fi}/file{fi}.go b/pkg/mod{fi}/file{fi}.go\n")
        f.write("index 1111111..2222222 100644\n")
        f.write(f"--- a/pkg/mod{fi}/file{fi}.go\n+++ b/pkg/mod{fi}/file{fi}.go\n")
        for h in range(12):
            start = h * 30 + 1
            f.write(f"@@ -{start},14 +{start},15 @@ func handler{h}() error {{\n")
            for k in range(4):
                f.write(f" \tctx := context.WithValue(base, key{k}, val{k})\n")
            f.write(f"-\tif err := run(ctx, {h}); err != nil {{\n")
            f.write(f"+\tif err := run(ctx, {h}, opts); err != nil {{\n")
            f.write(f"-\t\treturn fmt.Errorf(\"run: %w\", err)\n")
            f.write(f"+\t\treturn fmt.Errorf(\"run {h}: %w\", err)\n")
            f.write(f"+\tmetrics.Observe(\"handler\", {h})\n")
            for k in range(4):
                f.write(f" \tdefer cleanup{k}(ctx)\n")
            written += 14
            if written >= dl:
                break
        fi += 1

os.replace(f"{out}/.log.txt.tmp", f"{out}/log.txt")
os.replace(f"{out}/.big.diff.tmp", f"{out}/big.diff")
print(f"log.txt   {n:,} commits")
print(f"big.diff  ~{written:,} lines across {fi} files")
PY
