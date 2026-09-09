# gitten desktop concept

Runnable design reference for the desktop application. Opens directly into the compact workspace: directory-grouped files on the left, diff in the center, commit inspector on the right.

```sh
cd artifacts/guide-v2
bun install
bun run dev
```

Open the printed URL. The command starts Vite without opening a browser. `bun run build` creates `dist/`; `bun run preview` serves that build.

## Interaction

Select a file to view its diff. Check a file or use Stage hunk to update the index and commit inspector. Partially staged files display a mixed checkbox and hunk count. Summary and optional description remain intact when navigating between Changes and History.

- ⌘K / Ctrl+K: commands, including appearance and Reset demo.
- ⌘1 / ⌘2: Changes / History.
- ⌘Enter: commit confirmation.
- `/`: file filter.
- Tab / Enter / Escape: focus, activate, dismiss dialogs.

## Scope

This is a frontend reference application using in-memory fixtures. No repository reads, writes, push, or network operations occur. Reload or Reset demo restores the fixtures. Branch and stash dialogs are read-only. Partial commits display a receipt and end that scenario; full commits show a clean working tree. Fixture counts and historical excerpts are illustrative.

## Implementation

[IMPLEMENTATION.md](IMPLEMENTATION.md) is the specification for agents implementing the GPUI desktop client. The HTML application is a visual and interaction reference, not production Git logic.

- `src/main.js`: sample data, screen composition, and interactions.
- `src/style.css`: selected appearance, dimensions, and responsive rules.
- `index.html`: application entry point.
- `vite.config.js`: relative asset base for static builds.

Validation: production build and non-browser interaction checks. Browser-rendered visual verification remains pending because no browser connection was available in the authoring session.
