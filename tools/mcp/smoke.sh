#!/usr/bin/env bash
# Exercise every MCP tool headlessly and assert each reply parses as JSON.
#
#   tools/mcp/smoke.sh
#
# Exits non-zero on the first failure. Replies may be degraded (WS1-WS4 not
# merged yet means fallbacks answer) — what is asserted is shape, not which
# backend served: valid JSON, and the `backend` + `ok` fields present. A tool
# that cannot run here must still answer honestly, never with a traceback.
set -uo pipefail
cd "$(dirname "$0")/../.."

MCP=tools/mcp/server.py
FAILED=""

say() { printf '%s\n' "  $1"; }

fail() { printf '  ✗ %s\n' "$1"; FAILED="$FAILED $1"; }

# call <tool> <args-json>: the reply must parse and carry backend + ok.
call() {
  local tool=$1 args=${2:-'{}'}
  local out
  if ! out=$(python3 "$MCP" call "$tool" "$args" 2>&1); then
    fail "$tool (exit $?)"; printf '%s\n' "$out" | head -5 | sed 's/^/    /'; return
  fi
  if ! python3 -c '
import json, sys
r = json.loads(sys.stdin.read())
assert isinstance(r, dict), "reply is not an object"
assert "backend" in r, "no backend field"
assert "ok" in r, "no ok field"
' <<<"$out"; then
    fail "$tool (bad shape)"; printf '%s\n' "$out" | head -5 | sed 's/^/    /'; return
  fi
  say "✓ $tool [$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("backend"))' <<<"$out")]"
}

echo "── mcp adapter ───────────────────────────────────────────"
python3 -m py_compile tools/mcp/server.py && say "✓ py_compile" || fail "py_compile"
python3 -c 'import json; d=json.load(open("tools/mcp/tools.json")); assert len(d["tools"])==6, d; print("  ✓ tools.json: 6 tools")' || fail "tools.json"

call inspect.keys '{}'
call dispatch '{"key":"j"}'
call inspect.meta '{"view":"diff","repo":".","rev":"HEAD~1..HEAD","via":"dump"}'
call inspect.rows '{"view":"diff","repo":".","rev":"HEAD~1..HEAD","count":20,"via":"dump"}'
call inspect.rows '{"view":"commits","repo":".","rev":"5","count":5,"via":"dump"}'
call diffcheck '{"repo":".","revspec":"HEAD~1..HEAD"}'
call bench '{"suite":"quick"}'

# The web loopback path, once: meta over a spawned server. Slow cold (it
# builds gitten-web), honest when it fails — still valid JSON either way.
call inspect.meta '{"view":"diff","repo":".","rev":"HEAD~1..HEAD","via":"web"}'

# One full stdio round-trip: initialize, list, one call. Asserts the JSON-RPC
# envelopes, not just the tool payloads.
echo "── stdio round-trip ──────────────────────────────────────"
if python3 - "$MCP" <<'EOF'
import json, subprocess, sys
mcp = sys.argv[1]
msgs = [
  {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}},
  {"jsonrpc":"2.0","method":"notifications/initialized"},
  {"jsonrpc":"2.0","id":2,"method":"tools/list"},
  {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"dispatch","arguments":{"key":"j"}}},
  {"jsonrpc":"2.0","id":4,"method":"ping"},
]
p = subprocess.run(["python3", mcp], input="\n".join(json.dumps(m) for m in msgs),
                   capture_output=True, text=True, timeout=120)
replies = [json.loads(line) for line in p.stdout.splitlines() if line.strip()]
by_id = {r["id"]: r for r in replies if "id" in r}
assert by_id[1]["result"]["serverInfo"]["name"] == "gitten-mcp", by_id
assert len(by_id[2]["result"]["tools"]) == 6, by_id[2]
assert by_id[3]["result"]["content"], by_id[3]
assert by_id[4]["result"] == {}, by_id[4]
print("  ✓ stdio: initialize → list(6) → call → ping")
EOF
then :; else fail "stdio round-trip"; fi

echo
if [ -n "$FAILED" ]; then echo "✗ failed:$FAILED"; exit 1; fi
echo "✓ mcp smoke green"
