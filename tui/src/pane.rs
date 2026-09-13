//! The pane seam: the operations every tenant of the registry answers.
//!
//! Every tenant — the commit list, the diff, the ref panes, a compiled-in
//! extension — is a `Pane`: a thing the shell can name a mode for, size,
//! draw, ask its status, hand the mouse to, search, and run commands on.
//! What it is *not*: the tenant's data story. Label, generation and the
//! repository read a refresh performs belong to the registry entry that
//! holds the pane — acquisition is `gitten-app`'s, and a pane is drawing
//! and input only.
//!
//! The trait exists because the answer used to be written per variant, ten
//! matches deep, for operations that differ in nothing but the view they
//! land on — the same test that put [`Scrollable`] in `core`. The `view.*`
//! and `search.*` halves are so uniform that [`Pane::run`] ships them as a
//! provided method; a pane writes only [`Pane::verbs`], the commands that
//! are its own.

use crate::screen::Screen;
use gitten_core::host::Host;
use gitten_core::runs::Run;
use gitten_core::view::{run_view_commands, Scrollable};

/// One pane tenant, independent of how the shell lays panes out.
///
/// Built-ins and compiled-in extensions enter through the same object-safe
/// seam — the window's own `Pane` trait is the same shape, for the same
/// reason. Stable naming, placement and focus belong to the registry; what
/// lives here is drawing, input and the commands the pane answers for
/// itself.
///
/// A supertrait of [`Scrollable`] rather than a member returning one: a
/// pane *is* a scrollable list, and upcasting the object is what lets
/// [`run_view_commands`] — the shared `view.*` vocabulary — address it by
/// name.
pub trait Pane: Scrollable {
    /// The scrollable half, as the trait object [`run_view_commands`]
    /// addresses — `self`, on every in-tree pane. A required method rather
    /// than `self` written once here: through `dyn Pane` there is no sized
    /// `Self` left to unsize, so the impl writes the cast on its concrete
    /// type.
    fn scrollable(&mut self) -> &mut dyn Scrollable;

    /// Which mode's bindings are live — the name the keymap and
    /// `gitten.toml` use.
    fn mode(&self) -> &'static str;

    /// How much lead the cursor keeps at the edge. `[view] scrolloff` —
    /// cheap to call every resize, which is how a saved config file reaches
    /// a pane already on screen.
    fn set_scrolloff(&mut self, rows: usize);

    /// A new size — this pane's own content rectangle, never the whole
    /// screen. `host` rides along for the pane whose size decides a reflow
    /// budget — the diff's, which reflows to the pane's width — and is free
    /// to the rest.
    fn resize(&mut self, cols: usize, height: usize, host: &Host);

    /// Paints into `screen`, at `x` of row `y` onward, inside this pane's
    /// own columns. `out` is the diff's run-list buffer; a pane whose rows
    /// are cells, not shaped spans, ignores it.
    fn paint(
        &self,
        screen: &mut Screen,
        x: usize,
        y: usize,
        focused: bool,
        host: &Host,
        out: &mut Vec<Run>,
    );

    /// One line describing where the keyboard is, for the status row.
    fn status(&self, host: &Host) -> String;

    /// The scrollbar at the edge geometry hands it. The pane does not
    /// choose.
    fn paint_bar(
        &self,
        screen: &mut Screen,
        x: usize,
        divider: Option<usize>,
        y: usize,
        host: &Host,
    );

    /// A press in the list, in this pane's own coordinates — the count and
    /// the modifier arrive as scalars rather than as an event type, which
    /// is what keeps the views free of `term`. `clicks` counts the press: a
    /// double-click selects a word where a word is a thing to select, and
    /// a list that has none ignores it.
    fn press(&mut self, col: usize, row: usize, clicks: u8, extend: bool, host: &Host);

    /// The pointer moved with the button down. `row` is signed: a row
    /// above the pane is negative and scrolls it. Nothing by default — a
    /// list with no drag selection and an indicator bar has nothing a held
    /// button can do.
    fn drag(&mut self, _col: usize, _row: isize, _host: &Host) {}

    /// The button released. Nothing by default — see `drag`.
    fn release(&mut self) {}

    /// What `copy.selection` copies here — the selection, or the row the
    /// cursor is on when there is none. The empty answer skips the
    /// clipboard entirely.
    fn copy_text(&self) -> String {
        String::new()
    }

    /// What the *mouse* has selected, and nothing else — the empty answer
    /// is what lets copy-on-select tell a gesture that selected something
    /// from one that only moved the cursor.
    fn selection(&self) -> String {
        String::new()
    }

    /// `select.all` — a no-op on a pane with nothing to select is still an
    /// answer, the same one the commit graph gives.
    fn select_all(&mut self) {}

    /// `select.none`. `true` says a selection was standing and is gone —
    /// the difference the status line reports between "cleared" and "there
    /// was nothing".
    fn select_none(&mut self) -> bool {
        false
    }

    // -------------------------------------------------------------- search

    /// The standing query — `None` while the pane is whole. What puts the
    /// `search` mode on the stack, and what a second `/` seeds from.
    fn search_query(&self) -> Option<&str> {
        None
    }

    /// Whether a search stands in this pane — the filter a prompt left
    /// behind, or the diff's standing query. `n`/`N` and the clearing `esc`
    /// hang off this.
    fn search_standing(&self) -> bool {
        self.search_query().is_some()
    }

    /// The live count while a search stands — `15/30` hits over loaded on a
    /// list, `3/12` matches on the diff. `None` when there is no note to
    /// draw.
    fn search_note(&self) -> Option<String> {
        None
    }

    /// Whether the pane answers the `search.*` vocabulary at all —
    /// capability, not state: a pane with no indexable text refuses the
    /// verbs rather than consuming them to no effect, which is the honest
    /// answer a `run` should give for a command it cannot honor.
    fn searchable(&self) -> bool {
        true
    }

    /// What `*.search` opens with, or the refusal the status line says.
    /// `command` is the verb's own name, for the refusal's sentence — a
    /// pane answers with a whole message because the two refusals in the
    /// tree ("not supported here", "no text to search") are worded
    /// differently.
    fn search_seed(&self, command: &str) -> Result<String, String> {
        if !self.searchable() {
            return Err(format!("{command} is not supported here"));
        }
        Ok(self.search_query().unwrap_or_default().to_string())
    }

    /// A live query change — once per keystroke, and never anywhere else.
    /// A list filters; the diff walks to its matches, because a filtered
    /// diff is not a diff. Nothing by default: a pane with no search has
    /// no edit to answer.
    fn search_edit(&mut self, _query: &str) {}

    /// The search off — restoring whatever stood before it: the unfiltered
    /// list, or the diff without its standing query.
    fn search_clear(&mut self) {}

    /// The next — or previous — match of the standing search.
    fn search_next(&mut self, _by: isize) {}

    // ---------------------------------------------------------- the account

    /// A degraded read's account, drained — the warning a refresh parked on
    /// the pane until the whole wave landed. `None` for a pane with nothing
    /// to say, and for every pane that never degrades.
    fn take_warning(&mut self) -> Option<String> {
        None
    }

    // --------------------------------------------------------- the commands

    /// The pane's own verbs — the `foo.*` commands past the shared
    /// `view.*`/`search.*` vocabulary. `false` is "not one of mine".
    fn verbs(&mut self, _command: &str, _host: &Host) -> bool {
        false
    }

    /// Runs a command, or says it does not know it.
    ///
    /// The `view.*` half is [`run_view_commands`], and `search.next` /
    /// `search.prev` are the same answer for every searchable pane — the
    /// same list for all of them is what makes them bindable in
    /// `gitten_core::command::GLOBAL`: a key that scrolls one list scrolls
    /// every list, and nothing had to say so twice. Everything else is the
    /// pane's own, asked through [`verbs`](Self::verbs).
    ///
    /// The pane moves are *not* here — they are the shell's, answered from
    /// the registry before this is ever asked, because a pane command aimed
    /// at a view would be a pane command that stops working the day a
    /// second list registers.
    fn run(&mut self, command: &str, host: &Host) -> bool {
        if run_view_commands(self.scrollable(), command, host.view.rows) {
            return true;
        }
        if !self.searchable() {
            return self.verbs(command, host);
        }
        match command {
            "search.next" => {
                self.search_next(1);
                true
            }
            "search.prev" => {
                self.search_next(-1);
                true
            }
            _ => self.verbs(command, host),
        }
    }
}
