# The commit graph

Lane assignment is pure and lives in `core`; geometry and painting live in the
shell. The split is not cosmetic — lane assignment is where the bugs are, and it
is testable without opening a window.

## Input: topological order is mandatory

Always log with `--topo-order`. It is what git itself uses for `--graph`, and
`assign_lanes` assumes it: without it branches interleave and the drawing is
simply wrong.

It is *not* a width optimisation. It narrows git/git from 417 lanes to 280 and
*widens* cmux from 19 to 73. Correctness, not compactness.

## Lane assignment

```rust
pub fn assign_lanes(commits: &[Commit]) -> Vec<GraphRow>

pub struct GraphRow {
    pub lane: usize,        // where this commit's dot sits
    pub through: Vec<usize>,// lanes passing straight through this row
    pub merges: Vec<usize>, // lanes converging into this commit
    pub forks: Vec<usize>,  // lanes opened for a merge's 2nd+ parents
}
```

One vector of "which sha is this lane waiting for", walked newest-first:

```
   lanes:  [a]   [ ]   [ ]        commit a, parents b c
     a  ●         claim lane 0, first parent b stays in 0, c forks to 1
     |\
     b  ●  |      lanes: [b][c]   b is in 0, c passes through
     | c   ●      lanes: [d][d]   both lanes now wait on d
     |/
     d  ●         d found in lane 0; lane 1 also waited on it → merges=[1]
```

The invariants worth knowing, both asserted by `examples/verify.rs` against real
history:

1. A commit's **first parent continues on the same lane** — unless an earlier
   commit already claimed that lane for the same parent, which is a legitimate
   collapse.
2. A merge's **second and later parents never land on the merge's own lane**.

## Colour belongs to the branch, not the column

Colouring by lane index is the obvious thing and it is wrong: lane 1 is recycled
the moment a branch merges, so unrelated branches come out the same blue and read
as one long-running thing.

So `Hues` walks the history and hands each *new* lane the next hue on the wheel,
skipping any hue a concurrently live lane holds, and releases it when the branch
ends. Consecutive branches differ even when they share a column.

`LANE_HUES = 6` is the size of that ledger — how many live branches can be told
apart at a glance — and is not the number of colours a theme ships. A hue resolves
through `theme.lane(hue)`, which cycles over whatever the theme provides.

## The cap

`MAX_LANES = 12`, hard. git/git reaches 280 concurrent lanes, which is a 3,920px
gutter that pushes the commit text clean off the screen. Nobody reads past a
dozen; git's own `--graph` is unreadable well before that.

Lanes past the cap collapse onto the last column in `theme.lane_overflow` grey —
visible as "there is more over here" rather than silently misdrawn. They collapse
in the data too (`fn cap`), or git/git would queue 280 identical quads per row.

## Per-row width

Each row's gutter is measured from **its own** lanes, not the widest row in the
repository:

```
  ●            fix the thing                 ← trunk row: text starts at column 1
  |
  | ●          add the other thing
  |/
  ●            merge branch 'x'
  |\
  | | ●        deep in a wide merge fan       ← this row pushes its text right
```

A commit alone on the trunk gets nearly the whole window for its subject. Widths
are measured in whole lanes so text steps on the lane grid: ragged by a column
reads as "the graph is wider here", ragged by three pixels reads as broken.

## Drawing

`row_draws` flattens each row, once at load, into what painting needs — so the
paint callback never touches the commit list:

```rust
pub struct RowDraw {
    lane, hue, is_merge,
    lines:  Vec<Line>,    // verticals: lane, hue, up, down
    curves: Vec<Curve>,   // half an S: lane, partner, hue, down
    width:  f32,          // measured here, not per frame
}
```

One `canvas()` per row, so the drawing virtualizes with `uniform_list` for free.

Inside a row:

- **Verticals are quads.** A vertical line is a rectangle, and a quad costs a
  fraction of a tessellated stroke path.
- **Curves are half an S each.** A curve touches its lane on the dot line and
  crosses the row boundary halfway to its partner, where the next row picks it up.
  Each half runs `OVERSHOOT = 0.5px` past the boundary along its own tangent —
  collinear, so it cannot kink — because two antialiased butt caps meeting exactly
  leave a visible crease.
- **A dot is one quad with corner radii at half its size**, which *is* a circle,
  and the shader antialiases it better than tessellation would. The middle is
  punched out with `theme.chrome.bg`, so lines pass *behind* a node and the graph
  reads as holes in the sheet.

Geometry constants: `ROW_H = 22`, `LANE_W = 14`, `STROKE = 2` (straddles no pixel
boundary — a lane centre lands on 7, edges on 6 and 8, crisp at any scale factor),
`DOT_R = 4.5`, `MERGE_R = 5.5` so a join is findable while scrolling, `RING = 0.45`
of the radius, `GAP = 6` between the last stroke and the first letter.

## Row layout

lazygit's order and lazygit's spacing:

```
├─────────┼────┼──────────────┼──────────────────────────────►
│  sha 90 │ 26 │ own width    │  subject
│ a1b2c3d │ JH │  ● │         │  Refine the commit graph
└─────────┴────┴──────────────┴──────────────────────────────►
              author initials, coloured by name hash
```

Initials are two letters, first and last name — `Junio C Hamano` → `JH`, never the
middle one — resolved once at load with their colour, because that is not a
per-frame job.

## Checking it

```
./check.sh                                        # includes both examples below
cargo run -p plait-core --example verify --release # lane invariants on real history
cargo run -p plait-core --example shape --release  # topology statistics
```

`shape` on the two real repositories, for a sense of what "wide" means:

| repo | merges | p50 lanes | p99 | max | rows at 1 lane |
|---|---|---|---|---|---|
| git/git | 25.8% | 126 | 226 | 280 | 0.9% |
| cmux | 16.2% | 9 | 70 | 73 | 7.2% |

Big popular repos are not a substitute for git/git here: bun has 17k commits and
97% of rows sit at one lane, because squash-merge workflows produce straight
lines.
