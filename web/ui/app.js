// The drawing half, and only the drawing half.
//
// Nothing in here diffs, highlights, pairs a removal with an addition or
// decides where a line breaks — all of that arrived from `core`, over the wire,
// already done. What this file owns is a virtual list, a theme applied as
// custom properties, and the keys.
//
// The two things worth knowing before changing it:
//
// 1. Rows are addressed in *visual* row space, after wrapping. That is the
//    space the scrollbar lives in and the space the server slices, so a wrap
//    change is a new row count and a cleared cache, not a reflow of the DOM.
// 2. The column budget is measured in the browser, from the font the browser
//    actually resolved. Taking `font.advance` from the server would be
//    measuring the desktop's font and wrapping to it.

const el = (id) => document.getElementById(id);
const dom = {
  label: el("label"),
  stats: el("stats"),
  wrap: el("wrap"),
  scroll: el("scroll"),
  spacer: el("spacer"),
  window: el("window"),
  position: el("position"),
};

/** Rows per request. Big enough that a page-down is one fetch, small enough
 *  that the first paint does not wait on a megabyte. */
const PAGE = 400;

/** Rows drawn above and below the viewport, so a fast scroll has something to
 *  show before its fetch lands. */
const OVERSCAN = 12;

const state = {
  meta: null,
  /** page index -> array of rows, or a Promise while it is in flight. */
  pages: new Map(),
  total: 0,
  rowH: 18,
  cols: 0,
  cursor: 0,
  /** Bumped on every reflow. A page that arrives from an older generation is
   *  cut for a width that is no longer on screen, so it is dropped. */
  generation: 0,
  /** The newest reflow to have been *issued*. A reflow is two awaits long, so
   *  two resizes close together can have their replies land in either order —
   *  and applying the older one leaves the row count, the wrap and the cache
   *  describing a width that is not on screen. Whoever is not newest returns
   *  without touching anything. */
  reflowing: 0,
  first: -1,
  last: -1,
};

// --------------------------------------------------------------------- theme

/** The theme, as custom properties. One place, so the stylesheet holds no
 *  colours and `plait.toml` stays the only thing that decides them. */
function applyTheme(meta) {
  const s = document.documentElement.style;
  const t = meta.theme;
  const set = (name, value) => s.setProperty(name, value);

  for (const [k, v] of Object.entries(t.chrome)) {
    set(`--${camelToDash(k)}`, v);
  }
  for (const [k, v] of Object.entries(t.diff)) {
    set(`--${camelToDash(k)}`, v);
  }
  set("--chrome-bg", t.chrome.bg);
  set("--chrome-fg", t.chrome.fg);
  set("--font-family", `"${meta.font.family}", ui-monospace, monospace`);
  set("--font-size", `${meta.font.size}px`);

  // A row is a fixed height because that is what makes a 714k-row diff scroll
  // at all — the same constraint `uniform_list` puts on the window, arrived at
  // for the same reason.
  state.rowH = Math.max(14, Math.round(meta.font.size * 1.45));
  set("--row-h", `${state.rowH}px`);
}

const camelToDash = (s) => s.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`);

// ---------------------------------------------------------- the column budget

/** One character's advance, measured in the font the browser actually resolved.
 *
 *  Not `meta.font.advance`: that is measured for the desktop's font stack, and
 *  if the browser fell back to something else, wrapping to it puts the break in
 *  the wrong place on every line. Measured over many characters so the result
 *  is not one rounding error. */
function charWidth() {
  const probe = document.createElement("span");
  probe.style.cssText = "position:absolute;visibility:hidden;white-space:pre";
  probe.textContent = "0".repeat(100);
  dom.window.appendChild(probe);
  const w = probe.getBoundingClientRect().width / 100;
  probe.remove();
  return w || 8;
}

/** Columns the text column can hold.
 *
 *  The chrome is what the row draws *before* its text — two gutters, a sign and
 *  the padding — and it is subtracted here rather than server-side because the
 *  presentation owns what it draws around the text. That is the same division
 *  the shell's `columns` and `TEXT_CHROME` make. */
function columns() {
  const size = state.meta ? state.meta.font.size : 13;
  const chrome = 16 + 10.5 * size;
  const usable = dom.scroll.clientWidth - chrome;
  return Math.max(8, Math.floor(usable / charWidth()));
}

// ----------------------------------------------------------------- the server

async function fetchMeta() {
  const wrap = state.wrapName ? `&wrap=${encodeURIComponent(state.wrapName)}` : "";
  const r = await fetch(`/api/meta?cols=${state.cols}${wrap}`);
  if (!r.ok) throw new Error(await r.text());
  return r.json();
}

function pageOf(row) {
  return Math.floor(row / PAGE);
}

/** Loads the pages covering a range, and repaints as each lands.
 *
 *  Fire-and-forget on purpose: a scroll must not await anything, so the rows
 *  that are already cached paint immediately and the rest replace placeholders
 *  when they arrive. */
function ensure(from, to) {
  const generation = state.generation;
  for (let p = pageOf(from); p <= pageOf(to); p++) {
    if (state.pages.has(p)) continue;
    const wrap = state.wrapName ? `&wrap=${encodeURIComponent(state.wrapName)}` : "";
    const url = `/api/rows?from=${p * PAGE}&count=${PAGE}&cols=${state.cols}${wrap}`;
    const inflight = fetch(url)
      .then((r) => r.json())
      .then((payload) => {
        // Cut for a width that is no longer on screen.
        if (generation !== state.generation) return;
        state.pages.set(p, payload.rows);
        paint(true);
      })
      .catch(() => state.pages.delete(p));
    state.pages.set(p, inflight);
  }
}

function rowAt(index) {
  const page = state.pages.get(pageOf(index));
  return Array.isArray(page) ? page[index % PAGE] : undefined;
}

// ----------------------------------------------------------------- the drawing

const escape = (s) =>
  s.replace(/[&<>]/g, (c) => (c === "&" ? "&amp;" : c === "<" ? "&lt;" : "&gt;"));

/** Which background a piece of text lands on, and therefore which resolved
 *  syntax colour it takes. The shell's own mapping, and it has to stay the
 *  shell's: a token's foreground is resolved against what it is drawn on, which
 *  is the entire reason `Surface` exists. */
function surfaceOf(kind, moved, word) {
  if (kind === "context") return "context";
  if (moved) return kind === "added" ? "movedAdded" : "movedRemoved";
  if (word) return kind === "added" ? "addedWord" : "removedWord";
  return kind;
}

function pieceHtml(p, kind, moved, theme) {
  const surface = surfaceOf(kind, moved, !!p.w);
  const style = [];
  if (p.k) {
    const s = theme.syntax[surface][p.k];
    style.push(`color:${s.fg}`);
    if (s.bold) style.push("font-weight:600");
    if (s.italic) style.push("font-style:italic");
  }
  if (p.w) style.push(`background:${theme.background[surface]}`);
  const text = escape(p.t);
  return style.length ? `<span style="${style.join(";")}">${text}</span>` : text;
}

function rowHtml(row, index) {
  const cursor = index === state.cursor ? " cursor" : "";
  if (!row) return `<div class="row pending${cursor}">…</div>`;

  if (row.type === "file") {
    return (
      `<div class="row file${cursor}">` +
      `<span class="path">${escape(row.path)}</span>` +
      `<span class="adds">+${row.adds}</span>` +
      `<span class="dels">-${row.dels}</span>` +
      `</div>`
    );
  }
  if (row.type === "hunk") {
    return `<div class="row hunk${cursor}">${escape(row.header)}</div>`;
  }

  const theme = state.meta.theme;
  const moved = !!row.moved;
  const sign = row.kind === "added" ? "+" : row.kind === "removed" ? "-" : " ";
  // A continuation carries no number and no sign: the background says which
  // line it belongs to, and an empty gutter says it is not a line of its own.
  const old = row.cont ? "" : (row.old ?? "");
  const now = row.cont ? "" : (row.new ?? "");
  const text = row.x.map((p) => pieceHtml(p, row.kind, moved, theme)).join("");
  return (
    `<div class="row line ${row.kind}${moved ? " moved" : ""}${cursor}">` +
    `<span class="num">${old}</span>` +
    `<span class="num">${now}</span>` +
    `<span class="sign">${row.cont ? " " : sign}</span>` +
    `<span class="text">${text}</span>` +
    `</div>`
  );
}

/** Draws the visible window. `force` redraws even if the range did not move,
 *  which is what a newly arrived page and a cursor move need. */
function paint(force) {
  if (!state.meta) return;
  const viewport = Math.ceil(dom.scroll.clientHeight / state.rowH);
  const first = Math.max(0, Math.floor(dom.scroll.scrollTop / state.rowH) - OVERSCAN);
  const last = Math.min(state.total, first + viewport + 2 * OVERSCAN);
  if (!force && first === state.first && last === state.last) return;
  state.first = first;
  state.last = last;

  ensure(first, Math.max(first, last - 1));

  let html = "";
  for (let i = first; i < last; i++) {
    html += rowHtml(rowAt(i), i);
  }
  dom.window.innerHTML = html;
  dom.window.style.transform = `translateY(${first * state.rowH}px)`;
  dom.position.textContent = state.total
    ? `row ${state.cursor + 1} / ${state.total}`
    : "no rows";
}

function chrome(meta) {
  dom.label.textContent = `plait · ${meta.label}`;
  dom.wrap.textContent = `wrap: ${meta.wrap.selected}`;
  const bits = [
    `${meta.files.length} files`,
    `${meta.lines} lines`,
    `${meta.rows} rows`,
  ];
  if (meta.moved) bits.push(`${meta.moved} moved`);
  // Never silently: a wrap whose breaks were all thrown away looks exactly like
  // a wrap with nothing to do.
  if (meta.wrap.rejected) bits.push(`${meta.wrap.rejected} invalid breaks`);
  bits.push(`intraline ${meta.intralineMs.toFixed(0)}ms`);
  bits.push(`syntax ${meta.syntaxMs.toFixed(0)}ms`);
  dom.stats.textContent = bits.join(" · ");
}

// ------------------------------------------------------------------- the loop

/** Re-reads the row count for the current width, and throws away rows cut for
 *  the old one. Keeps the reading position as a fraction, which is honest but
 *  not what the window does — it anchors on the logical row. Doing the same
 *  here needs the logical index on the wire. */
async function reflow() {
  const token = ++state.reflowing;
  const ratio = state.total ? state.cursor / state.total : 0;
  state.cols = columns();
  state.generation++;
  state.pages.clear();
  const meta = await fetchMeta();
  // Superseded while this was in flight.
  if (token !== state.reflowing) return;
  state.meta = meta;
  state.total = meta.rows;
  state.wrapName = meta.wrap.selected;
  applyTheme(meta);
  chrome(meta);
  dom.spacer.style.height = `${state.total * state.rowH}px`;
  state.cursor = Math.min(state.total - 1, Math.round(ratio * state.total));
  paint(true);
}

function moveTo(row, centre) {
  state.cursor = Math.max(0, Math.min(state.total - 1, row));
  const top = dom.scroll.scrollTop;
  const height = dom.scroll.clientHeight;
  const y = state.cursor * state.rowH;
  if (centre) {
    dom.scroll.scrollTop = y - height / 2;
  } else if (y < top) {
    dom.scroll.scrollTop = y;
  } else if (y + state.rowH > top + height) {
    dom.scroll.scrollTop = y + state.rowH - height;
  }
  paint(true);
}

async function cycleWrap() {
  const names = state.meta.wrap.names;
  const at = names.indexOf(state.meta.wrap.selected);
  state.wrapName = names[(at + 1) % names.length];
  await reflow();
}

function keys(e) {
  if (e.metaKey || e.ctrlKey) {
    // ctrl-d / ctrl-u, lazygit's half-page.
    const half = Math.floor(dom.scroll.clientHeight / state.rowH / 2);
    if (e.key === "d") return moveTo(state.cursor + half), e.preventDefault();
    if (e.key === "u") return moveTo(state.cursor - half), e.preventDefault();
    return;
  }
  switch (e.key) {
    case "j":
    case "ArrowDown":
      moveTo(state.cursor + 1);
      break;
    case "k":
    case "ArrowUp":
      moveTo(state.cursor - 1);
      break;
    case "g":
      moveTo(0);
      break;
    case "G":
      moveTo(state.total - 1);
      break;
    case "w":
      cycleWrap();
      break;
    default:
      return;
  }
  e.preventDefault();
}

function wire() {
  dom.scroll.addEventListener("scroll", () => paint(false), { passive: true });
  dom.wrap.addEventListener("click", cycleWrap);
  document.addEventListener("keydown", keys);
  // A resize only matters when it crosses a character boundary — that is what
  // makes dragging a window free, and it is the same check `Rows::reflow` does.
  //
  // Two signals, not one. `resize` is what fires when the window is dragged and
  // is available everywhere; the observer additionally catches the layout
  // changing underneath a container that the window did not — which is what a
  // pane divider will be. Debounced together, so both arriving costs one
  // reflow.
  let pending;
  const resized = () => {
    clearTimeout(pending);
    pending = setTimeout(() => {
      if (columns() !== state.cols) reflow();
      else paint(true);
    }, 80);
  };
  window.addEventListener("resize", resized);
  if (typeof ResizeObserver === "function") {
    // Held in a variable: an observer is only guaranteed to outlive the call
    // that made it for as long as something can still reach it.
    state.observer = new ResizeObserver(resized);
    state.observer.observe(dom.scroll);
  }
}

async function main() {
  wire();
  try {
    // Two fetches on purpose. The column budget is measured in the font the
    // browser resolved, and the font arrives with the theme — so the first
    // request exists to learn the face, and only the second can ask for rows
    // cut to it. Measuring before this point measures the default font and
    // wraps every line to the wrong column.
    // `state.meta` and not just the theme: `columns()` reads the font size off
    // it to work out what the row draws before its text, and a `meta` that is
    // still null falls back to a different size — which measures a different
    // budget, wraps to a different column, and reports a row count that no
    // later reflow agrees with. Measured on this repo: 105 columns on load
    // against 104 after, which is 45 rows of difference.
    state.meta = await fetchMeta();
    applyTheme(state.meta);
    await reflow();
    dom.scroll.focus();
  } catch (e) {
    dom.label.textContent = "plait";
    dom.stats.textContent = String(e.message || e);
    dom.stats.className = "error";
  }
}

main();
