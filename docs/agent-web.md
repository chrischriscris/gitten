# The web loopback agent API

`gitten-web` is a proof, not a product — see `architecture.md` — but the proof
is exactly what an agent needs: the whole pipeline runs in the process you
started, and a loopback HTTP API exposes it as data. No browser required. This
page is the complete reference for driving it with `curl`.

Boot it the usual way, on either view:

```sh
cargo run -q --locked -p gitten-web -- diff . --port 7499
cargo run -q --locked -p gitten-web -- commits . --port 7499
```

Everything is `127.0.0.1` only, with no auth of any kind. The server refuses
any request whose `Host` is not a loopback name it could have been reached by,
so a page in a browser cannot rebind its way in — and the one `POST` moves
only the server's cursor. Nothing reachable here changes the repository; verbs
run in the terminal that started the server, never over loopback.

## Reads: `GET`

Meta first — the theme, the font, the file list, the row count — then windows
of rows, which is what a scroll is:

```sh
curl 'http://127.0.0.1:7499/api/meta?cols=0'
curl 'http://127.0.0.1:7499/api/rows?from=440&count=40'
curl 'http://127.0.0.1:7499/api/rows?from=440&count=40&cols=100&wrap=word'
```

`from` and `count` address visual rows — after wrapping — and a window past
the end is an empty list, not an error. `count` caps at 2000, so a whole
714k-row diff cannot arrive in one response. On the commits view the pair is
`/api/meta` (with `"kind":"commits"`) and `/api/commits?from=0&count=40`.

## The keymap and the configuration: `GET`

What is bound, from the same `Keymap::help` the window's help panel reads —
one flat list per view, with the mode on each row:

```sh
curl http://127.0.0.1:7499/api/keys
```

```json
{"kind":"keys",
 "diff":[{"mode":"global","command":"view.down","keys":"j / down","doc":"one row down"}],
 "commits":[…]}
```

What is configured — every wrap, differ and theme name registered, with which
one is selected — so `?wrap=word` is never a guess:

```sh
curl http://127.0.0.1:7499/api/config
```

```json
{"kind":"config","label":"…","layout":"unified",
 "wrap":{"names":["off","word","char"],"selected":"word"},
 "differ":{"names":["histogram","patience","myers"],"selected":"histogram","context":3},
 "themes":{"names":["dark","light","slate"],"selected":"dark"},
 "view":{"scrollRows":1,"scrolloff":3,"scrollbar":true}}
```

Whether the server is up — poll this before trusting any other route, and
after a restart to learn the process is new:

```sh
curl http://127.0.0.1:7499/api/health
```

```json
{"ok":true,"service":"gitten-web","version":"0.0.0"}
```

## The cursor: `POST /api/dispatch`

One route takes `POST`, with a `Content-Length` and a JSON body capped at
16 KB — a command name and four optional integers:

```sh
curl -X POST http://127.0.0.1:7499/api/dispatch -d '{"command":"view.down"}'
curl -X POST http://127.0.0.1:7499/api/dispatch -d '{"command":"view.down","args":{"by":5}}'
curl -X POST http://127.0.0.1:7499/api/dispatch -d '{"command":"view.page-down","args":{"pages":2,"height":50}}'
curl -X POST http://127.0.0.1:7499/api/dispatch -d '{"command":"view.down","args":{"row":440,"by":0}}'
curl -X POST http://127.0.0.1:7499/api/dispatch -d '{"command":"diff.next-file"}'
```

`row` moves the cursor absolutely first, in `?from=` addressing, and the named
command runs after it; `height` sets how many rows a screenful is (40 until
told otherwise); `by` and `pages` scale the one command they accompany. What
runs is the cursor verbs — `view.down/up/page-down/page-up/top/bottom/
scroll-down/scroll-up` and the `diff.next-file/prev-file` walk — resolved
against the loaded view through the command registry. A dispatch that ran
answers where the cursor landed:

```json
{"ok":true,"command":"view.down",
 "viewport":{"cursor":441,"top":405,"len":1215,"height":40},
 "status":"row 441 of 1215 · top 405"}
```

A dispatch that did not run answers a machine `code` and a `hint` with a next
step, because an error an agent cannot act on is a dead end in the one client
with nobody at the keyboard:

```sh
curl -X POST http://127.0.0.1:7499/api/dispatch -d '{"command":"frobnicate"}'
# {"ok":false,"error":"no such command \"frobnicate\"","code":"unknown-command",
#  "hint":"GET /api/keys lists the commands this view answers to"}      404

curl -X POST http://127.0.0.1:7499/api/dispatch -d '{"command":"commits.search"}'
# {"ok":false,"error":"commits.search needs the commits view","code":"wrong-view",…}  404

curl -X POST http://127.0.0.1:7499/api/dispatch -d '{"command":"repo.push"}'
# {"ok":false,"error":"repo.push is not actionable over HTTP","code":"unavailable",
#  "hint":"this API never changes the repository — run the verb in the terminal
#   that started this server"}                                           422

curl -X POST http://127.0.0.1:7499/api/dispatch -d '{}'
# {"ok":false,…,"code":"bad-request",…}                                  400
```

Dispatch is `POST`-only and `POST` is dispatch-only: `GET /api/dispatch` and
`POST /api/rows` are both `405`, because a URL that moves state belongs in no
history, prefetch or log, and a second way to read the rows is a second way
to be stale.
