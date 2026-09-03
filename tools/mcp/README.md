# gitten MCP adapter

A stdio JSON-RPC (MCP) wrapper around the doors gitten already has. Python 3
stdlib only — no npm, no pip, nothing to install.

```
python3 tools/mcp/server.py                      # MCP over stdio
python3 tools/mcp/server.py list                 # the tool catalog as JSON
python3 tools/mcp/server.py call inspect.meta '{"view":"diff","repo":"."}'
tools/mcp/smoke.sh                               # exercise every tool headlessly
```

How it answers, in order: a future `inspect --json` door when one exists
(feature-detected, never assumed), the web loopback API (`/api/meta`,
`/api/rows`, `/api/commits` on a free loopback port it spawns and reaps), and
one headless terminal frame (`gitten-tui --example dump`) parsed as text.
Every reply names its `backend`, and a fallback reply says `degraded: true`
with the reason — a caller can always tell which door answered.

Standalone by design: this directory is not a workspace member and reaches
everything through subprocesses. That is rule 1 with the serial numbers filed
off — anything here does, an extension could do too.

See `docs/agent-mcp.md` for the agent-side guide and the tool catalog.
