#!/usr/bin/env bash
# The rules in AGENTS.md and the docs it points to that a grep can hold,
# held by a grep.
#
#   ./dev lint        or   tools/lint.sh
#
# A rule written only as prose is a rule every new contributor — and every
# agent — has to read, remember and apply, and each of these has been broken
# at least once on the way to being written down. Here they cost nothing to
# keep: the pre-commit hook and ./check.sh both run this, and it finishes in
# well under a second.
#
# Each failure says which rule, where, and what to do instead, because the
# reader is as likely to be an agent mid-task as a person. Exits non-zero on
# any failure. Add a rule here when a sentence in AGENTS.md can be checked
# mechanically; leave it out when the check would need judgement.
set -uo pipefail
cd "$(dirname "$0")/.."

CRATES=(app cli core git gui tui web)
FAILED=0

# fail <rule> <what to do instead> <hits...>
fail() {
  local rule=$1 fix=$2
  shift 2
  printf '\033[31m✗ %s\033[0m\n' "$rule"
  printf '%s\n' "$@" | sed 's/^/    /'
  printf '  → %s\n\n' "$fix"
  FAILED=1
}

# Source lines only: a doc comment that *names* a forbidden thing — and
# several do, to explain why it is forbidden — is not a use of it.
code() { # code <pattern> <paths...>
  local pattern=$1
  shift
  grep -rnE --include='*.rs' "$pattern" "$@" 2>/dev/null \
    | grep -vE '^[^:]+:[0-9]+:\s*//'
}

# 1. `core` has no dependencies. Dev-dependencies for its examples are fine.
deps=$(sed -n '/^\[dependencies\]/,/^\[/p' core/Cargo.toml \
  | grep -vE '^\s*(#|$|\[)')
[ -n "$deps" ] && fail "core/ has a dependency" \
  "core compiles in a second and tests with no window because it has none (AGENTS.md, \"The one architectural rule\"). Write it in core, or put the code that needs the crate in app/ or git/." \
  "$deps"

# 2. No platform conditionals. Linux is the check that nothing was written badly.
hits=$(code 'cfg\((not\()?target_os' "${CRATES[@]}")
[ -n "$hits" ] && fail "a cfg(target_os) in the tree" \
  "Find the portable mechanism (AGENTS.md, \"macOS first, Linux never ruled out\"); a binary found on PATH, OSC 52, GPUI's own platform layer. If there truly is none, that is a decision for the human, not a cfg." \
  "$hits"

hits=$(grep -nE '^\s*(objc|objc2|cocoa|core-foundation|core-graphics)\b' */Cargo.toml 2>/dev/null)
[ -n "$hits" ] && fail "a macOS-only crate in a manifest" \
  "GPUI already wraps the platform; reach it through GPUI (AGENTS.md, \"macOS first, Linux never ruled out\")." \
  "$hits"

# 3. Git output is bytes. Latin-1 author names are real and git/git panics it.
hits=$(code 'read_to_string' git/src)
[ -n "$hits" ] && fail "read_to_string in the git layer" \
  "Read bytes and String::from_utf8_lossy (AGENTS.md, \"Never read_to_string anything from git\"). One bad byte must never hide a repository." \
  "$hits"

# 4. A test module's `use super::*` under a parent that globs gpui shadows the
#    builtin #[test] with gpui's macro, which recurses to the limit.
hits=""
for f in $(grep -rlE '^use gpui::\*;' gui/src 2>/dev/null); do
  h=$(grep -nE '^\s*use super::\*;' "$f" | sed "s|^|$f:|")
  [ -n "$h" ] && hits="$hits$h"$'\n'
done
hits=${hits%$'\n'}
[ -n "$hits" ] && fail "use super::* in a test module beside use gpui::*" \
  "Import the names the test uses: use super::{a, b} (docs/gpui-notes.md, \"Five traps\")." \
  "$hits"

# 5. GPUI is pinned by Cargo.lock alone. A rev in the manifest splits the Zed
#    tree into two source variants and compiles GPUI twice.
hits=$(grep -nE 'zed-industries/zed.*\brev\s*=' gui/Cargo.toml 2>/dev/null)
[ -n "$hits" ] && fail "a rev on a GPUI dependency" \
  "Remove it; the lockfile is the pin. Bump GPUI with cargo update -p gpui -p gpui-component and match rust-toolchain.toml (AGENTS.md, \"GPUI is pinned only by Cargo.lock\")." \
  "$hits"

if [ "$FAILED" -eq 0 ]; then
  printf '\033[2mlint: clean\033[0m\n'
fi
exit "$FAILED"
