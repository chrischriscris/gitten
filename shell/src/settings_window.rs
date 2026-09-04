//! The settings window: every knob, on its own surface.
//!
//! `,` opens a second OS window — "gitten — Settings" — instead of standing an
//! overlay over the main one. A window, because the surface earned one: search
//! across sections, a section sidebar that stays put, and one line of teaching
//! per row do not fit a 560px overlay without becoming a scroll maze. What the
//! window is *not* is a second client: it draws from the same [`Host`] global
//! and every knob turns through the main shell's [`DevShell::settings_apply`],
//! so there is still one implementation of each setting — the window is
//! drawing and input, like every client must be.
//!
//! The keyboard answer, because a window with no keymap would be a mouse
//! surface: the window resolves against [`settings::MODE`] alone — the same
//! mode the overlay resolved against, so the shipped bindings and any
//! extension's all mean what they mean in the main window — plus
//! [`input::MODE`] while the search field holds focus. That is the whole key
//! context: a fixed set, not the full stack, because the stack's lower modes
//! name panes this window does not have. `esc` and `,` close; closing returns
//! focus to the main window, which never stopped living underneath.
//!
//! Reuse, not duplication: opening while open activates the window instead,
//! tracked in the [`Open`] global. Closing the main window while settings
//! stand leaves the app alive on the settings window — an edge, not a crash:
//! the main entity outlives its window and the knobs keep turning.
//!
//! The rows themselves stay in [`settings`]: built from the registries, live
//! values, one line of teaching each. This file holds the surface — the
//! filter, the selection, the window-local dispatch — and nothing about what
//! a knob does.

use crate::chrome::{gap_m, RADIUS, ROW_BAR};
use crate::{config, dispatch, input, settings};
use gitten_core::command::{chord_string, Key, Modes, Resolve};
use gitten_core::theme::Surface;
use gpui::*;
use gpui_component::Root;

/// The settings window's handle, for reuse-instead-of-duplicate. `None`
/// until the first open; a closed window fails its update, which is the
/// signal to open a new one rather than state to clear.
#[derive(Default)]
pub(crate) struct Open(pub Option<WindowHandle<Root>>);
impl Global for Open {}

/// Open the settings window over `main`, or activate it if it stands.
/// Every door — `,`, the gear, the menu, `cmd-,` — arrives through the one
/// `settings` command, so this is the one opener and there is not a second.
pub(crate) fn open(main: Entity<crate::DevShell>, cx: &mut App) {
    if activate(cx) {
        return;
    }
    // Opening draws synchronously, and the first draw reads the main shell —
    // which is still on the stack when this arrives through `run_command`.
    // Defer past it, the way Zed defers past the workspace.
    cx.defer(move |cx| {
        // Two fast presses queue two defers; the first opens, the second
        // must activate instead of duplicating.
        if activate(cx) {
            return;
        }
        let mut options = crate::window_options("gitten — Settings".into());
        options.window_bounds = Some(WindowBounds::centered(size(px(740.0), px(560.0)), cx));
        if let Ok(handle) = cx.open_window(options, |window, cx| {
            let win = cx.new(|cx| SettingsWindow::new(main.clone(), window, cx));
            window.focus(&win.read(cx).search_focus(cx), cx);
            cx.new(|cx| Root::new(win, window, cx))
        }) {
            cx.set_global(Open(Some(handle)));
        }
    });
}

/// Activate the standing settings window, if there is one. A closed window
/// fails its update, which is the signal to open new rather than state to
/// clear.
fn activate(cx: &mut App) -> bool {
    cx.global::<Open>().0.is_some_and(|handle| {
        handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    })
}

/// What the flat selection names: a registry row, or the file fallback.
/// The fallback is last, past every row — a stale index lands on it rather
/// than on a neighbouring knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Item {
    Row { s: usize, r: usize },
    File,
}

/// The flat address space the selection moves in: every row of every
/// section, then the file fallback. One past the end of [`settings::at`].
pub(crate) fn flat_len(sections: &[settings::Section]) -> usize {
    settings::len(sections) + 1
}

/// The item a flat index names. `None` past the fallback.
pub(crate) fn flat_at(sections: &[settings::Section], index: usize) -> Option<Item> {
    match settings::at(sections, index) {
        Some((s, r)) => Some(Item::Row { s, r }),
        None if index == settings::len(sections) => Some(Item::File),
        None => None,
    }
}

/// Whether a row survives the filter. Every query word must appear in the
/// label or the value; a section title match keeps the whole section and is
/// decided by the caller, not here.
pub(crate) fn matches(query: &str, label: &str, value: &str) -> bool {
    // Lowercased here, not by the caller: case never distinguishes a knob.
    let lowered = query.to_lowercase();
    lowered
        .split_whitespace()
        .all(|word| label.to_lowercase().contains(word) || value.to_lowercase().contains(word))
}

pub(crate) struct SettingsWindow {
    main: Entity<crate::DevShell>,
    search: Entity<input::Input>,
    nav: FocusHandle,
    scroll: ScrollHandle,
    query: String,
    sel: usize,
    focused: Option<FocusHandle>,
    pending: Vec<Vec<Key>>,
    footer: Option<String>,
}

impl SettingsWindow {
    fn new(main: Entity<crate::DevShell>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| input::Input::new("filter", "Filter settings…", "", cx));
        // The field speaks its exits the way every prompt's does: resolved
        // once, here, against the input mode it will run under.
        let mut modes = Modes::new();
        modes.push(input::MODE);
        let host = config::host(cx);
        let accept = host
            .keys
            .live_keys_for("input.accept", &modes)
            .into_iter()
            .next();
        let cancel = host
            .keys
            .live_keys_for("input.cancel", &modes)
            .into_iter()
            .next();
        search.update(cx, |field, _| field.set_exits(accept, cancel));
        // Detached, not stored: the feed lives exactly as long as the
        // window does, and there is no close-the-field-keep-the-window
        // state for an unsubscribe to name.
        cx.subscribe(&search, |this: &mut Self, _, event: &input::Event, cx| {
            // Reopening starts at the top: the rows are a different set
            // every keystroke, and an offset the last query left is a
            // promise about rows that no longer exist.
            if let input::Event::Edited(text) = event {
                this.query = text.clone();
                this.sel = 0;
                cx.notify();
            }
        })
        .detach();
        Self {
            main,
            search,
            nav: cx.focus_handle(),
            scroll: ScrollHandle::default(),
            query: String::new(),
            sel: 0,
            focused: None,
            pending: Vec::new(),
            footer: None,
        }
    }

    fn search_focus(&self, cx: &App) -> FocusHandle {
        self.search.read(cx).focus_handle()
    }

    /// The sections with the filter applied: whole sections whose title
    /// matches, matching rows elsewhere, nothing else. Order kept — the
    /// sidebar must not reshuffle under typing fingers.
    fn filtered(&self, sections: &[settings::Section]) -> Vec<settings::Section> {
        let query = self.query.trim().to_lowercase();
        if query.is_empty() {
            return sections.to_vec();
        }
        sections
            .iter()
            .filter_map(|section| {
                if section.title.to_lowercase().contains(&query) {
                    return Some(section.clone());
                }
                let rows = section
                    .rows
                    .iter()
                    .filter(|row| matches(&query, row.label, &row.value))
                    .cloned()
                    .collect::<Vec<_>>();
                (!rows.is_empty()).then_some(settings::Section {
                    title: section.title,
                    rows,
                })
            })
            .collect()
    }

    /// The selection, clamped into what the filter leaves. A stale index is
    /// an unmoved one, never a neighbouring knob.
    fn clamp(&mut self, filtered: &[settings::Section]) {
        self.sel = self.sel.min(flat_len(filtered).saturating_sub(1));
    }

    fn step(&mut self, filtered: &[settings::Section], by: isize, cx: &mut Context<Self>) {
        let count = flat_len(filtered);
        if count == 0 {
            return;
        }
        let next = (self.sel as isize + by).clamp(0, count as isize - 1) as usize;
        if next != self.sel {
            self.sel = next;
            self.footer = None;
            cx.notify();
        }
    }

    fn jump(&mut self, filtered: &[settings::Section], bottom: bool, cx: &mut Context<Self>) {
        let count = flat_len(filtered);
        if count == 0 {
            return;
        }
        self.sel = match bottom {
            true => count - 1,
            false => 0,
        };
        self.footer = None;
        cx.notify();
    }

    /// Turns the selected item by `dir`: a choice cycles, a number steps, a
    /// switch flips — through the main shell's apply, so the live route and
    /// the file write are the overlay's old ones, not a second copy. The
    /// file fallback opens `gitten.toml` in `$EDITOR` instead.
    fn adjust(&mut self, filtered: &[settings::Section], dir: i32, cx: &mut Context<Self>) {
        match flat_at(filtered, self.sel) {
            Some(Item::Row { s, r }) => {
                let row = &filtered[s].rows[r];
                if !row.enabled {
                    self.footer = Some("set by the repository — not here".to_string());
                    cx.notify();
                    return;
                }
                let setting = row.setting;
                self.main.update(cx, |main, cx| {
                    main.settings_apply(setting, dir, cx);
                });
                self.footer = None;
            }
            Some(Item::File) => match self.main.read(cx).open_config_in_editor() {
                Ok(()) => self.footer = None,
                Err(error) => self.footer = Some(error.to_string()),
            },
            None => {}
        }
        cx.notify();
    }

    fn close(window: &mut Window) {
        window.remove_window();
    }

    /// One named command, run. The fixed context: the settings mode's verbs
    /// that name rows, the input mode's two exits while the field holds
    /// focus, and the way out. Anything else the map resolves is either the
    /// help this window cannot show — said, not swallowed — or a binding a
    /// future map added, which this fixed context honestly does not speak.
    fn dispatch(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let sections = self.main.read(cx).settings_sections(cx);
        let filtered = self.filtered(&sections);
        self.clamp(&filtered);
        match name {
            "view.down" => self.step(&filtered, 1, cx),
            "view.up" => self.step(&filtered, -1, cx),
            "view.top" => self.jump(&filtered, false, cx),
            "view.bottom" => self.jump(&filtered, true, cx),
            "view.left" => self.adjust(&filtered, -1, cx),
            "view.right" => self.adjust(&filtered, 1, cx),
            "settings.apply" => self.adjust(&filtered, 1, cx),
            "back" | "settings" | "input.cancel" => Self::close(window),
            // The field's enter is not a commit: the filter stays, the rows
            // take the keyboard. Focus, not state — the query survives.
            "input.accept" => window.focus(&self.nav, cx),
            "help" => {
                self.footer = Some("the keymap lives in the main window".to_string());
                cx.notify();
            }
            _ => {}
        }
    }

    /// One keypress, wherever it landed in this window. Translation,
    /// resolution, dispatch — the main shell's pipeline, narrowed to the two
    /// modes this window speaks. Anything consumed stops propagation, because
    /// the alternative is a meaning firing in the main window behind it.
    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // A pending chord is a promise about where the keyboard is; a focus
        // change breaks it. Cheap to check, never wrong.
        let now_focused = window.focused(cx);
        if self.focused != now_focused {
            self.focused = now_focused;
            self.pending.clear();
        }
        let candidates = dispatch::translate(&ev.keystroke);
        if candidates.is_empty() {
            return;
        }
        let host = config::host(cx);
        let in_field = self.focused == Some(self.search.read(cx).focus_handle());
        self.pending.push(candidates);
        // One candidate list per press, handed over whole: which spelling runs
        // is the map's decision, made against the chord at once.
        let typed: Vec<&[Key]> = self.pending.iter().map(Vec::as_slice).collect();
        // While the field holds focus it owns the keyboard the way a native
        // field does: resolved against the input mode alone, so a `j` types
        // instead of moving the rows. Anywhere else, the settings mode alone.
        let resolved = match in_field {
            true => host.keys.resolve_mode_any(input::MODE, &typed),
            false => host.keys.resolve_mode_any(settings::MODE, &typed),
        };
        match resolved {
            Resolve::Pending => {}
            Resolve::Run(name) => {
                let name = name.to_string();
                self.pending.clear();
                self.footer = None;
                cx.stop_propagation();
                cx.notify();
                self.dispatch(&name, window, cx);
                return;
            }
            Resolve::None => {
                if in_field {
                    // Not an app command in this mode, so it is text-field
                    // mechanics or text for the platform input handler. Let it
                    // continue down the focus path untouched.
                    self.pending.clear();
                    return;
                }
                // Named by the spellings as they were typed — the insert when
                // there was one, the key underneath when there was not.
                let shown: Vec<Key> = self.pending.iter().map(|c| c[0]).collect();
                let unknown = chord_string(&shown);
                self.pending.clear();
                // Said, not swallowed: a key that does nothing and a key that
                // is not bound look identical, and only one of them is worth
                // opening `?` about.
                self.footer = Some(format!("{unknown} is not bound"));
            }
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = config::host(cx);
        let c = &host.theme.chrome;
        let sections = self.main.read(cx).settings_sections(cx);
        let filtered = self.filtered(&sections);
        self.clamp(&filtered);
        let me = cx.entity().downgrade();
        let total_rows = settings::len(&sections);

        // Sidebar entries: the filtered sections, then the file fallback.
        let mut flat = 0;
        let side = filtered
            .iter()
            .enumerate()
            .map(|(s, section)| {
                let start = flat;
                flat += section.rows.len();
                let count = section.rows.len();
                let full = sections
                    .iter()
                    .find(|full| full.title == section.title)
                    .map(|full| full.rows.len())
                    .unwrap_or(count);
                let active = match flat_at(&filtered, self.sel) {
                    Some(Item::Row { s: at, .. }) => at == s,
                    _ => false,
                };
                let me = me.clone();
                div()
                    .id(SharedString::from(format!("settings-sec-{s}")))
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(24.0))
                    .px_2()
                    .rounded(px(RADIUS))
                    .bg(rgb(match active {
                        true => c.selection_bg,
                        false => c.title_bg,
                    }))
                    .cursor_pointer()
                    .child(
                        div()
                            .text_color(rgb(match active {
                                true => c.fg,
                                false => c.dim,
                            }))
                            .child(section.title),
                    )
                    .child(
                        div()
                            .font_family(host.font.family.clone())
                            .text_color(rgb(c.faint))
                            .child(SharedString::from(match count == full {
                                true => format!("{count}"),
                                false => format!("{count}/{full}"),
                            })),
                    )
                    .on_click(move |_, _, cx| {
                        _ = me.update(cx, |this, cx| {
                            this.sel = start;
                            this.footer = None;
                            cx.notify();
                        });
                    })
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        // Rows under their headings, then the file fallback row.
        let mut index = 0;
        let mut body = filtered
            .iter()
            .flat_map(|section| {
                let heading = div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .h(px(24.0))
                    .pt(gap_m(&host.font))
                    .text_color(rgb(c.accent))
                    .child(section.title)
                    .into_any_element();
                let rows = section.rows.iter().map(|row| {
                    let i = index;
                    index += 1;
                    let selected = i == self.sel;
                    let me = me.clone();
                    let adjust = me.clone();
                    let value = row.value.clone();
                    div()
                        .id(SharedString::from(format!("settings-row-{i}")))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(gap_m(&host.font))
                        .h(px(28.0))
                        .px(gap_m(&host.font))
                        .rounded(px(RADIUS))
                        .bg(rgb(match selected {
                            true => c.selection_bg,
                            false => c.title_bg,
                        }))
                        .border_l(px(ROW_BAR))
                        .border_color(rgb(match selected {
                            true => c.accent,
                            false => c.title_bg,
                        }))
                        .cursor_pointer()
                        .child(
                            div()
                                .w(px(130.0))
                                .flex_none()
                                .font_family(host.font.family.clone())
                                .truncate()
                                .text_color(rgb(match row.enabled {
                                    true => c.fg,
                                    false => host.theme.dim_on(Surface::Title),
                                }))
                                .child(row.label),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_grow(1.0)
                                .truncate()
                                .text_color(rgb(c.dim))
                                .child(row.desc),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("settings-value-{i}")))
                                .flex_none()
                                .font_family(host.font.family.clone())
                                .text_color(rgb(match selected {
                                    true => c.accent,
                                    false => c.fg,
                                }))
                                .child(SharedString::from(value))
                                .on_click(move |_, _, cx| {
                                    _ = adjust.update(cx, |this, cx| {
                                        let sections = this.main.read(cx).settings_sections(cx);
                                        let filtered = this.filtered(&sections);
                                        this.sel = i;
                                        this.adjust(&filtered, 1, cx);
                                    });
                                }),
                        )
                        .on_click(move |_, _, cx| {
                            _ = me.update(cx, |this, cx| {
                                this.sel = i;
                                this.footer = None;
                                cx.notify();
                            });
                        })
                        .into_any_element()
                });
                std::iter::once(heading).chain(rows.collect::<Vec<_>>())
            })
            .collect::<Vec<_>>();
        {
            let i = index;
            let selected = i == self.sel;
            let me = me.clone();
            let open = me.clone();
            body.push(
                div()
                    .id("settings-row-file")
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(gap_m(&host.font))
                    .h(px(28.0))
                    .px(gap_m(&host.font))
                    .rounded(px(RADIUS))
                    .bg(rgb(match selected {
                        true => c.selection_bg,
                        false => c.title_bg,
                    }))
                    .border_l(px(ROW_BAR))
                    .border_color(rgb(match selected {
                        true => c.accent,
                        false => c.title_bg,
                    }))
                    .cursor_pointer()
                    .child(
                        div()
                            .w(px(130.0))
                            .flex_none()
                            .font_family(host.font.family.clone())
                            .text_color(rgb(c.fg))
                            .child("keys"),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_grow(1.0)
                            .truncate()
                            .text_color(rgb(c.dim))
                            .child("custom bindings live in [keys] — no GUI row, by design"),
                    )
                    .child(
                        div()
                            .id("settings-file-open")
                            .flex_none()
                            .text_color(rgb(match selected {
                                true => c.accent,
                                false => c.fg,
                            }))
                            .child("Edit in gitten.toml")
                            .on_click(move |_, _, cx| {
                                _ = open.update(cx, |this, cx| {
                                    let sections = this.main.read(cx).settings_sections(cx);
                                    let filtered = this.filtered(&sections);
                                    this.sel = i;
                                    this.adjust(&filtered, 1, cx);
                                });
                            }),
                    )
                    .on_click(move |_, _, cx| {
                        _ = me.update(cx, |this, cx| {
                            this.sel = i;
                            this.footer = None;
                            cx.notify();
                        });
                    })
                    .into_any_element(),
            );
        }

        let footer_right = match &self.footer {
            Some(message) => message.clone(),
            None => format!("{} rows · {} sections", total_rows, sections.len()),
        };
        let me = me.clone();
        div()
            .capture_key_down(cx.listener(Self::on_key))
            .track_focus(&self.nav)
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(c.bg))
            .text_size(px(host.font.size))
            .font_family(host.font.family.clone())
            .text_color(rgb(c.dim))
            // The header clears the platform traffic lights the way the main
            // strip does: inset from the left, centred in the band.
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(44.0))
                    .pl(px(64.0))
                    .pr(px(16.0))
                    .child(div().text_color(rgb(c.accent)).child("settings"))
                    .child(
                        div()
                            .id("settings-done")
                            .cursor_pointer()
                            .text_color(rgb(c.accent))
                            .child("done")
                            .on_click(move |_, window, _| {
                                window.remove_window();
                            }),
                    ),
            )
            // The search band: the prompt field's own bar, repurposed — its
            // top border divides the header the way it divides the strip.
            .child(self.search.clone())
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .flex_none()
                            .w(px(190.0))
                            .flex()
                            .flex_col()
                            .p_2()
                            .gap(px(1.0))
                            .border_r_1()
                            .border_color(rgb(c.border))
                            .children(side)
                            .child(
                                div()
                                    .id("settings-sec-file")
                                    .flex()
                                    .items_center()
                                    .h(px(24.0))
                                    .px_2()
                                    .rounded(px(RADIUS))
                                    .cursor_pointer()
                                    .text_color(rgb(c.dim))
                                    .child("file")
                                    .on_click({
                                        let me = me.clone();
                                        move |_, _, cx| {
                                            _ = me.update(cx, |this, cx| {
                                                let sections =
                                                    this.main.read(cx).settings_sections(cx);
                                                this.sel = settings::len(&this.filtered(&sections));
                                                this.footer = None;
                                                cx.notify();
                                            });
                                        }
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id("settings-rows")
                            .flex_1()
                            .min_w_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .p(px(8.0))
                            .children(body),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(30.0))
                    .px(px(16.0))
                    .border_t_1()
                    .border_color(rgb(c.border))
                    .text_color(rgb(host.theme.quiet_on(c.title_bg)))
                    .child("changes apply now and save to gitten.toml")
                    .child(SharedString::from(footer_right)),
            )
    }
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]` with
    // GPUI's own attribute macro and every test in here fails to expand.
    use super::{flat_at, flat_len, matches, Item};

    #[test]
    fn the_filter_keeps_rows_that_hold_any_word() {
        assert!(matches("mov", "moves", "3"));
        assert!(matches("MOV", "moves", "3"));
        assert!(matches("in heu", "indent heuristic", "on"));
        assert!(!matches("mov", "context", "3"));
        assert!(!matches("xyz", "layout", "unified"));
    }

    #[test]
    fn the_flat_address_space_ends_on_the_file_fallback() {
        use crate::settings::{Row, Section, Setting};
        let sections = vec![Section {
            title: "view",
            rows: vec![
                Row {
                    setting: Setting::Layout,
                    label: "layout",
                    value: "unified".into(),
                    desc: "",
                    enabled: true,
                },
                Row {
                    setting: Setting::Wrap,
                    label: "wrap",
                    value: "word".into(),
                    desc: "",
                    enabled: true,
                },
            ],
        }];
        assert_eq!(flat_len(&sections), 3);
        assert_eq!(flat_at(&sections, 0), Some(Item::Row { s: 0, r: 0 }));
        assert_eq!(flat_at(&sections, 1), Some(Item::Row { s: 0, r: 1 }));
        assert_eq!(flat_at(&sections, 2), Some(Item::File));
        assert_eq!(flat_at(&sections, 3), None);
    }
}
