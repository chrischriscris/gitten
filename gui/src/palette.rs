//! The Commands palette: every named command, filterable.
//!
//! The toolbar button and cmd-k open it; it lists the registry — one entry
//! per name with its doc line — so a command an extension registers appears
//! here with no line changing anywhere else. Typing narrows by substring
//! over name and doc; arrows move, Enter or a click runs through the one
//! named dispatch, Esc leaves and hands focus back to the files pane.
//!
//! Only what the window runs is listed: the rows pass through the shell's
//! strict [`Availability`](gitten_core::command::Availability), so a name
//! nothing here answers has no row, and one this launch turns away (a
//! fixture has no repository to write to) keeps its row and says why where
//! the doc line was. The dispatch reads the same map, so choosing a
//! disabled row says that same sentence in the band. The help overlay used
//! to stand here; a native window lists commands instead of describing
//! keys.

use crate::{config, input, modal};
use gitten_core::command::{Availability, Command, Usable};
use gpui::*;

/// Rows drawn before the list stops: enough to read, few enough to stay a
/// panel. Typing narrows far faster than scrolling would.
pub(crate) const VISIBLE_ROWS: usize = 14;

/// One row: the command, its doc line, and the key the shared map binds it
/// to.
///
/// The key comes from the live [`Keymap`](gitten_core::command::Keymap)
/// rather than a table here, so a `[keys]` rebinding rewrites the column and
/// an extension's own binding appears in it without this file changing. The
/// platform's chords are not in the shared map — the platform modifier never
/// reaches `command::Key` — so they are absent from the column for the same
/// reason they are absent from every client's help: each window advertises
/// its own in its chrome, where this one names `cmd-k` and `cmd-enter`.
pub(crate) struct Row {
    pub name: String,
    /// The doc line, or for a disabled row the reason it will not run.
    pub doc: String,
    /// Listed but turned away by this launch; drawn quieter, and choosing it
    /// says [`doc`](Self::doc) in the band.
    pub disabled: bool,
    /// Empty for a command nothing is bound to — a name reached only from
    /// here, which is not a gap to paper over with a guess.
    pub keys: String,
}

/// The keys to name on a row: the map's first binding for the command, with
/// the pointer's two pseudo-chords left out. A keycap is a key — `wheelup`
/// is a gesture the map spells as a chord so a wheel event can resolve to a
/// command, and reading it as a key would send someone looking for it.
pub(crate) fn key_column(keys: &[String]) -> String {
    keys.iter()
        .find(|key| !matches!(key.as_str(), "wheeldown" | "wheelup"))
        .cloned()
        .unwrap_or_default()
}

/// The rows the window can show: registry order kept — the order `[keys]`
/// and the terminal already agree on — with every name `availability` calls
/// unsupported left out, and a disabled one's reason standing where its doc
/// line was. The query matches what the row says, so a disabled row is
/// found by its reason as well as by its name.
pub(crate) fn filtered<'a>(
    commands: &'a [Command],
    availability: &'a Availability,
    query: &str,
) -> Vec<(&'a Command, Option<&'a str>)> {
    let q = query.trim().to_lowercase();
    commands
        .iter()
        .filter_map(|c| match availability.state(&c.name) {
            Usable::Available => Some((c, None)),
            Usable::Disabled(reason) => Some((c, Some(reason.as_str()))),
            Usable::Unsupported => None,
        })
        .filter(|(c, reason)| {
            let doc = reason.unwrap_or(&c.doc);
            q.is_empty() || c.name.to_lowercase().contains(&q) || doc.to_lowercase().contains(&q)
        })
        .collect()
}

impl crate::DevShell {
    /// Open the palette: fresh query, selection on top, keyboard in the
    /// filter field. The field entity is built once and reused — rebuilding
    /// per open would drop the subscription that mirrors its text.
    pub(crate) fn open_palette(&mut self, cx: &mut Context<Self>) {
        if self.palette_field.is_none() {
            let field = cx.new(|cx| input::Input::new("", "Search commands…", "", cx));
            // Embedded: the reference's rounded search box. The footer names
            // the keys, so the field draws no exits of its own.
            field.update(cx, |field, _| field.set_embedded());
            let sub = cx.subscribe(&field, |this: &mut Self, _, event, cx| {
                if let input::Event::Edited(text) = event {
                    this.palette_query = text.clone();
                    this.palette_sel = 0;
                    cx.notify();
                }
            });
            self.palette_field = Some(field);
            self.palette_sub = Some(sub);
        }
        if let Some(field) = self.palette_field.clone() {
            field.update(cx, |field, cx| {
                field.set_text(String::new(), cx);
            });
        }
        self.palette_query.clear();
        self.palette_sel = 0;
        self.palette_open = true;
        cx.notify();
    }

    /// Close the palette and hand the keyboard back to the files pane —
    /// the same restoration every dialog in this window keeps — or, where
    /// the launch has no files pane (a patch, a fixture), to the window
    /// itself, without a "no files pane" sentence nobody asked for.
    pub(crate) fn close_palette(&mut self, cx: &mut Context<Self>) {
        self.palette_open = false;
        self.palette_sel = 0;
        match self.panes.position("files") {
            Some(_) => self.focus_named("files", cx),
            None => self.reclaim_focus(),
        }
        cx.notify();
    }

    /// The registry rows under the current query, in registry order.
    pub(crate) fn palette_rows(&self, cx: &App) -> Vec<Row> {
        let host = config::host(cx);
        let availability = self.availability();
        filtered(host.commands.all(), &availability, &self.palette_query)
            .into_iter()
            .map(|(c, reason)| Row {
                name: c.name.clone(),
                doc: reason.unwrap_or(&c.doc).to_string(),
                disabled: reason.is_some(),
                keys: key_column(&host.keys.keys_for(&c.name)),
            })
            .collect()
    }

    pub(crate) fn palette_step(&mut self, by: isize, cx: &mut Context<Self>) {
        let count = self.palette_rows(cx).len();
        if count == 0 {
            return;
        }
        let next = (self.palette_sel as isize + by).clamp(0, count as isize - 1) as usize;
        if next != self.palette_sel {
            self.palette_sel = next;
            cx.notify();
        }
    }

    /// Run the selected row through the one named dispatch and leave — a run
    /// is one decision and not two.
    ///
    /// The keyboard goes where the command put it, not back to the files
    /// pane: `workspace.history` from here used to land on History and be
    /// walked straight back to Changes by that refocus, and over a launch
    /// with no files pane the refocus's own "no files pane" overwrote the
    /// sentence the command had just said. Only the filter's focus record is
    /// cleared, so the window and not a vanished field holds the keys.
    pub(crate) fn run_palette_selection(&mut self, cx: &mut Context<Self>) {
        let rows = self.palette_rows(cx);
        let Some(name) = rows.get(self.palette_sel).map(|row| row.name.clone()) else {
            return;
        };
        self.palette_open = false;
        self.palette_sel = 0;
        self.reclaim_focus();
        self.run_command(&name, cx);
    }

    pub(crate) fn render_palette(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let host = config::host(cx);
        // The filter owns the keyboard while the palette stands: focusing
        // on every render is idempotent and keeps typing alive across
        // re-renders without tracking who focused what.
        let c = &host.theme.chrome;
        if let Some(field) = self.palette_field.clone() {
            window.focus(&field.read(cx).focus_handle(), cx);
        }
        let rows = self.palette_rows(cx);
        let visible: Vec<_> = rows.iter().take(VISIBLE_ROWS).collect();
        let sel = self.palette_sel.min(visible.len().saturating_sub(1));
        let me = cx.entity().downgrade();
        let close = modal::close_button(&host, "palette-close")
            .on_click({
                let me = me.clone();
                move |_, _, cx| {
                    _ = me.update(cx, |this, cx| this.close_palette(cx));
                }
            })
            .into_any_element();
        modal::centered(
            &host,
            modal::Width::Exact(560.0),
            vec![
                modal::heading(&host, "Commands", Some(close)).into_any_element(),
                self.palette_field
                    .clone()
                    .map(|f| f.into_any_element())
                    .unwrap_or_else(|| div().into_any_element()),
                div().h(px(10.0)).flex_none().into_any_element(),
                div()
                    .flex()
                    .flex_col()
                    .children(visible.iter().enumerate().map(|(i, row)| {
                        let me = me.clone();
                        let (name, doc, keys) =
                            (row.name.clone(), row.doc.clone(), row.keys.clone());
                        // A disabled row reads as furniture: the name in the
                        // doc's ink, the reason in the quiet one, so the
                        // runnable rows around it keep the accent to
                        // themselves.
                        let disabled = row.disabled;
                        div()
                            .id(SharedString::from(format!("palette-row-{i}")))
                            .flex()
                            .items_start()
                            .gap_2()
                            .py(px(10.0))
                            .px_2()
                            .rounded(px(5.0))
                            .bg(rgb(match i == sel {
                                true => c.selection_bg,
                                false => c.bg,
                            }))
                            .cursor_pointer()
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(rgb(match (i == sel, disabled) {
                                        (true, _) => c.fg,
                                        (false, true) => c.dim,
                                        (false, false) => c.accent,
                                    }))
                                    .child(SharedString::from(name)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(rgb(match disabled {
                                        true => host.theme.quiet_on(c.bg),
                                        false => c.dim,
                                    }))
                                    .child(SharedString::from(doc)),
                            )
                            // The key, at the row's end, in the same ink as
                            // every other label glance-read rather than read:
                            // raw `faint` is under the furniture floor here,
                            // so it resolves against the row it lands on.
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(rgb(host.theme.quiet_on(c.bg)))
                                    .child(SharedString::from(keys)),
                            )
                            .on_click(move |_, _, cx| {
                                _ = me.update(cx, |this, cx| {
                                    this.palette_sel = i;
                                    this.run_palette_selection(cx);
                                });
                            })
                    }))
                    .into_any_element(),
                modal::hint(&host, "type to filter · ↑↓ move · enter runs · esc leaves"),
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{filtered, key_column};
    use gitten_core::command::{Availability, Command, Keymap};

    fn command(name: &str, doc: &str) -> Command {
        Command {
            name: name.into(),
            doc: doc.into(),
            hint: None,
            mutates: false,
        }
    }

    #[test]
    fn an_empty_query_keeps_registry_order() {
        let all = vec![
            command("repo.push", "send the current branch"),
            command("files.stage", "stage the file"),
        ];
        let a = Availability::lenient();
        let out = filtered(&all, &a, "");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0.name, "repo.push");
    }

    #[test]
    fn a_query_matches_name_or_doc_case_insensitively() {
        let all = vec![
            command("repo.push", "send the current branch"),
            command("files.stage", "stage the file"),
        ];
        let a = Availability::lenient();
        assert_eq!(filtered(&all, &a, "PUSH").len(), 1);
        assert_eq!(filtered(&all, &a, "stage").len(), 1);
        assert_eq!(filtered(&all, &a, "branch").len(), 1);
        assert!(filtered(&all, &a, "zzz").is_empty());
    }

    /// Strict: a name the window does not answer has no row at all, and a
    /// disabled one keeps its row with the reason where the doc was — which
    /// is also what the query reads.
    #[test]
    fn only_what_runs_is_listed_and_a_disabled_row_says_why() {
        let all = vec![
            command("repo.push", "send the current branch"),
            command("files.stage", "stage the file"),
            command("remotes.fetch", "fetch the remote"),
        ];
        let mut a = Availability::strict();
        a.available(["files.stage"]);
        a.disabled("repo.push", "a fixture has no repository to sync");
        let out = filtered(&all, &a, "");
        let names: Vec<_> = out.iter().map(|(c, _)| c.name.as_str()).collect();
        assert_eq!(names, ["repo.push", "files.stage"]);
        assert_eq!(out[0].1, Some("a fixture has no repository to sync"));
        assert_eq!(out[1].1, None);
        assert_eq!(filtered(&all, &a, "fixture").len(), 1);
        assert!(filtered(&all, &a, "send the current").is_empty());
    }

    /// The column is the live map's answer and not a table beside it: a
    /// rebinding is what the row names on the next frame, and an extension's
    /// own binding arrives with no line of this file changing.
    #[test]
    fn the_key_column_follows_the_live_keymap() {
        let builtin = Keymap::builtin();
        assert_eq!(key_column(&builtin.keys_for("view.down")), "j");
        let mut rebound = Keymap::empty();
        rebound
            .bind("global", "n", "view.down")
            .expect("n is unclaimed");
        assert_eq!(key_column(&rebound.keys_for("view.down")), "n");
        // A name nothing binds says so by leaving the column empty, which is
        // the honest answer and not a gap to fill with a guess.
        assert_eq!(key_column(&rebound.keys_for("nothing.binds.this")), "");
    }

    /// The pointer's two pseudo-chords are gestures the map spells as chords
    /// so a wheel event can resolve to a command. A row that named
    /// `wheeldown` would send someone to the keyboard looking for it.
    #[test]
    fn the_key_column_never_names_a_wheel() {
        assert_eq!(key_column(&["wheeldown".into(), "ctrl-e".into()]), "ctrl-e");
        assert_eq!(key_column(&["wheelup".into()]), "");
    }
}
