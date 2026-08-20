#!/usr/bin/env bash
# Re-download the real fixtures. They are deliberately not committed — 32 MB of
# diffs plus a 113 MB clone do not belong in this repo's history.
#
# Each of these is pathological in a different direction; see AGENTS.md.
set -euo pipefail
OUT="$(cd "$(dirname "$0")" && pwd)/real"
mkdir -p "$OUT"

for pr in 30683 30698 33933; do
  [ -s "$OUT/pr$pr.diff" ] && { echo "  pr$pr.diff already here"; continue; }
  echo "  fetching oven-sh/bun#$pr ..."
  curl -sL --max-time 300 "https://github.com/oven-sh/bun/pull/$pr.diff" -o "$OUT/pr$pr.diff"
done

# The markdown case. Every other diff fixture here is code, and the rendered
# Markdown presentation is scanning and rewriting *prose* — a different
# distribution entirely: rust-lang/book is 86% paragraph lines where a technical
# docs tree is nearer a third, with eight times the headings. See
# docs/measurements.md for both shapes and the one-liner that makes the other.
if [ ! -s "$OUT/md.diff" ]; then
  echo "  building md.diff from rust-lang/book ..."
  BOOK="${TMPDIR:-/tmp}/plait-book"
  [ -d "$BOOK/.git" ] || git clone --quiet --no-checkout https://github.com/rust-lang/book.git "$BOOK"
  git -C "$BOOK" diff 'HEAD~300..HEAD' -- '*.md' > "$OUT/md.diff"
  printf '  md.diff %s lines\n' "$(wc -l < "$OUT/md.diff" | tr -d ' ')"
else
  echo "  md.diff already here"
fi

# The tree stress case. Blobless: history metadata only, no file contents.
if [ ! -d "$HOME/Projects/git/.git" ]; then
  echo "  cloning git/git (blobless, ~120 MB) ..."
  git clone --filter=blob:none --no-checkout --quiet https://github.com/git/git.git "$HOME/Projects/git"
fi
echo "done. see AGENTS.md for what each fixture stresses."
