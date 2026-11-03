---
name: verify-gitten
description: See, drive and screenshot gitten's desktop window headlessly, with no window on screen and no focus taken, to check a change to gui/ before calling it done. Use after any edit to gui/src, a theme or a layout; when asked to reproduce a desktop bug; or when asked what the window currently looks like or does.
---

# Verify the desktop window

`./dev verify` runs the real window on GPUI's test platform with an offscreen
renderer. It produces PNGs and a JSON state snapshot. Nothing appears on screen
and nothing takes focus, so run it freely, unlike `./dev gui`. The full
reference is `docs/agent-verify.md`.

## The loop

1. Make the change.
2. Reproduce the state the change is about and take a shot of it:

   ```sh
   ./dev verify --json -s 'cmd workspace.history' -s 'shot history' -s state -- commits .
   ```
3. **Read the PNG** with the Read tool and look at it. A step that passed only
   says the window answered. Whether it looks right is your call.
4. When the change is a fix, take the same shot on `main` too (a worktree or
   `git stash`) and compare the two. "It looks fine" without a before is not
   evidence.
5. Report what you saw and attach the shot paths. If the run could not show
   something (timing, IME, the traffic lights), say so.

## Getting to a state

Prefer named commands. They reach the same dispatch a key does, and a script
written with them survives a layout change. Reach for `click` only when a click
is what you are testing.

| To reach | Steps |
|---|---|
| Changes / History | `cmd workspace.changes` / `cmd workspace.history` |
| Branches / Stashes panel | `cmd branches.focus` / `cmd stashes.focus` |
| Command palette, filtered | `key cmd-k` then `type branch` |
| Theme picker / settings | `cmd theme.picker` / `cmd settings` |
| Repository switcher | `cmd project.switch` |
| Hide the left rail / inspector | `cmd workspace.toggle-sidebar` / `cmd workspace.toggle-inspector` |
| Next diff presentation | `cmd diff.cycle-layout` |
| Stage the selected file / all | `cmd files.stage` / `cmd files.stage-all` |
| A given palette | `GITTEN_QA_THEME=github-dark ./dev verify …` |
| A given window size | `GITTEN_QA_GEOM=900x700 ./dev verify …` |

Every command name, and which file in `gui/` answers it, is in
`docs/features.md`. A `—` in its gui column means the window probably can't do
it at all, which is worth knowing before writing steps for it.

**For writes, use a scratch repository.** Never stage, commit or reset in the
user's checkout:

```sh
R=$(mktemp -d) && (cd "$R" && git init -q && echo a > a.txt && git add . \
  && git -c user.name=t -c user.email=t@t commit -qm init && echo b >> a.txt)
./dev verify -s 'cmd files.focus' -s 'cmd files.stage' -s 'shot staged' -- commits "$R"
```

## Gotchas

- **The desktop ignores most keys by design.** It answers arrows, Tab, Enter,
  Esc and the `cmd-` chords. `key space` reporting `"handled": false` is
  correct; use `cmd files.stage`.
- **Click coordinates are logical pixels.** Halve what you measure on a Retina
  PNG (`scale` is in `state`). A click reports nothing itself, so follow it with
  `state` or `shot`.
- **Pane labels don't show staging.** "2 changed" reads the same before and
  after a stage. Use a shot, or `generation` in `state`, which moves when a wave
  lands.
- **A write step should read `busyMs` in the tens.** Every settling step reports
  how long the window stayed busy. Near 2000 means the write's own refresh
  didn't land and the repository watcher caught up instead, which is a bug. If
  a step reports `"unsettled": true`, the window really was still busy after
  10 s.
- **Startup prints `config: diff.* applies on the next launch`** for a config
  with a `[diff]` table. That's a known app quirk, not your change.
- **Timings from a verify run are meaningless.** The clock is simulated. Use
  `./dev --release dump` or `fixtures/bench.sh`.

## When this skill is wrong

If a step can't reach something the window can, the door is missing a verb.
Add it to `gui/src/verify.rs` through an entry point a person's input already
reaches, then update this file and `docs/agent-verify.md` in the same change.
