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
//! gives the whole body to the focused pane alone. An absent pane answers its
//! focus command with `no <name> pane` exactly as the window does. Vertical
//! stacking of lists is nobody's fallback here: terminal height is the scarcer
//! axis, and two short viewports are less useful than one honest one.
//!
//! # Sections, and why the column is not eight slices
//!
//! The lists do not each get a slice of the column — they are grouped into
//! [`SECTIONS`], and the sections split the column in equal shares, the way
//! lazygit's panels do. Eight equal slices of twenty-two rows is three rows
//! each: a header, two commits, and nothing anybody can read. Four sections
//! is a header row of tabs plus four or five content rows each, and every
//! section keeps its rows whether the keyboard is in it or not — the focused
//! one is told apart by its header highlight alone. `[`/`]` walk the tabs of
//! the focused section; the number keys name a section and reach the tab it
//! is showing, which is why `2` is still `files.focus` and not a command
//! invented for the sidebar.
//!
//! Which tab each section shows is "the one the keyboard sat on last" — so
//! [`Panes`] keeps one recency list and [`Panes::spots`] resolves it once per
//! layout, including while the *diff* has the keyboard. A [`Layout`] is still
//! a pure function of the spots and the body: [`Spot::shown`] is the
//! registry's memory arriving as data, not state the geometry keeps.
//!
//! A tab whose pane never registered is not drawn and not reachable — a
//! fixture launch has one list, so it draws one header and not five — and a
//! section with no registered tab at all collapses out of the column
//! entirely. Nothing is advertised that a keypress could not land on.

/// The mode the keyboard is in while it moves between lists — the name the
/// keymap and `gitten.toml` use for the Ctrl-J/Ctrl-K and `[`/`]` bindings.
/// It is only ever pushed when a second sidebar list exists; with one list
/// neither the cycle nor the tabs have anything to say and the mode would be
/// a lie on the help panel.
pub const MODE: &str = "panes";

/// The mode the keyboard is in while the list it is on shares its section
/// with another one — the name the keymap and `gitten.toml` use for the
/// `[`/`]` bindings.
///
/// Its own mode rather than a corner of [`MODE`], pushed whenever two or
/// more sidebar tabs are registered: the ring is the whole sidebar's, so a
/// focused `stashes` shares nothing yet still wraps to the column's head.
/// A fixture launch with one list is the one shape without a ring, and a
/// `[` advertised there would be exactly the lie a mode-scoped help panel
/// exists to prevent. The window has no sections and never pushes it, which
/// is the other half of the same argument.
pub const TABS: &str = "tabs";

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

/// One sidebar section: a group of lists that share a slot in the column,
/// the way lazygit's panels do.
///
/// A section draws one header row of tabs — one per *registered* list in it —
/// and every section gets an equal share of the column's rows below its
/// header, focused or not. The tabs
/// are the pane names, in the order they are written here, and the order they
/// are written here is the order the keyboard walks: [`canonical_rank`] is the
/// flattened index of this table and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    /// The tab names, in the order they draw and the order `[`/`]` cycle.
    /// The first one is the section's own name, which is why the focus key the
    /// header advertises is its — lazygit's `2` is `2-Files`.
    pub tabs: &'static [&'static str],
}

/// The sidebar sections, in reading order — lazygit's, which the number keys
/// spell out: status, files, branches, commits, then the stash at the foot.
///
/// The `status` slot is reserved and nothing registers into it today, so the
/// section collapses out of the column entirely — the same answer an absent
/// `worktrees` gets. It stays in the table so the numbering below it is
/// lazygit's rather than one off it, and so `status.focus` names a rank
/// instead of falling to the tail with the extensions.
///
/// A sidebar name outside this table is a section of its own at the tail:
/// grouping is what a section *is*, and an extension nobody grouped is not
/// silently tabbed behind an extension it has never heard of.
pub const SECTIONS: &[Section] = &[
    Section { tabs: &["status"] },
    Section {
        tabs: &["files", "worktrees"],
    },
    Section {
        tabs: &["branches", "remotes", "tags"],
    },
    Section {
        tabs: &["commits", "reflog"],
    },
    Section { tabs: &["stashes"] },
];

/// Cells between two tab names on a section header — lazygit's separator,
/// and the one the paint draws and the tab spans are worked out against.
pub const TAB_GAP: &str = " - ";

/// Cells of padding either side of the key and before the first tab — the
/// same two the pane headers have always had.
const HEADER_PAD: usize = 2;

/// The section and tab a built-in sidebar name sits in, or `None` for a name
/// outside [`SECTIONS`].
pub fn canonical_slot(name: &str) -> Option<(usize, usize)> {
    SECTIONS.iter().enumerate().find_map(|(s, section)| {
        section
            .tabs
            .iter()
            .position(|tab| *tab == name)
            .map(|t| (s, t))
    })
}

/// The canonical sidebar ranks: [`SECTIONS`] flattened, so the walk order and
/// the reading order are the order the column draws — section by section, tab
/// by tab. A name outside the table keeps its registration order behind all
/// of these.
pub fn canonical_rank(name: &str) -> Option<usize> {
    let mut rank = 0;
    for section in SECTIONS {
        for tab in section.tabs {
            if *tab == name {
                return Some(rank);
            }
            rank += 1;
        }
    }
    None
}

/// Where a pane draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// A list in the sidebar column. `rank` is the pane's place in the walk
    /// and cycle orders and `section` the slot it shares with its tabs;
    /// [`Placement::sidebar`] fills both in from [`SECTIONS`], which is the
    /// only way a built-in name should be registered.
    ///
    /// `section: None` is a name outside the table: a section of its own at
    /// the tail of the column, never tabbed behind somebody else's list.
    Sidebar { section: Option<usize>, rank: usize },
    /// The main region. One slot, reserved for `diff`.
    Main,
}

impl Placement {
    /// The placement for a pane named `name`: its section and canonical rank
    /// when it is one of the built-in lists, a section of its own at the tail
    /// of the order when it is not.
    pub fn sidebar(name: &str) -> Self {
        Self::Sidebar {
            section: canonical_slot(name).map(|(s, _)| s),
            rank: canonical_rank(name).unwrap_or(usize::MAX),
        }
    }

    /// The pane's place in the walk order, or `0` for the main region — which
    /// never takes part in the sidebar sort.
    fn rank(&self) -> usize {
        match self {
            Self::Sidebar { rank, .. } => *rank,
            Self::Main => 0,
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

/// One tab drawn on a section header: its pane, the cells its name covers,
/// and whether it is the tab its section is showing.
///
/// The cells are the *name's* — not the gap either side of it — because a
/// click on a tab is a click on the word, and a click on the gap is a click
/// on the section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub name: String,
    pub x: usize,
    pub width: usize,
    pub active: bool,
}

impl Tab {
    /// Whether a cell of the screen is on this tab's name.
    pub fn contains(&self, col: usize, row: usize, y: usize) -> bool {
        row == y && col >= self.x && col < self.x.saturating_add(self.width)
    }
}

/// One sidebar section's header row, as the layout laid it out.
///
/// Text positions, not text: the paint reads the spans back rather than
/// re-deriving them, and so does the hit test — which is the only way a click
/// on a tab name and the highlight under it can agree. Resolved once per
/// layout, so nothing here is arithmetic a frame repeats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// The one row the header draws on, at the sidebar's full width.
    pub rect: Rect,
    /// The key that focuses the section — the first key bound to its first
    /// drawn tab's `<name>.focus`, carried in on [`Spot::key`]. Empty when
    /// nothing is bound, in which case the header shows no key at all.
    pub key: String,
    /// The tabs, left to right, in [`SECTIONS`] order.
    pub tabs: Vec<Tab>,
}

impl Header {
    /// The tab whose name covers a cell of the screen.
    pub fn hit(&self, col: usize, row: usize) -> Option<&Tab> {
        self.tabs
            .iter()
            .find(|tab| tab.contains(col, row, self.rect.y))
    }
}

/// Where every pane sits, as the layout decided it.
///
/// A pane with no rectangle is hidden — the narrow layout's answer for the
/// unfocused pane, and a section's answer for every tab but the one it is
/// showing — and a hit test or a paint over it finds nothing. Built by a
/// [`Layout`] when the screen size, the focus or the registrations change,
/// and read-only until one of those happens again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Geometry {
    rects: Vec<(String, Rect)>,
    headers: Vec<Header>,
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

    /// The tab name under a cell of the screen, when the cell is on a section
    /// header and on a tab's word. A collapsed section's header is one row and
    /// [`Geometry::hit`] answers it with the tab it is showing, so this is the
    /// finer question the mouse asks first: *which* tab was clicked.
    pub fn hit_tab(&self, col: usize, row: usize) -> Option<&str> {
        self.headers()
            .iter()
            .find_map(|h| h.hit(col, row))
            .map(|tab| tab.name.as_str())
    }

    /// Every placed pane, in layout order: the sidebar column top to bottom,
    /// then the main region.
    pub fn placed(&self) -> impl Iterator<Item = (&str, Rect)> {
        self.rects.iter().map(|(n, r)| (n.as_str(), *r))
    }

    /// The section headers, top to bottom. The main region has no section and
    /// so no entry here — it draws its own one-pane header.
    pub fn headers(&self) -> &[Header] {
        &self.headers
    }

    /// The section header a pane's row belongs to, if it has one — which is
    /// how the paint knows whether a rectangle's header row is a section's
    /// tabs or a lone pane's name.
    pub fn header_of(&self, name: &str) -> Option<&Header> {
        self.headers()
            .iter()
            .find(|h| h.tabs.iter().any(|tab| tab.name == name))
    }

    fn put(&mut self, name: &str, rect: Rect) {
        if rect.width > 0 && rect.height > 0 {
            self.rects.push((name.to_string(), rect));
        }
    }
}

/// One registered pane, as a [`Layout`] sees it: a snapshot of the registry
/// with nothing in it but the facts geometry needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spot<'a> {
    pub name: &'a str,
    pub placement: Placement,
    pub focused: bool,
    /// Whether this is the tab its section is showing — the pane in it the
    /// keyboard sat on last. Exactly one registered spot per section carries
    /// it, and the main region always does: it is nobody's tab.
    pub shown: bool,
    /// The key that focuses this pane, from the live keymap — data the
    /// registry does not hold, filled in by the caller. Empty is honest: an
    /// unbound pane advertises no key rather than a stale one.
    pub key: &'a str,
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

/// The sidebar sections a set of spots actually has: the [`SECTIONS`] table
/// filtered down to the names that registered, then a singleton section per
/// ungrouped name, in walk order.
///
/// An empty section is not in the answer at all — that is what "an absent
/// pane collapses its tab out of the header" means, and it is why a fixture
/// launch with one list draws one header and not five.
fn sections<'s, 'a>(spots: &'s [Spot<'a>]) -> Vec<Vec<&'s Spot<'a>>> {
    let sidebars = |section: Option<usize>| -> Vec<&'s Spot<'a>> {
        let mut group: Vec<&'s Spot<'a>> = spots
            .iter()
            .filter(
                |s| matches!(s.placement, Placement::Sidebar { section: at, .. } if at == section),
            )
            .collect();
        // Stable, so equally-ranked panes — the extensions, which all take
        // [`usize::MAX`] — keep their registration order behind the built-ins.
        group.sort_by_key(|s| s.placement.rank());
        group
    };
    let mut out: Vec<Vec<&'s Spot<'a>>> = (0..SECTIONS.len())
        .map(|at| sidebars(Some(at)))
        .filter(|group| !group.is_empty())
        .collect();
    // An ungrouped name is a section of one, at the tail, in registration
    // order — the same place [`canonical_rank`] already puts it in the walk.
    out.extend(sidebars(None).into_iter().map(|spot| vec![spot]));
    out
}

/// The header row a section draws into `rect`, with every tab's cells worked
/// out: `  <key>  <tab> - <tab>`, clipped to the row it has.
///
/// Public because a replacement [`Layout`] that keeps the tabs is entitled to
/// the same arithmetic — the alternative is a second copy of it, and a second
/// copy is what puts the highlight and the click on different words.
pub fn header(group: &[&Spot<'_>], rect: Rect) -> Header {
    let key = group
        .iter()
        .find_map(|s| (!s.key.is_empty()).then_some(s.key))
        .unwrap_or("")
        .to_string();
    let mut x = rect.x + HEADER_PAD.min(rect.width);
    if !key.is_empty() {
        x += gitten_tui::screen::width(&key) + HEADER_PAD;
    }
    let gap = gitten_tui::screen::width(TAB_GAP);
    let mut tabs = Vec::with_capacity(group.len());
    for (i, spot) in group.iter().enumerate() {
        if i > 0 {
            x += gap;
        }
        let full = gitten_tui::screen::width(spot.name);
        // Clipped, not dropped: the pen clips the text the same way, and a
        // tab half off the edge is still the tab the visible half names.
        let room = rect.right().saturating_sub(x.min(rect.right()));
        tabs.push(Tab {
            name: spot.name.to_string(),
            x,
            width: full.min(room),
            active: spot.shown,
        });
        x += full;
    }
    Header { rect, key, tabs }
}

/// The built-in layout: a sidebar column of tabbed sections beside one main
/// region at [`WIDE_AT`] columns and wider, the focused pane alone below it.
///
/// With no sidebar registered there is no divider and no sidebar column: the
/// main region takes the whole body, which is what a diff-shaped launch
/// should look like at any width.
///
/// The sidebar is lazygit's: every section gets one header row of tabs, and
/// the sections split the column's remaining rows in equal shares — the
/// focused one told apart by its header highlight alone. Eight lists over
/// twenty-two rows is three rows each, which is a viewport nobody can read;
/// four sections is a header plus four or five content rows each. Only the
/// tab a section is showing gets a rectangle; the tabs behind it are hidden
/// exactly as the narrow layout hides an unfocused pane, and are resized
/// when they are next shown. A main pane beyond the first is not the
/// built-in's to place — the slot is reserved for one diff, and a second
/// one is a layout an extension owns.
#[derive(Debug, Clone, Copy, Default)]
pub struct BuiltinLayout;

impl Layout for BuiltinLayout {
    fn arrange(&self, spots: &[Spot<'_>], body: Rect) -> Geometry {
        let groups = sections(spots);
        let mains: Vec<&Spot<'_>> = spots
            .iter()
            .filter(|s| matches!(s.placement, Placement::Main))
            .collect();

        let mut g = Geometry::default();
        let wide = body.width >= WIDE_AT;
        match (groups.is_empty(), wide) {
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
                // [`DIFF_MIN`] is not a clamp here — it is what the arithmetic
                // above already guarantees at [`WIDE_AT`] and wider: the
                // sidebar's floor of 40 and the one divider leave at least 55
                // columns of body for the main region. Said where it holds,
                // so a change to any of the four constants that breaks the
                // guarantee breaks a build instead of a window.
                debug_assert!(
                    diff_w >= DIFF_MIN,
                    "{body:?}: sidebar {sidebar_w} + divider leaves {diff_w}"
                );
                let n = groups.len();
                // Equal shares, remainder to the earlier sections — the same
                // convention the eight slices used before sections existed.
                // A pure function of the spots and the body: no section is
                // expanded, so every section's rectangle is decided here.
                let base = body.height / n;
                let rem = body.height % n;
                let mut y = body.y;
                for (i, group) in groups.iter().enumerate() {
                    let tall = base + usize::from(i < rem);
                    // A section with no rows is dropped, tail first — the
                    // same answer as a body too short for its headers, and
                    // for the same reason: nothing is drawn upside down.
                    if tall == 0 {
                        continue;
                    }
                    let rect = Rect {
                        x: body.x,
                        y,
                        width: sidebar_w,
                        height: tall,
                    };
                    g.headers.push(header(group, rect.header()));
                    // One rectangle per section, and it is the shown tab's:
                    // a section is its header row and its content rows, which
                    // is what makes a click on it a click on that pane.
                    if let Some(spot) = group.iter().find(|s| s.shown).or_else(|| group.first()) {
                        g.put(spot.name, rect);
                    }
                    y += tall;
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
            // viewport is worth more than two cramped ones. A focused sidebar
            // list still draws its section's tabs, so `[`/`]` mean here what
            // they mean beside a diff.
            (false, false) => {
                if let Some(spot) = spots.iter().find(|s| s.focused) {
                    g.put(spot.name, body);
                    if let Some(group) = groups
                        .iter()
                        .find(|group| group.iter().any(|s| s.name == spot.name))
                    {
                        g.headers.push(header(group, body.header()));
                    }
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
    /// The sidebar names the keyboard has sat on, most recent first.
    ///
    /// The fact a tabbed sidebar cannot draw without and focus alone cannot
    /// answer: which tab each section is showing. It is "the one the keyboard
    /// sat on last", so one recency list is the whole of the state — and it
    /// is a list of names rather than of indices, so a registration cannot
    /// silently repoint it.
    recent: Vec<String>,
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
            recent: Vec::new(),
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
            self.touch(at);
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
        self.touch(self.focused);
        None
    }

    /// Marks a sidebar entry as the one the keyboard sat on last, which makes
    /// it its section's shown tab. A main pane is
    /// not a tab and does not disturb the sidebar's memory: the diff taking
    /// the keyboard leaves the sidebar showing exactly what it was showing,
    /// which is what `esc` back into the column expects to find.
    fn touch(&mut self, at: usize) {
        let Some(entry) = self.entries.get(at) else {
            return;
        };
        if !matches!(entry.placement, Placement::Sidebar { .. }) {
            return;
        }
        let name = entry.name.clone();
        self.recent.retain(|n| *n != name);
        self.recent.insert(0, name);
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

    /// Every registered name, in registration order. The walk and cycle
    /// orders are *derived* from this rather than being it — see
    /// [`Panes::list_order`].
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.entries.iter_mut().map(|e| &mut e.value)
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
        if at >= self.entries.len() {
            return false;
        }
        self.touch(at);
        if at == self.focused {
            return false;
        }
        self.focused = at;
        true
    }

    /// The sidebar lists, in the order the number keys name them and the
    /// keyboard walks them: the built-ins in [`canonical_rank`] order —
    /// section by section, tab by tab, so the walk follows the column — then
    /// whatever an extension registered. The `pane.next`/`pane.prev`
    /// cycle and the `panes` mode both read this, so a second list arriving
    /// later is a `register` call and not a dispatch edit.
    pub fn list_order(&self) -> Vec<&str> {
        let mut sidebars: Vec<(usize, usize, &str)> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e.placement, Placement::Sidebar { .. }))
            .map(|(i, e)| (e.placement.rank(), i, e.name.as_str()))
            .collect();
        sidebars.sort_by_key(|(rank, i, _)| (*rank, *i));
        sidebars.into_iter().map(|(_, _, name)| name).collect()
    }

    /// The pane one step `by` around the pane ring — every sidebar section,
    /// standing for the tab it is showing, then the main region, wrapping.
    /// What `h`/`l` cycle: a pane is a section or the main region, and a
    /// section is reached through the tab it shows. `None` when there is no
    /// pane to cycle to — a single pane is no cycle.
    pub fn cycle_sections(&self, by: isize) -> Option<&str> {
        let mut order: Vec<&str> = Vec::new();
        for name in self.list_order() {
            let group = self.group_of(name)?;
            if order.iter().any(|n| self.group_of(n) == Some(group)) {
                continue; // the section already stands in the ring
            }
            order.push(self.shown_of(group)?);
        }
        order.extend(
            self.entries
                .iter()
                .filter(|e| matches!(e.placement, Placement::Main))
                .map(|e| e.name.as_str()),
        );
        if order.len() < 2 {
            return None;
        }
        let focused = self.focused_name();
        let at = order.iter().position(|n| *n == focused)?;
        let next = (at as isize + by).rem_euclid(order.len() as isize) as usize;
        Some(order[next])
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

    /// The section a sidebar pane shares, as a key two names can be compared
    /// on. A name outside [`SECTIONS`] is a section of one, so its own name is
    /// the key; the main region has no section at all.
    fn group_of(&self, name: &str) -> Option<(Option<usize>, &str)> {
        let at = self.position(name)?;
        let entry = &self.entries[at];
        match entry.placement {
            Placement::Sidebar {
                section: Some(s), ..
            } => Some((Some(s), "")),
            Placement::Sidebar { section: None, .. } => Some((None, entry.name.as_str())),
            Placement::Main => None,
        }
    }

    /// How long ago the keyboard sat on a sidebar pane: 0 for the last one,
    /// [`usize::MAX`] for one it has never been on.
    fn recency(&self, name: &str) -> usize {
        self.recent
            .iter()
            .position(|n| n == name)
            .unwrap_or(usize::MAX)
    }

    /// The registered tabs of the section `name` sits in, in draw order —
    /// what `[`/`]` cycle and what a section header lists. Empty for the main
    /// region and for a name nothing registered: neither is a tab.
    pub fn section_tabs(&self, name: &str) -> Vec<&str> {
        let Some(group) = self.group_of(name) else {
            return Vec::new();
        };
        self.list_order()
            .into_iter()
            .filter(|n| self.group_of(n) == Some(group))
            .collect()
    }

    /// Cycles the focus one tab along the focused pane's own section,
    /// wrapping — what `[`/`]` do. `false` when the keyboard is not on a
    /// sidebar list, or when its section has no second registered tab, which
    /// is the honest answer for a `stashes` on its own.
    pub fn cycle_tab(&mut self, by: isize) -> bool {
        let focused = self.focused_name().to_string();
        let tabs: Vec<String> = self
            .section_tabs(&focused)
            .into_iter()
            .map(str::to_string)
            .collect();
        if tabs.len() < 2 {
            return false;
        }
        let at = tabs.iter().position(|n| *n == focused).unwrap_or(0);
        let next = (at as isize + by).rem_euclid(tabs.len() as isize) as usize;
        self.focus_named(&tabs[next])
    }

    /// The tab `group` is showing: the registered member the keyboard sat on
    /// last, earliest registration breaking ties. The one comparison the
    /// layout and the section-focus commands both read, so a header, a click
    /// and a number key agree on what a section shows.
    fn shown_of(&self, group: (Option<usize>, &str)) -> Option<&str> {
        self.list_order()
            .into_iter()
            .filter(|n| self.group_of(n) == Some(group))
            .min_by_key(|n| (self.recency(n), self.position(n)))
    }

    /// The tab `name`'s section is showing — the member the keyboard sat on
    /// last. `None` for the main region and for a name nothing registered:
    /// neither sits in a section. What the section-focus commands (the number
    /// keys' `<name>.focus` names) land on, and what a section header
    /// highlights.
    pub fn shown_tab(&self, name: &str) -> Option<&str> {
        self.shown_of(self.group_of(name)?)
    }

    /// The registry as a [`Layout`] sees it, in registration order.
    ///
    /// Which tab each section shows is resolved here, once per layout rather
    /// than once per row, off the recency list — so the sidebar keeps showing
    /// what it was showing when the diff takes the keyboard. [`Spot::key`] is
    /// left empty — the keymap is the caller's to read, not the registry's.
    pub fn spots(&self) -> Vec<Spot<'_>> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                // The main region is nobody's tab, so it is always shown.
                let shown = match self.group_of(&e.name) {
                    None => true,
                    Some(group) => self.shown_of(group) == Some(e.name.as_str()),
                };
                Spot {
                    name: e.name.as_str(),
                    placement: e.placement,
                    focused: i == self.focused,
                    shown,
                    key: "",
                }
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
        // What a launch does: `register` focuses what it registers, and the
        // restoration is written out — so the open section is the commits'
        // and not whichever tenant happened to be last.
        p.focus_named("commits");
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
        assert_eq!(p.names().count(), 4, "a registration appended a duplicate");
        assert_eq!(p.focused_name(), "ext-b", "register did not focus");

        // Replacing keeps the name where it was, focuses the replacement, and
        // grows nothing.
        assert_eq!(p.register("ext-a", Placement::sidebar("ext-a"), 5), Some(3));
        assert_eq!(p.names().count(), 4);
        assert_eq!(p.position("ext-a"), Some(2), "a replacement moved");
        assert_eq!(*p.get("ext-a").unwrap(), 5);
        assert_eq!(p.focused_name(), "ext-a", "focus did not stay stable");

        // Canonical built-ins before extensions, and Main last: the pane ring
        // closes on the diff.
        assert_eq!(p.list_order(), ["commits", "ext-a", "ext-b"]);
        p.focus_named("ext-b");
        assert_eq!(p.cycle_sections(1), Some("diff"));
    }

    #[test]
    fn h_l_cycle_panes_and_the_sidebar_cycle_wraps() {
        let mut p = full();
        p.focus_named("status");
        // Right cycles the panes — each section standing for the tab it is
        // showing, the main region closing the ring — and wraps.
        for expected in ["files", "branches", "commits", "stashes", "diff", "status"] {
            let name = p.cycle_sections(1).expect("a pane to cycle to").to_string();
            p.focus_named(&name);
            assert_eq!(p.focused_name(), expected);
        }
        // Left cycles back the same ring.
        let name = p.cycle_sections(-1).expect("a pane").to_string();
        p.focus_named(&name);
        assert_eq!(p.focused_name(), "diff");
        let name = p.cycle_sections(-1).expect("a pane").to_string();
        p.focus_named(&name);
        assert_eq!(p.focused_name(), "stashes");

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

        // Two panes: the pane cycle has both, the list cycle has no second
        // list.
        let mut p = Panes::new();
        p.register("commits", Placement::sidebar("commits"), 1);
        p.register("diff", Placement::Main, 2);
        p.focus_named("commits");
        assert_eq!(p.cycle_sections(1), Some("diff"));
        p.focus_named("diff");
        assert_eq!(p.cycle_sections(-1), Some("commits"));
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

            // The sidebar column is one column: every section slice shares x
            // and width, and their heights tile the body. With one registered
            // tab per section here, that is five slices in equal shares —
            // the remainder to the earlier sections — focused or not: no
            // section expands, and the keyboard is told apart by highlight
            // alone.
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
            assert_eq!(
                slices.iter().map(|r| r.height).collect::<Vec<_>>(),
                [5, 5, 4, 4, 4],
                "{width}: the sections did not split the column equally"
            );

            // One header per section, in the same order, each on its slice's
            // one header row — and every slice has content rows under it.
            assert_eq!(g.headers().len(), 5, "{width}: a section lost its header");
            for (h, slice) in g.headers().iter().zip(&slices) {
                assert_eq!(h.rect, slice.header(), "{width}: {h:?}");
                assert_eq!(h.tabs.len(), 1, "{width}: {h:?}");
                assert!(h.tabs[0].active, "{width}: a lone tab is not active");
                assert!(slice.content().height > 0, "{width}: {h:?} drew no rows");
            }
        }
    }

    /// The tab a drawn section is showing, by name.
    fn active(h: &Header) -> Option<&str> {
        h.tabs
            .iter()
            .find(|tab| tab.active)
            .map(|tab| tab.name.as_str())
    }

    /// The eight sidebar lists a repository launch registers, plus the diff —
    /// the shape the sections were invented for.
    fn tabbed() -> Panes<&'static str> {
        let mut p = Panes::new();
        for name in [
            "commits",
            "stashes",
            "remotes",
            "tags",
            "reflog",
            "worktrees",
            "files",
            "branches",
        ] {
            p.register(name, Placement::sidebar(name), name);
        }
        p.register("diff", Placement::Main, "diff");
        p.focus_named("commits");
        p
    }

    #[test]
    fn sections_group_the_lists_and_the_walk_follows_the_column() {
        let p = tabbed();
        // The canonical order is the sections flattened, which is the order
        // the column draws: files then worktrees, branches then remotes then
        // tags, commits then reflog, the stack at the foot. Registration
        // order — commits first, files seventh — does not show through.
        assert_eq!(
            p.list_order(),
            [
                "files",
                "worktrees",
                "branches",
                "remotes",
                "tags",
                "commits",
                "reflog",
                "stashes"
            ]
        );

        // And each name knows the tabs it shares a slot with.
        assert_eq!(p.section_tabs("worktrees"), ["files", "worktrees"]);
        assert_eq!(p.section_tabs("tags"), ["branches", "remotes", "tags"]);
        assert_eq!(p.section_tabs("reflog"), ["commits", "reflog"]);
        assert_eq!(p.section_tabs("stashes"), ["stashes"]);
        // The main region is nobody's tab.
        assert!(p.section_tabs("diff").is_empty());

        // The keyboard is on `commits`, so its section shows it and the
        // others show their first tab — which is what the headers say.
        let g = BuiltinLayout.arrange(
            &p.spots(),
            Rect {
                x: 0,
                y: 1,
                width: 120,
                height: 22,
            },
        );
        let showing: Vec<Option<&str>> = g.headers().iter().map(active).collect();
        assert_eq!(
            showing,
            [
                Some("files"),
                Some("branches"),
                Some("commits"),
                Some("stashes")
            ]
        );
    }

    #[test]
    fn every_section_gets_an_equal_share_and_only_shown_tabs_are_placed() {
        let layout = BuiltinLayout;
        let body = Rect {
            x: 0,
            y: 1,
            width: 120,
            height: 22,
        };
        let mut p = tabbed();
        let g = layout.arrange(&p.spots(), body);

        // Four sections, four headers, four rectangles — one per section, and
        // each is the tab that section is showing. The seven panes behind the
        // shown tabs have no rectangle at all: hidden exactly as the narrow
        // layout hides an unfocused pane.
        assert_eq!(g.headers().len(), 4);
        let placed: Vec<&str> = g.placed().map(|(n, _)| n).collect();
        assert_eq!(placed, ["files", "branches", "commits", "stashes", "diff"]);
        for behind in ["worktrees", "remotes", "tags", "reflog"] {
            assert_eq!(g.rect(behind), None, "{behind} kept a rectangle");
        }

        // Heights: a header row each, and an equal share of the rest — 22
        // rows over four sections is 6, 6, 5, 5, remainder to the earlier
        // sections. The keyboard being in the commits section changes
        // nothing about the arithmetic: focus is highlight, not height.
        assert_eq!(g.rect("files").unwrap().height, 6);
        assert_eq!(g.rect("branches").unwrap().height, 6);
        assert_eq!(g.rect("commits").unwrap().height, 5);
        assert_eq!(g.rect("stashes").unwrap().height, 5);

        // Tabbing along a section moves the shown tab and the rectangle with
        // it, and the section keeps its share.
        assert!(p.cycle_tab(1));
        assert_eq!(p.focused_name(), "reflog");
        let g = layout.arrange(&p.spots(), body);
        assert_eq!(g.rect("commits"), None, "the tab behind kept its rows");
        assert_eq!(g.rect("reflog").unwrap().height, 5);
        assert_eq!(active(&g.headers()[2]), Some("reflog"));
        // It wraps within its own section and never leaves it.
        assert!(p.cycle_tab(1));
        assert_eq!(p.focused_name(), "commits");
        assert!(p.cycle_tab(-1));
        assert_eq!(p.focused_name(), "reflog");
        // A section of one has no second tab, and says so rather than moving.
        p.focus_named("stashes");
        assert!(!p.cycle_tab(1), "a lone tab is not a cycle");
        assert_eq!(p.focused_name(), "stashes");
        // Nor does the main region, which is not a section.
        p.focus_named("reflog");
        p.focus_named("diff");
        assert!(!p.cycle_tab(1));
        assert_eq!(p.focused_name(), "diff");

        // The diff holding the keyboard leaves the sidebar showing what it
        // was showing — `reflog`, in its section's unchanged share.
        let g = layout.arrange(&p.spots(), body);
        assert_eq!(g.rect("reflog").unwrap().height, 5);
        assert_eq!(active(&g.headers()[2]), Some("reflog"));
    }

    #[test]
    fn an_absent_pane_collapses_its_tab_and_an_empty_section_collapses_out() {
        let layout = BuiltinLayout;
        let body = Rect {
            x: 0,
            y: 1,
            width: 120,
            height: 22,
        };
        // A fixture launch: one list and the diff, no repository behind it.
        let mut p = Panes::new();
        p.register("commits", Placement::sidebar("commits"), "commits");
        p.register("diff", Placement::Main, "diff");
        p.focus_named("commits");
        let g = layout.arrange(&p.spots(), body);
        assert_eq!(g.headers().len(), 1, "an empty section drew a header");
        assert_eq!(
            g.headers()[0]
                .tabs
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["commits"],
            "an absent reflog was advertised as a tab"
        );
        assert_eq!(g.rect("commits").unwrap().height, body.height);

        // Half a section: `tags` without `branches` or `remotes` is the whole
        // header, and the two sections split the column equally — 11 and 11.
        let mut p = Panes::new();
        p.register("tags", Placement::sidebar("tags"), "tags");
        p.register("files", Placement::sidebar("files"), "files");
        p.focus_named("tags");
        let g = layout.arrange(&p.spots(), body);
        assert_eq!(g.headers().len(), 2);
        assert_eq!(
            g.headers()[1]
                .tabs
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["tags"]
        );
        assert_eq!(g.rect("tags").unwrap().height, 11);
        assert_eq!(g.rect("files").unwrap().height, 11);
        assert_eq!(p.section_tabs("tags"), ["tags"]);

        // An ungrouped name is a section of its own at the tail, never tabbed
        // behind a built-in it has never heard of.
        let mut p = tabbed();
        p.register("ext", Placement::sidebar("ext"), "ext");
        p.focus_named("commits");
        assert_eq!(p.section_tabs("ext"), ["ext"]);
        let g = layout.arrange(&p.spots(), body);
        assert_eq!(g.headers().len(), 5);
        assert_eq!(
            g.headers()[4]
                .tabs
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["ext"]
        );
    }

    #[test]
    fn a_header_lays_its_tabs_out_where_the_mouse_finds_them() {
        let layout = BuiltinLayout;
        let body = Rect {
            x: 0,
            y: 1,
            width: 120,
            height: 22,
        };
        let p = tabbed();
        // The key is the caller's to supply — the registry does not read the
        // keymap — and the section advertises its *first* drawn tab's.
        let mut spots = p.spots();
        for spot in &mut spots {
            spot.key = match spot.name {
                "files" => "2",
                "branches" => "3",
                "commits" => "4",
                "stashes" => "5",
                _ => "",
            };
        }
        let g = layout.arrange(&spots, body);
        let files = &g.headers()[0];
        assert_eq!(files.key, "2");
        // `  2  files - worktrees`: two pad, the key, two pad, then the tabs
        // three cells apart.
        assert_eq!((files.tabs[0].x, files.tabs[0].width), (5, 5));
        assert_eq!((files.tabs[1].x, files.tabs[1].width), (13, 9));
        assert!(files.tabs[0].active && !files.tabs[1].active);

        // The mouse asks the same table: a click on a tab's word names that
        // tab, and a click on the gap between them names none — the section
        // underneath answers that, through `hit`.
        assert_eq!(g.hit_tab(5, 1), Some("files"));
        assert_eq!(g.hit_tab(9, 1), Some("files"));
        assert_eq!(g.hit_tab(10, 1), None);
        assert_eq!(g.hit_tab(13, 1), Some("worktrees"));
        assert_eq!(g.hit_tab(21, 1), Some("worktrees"));
        assert_eq!(g.hit_tab(22, 1), None);
        assert_eq!(g.hit(10, 1), Some("files"), "the header is its section's");
        // A row inside a section rather than on its header has no tabs on it:
        // row 2 is the files section's content, and the branches header is
        // down at row 7 now that every section keeps its own share.
        assert_eq!(g.hit_tab(5, 2), None);
        assert_eq!(g.hit_tab(5, 6), None);
        assert_eq!(g.hit_tab(5, 7), Some("branches"));
        // And the header a pane's rows sit under is findable by name, which
        // is how the paint knows a section header from a lone pane's.
        assert_eq!(g.header_of("worktrees").map(|h| h.rect.y), Some(1));
        assert_eq!(g.header_of("diff"), None);

        // An unbound section shows no key at all and its tabs start where the
        // key would have been.
        let mut bare = p.spots();
        for spot in &mut bare {
            spot.key = "";
        }
        let g = layout.arrange(&bare, body);
        assert_eq!(g.headers()[0].key, "");
        assert_eq!(g.headers()[0].tabs[0].x, 2);

        // A header narrower than its tabs clips them rather than dropping
        // them: the pen clips the text the same way.
        let narrow = Rect { width: 10, ..body };
        let g = layout.arrange(&spots, narrow);
        let files = &g.headers()[0];
        assert_eq!((files.tabs[0].x, files.tabs[0].width), (5, 5));
        assert_eq!(files.tabs[1].width, 0, "{:?}", files.tabs);
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
