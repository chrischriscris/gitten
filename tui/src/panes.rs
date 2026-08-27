//! Registered pane tenants, their placement, and the geometry that lays them out.
//!
//! This is the terminal's answer to `shell::panes::Panes` and the window's
//! sidebar-plus-main layout, and it is deliberately generic: a pane is a stable
//! name and a value the app supplies, and nothing in here knows what a
//! [`Screens`](crate::screen::Screen), a commit or a crossterm event is. Adding
//! a files or branches tenant is a `register` call — not a new branch in layout
//! or dispatch, which is the whole point of holding the names in a registry
//! instead of in fields.
//!
//! # Three things, and who owns each
//!
//! - **Identity and focus** are here: stable names, registration, the focused
//!   index, the two orders the keyboard walks. `core::command` already holds
//!   the command names (`pane.left`, `commits.focus`, …); this module holds the
//!   panes they name.
//! - **Geometry** is here as data — a [`Rect`] per pane, cached by the caller —
//!   because a pane rectangle is client drawing/input. The arithmetic is
//!   terminal-only: cells, not fractions of an em.
//! - **The layout policy** is a [`Layout`] trait with a built-in, so a
//!   compiled-in client extension can replace the built-in geometry without
//!   touching registry or dispatch — the same seam `Glyphs` and `Bar` offer.
//!
//! # The built-in shape
//!
//! lazygit's: a sidebar column of lists beside one main region, one column of
//! divider between them, and below [`WIDE_AT`] columns a **narrow** layout that
//! gives the whole body to the focused pane alone. The two views the terminal
//! ships today are `commits` (sidebar) and `diff` (main); the other four
//! sidebar names are reserved slots, and an absent pane answers its focus
//! command with `no <name> pane` exactly as the window does. Vertical stacking
//! of lists is nobody's fallback here: terminal height is the scarcer axis, and
//! two short viewports are less useful than one honest one.

/// Body width at which the sidebar and the main region sit side by side.
///
/// 40 columns draw an abbreviated sha, an author, a useful graph and a
/// subject; 55 carry the diff's gutters and readable text; one column between
/// them belongs to nobody. 96 is the smallest width where all three hold —
/// measured in cells against the shipped panes, not derived from anything.
pub const WIDE_AT: usize = 96;

/// The sidebar never draws narrower than this, wide mode or not.
pub const SIDEBAR_MIN: usize = 40;

/// The main region never draws narrower than this, wide mode or not.
pub const DIFF_MIN: usize = 55;

/// The share of the body the sidebar asks for, in percent, before the floor
/// above applies. The window's [`SIDEBAR_SHARE`](shell) is the same number in
/// a different unit.
const SIDEBAR_SHARE: usize = 32;

/// Columns between the sidebar and the main region, owned by neither.
pub const DIVIDER: usize = 1;

/// The canonical sidebar ranks, matching the window's list order — lazygit's
/// reading order, which the number keys spell out: status, files, branches,
/// commits, then the stash at the foot. A name outside the table keeps its
/// registration order behind all of these.
pub fn canonical_rank(name: &str) -> Option<usize> {
    match name {
        "status" => Some(0),
        "files" => Some(1),
        "branches" => Some(2),
        "commits" => Some(3),
        "stashes" => Some(4),
        _ => None,
    }
}

/// Where a pane draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// A list in the sidebar column. `rank` is the pane's place in the walk
    /// and cycle orders; [`Placement::sidebar`] fills it in from
    /// [`canonical_rank`], which is the only way a built-in name should be
    /// registered.
    Sidebar { rank: usize },
    /// The main region. One slot, reserved for `diff`.
    Main,
}

impl Placement {
    /// The placement for a pane named `name`: its canonical rank when it is
    /// one of the five built-in lists, the tail of the order when it is not.
    pub fn sidebar(name: &str) -> Self {
        Self::Sidebar {
            rank: canonical_rank(name).unwrap_or(usize::MAX),
        }
    }
}

/// One rectangle of the body, in terminal cells.
///
/// `x` and `y` are columns and rows of the whole screen, so a painter can be
/// handed this and the screen and clip itself without anybody subtracting
/// chrome twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Rect {
    /// Whether a cell of the screen is inside this rectangle.
    pub fn contains(&self, col: usize, row: usize) -> bool {
        col >= self.x
            && row >= self.y
            && col < self.x.saturating_add(self.width)
            && row < self.y.saturating_add(self.height)
    }

    /// The one header row.
    pub fn header(&self) -> Rect {
        Rect { height: 1, ..*self }
    }

    /// Everything under the header: the rows a view paints and resizes to.
    ///
    /// Saturating, because a body one row tall has a header and no content,
    /// and a pane that cannot draw content must not panic about it.
    pub fn content(&self) -> Rect {
        Rect {
            x: self.x,
            y: self.y.saturating_add(1),
            width: self.width,
            height: self.height.saturating_sub(1),
        }
    }

    /// One column past the right edge.
    pub fn right(&self) -> usize {
        self.x.saturating_add(self.width)
    }
}

/// Where every pane sits, as the layout decided it.
///
/// A pane with no rectangle is hidden — the narrow layout's answer for the
/// unfocused pane — and a hit test or a paint over it finds nothing. Built by
/// a [`Layout`] when the screen size, the focus or the registrations change,
/// and read-only until one of those happens again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Geometry {
    rects: Vec<(String, Rect)>,
}

impl Geometry {
    /// The rectangle of one pane, or `None` when the layout hid it.
    pub fn rect(&self, name: &str) -> Option<Rect> {
        self.rects.iter().find(|(n, _)| n == name).map(|(_, r)| *r)
    }

    /// The pane under a cell of the screen, for a mouse press.
    pub fn hit(&self, col: usize, row: usize) -> Option<&str> {
        self.rects
            .iter()
            .find(|(_, r)| r.contains(col, row))
            .map(|(n, _)| n.as_str())
    }

    /// Every placed pane, in layout order: the sidebar column top to bottom,
    /// then the main region.
    pub fn placed(&self) -> impl Iterator<Item = (&str, Rect)> {
        self.rects.iter().map(|(n, r)| (n.as_str(), *r))
    }

    fn put(&mut self, name: &str, rect: Rect) {
        if rect.width > 0 && rect.height > 0 {
            self.rects.push((name.to_string(), rect));
        }
    }
}

/// One registered pane, as a [`Layout`] sees it: a snapshot of the registry
/// with nothing in it but the three facts geometry needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spot<'a> {
    pub name: &'a str,
    pub placement: Placement,
    pub focused: bool,
}

/// What decides where panes sit.
///
/// A function of the registered panes and the body rectangle, and nothing
/// else — so a layout is a pure, cacheable answer. The built-in is
/// [`BuiltinLayout`]; a compiled-in client extension replaces it without
/// touching the registry, focus, or dispatch.
pub trait Layout {
    fn arrange(&self, spots: &[Spot<'_>], body: Rect) -> Geometry;
}

/// The built-in layout: a sidebar column of lists beside one main region at
/// [`WIDE_AT`] columns and wider, the focused pane alone below it.
///
/// With no sidebar registered there is no divider and no sidebar column: the
/// main region takes the whole body, which is what a diff-shaped launch
/// should look like at any width. Sidebar lists share the column by equal
/// slices, the remainder going to the earlier panes; a main pane beyond the
/// first is not the built-in's to place — the slot is reserved for one diff,
/// and a second one is a layout an extension owns.
#[derive(Debug, Clone, Copy, Default)]
pub struct BuiltinLayout;

impl Layout for BuiltinLayout {
    fn arrange(&self, spots: &[Spot<'_>], body: Rect) -> Geometry {
        let mut sidebars: Vec<&Spot<'_>> = spots
            .iter()
            .filter(|s| matches!(s.placement, Placement::Sidebar { .. }))
            .collect();
        // Stable, so equally-ranked panes — the extensions, which all take
        // [`usize::MAX`] — keep their registration order behind the built-ins.
        sidebars.sort_by_key(|s| match s.placement {
            Placement::Sidebar { rank } => rank,
            Placement::Main => 0,
        });
        let mains: Vec<&Spot<'_>> = spots
            .iter()
            .filter(|s| matches!(s.placement, Placement::Main))
            .collect();

        let mut g = Geometry::default();
        let wide = body.width >= WIDE_AT;
        match (sidebars.is_empty(), wide) {
            // Nothing beside the main region at any width: it is the body.
            (true, _) => {
                if let Some(main) = mains.first() {
                    g.put(main.name, body);
                }
            }
            // Wide: the sidebar asks for its share, floored, then the divider,
            // and the diff takes the rest — at least [`DIFF_MIN`] wide by
            // construction at [`WIDE_AT`] and above.
            (false, true) => {
                let share = body.width * SIDEBAR_SHARE / 100;
                let sidebar_w = share.max(SIDEBAR_MIN).min(body.width);
                let diff_x = body.x.saturating_add(sidebar_w).saturating_add(DIVIDER);
                let diff_w = body.right().saturating_sub(diff_x);
                let n = sidebars.len();
                let height = body.height / n;
                let extra = body.height % n;
                for (i, spot) in sidebars.iter().enumerate() {
                    let top = body.y + i * height + i.min(extra);
                    let tall = height + usize::from(i < extra);
                    g.put(
                        spot.name,
                        Rect {
                            x: body.x,
                            y: top,
                            width: sidebar_w,
                            height: tall,
                        },
                    );
                }
                if let Some(main) = mains.first() {
                    g.put(
                        main.name,
                        Rect {
                            x: diff_x,
                            y: body.y,
                            width: diff_w,
                            height: body.height,
                        },
                    );
                }
            }
            // Narrow: the focused pane alone, at the full body. The others are
            // not placed at all — not squeezed, not stacked: one honest
            // viewport is worth more than two cramped ones.
            (false, false) => {
                if let Some(spot) = spots.iter().find(|s| s.focused) {
                    g.put(spot.name, body);
                }
            }
        }
        g
    }
}

struct Entry<T> {
    name: String,
    placement: Placement,
    value: T,
}

/// The terminal's pane registry: stable names, placement, and focus.
///
/// `T` is the per-view adapter the app keeps beside each view — the registry
/// itself never learns what it holds. Registering a name that already exists
/// replaces that tenant in place and focuses it, so opening a diff is a
/// `register` and not a layout branch; the count of panes never grows by
/// accident.
pub struct Panes<T> {
    entries: Vec<Entry<T>>,
    focused: usize,
    /// Bumped on every registration, so a cached [`Geometry`] can be keyed on
    /// it and a replacement invalidates the cache without a comparison per
    /// pane.
    generation: usize,
}

impl<T> Panes<T> {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            focused: 0,
            generation: 0,
        }
    }

    /// Adds a tenant, or replaces one already registered under `name`, and
    /// focuses it. Returns the replaced tenant when there was one.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        placement: Placement,
        value: T,
    ) -> Option<T> {
        let name = name.into();
        self.generation += 1;
        if let Some(at) = self.entries.iter().position(|e| e.name == name) {
            self.focused = at;
            let entry = &mut self.entries[at];
            entry.placement = placement;
            return Some(std::mem::replace(&mut entry.value, value));
        }
        self.entries.push(Entry {
            name,
            placement,
            value,
        });
        self.focused = self.entries.len() - 1;
        None
    }

    /// A tenant by its stable registration name. Drawing and dispatch read
    /// through here instead of assuming any index is `commits` or `diff`.
    pub fn get(&self, name: &str) -> Option<&T> {
        self.position(name).map(|at| &self.entries[at].value)
    }

    /// Mutable, for a press or a command that acts on one named pane.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut T> {
        let at = self.position(name)?;
        Some(&mut self.entries[at].value)
    }

    /// Where a tenant lives, by its stable registration name — what a
    /// focus-by-name command (`commits.focus`) needs to find.
    pub fn position(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.name == name)
    }

    /// The placement a tenant was registered under.
    pub fn placement(&self, name: &str) -> Option<Placement> {
        self.position(name).map(|at| self.entries[at].placement)
    }

    /// Every registered name, in registration order. The walk and cycle
    /// orders are *derived* from this rather than being it — see
    /// [`Panes::list_order`].
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.entries.iter().map(|e| &e.value)
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.entries.iter_mut().map(|e| &mut e.value)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many times the registry has been registered into. Part of the
    /// geometry cache key, so a replacement invalidates it.
    pub fn generation(&self) -> usize {
        self.generation
    }

    /// The focused tenant's index.
    pub fn focused_index(&self) -> usize {
        self.focused
    }

    pub fn focused(&self) -> Option<&T> {
        self.entries.get(self.focused).map(|e| &e.value)
    }

    pub fn focused_mut(&mut self) -> Option<&mut T> {
        self.entries.get_mut(self.focused).map(|e| &mut e.value)
    }

    /// The focused tenant's stable registration name — what the title bar,
    /// the status prefix and a search prompt name it by.
    pub fn focused_name(&self) -> &str {
        self.entries
            .get(self.focused)
            .map(|e| e.name.as_str())
            .unwrap_or("")
    }

    /// The focused tenant's placement, or `None` on an empty registry.
    pub fn focused_placement(&self) -> Option<Placement> {
        self.entries.get(self.focused).map(|e| e.placement)
    }

    /// Focuses the tenant registered under `name`. Says whether it moved,
    /// which is what a caller that reports an absent pane distinguishes.
    pub fn focus_named(&mut self, name: &str) -> bool {
        match self.position(name) {
            Some(at) => self.focus(at),
            None => false,
        }
    }

    pub fn focus(&mut self, at: usize) -> bool {
        if at >= self.entries.len() || at == self.focused {
            return false;
        }
        self.focused = at;
        true
    }

    /// The sidebar lists, in the order the number keys name them and the
    /// keyboard walks them: the five built-ins in [`canonical_rank`] order,
    /// then whatever an extension registered. The `pane.next`/`pane.prev`
    /// cycle and the `panes` mode both read this, so a second list arriving
    /// later is a `register` call and not a dispatch edit.
    pub fn list_order(&self) -> Vec<&str> {
        let mut sidebars: Vec<(usize, usize, &str)> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e.placement, Placement::Sidebar { .. }))
            .map(|(i, e)| {
                (
                    match e.placement {
                        Placement::Sidebar { rank } => rank,
                        Placement::Main => 0,
                    },
                    i,
                    e.name.as_str(),
                )
            })
            .collect();
        sidebars.sort_by_key(|(rank, i, _)| (*rank, *i));
        sidebars.into_iter().map(|(_, _, name)| name).collect()
    }

    /// The full reading order the `pane.left`/`pane.right` walk follows: the
    /// sidebar lists top to bottom, then the main region. Left of the diff is
    /// the sidebar's foot; right of the last list is the diff; an edge stops.
    pub fn reading_order(&self) -> Vec<&str> {
        let mut order = self.list_order();
        order.extend(
            self.entries
                .iter()
                .filter(|e| matches!(e.placement, Placement::Main))
                .map(|e| e.name.as_str()),
        );
        order
    }

    /// The neighbour one step `by` along the reading order, or `None` at
    /// either edge — the walk never wraps, because the number keys already
    /// cover the jumping.
    pub fn walk(&self, by: isize) -> Option<&str> {
        let order = self.reading_order();
        let focused = self.focused_name();
        let at = order.iter().position(|name| *name == focused)?;
        let next = at as isize + by;
        if next < 0 || next >= order.len() as isize {
            return None;
        }
        order.get(next as usize).copied()
    }

    /// Cycles focus by an offset through the sidebar lists only, wrapping —
    /// what `pane.next`/`pane.prev` do once a second list exists. `None` when
    /// there is no second list to cycle to, which is the honest answer while
    /// only one ships.
    pub fn cycle_sidebar(&mut self, by: isize) -> bool {
        let order = self.list_order();
        if order.len() < 2 {
            return false;
        }
        let focused = self.focused_name();
        let current = order.iter().position(|name| *name == focused).unwrap_or(0);
        let next = (current as isize + by).rem_euclid(order.len() as isize) as usize;
        let name = order[next].to_string();
        self.focus_named(&name)
    }

    /// The registry as a [`Layout`] sees it, in registration order.
    pub fn spots(&self) -> Vec<Spot<'_>> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| Spot {
                name: e.name.as_str(),
                placement: e.placement,
                focused: i == self.focused,
            })
            .collect()
    }
}

impl<T> Default for Panes<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Five built-in slots and a main, registered the way the app does it.
    fn full() -> Panes<&'static str> {
        let mut p = Panes::new();
        p.register("status", Placement::sidebar("status"), "status");
        p.register("files", Placement::sidebar("files"), "files");
        p.register("branches", Placement::sidebar("branches"), "branches");
        p.register("commits", Placement::sidebar("commits"), "commits");
        p.register("stashes", Placement::sidebar("stashes"), "stashes");
        p.register("diff", Placement::Main, "diff");
        p
    }

    #[test]
    fn registration_replaces_by_name_and_preserves_canonical_order() {
        let mut p = Panes::new();
        assert_eq!(
            p.register("commits", Placement::sidebar("commits"), 1),
            None
        );
        assert_eq!(p.register("diff", Placement::Main, 2), None);
        assert_eq!(p.register("ext-a", Placement::sidebar("ext-a"), 3), None);
        assert_eq!(p.register("ext-b", Placement::sidebar("ext-b"), 4), None);
        assert_eq!(p.len(), 4, "a registration appended a duplicate");
        assert_eq!(p.focused_name(), "ext-b", "register did not focus");

        // Replacing keeps the name where it was, focuses the replacement, and
        // grows nothing.
        assert_eq!(p.register("ext-a", Placement::sidebar("ext-a"), 5), Some(3));
        assert_eq!(p.len(), 4);
        assert_eq!(p.position("ext-a"), Some(2), "a replacement moved");
        assert_eq!(*p.get("ext-a").unwrap(), 5);
        assert_eq!(p.focused_name(), "ext-a", "focus did not stay stable");

        // Canonical built-ins before extensions, and Main last.
        assert_eq!(p.list_order(), ["commits", "ext-a", "ext-b"]);
        assert_eq!(p.reading_order(), ["commits", "ext-a", "ext-b", "diff"]);
    }

    #[test]
    fn pane_walk_stops_and_sidebar_cycle_wraps() {
        let mut p = full();
        p.focus_named("status");
        // Right walks the whole reading order and stops at the diff.
        for expected in ["files", "branches", "commits", "stashes", "diff"] {
            assert_eq!(p.walk(1), Some(expected), "at {}", p.focused_name());
            let name = p.walk(1).expect("in range").to_string();
            p.focus_named(&name);
            assert_eq!(p.focused_name(), expected);
        }
        assert_eq!(p.walk(1), None, "the walk wrapped past the diff");
        // Left walks back and stops at the top of the stack.
        assert_eq!(p.walk(-1), Some("stashes"));
        p.focus_named("status");
        assert_eq!(p.walk(-1), None, "the walk wrapped above status");

        // The cycle wraps through the sidebar only, never reaching the diff.
        p.focus_named("stashes");
        assert!(p.cycle_sidebar(1));
        assert_eq!(p.focused_name(), "status", "next did not wrap");
        assert!(p.cycle_sidebar(-1));
        assert_eq!(p.focused_name(), "stashes", "prev did not wrap");
        p.focus_named("diff");
        assert!(p.cycle_sidebar(1), "cycling from the diff still works");
        // The desktop's arithmetic: an unfound focus reads as position 0, so
        // next from the main region is the *second* list, and prev wraps to
        // the foot of the stack.
        assert_eq!(p.focused_name(), "files");
        p.focus_named("diff");
        assert!(p.cycle_sidebar(-1));
        assert_eq!(p.focused_name(), "stashes");

        // Two panes: the walk moves, the cycle has no second list.
        let mut p = Panes::new();
        p.register("commits", Placement::sidebar("commits"), 1);
        p.register("diff", Placement::Main, 2);
        p.focus_named("commits");
        assert_eq!(p.walk(1), Some("diff"));
        p.focus_named("diff");
        assert_eq!(p.walk(-1), Some("commits"));
        assert!(!p.cycle_sidebar(1), "one list is not a cycle");
        assert_eq!(p.focused_name(), "diff", "a refused cycle moved focus");
    }

    #[test]
    fn wide_geometry_has_one_owned_divider_and_no_overlap() {
        let layout = BuiltinLayout;
        for width in [WIDE_AT, 120, 160] {
            let body = Rect {
                x: 0,
                y: 1,
                width,
                height: 22,
            };
            let p = full();
            let g = layout.arrange(&p.spots(), body);

            // Every sidebar list placed, each at least the floor wide, in the
            // column's canonical order top to bottom.
            let sidebar = g.rect("status").expect("status placed");
            let commits = g.rect("commits").expect("commits placed");
            assert_eq!(sidebar.x, body.x);
            let share = width * SIDEBAR_SHARE / 100;
            assert_eq!(
                sidebar.width,
                share.max(SIDEBAR_MIN),
                "{width}: sidebar width"
            );
            assert_eq!(commits.x, body.x, "the column is one column wide");

            // The main region: at least its floor, one divider column to the
            // right of the sidebar, and no cell shared with it.
            let diff = g.rect("diff").expect("diff placed");
            assert!(diff.width >= DIFF_MIN, "{width}: {diff:?}");
            assert_eq!(
                diff.x,
                sidebar.right() + DIVIDER,
                "the divider is exactly one cell"
            );
            assert_eq!(diff.y, body.y);
            assert_eq!(diff.height, body.height);

            // Disjoint: no two placed panes share a cell.
            let placed: Vec<(&str, Rect)> = g.placed().collect();
            for (i, (n, r)) in placed.iter().enumerate() {
                for (m, o) in placed.iter().skip(i + 1) {
                    let apart = r.x + r.width <= o.x
                        || o.x + o.width <= r.x
                        || r.y + r.height <= o.y
                        || o.y + o.height <= r.y;
                    assert!(apart, "{width}: {n} and {m} share a cell");
                }
            }
            // The sidebar column, the divider and the diff cover the body.
            assert_eq!(
                sidebar.width + DIVIDER + diff.width,
                body.width,
                "{width}: the body is not covered"
            );
            assert_eq!(
                diff.right(),
                body.right(),
                "{width}: the diff stopped short of the edge"
            );

            // Headers leave a nonnegative content rectangle.
            for (_, r) in g.placed() {
                assert_eq!(r.content().height + 1, r.height, "{r:?}");
            }

            // The sidebar column is one column: every sidebar slice shares x
            // and width, and their heights tile the body.
            let slices: Vec<Rect> = ["status", "files", "branches", "commits", "stashes"]
                .iter()
                .filter_map(|n| g.rect(n))
                .collect();
            assert_eq!(slices.len(), 5, "{width}: a sidebar pane was dropped");
            assert!(slices.windows(2).all(|w| w[0].right() == w[1].right()
                && w[0].x == w[1].x
                && w[0].y + w[0].height == w[1].y));
            assert_eq!(
                slices.iter().map(|r| r.height).sum::<usize>(),
                body.height,
                "{width}: the sidebar column does not tile"
            );
        }
    }

    #[test]
    fn narrow_geometry_shows_only_the_focused_pane() {
        let layout = BuiltinLayout;
        let mut p = Panes::new();
        p.register("commits", Placement::sidebar("commits"), 1);
        p.register("diff", Placement::Main, 2);
        for width in [WIDE_AT - 1, 80, 0] {
            for focused in ["commits", "diff"] {
                p.focus_named(focused);
                let body = Rect {
                    x: 0,
                    y: 1,
                    width,
                    height: 22,
                };
                let g = layout.arrange(&p.spots(), body);
                match width {
                    0 => assert!(
                        g.placed().next().is_none(),
                        "a pane was placed in a zero-width body"
                    ),
                    _ => {
                        let (name, rect) = g.placed().collect::<Vec<_>>()[0];
                        assert_eq!(g.placed().count(), 1, "{width}: two panes visible");
                        assert_eq!(name, focused, "{width}: the wrong pane is visible");
                        assert_eq!(rect, body, "{width}: the pane is not the body");
                    }
                }
            }
        }

        // Focus switching swaps visibility without touching anybody's state —
        // the geometry is a function of the spots, and the spots carry no
        // viewport.
        p.focus_named("commits");
        let narrow = Rect {
            x: 0,
            y: 1,
            width: 80,
            height: 22,
        };
        let a = layout.arrange(&p.spots(), narrow);
        assert_eq!(a.rect("commits"), Some(narrow));
        assert_eq!(a.rect("diff"), None, "the hidden pane kept a rectangle");
        p.focus_named("diff");
        let b = layout.arrange(&p.spots(), narrow);
        assert_eq!(b.rect("diff"), Some(narrow));
        assert_eq!(b.rect("commits"), None);

        // A diff-shaped launch is full width at any width: no sidebar, no
        // divider, no empty column.
        let mut p = Panes::new();
        p.register("diff", Placement::Main, 1);
        for width in [WIDE_AT, 120, 95, 80] {
            let body = Rect {
                x: 0,
                y: 1,
                width,
                height: 22,
            };
            let g = layout.arrange(&p.spots(), body);
            assert_eq!(
                g.rect("diff"),
                Some(body),
                "{width}: the diff did not take the whole body"
            );
        }
    }

    #[test]
    fn degenerate_dimensions_are_survivable_and_saturating() {
        let layout = BuiltinLayout;
        let p = full();
        // A body one row tall: a header with no content row under it.
        let g = layout.arrange(
            &p.spots(),
            Rect {
                x: 0,
                y: 1,
                width: 120,
                height: 1,
            },
        );
        assert!(g.placed().all(|(_, r)| r.content().height == 0), "{g:?}");
        // A zero-area body places nothing and panics nowhere.
        let g = layout.arrange(
            &p.spots(),
            Rect {
                x: 0,
                y: 1,
                width: 120,
                height: 0,
            },
        );
        assert_eq!(g.placed().count(), 0, "{g:?}");
        // More sidebars than rows: the tail is dropped rather than drawn
        // upside down.
        let g = layout.arrange(
            &p.spots(),
            Rect {
                x: 0,
                y: 1,
                width: 120,
                height: 2,
            },
        );
        assert_eq!(g.placed().count(), 3, "{g:?}");

        // A header/content split never runs backwards.
        let r = Rect {
            x: 3,
            y: 4,
            width: 10,
            height: 5,
        };
        assert_eq!(r.header(), Rect { height: 1, ..r });
        assert_eq!(
            r.content(),
            Rect {
                x: 3,
                y: 5,
                width: 10,
                height: 4
            }
        );
        assert!(r.contains(3, 4) && r.contains(12, 8));
        assert!(!r.contains(13, 8) && !r.contains(3, 9) && !r.contains(2, 4));
        assert_eq!(r.right(), 13);
    }
}
