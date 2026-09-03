#!/usr/bin/env python3
"""gitten MCP adapter: a stdio JSON-RPC wrapper around the existing doors.

An agent that wants to *see* a repository the way gitten does should not have
to reimplement anything to do it. This adapter shells out to the surfaces that
already exist — the terminal's headless `dump` example today, a future `gitten
inspect --json` when it lands, the web loopback API — and hands the answers
back over MCP. Python 3 stdlib only: no npm, no pip, nothing to install.

Rule 1, demonstrated: this is a fourth client written without a patch to the
repo. It is not a workspace member, imports no crate, and reaches everything
through subprocesses any extension could equally spawn. If a second client
needs something that is not reachable this way, that is a bug in the layering,
not a thing to add here.

Degradation is the point, not a fallback: workstreams WS1-WS4 (machine JSON,
`inspect`, keymap endpoint, dispatch endpoint) may not have merged yet, so
every tool feature-detects `--json` first and parses the human output when it
is not there. Every reply names its `backend` so a caller can tell which one
answered.
"""

import json
import os
import re
import socket
import subprocess
import sys
import urllib.request
from pathlib import Path

VERSION = "0.1.0"

HERE = Path(__file__).resolve().parent


def repo_root():
    """The checkout this adapter was unpacked with, or $GITTEN_ROOT."""
    hit = os.environ.get("GITTEN_ROOT")
    if hit:
        return Path(hit)
    here = HERE
    for cand in [here.parent.parent] + list(here.parents):
        if (cand / "Cargo.toml").exists() and (cand / "tools" / "mcp").exists():
            return cand
        if (cand / "Cargo.toml").exists() and cand.name != "mcp":
            # Standalone copy: Cargo.toml beside tools/, or cwd fallback below.
            pass
    # Walk up from here looking for the workspace manifest.
    for cand in here.parents:
        if (cand / "Cargo.toml").exists():
            return cand
    return Path.cwd()


ROOT = repo_root()
TOOLS = json.loads((HERE / "tools.json").read_text())["tools"]
TOOL_NAMES = [t["name"] for t in TOOLS]

_ANSI = re.compile(r"\x1b\[[0-9;]*m")


def strip_ansi(s):
    return _ANSI.sub("", s)


def run(cmd, timeout=180, env=None, input_text=None):
    """Run cmd under ROOT. Returns (returncode, stdout, stderr) as text."""
    merged = dict(os.environ)
    if env:
        merged.update(env)
    try:
        proc = subprocess.run(
            cmd,
            input=input_text,
            capture_output=True,
            text=True,
            errors="replace",
            timeout=timeout,
            cwd=str(ROOT),
        )
        return proc.returncode, proc.stdout, proc.stderr
    except subprocess.TimeoutExpired as ex:
        out = ex.stdout if isinstance(ex.stdout, str) else ""
        return 124, out or "", "timed out after %ss: %s" % (timeout, " ".join(cmd))
    except FileNotFoundError as ex:
        return 127, "", str(ex)


def trunc(text, limit=12000):
    if len(text) <= limit:
        return text
    return text[:limit] + "\n…(truncated %d chars)…" % (len(text) - limit)


# --------------------------------------------------------------------------
# Feature detection: WS1-WS4 may not have merged. Probe once per process.


_INSPECT_JSON = None


def inspect_json_available():
    """Whether a machine `inspect --json` door exists yet. Cached."""
    global _INSPECT_JSON
    if _INSPECT_JSON is None:
        rc, out, _ = run(
            [
                "cargo", "run", "-q", "--locked", "-p", "gitten-tui",
                "--example", "dump", "--", "inspect", "--json",
            ],
            timeout=240,
        )
        ok = rc == 0
        if ok:
            try:
                json.loads(out)
            except ValueError:
                ok = False
        _INSPECT_JSON = ok
    return _INSPECT_JSON


# --------------------------------------------------------------------------
# Backends


def dump_frame(view, repo, rev, cols=120, rows=40, at=0, layout=None, wrap=None,
               theme=None, frames=1):
    """One headless terminal frame. Always exists; parses the human output."""
    env = {
        "COLS": str(cols),
        "ROWS": str(rows),
        "AT": str(at),
        "FRAMES": str(frames),
        "GITTEN_STATS": "0",
    }
    if layout:
        env["LAYOUT"] = layout
    if wrap:
        env["WRAP"] = wrap
    if theme:
        env["THEME"] = theme
    args = ["cargo", "run", "-q", "--locked", "-p", "gitten-tui",
            "--example", "dump", "--", view, repo]
    if rev:
        args.append(str(rev))
    rc, out, err = run(args, timeout=300, env=env)
    return rc, strip_ansi(out), err


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def web_get(view, repo, rev, path, cols=0, timeout_s=120):
    """Serve one view over the web loopback API and GET one route.

    Returns (ok, payload_dict_or_error_string). The server is spawned on a
    free loopback port and always reaped.
    """
    port = free_port()
    args = ["cargo", "run", "-q", "--locked", "-p", "gitten-web",
            "--", view, repo]
    if rev:
        args.append(str(rev))
    args += ["--port", str(port)]
    proc = subprocess.Popen(
        args,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
        cwd=str(ROOT),
    )
    try:
        import time
        deadline = time.time() + timeout_s
        body = None
        last = ""
        base = "http://127.0.0.1:%d" % port
        # Wait for the listener by polling /api/meta; the port line is
        # printed before listen succeeds, so the URL alone proves nothing.
        while time.time() < deadline:
            if proc.poll() is not None:
                rest = proc.stdout.read() if proc.stdout else ""
                return False, "web server exited: %s" % trunc((last + rest).strip())
            try:
                with urllib.request.urlopen(base + "/api/meta?cols=%d" % cols,
                                            timeout=2) as res:
                    if res.status == 200:
                        break
            except Exception as ex:  # not up yet
                last = str(ex)
                time.sleep(0.5)
        else:
            return False, "web server did not answer in %ss" % timeout_s
        query = "?cols=%d" % cols
        if view == "diff" and path == "/api/rows":
            query = "?from=0&count=2000&cols=%d" % cols
        try:
            with urllib.request.urlopen(base + path + query, timeout=30) as res:
                body = res.read().decode("utf-8", "replace")
        except Exception as ex:
            return False, "GET %s failed: %s" % (path, ex)
        try:
            return True, json.loads(body)
        except ValueError as ex:
            return False, "web returned non-JSON from %s: %s" % (path, ex)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()


def gitten_toml_sources():
    """Candidate config files, in the order gitten reads them."""
    cands = []
    hit = os.environ.get("GITTEN_CONFIG")
    if hit:
        cands.append(Path(hit))
    cands.append(Path.cwd() / "gitten.toml")
    home = os.environ.get("HOME", "")
    if home:
        cands.append(Path(home) / ".config" / "gitten" / "gitten.toml")
    return cands


def parse_keys_toml(text):
    """Minimal [keys] + [keys.<mode>] reader. stdlib only, no TOML parser."""
    bindings = []
    mode = "global"
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("[") and line.endswith("]"):
            head = line[1:-1].strip()
            if head == "keys":
                mode = "global"
            elif head.startswith("keys."):
                mode = head[len("keys."):]
            else:
                mode = ""
            continue
        if mode and "=" in line:
            key, _, val = line.partition("=")
            key = key.strip().strip('"').strip("'")
            val = val.strip().strip('"').strip("'")
            if key:
                bindings.append({"key": key, "command": val, "mode": mode})
    return bindings


# --------------------------------------------------------------------------
# Tools


def tool_inspect_rows(a):
    view = a.get("view", "diff")
    repo = a.get("repo", ".")
    rev = str(a.get("rev", ""))
    count = max(1, min(int(a.get("count", 50)), 2000))
    frm = max(0, int(a.get("from", 0)))
    cols = int(a.get("cols", 120))
    rows_n = int(a.get("rows", 40))
    via = a.get("via", "auto")
    if view not in ("diff", "commits"):
        return {"ok": False, "backend": "none",
                "error": "no such view %r; try diff or commits" % view}
    if inspect_json_available():
        return {"ok": False, "backend": "inspect-json", "degraded": True,
                "note": "probe passed but no per-tool inspect call is wired yet; "
                        "falling back is not implemented for a passing probe",
                "view": view}
    if via in ("auto", "web"):
        if view == "diff":
            ok, payload = web_get(view, repo, rev, "/api/rows", cols=cols)
        else:
            # Commits cross the wire from their own route.
            ok, payload = web_get_commits(repo, rev, frm, count)
        if ok:
            if isinstance(payload, dict) and "rows" in payload:
                window = payload["rows"][frm:frm + count]
                return {"ok": True, "backend": "web", "view": view,
                        "from": frm, "total": payload.get("total"),
                        "rows": window}
            return {"ok": True, "backend": "web", "view": view,
                    "from": frm, "payload": payload}
        if via == "web":
            return {"ok": False, "backend": "web", "error": payload}
        web_error = payload
    else:
        web_error = None
    rc, out, err = dump_frame(view, repo, rev, cols=cols, rows=rows_n, at=frm,
                              layout=a.get("layout"), wrap=a.get("wrap"),
                              theme=a.get("theme"))
    if rc != 0:
        return {"ok": False, "backend": "dump",
                "error": trunc((err or out or "dump failed").strip())}
    lines = out.splitlines()
    window = [{"text": t} for t in lines[frm:frm + count]]
    note = ("parsed from one headless viewport (%d lines); "
            "paging past it needs the web loopback or a larger ROWS" % len(lines))
    if web_error:
        note += "; web unavailable: %s" % trunc(str(web_error), 300)
    return {"ok": True, "backend": "dump", "degraded": True, "view": view,
            "from": frm, "viewport_lines": len(lines), "rows": window,
            "status": lines[-1].strip() if lines else "",
            "timings": err.strip().splitlines()[-1] if err.strip() else "",
            "note": note}


def web_get_commits(repo, rev, frm, count):
    """Page the commit list over the loopback API."""
    port = free_port()
    limit = rev or "200"
    args = ["cargo", "run", "-q", "--locked", "-p", "gitten-web",
            "--", "commits", repo, str(limit), "--port", str(port)]
    proc = subprocess.Popen(
        args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, errors="replace", cwd=str(ROOT),
    )
    try:
        import time
        base = "http://127.0.0.1:%d" % port
        deadline = time.time() + 120
        while time.time() < deadline:
            if proc.poll() is not None:
                rest = proc.stdout.read() if proc.stdout else ""
                return False, "web server exited: %s" % trunc(rest.strip())
            try:
                with urllib.request.urlopen(
                        base + "/api/commits?from=%d&count=%d" % (frm, count),
                        timeout=2) as res:
                    return True, json.loads(res.read().decode("utf-8", "replace"))
            except Exception:
                time.sleep(0.5)
        return False, "web server did not answer in 120s"
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()


def tool_inspect_meta(a):
    view = a.get("view", "diff")
    repo = a.get("repo", ".")
    rev = str(a.get("rev", ""))
    cols = int(a.get("cols", 120))
    via = a.get("via", "auto")
    if via in ("auto", "web"):
        ok, payload = web_get(view, repo, rev, "/api/meta", cols=cols)
        if ok:
            payload = dict(payload)
            payload.update({"ok": True, "backend": "web", "view": view})
            return payload
        if via == "web":
            return {"ok": False, "backend": "web", "error": payload}
        web_error = payload
    else:
        web_error = None
    rc, out, err = dump_frame(view, repo, rev, cols=cols)
    if rc != 0:
        return {"ok": False, "backend": "dump",
                "error": trunc((err or out or "dump failed").strip())}
    lines = strip_ansi(out).splitlines()
    meta = {"ok": True, "backend": "dump", "degraded": True, "view": view,
            "repo": repo, "rev": rev,
            "status": lines[-1].strip() if lines else "",
            "viewport_lines": len(lines),
            "timings": err.strip().splitlines()[-1] if err.strip() else "",
            "note": "human status line, not the web meta payload"}
    if web_error:
        meta["web_error"] = trunc(str(web_error), 300)
    return meta


def tool_inspect_keys(a):
    want = a.get("mode", "")
    if inspect_json_available():
        return {"ok": False, "backend": "inspect-json", "degraded": True,
                "note": "probe passed but no keymap call is wired yet"}
    bindings = []
    sources = []
    for cand in gitten_toml_sources():
        try:
            text = cand.read_text()
        except OSError:
            continue
        sources.append(str(cand))
        bindings.extend(parse_keys_toml(text))
    if want:
        bindings = [b for b in bindings if b["mode"] in ("global", want)]
    return {"ok": True, "backend": "gitten.toml", "degraded": True,
            "bindings": bindings, "sources": sources,
            "note": "no machine keymap yet: only [keys] overrides are listed; "
                    "shipped defaults live in core::command (see the TUI ? panel)"}


def tool_dispatch(a):
    key = a.get("key", "")
    mode = a.get("mode", "global")
    if not key:
        return {"ok": False, "backend": "none",
                "error": "a key is required, e.g. {\"key\": \"j\"}"}
    if inspect_json_available():
        return {"ok": False, "backend": "inspect-json", "degraded": True,
                "note": "probe passed but no dispatch call is wired yet"}
    return {"ok": False, "backend": "none", "degraded": True,
            "key": key, "mode": mode,
            "note": "no machine dispatch yet (WS4): resolve it in the TUI ? "
                    "panel or web ui/app.js, which read the same core::command "
                    "map this will call when it lands"}


def tool_diffcheck(a):
    repo = a.get("repo", ".")
    revspec = a.get("revspec", "HEAD~4..HEAD")
    rc, out, err = run(
        ["cargo", "run", "-q", "--locked", "--release", "-p", "gitten-git",
         "--example", "diffcheck", "--", repo, revspec],
        timeout=600,
    )
    text = (out + ("\n" + err if err.strip() else "")).strip()
    return {"ok": rc == 0, "backend": "diffcheck",
            "repo": repo, "revspec": revspec, "output": trunc(text)}


def tool_bench(a):
    suite = a.get("suite", "quick")
    if suite == "full":
        rc, out, err = run(
            ["cargo", "run", "-q", "--locked", "--release", "-p", "gitten-core",
             "--example", "bench"],
            timeout=600,
        )
        text = (out + ("\n" + err if err.strip() else "")).strip()
        if rc != 0:
            return {"ok": False, "backend": "bench", "degraded": True,
                    "error": trunc(text or "bench failed"),
                    "note": "bench reads fixtures/log.txt + fixtures/big.diff; "
                            "missing fixtures fail here, not in core"}
        return {"ok": True, "backend": "bench", "output": trunc(text)}
    cols = int(a.get("cols", 120))
    rows_n = int(a.get("rows", 40))
    rc, out, err = dump_frame("diff", "--fixtures", "", cols=cols,
                              rows=rows_n, frames=5)
    if rc != 0:
        return {"ok": False, "backend": "dump",
                "error": trunc((err or out or "dump failed").strip())}
    return {"ok": True, "backend": "dump", "suite": "quick",
            "frame_lines": len(out.splitlines()),
            "timings": err.strip().splitlines()[-1] if err.strip() else "",
            "note": "one headless frame x5, not the pipeline bench; "
                    "suite=full runs core's bench (needs fixtures)"}


HANDLERS = {
    "inspect.rows": tool_inspect_rows,
    "inspect.meta": tool_inspect_meta,
    "inspect.keys": tool_inspect_keys,
    "dispatch": tool_dispatch,
    "diffcheck": tool_diffcheck,
    "bench": tool_bench,
}


def call_tool(name, args):
    handler = HANDLERS.get(name)
    if handler is None:
        return {"ok": False, "error": "no such tool %r; have %s" % (name, ", ".join(TOOL_NAMES))}
    if not isinstance(args, dict):
        return {"ok": False, "error": "arguments must be an object"}
    try:
        result = handler(args)
    except Exception as ex:  # a tool never takes the server down
        return {"ok": False, "backend": "none", "error": "%s: %s" % (type(ex).__name__, ex)}
    if not isinstance(result, dict):
        return {"ok": False, "error": "tool returned a non-object"}
    return result


# --------------------------------------------------------------------------
# MCP over stdio (newline-delimited JSON-RPC)


def rpc_result(msg_id, result):
    return {"jsonrpc": "2.0", "id": msg_id, "result": result}


def rpc_error(msg_id, code, message):
    err = {"jsonrpc": "2.0", "error": {"code": code, "message": message}}
    if msg_id is not None:
        err["id"] = msg_id
    return err


def handle_message(msg):
    if not isinstance(msg, dict) or msg.get("jsonrpc") != "2.0":
        return rpc_error(msg.get("id") if isinstance(msg, dict) else None,
                         -32600, "not a JSON-RPC 2.0 message")
    method = msg.get("method")
    msg_id = msg.get("id")
    params = msg.get("params") or {}
    # A notification (no id) gets no reply.
    is_notification = "id" not in msg

    if method == "initialize":
        result = {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "gitten-mcp", "version": VERSION},
        }
    elif method == "ping":
        result = {}
    elif method == "tools/list":
        result = {"tools": TOOLS}
    elif method == "tools/call":
        name = params.get("name", "")
        args = params.get("arguments") or {}
        outcome = call_tool(name, args)
        result = {
            "content": [{"type": "text", "text": json.dumps(outcome)}],
            "isError": not outcome.get("ok", False),
        }
    elif method.startswith("notifications/"):
        return None
    else:
        if is_notification:
            return None
        return rpc_error(msg_id, -32601, "no such method %r" % (method,))
    if is_notification:
        return None
    return rpc_result(msg_id, result)


def serve_stdio():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except ValueError as ex:
            sys.stdout.write(json.dumps(rpc_error(None, -32700, "bad JSON: %s" % ex)) + "\n")
            sys.stdout.flush()
            continue
        if isinstance(msg, list):
            replies = [r for r in (handle_message(m) for m in msg) if r is not None]
            if replies:
                sys.stdout.write(json.dumps(replies) + "\n")
                sys.stdout.flush()
        else:
            reply = handle_message(msg)
            if reply is not None:
                sys.stdout.write(json.dumps(reply) + "\n")
                sys.stdout.flush()


def usage():
    return ("usage:\n"
            "  server.py                       MCP over stdio (newline JSON-RPC)\n"
            "  server.py list                  print the tool catalog as JSON\n"
            "  server.py call NAME [ARGS-JSON] call one tool, print its JSON\n")


def main(argv):
    if len(argv) >= 2 and argv[1] in ("-h", "--help"):
        sys.stdout.write(usage())
        return 0
    if len(argv) >= 2 and argv[1] == "list":
        sys.stdout.write(json.dumps({"tools": TOOLS}, indent=2) + "\n")
        return 0
    if len(argv) >= 2 and argv[1] == "call":
        name = argv[2] if len(argv) >= 3 else ""
        try:
            args = json.loads(argv[3]) if len(argv) >= 4 else {}
        except ValueError as ex:
            sys.stderr.write("bad ARGS-JSON: %s\n" % ex)
            return 2
        sys.stdout.write(json.dumps(call_tool(name, args), indent=2) + "\n")
        return 0
    if len(argv) >= 2:
        sys.stderr.write(usage())
        return 2
    serve_stdio()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
