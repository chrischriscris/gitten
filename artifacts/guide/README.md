# gitten design guide

Modular Vite + vanilla TS version of the design guide. Split byte-faithfully
from `../design-guide.html`, which is legacy and stays untouched.

```sh
bun install
bun run dev      # open the printed URL, nothing else in the repo is affected
bun run build    # typecheck + static dist/
```

## Structure

- `index.html` — shell only: head, guide header, five empty view mounts.
- `src/main.ts` — imports styles (legacy cascade order), mounts views, exposes
  the legacy inline-handler names on `window`, runs the inits.
- `src/styles/` — one file per concern, cut along the original `/* --- */`
  banners: `tokens.css` (`theme.rs` vars + themes), `guide-chrome.css`,
  `desktop.css`, `modals.css`, `docs.css`, `tui.css`.
- `src/views/` — one `?raw` HTML partial per tab: `mockup`, `blueprint`,
  `tokens`, `keyboard`, `architecture`. Big static chunks stay plain HTML.
- `src/components/` — `modals.html` today; string-template `(data) => string`
  functions for repeated markup next (see its README).
- `src/interactions/` — one module per behavior: `tabs`, `annotations`,
  `client`, `tui`, `theme`, `keymap`, `panes`, `modals`, `feedback`,
  `keyboard`. Shared mutable state lives in `src/state.ts`, mock data in
  `src/data/guide-data.ts`.

## Conventions

- Partials keep legacy inline handlers for now; `main.ts` bridges them onto
  `window`. New code uses `addEventListener`.
- Tokens originate in `core/src/theme.rs` — change values there first, mirror
  here in `tokens.css`.
- Deliberately no framework (no React/Vue/Svelte, no SSR). If `components/`
  ever needs nested state or async, adopt one then — the file boundaries
  already match what components would look like.
