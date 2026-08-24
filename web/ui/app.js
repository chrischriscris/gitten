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
  keys: el("keys"),
};

/** Rows per request. Big enough that a page-down is one fetch, small enough
 *  that the first paint does not wait on a megabyte. */
const PAGE = 400;

/** Rows drawn above and below the viewport, so a fast scroll has something to
 *  show before its fetch lands. */
const OVERSCAN = 12;

const state = {
  meta: null,
  /** "diff" or "commits". From `meta`, so one page serves both and neither has
   *  to be told which it is before the first fetch. */
  kind: "diff",
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
 *  colours and `gitten.toml` stays the only thing that decides them. */
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
  const query = state.kind === "commits" ? "" : `?cols=${state.cols}${wrap}`;
  const r = await fetch(`/api/meta${query}`);
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
    // A commit list has no width to be cut for, so no budget and no wrap.
    const wrap = state.wrapName ? `&wrap=${encodeURIComponent(state.wrapName)}` : "";
    const url =
      state.kind === "commits"
        ? `/api/commits?from=${p * PAGE}&count=${PAGE}`
        : `/api/rows?from=${p * PAGE}&count=${PAGE}&cols=${state.cols}${wrap}`;
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

// ------------------------------------------------------------------ the graph

/** Columns per lane. Not derived from the font: a lane is a drawn thing, and
 *  tying its width to the type would reflow the graph when the text reflows.
 *  14 is the window's, so a repository looks the same in both. */
const LANE_W = 14;
const STROKE = 2;
const DOT_R = 4.5;
const MERGE_R = 5.5;

/** Where a lane is centred. */
const laneX = (lane) => lane * LANE_W + LANE_W / 2;

/** A lane's colour, or the collapsed grey when this row is hiding lanes past
 *  the cap.
 *
 *  `capped` and not `lane === maxLanes - 1`: a repository with exactly twelve
 *  lanes hides nothing, and dimming its last column would say there is more
 *  history over there when there is not. The server sends the fact. */
function laneColor(theme, hue, overflow) {
  if (overflow) return theme.laneOverflow;
  return theme.lanes[hue % theme.lanes.length] || theme.chrome.fg;
}

/** Half an S, as a cubic.
 *
 *  A branch changing lanes spans a *whole* row, and each row draws its own half:
 *  the two meet on the row boundary, at the midpoint between the two lanes,
 *  sharing a tangent. That is why the server sends a `partner` and a direction
 *  rather than a start and an end — see `gitten_core::graph`. The control points
 *  are the window's, so the curve has the same shape in both clients.
 *
 *  The last segment runs half a pixel *past* the boundary along the tangent: two
 *  antialiased ends meeting exactly leaves a faint crease, and a collinear
 *  overlap cannot kink. */
function halfS(x, partnerX, y, down, rowH) {
  const dx = (partnerX - x) / 2;
  const dy = down ? rowH / 2 : -rowH / 2;
  const [tx, ty] = [dx * 0.5, dy * 0.25];
  const len = Math.hypot(tx, ty);
  const over = len > 0 ? 0.5 / len : 0;
  return (
    `M${x} ${y}` +
    `C${x} ${y + dy * 0.5} ${x + dx * 0.5} ${y + dy * 0.75} ${x + dx} ${y + dy}` +
    (over ? `L${x + dx + tx * over} ${y + dy + ty * over}` : "")
  );
}

/** One row's gutter.
 *
 *  Straight halves as rects and curves as paths, then the node — last, because
 *  it is opaque and punches through whatever runs under it, so the lines read as
 *  passing behind. Same order as the window, for the same reason. */
function gutter(row, meta) {
  const t = meta.theme;
  const rowH = state.rowH;
  const w = meta.lanes * LANE_W;
  const mid = rowH / 2;
  const over = (lane) => !!row.capped && lane === meta.maxLanes - 1;
  let out = "";

  for (const l of row.lines || []) {
    const y0 = l.up ? 0 : mid;
    const y1 = l.down ? rowH : mid;
    if (y0 === y1) continue; // a lane that is curve at both ends
    out +=
      `<rect x="${laneX(l.lane) - STROKE / 2}" y="${y0}" ` +
      `width="${STROKE}" height="${y1 - y0}" fill="${laneColor(t, l.hue, over(l.lane))}"/>`;
  }
  for (const c of row.curves || []) {
    // Either end in the collapsed column makes the whole half overflow, or one
    // curve changes colour halfway across the gutter.
    const colour = laneColor(t, c.hue, over(Math.max(c.lane, c.partner)));
    const d = halfS(laneX(c.lane), laneX(c.partner), mid, !!c.down, rowH);
    out += `<path d="${d}" fill="none" stroke="${colour}" stroke-width="${STROKE}"/>`;
  }
  const r = row.merge ? MERGE_R : DOT_R;
  out +=
    `<circle cx="${laneX(row.lane)}" cy="${mid}" r="${r - STROKE / 2}" ` +
    `fill="${t.chrome.bg}" stroke="${laneColor(t, row.hue, over(row.lane))}" ` +
    `stroke-width="${STROKE}"/>`;

  return `<svg class="gutter" width="${w}" height="${rowH}" viewBox="0 0 ${w} ${rowH}">${out}</svg>`;
}

/** lazygit's order — sha, author, graph, subject — because the graph is the
 *  column that changes width, and putting it last would move the subject. */
function commitHtml(row, index) {
  const cursor = index === state.cursor ? " cursor" : "";
  if (!row) return `<div class="row commit pending${cursor}">…</div>`;
  return (
    `<div class="row commit${cursor}">` +
    `<span class="sha">${escape(row.sha)}</span>` +
    `<span class="who" style="color:${row.authorFg}" title="${escape(row.author)}">` +
    `${escape(row.initials)}</span>` +
    gutter(row, state.meta) +
    `<span class="subject">${escape(row.subject)}</span>` +
    `</div>`
  );
}

/** Which view is on screen. From `meta`, so one page serves both. */
function rowHtml(row, index) {
  return state.kind === "commits" ? commitHtml(row, index) : diffRowHtml(row, index);
}


function diffRowHtml(row, index) {
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
  const what = state.kind === "commits" ? "commit" : "row";
  dom.position.textContent = state.total
    ? `${what} ${state.cursor + 1} / ${state.total}`
    : `no ${what}s`;
}

function chrome(meta) {
  dom.label.textContent = `gitten · ${meta.label}`;
  if (meta.kind === "commits") {
    // The uncapped count against the drawn one: "280 lanes · 12 drawn" is worth
    // knowing, and silently drawing twelve is not.
    const bits = [`${meta.total} commits`, `${meta.concurrent} lanes`];
    if (meta.concurrent > meta.maxLanes) bits.push(`${meta.maxLanes} drawn`);
    dom.stats.textContent = bits.join(" · ");
    dom.wrap.hidden = true;
    dom.keys.textContent = "j k · g G · ctrl-d ctrl-u";
    return;
  }
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
  state.kind = meta.kind;
  // A diff's row count is *after* wrapping and moves with the width; a commit
  // list's is the number of commits and does not.
  state.total = meta.kind === "commits" ? meta.total : meta.rows;
  state.wrapName = meta.wrap ? meta.wrap.selected : null;
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
  if (state.kind === "commits") return;
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
      // Only a diff has a column budget to cross. A commit list just redraws.
      if (state.kind !== "commits" && columns() !== state.cols) reflow();
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
    state.kind = state.meta.kind;
    applyTheme(state.meta);
    await reflow();
    dom.scroll.focus();
  } catch (e) {
    dom.label.textContent = "gitten";
    dom.stats.textContent = String(e.message || e);
    dom.stats.className = "error";
  }
}

main();
