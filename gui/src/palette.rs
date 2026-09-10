//! The Commands palette: every named command, filterable.
//!
//! The toolbar button and cmd-k open it; it lists the registry — one entry
//! per name with its doc line — so a command an extension registers appears
//! here with no line changing anywhere else. Typing narrows by substring
//! over name and doc; arrows move, Enter or a click runs through the one
//! named dispatch, Esc leaves and hands focus back to the files pane. A
//! name nothing here implements answers "not supported here" through the
//! dispatch's own fallback — said, not swallowed. The help overlay used to
//! stand here; a native window lists commands instead of describing keys.

use crate::{config, input, modal};
use gitten_core::command::Command;
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
    pub doc: String,
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

/// Registry order kept — the order `[keys]` and the terminal already agree
/// on.
pub(crate) fn filtered<'a>(commands: &'a [Command], query: &str) -> Vec<&'a Command> {
    let q = query.trim().to_lowercase();
    commands
        .iter()
        .filter(|c| {
            // Runnable check needs the registry; the caller filters those.
            // Here: the query only.
            q.is_empty() || c.name.to_lowercase().contains(&q) || c.doc.to_lowercase().contains(&q)
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
    /// the same restoration every dialog in this window keeps.
    pub(crate) fn close_palette(&mut self, cx: &mut Context<Self>) {
        self.palette_open = false;
        self.palette_sel = 0;
        self.focus_named("files", cx);
        cx.notify();
    }

    /// The registry rows under the current query, in registry order.
    pub(crate) fn palette_rows(&self, cx: &App) -> Vec<Row> {
        let host = config::host(cx);
        filtered(host.commands.all(), &self.palette_query)
            .into_iter()
            .map(|c| Row {
                name: c.name.clone(),
                doc: c.doc.clone(),
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

    /// Run the selected row through the one named dispatch, then leave the
    /// way Esc would have — a run is one decision and not two.
    pub(crate) fn run_palette_selection(&mut self, cx: &mut Context<Self>) {
        let rows = self.palette_rows(cx);
        let Some(name) = rows.get(self.palette_sel).map(|row| row.name.clone()) else {
            return;
        };
        self.palette_open = false;
        self.palette_sel = 0;
        self.run_command(&name, cx);
        self.focus_named("files", cx);
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
                                    .text_color(rgb(match i == sel {
                                        true => c.fg,
                                        false => c.accent,
                                    }))
                                    .child(SharedString::from(name)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(rgb(c.dim))
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
    use gitten_core::command::{Command, Keymap};

    fn command(name: &str, doc: &str) -> Command {
        Command {
            name: name.into(),
            doc: doc.into(),
            hint: None,
        }
    }

    #[test]
    fn an_empty_query_keeps_registry_order() {
        let all = vec![
            command("repo.push", "send the current branch"),
            command("files.stage", "stage the file"),
        ];
        let out = filtered(&all, "");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "repo.push");
    }

    #[test]
    fn a_query_matches_name_or_doc_case_insensitively() {
        let all = vec![
            command("repo.push", "send the current branch"),
            command("files.stage", "stage the file"),
        ];
        assert_eq!(filtered(&all, "PUSH").len(), 1);
        assert_eq!(filtered(&all, "stage").len(), 1);
        assert_eq!(filtered(&all, "branch").len(), 1);
        assert!(filtered(&all, "zzz").is_empty());
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
