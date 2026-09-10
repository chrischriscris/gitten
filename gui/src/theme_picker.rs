//! The theme picker: every registered palette, as a card with a swatch.
//!
//! The guide-v2 reference draws this as a dialog off a palette button in the
//! title strip: a filter field, a two-column grid of swatches, and a footer
//! naming the keys. It lists the *registry* — [`gitten_core::theme::Themes`] —
//! so a palette an extension registers, or one `gitten.toml` defines, is a card
//! here the day it exists, with no line changing in this file.
//!
//! The one deliberate difference from the reference: **choosing a theme does
//! not close the picker**. The reference dismisses on any action; here the whole
//! point is to try several against the diff behind the scrim, and a picker that
//! throws itself away on the first candidate is a picker you reopen six times.
//! `esc` and the `×` are the ways out.

use crate::{config, input, modal};
use gpui::*;

/// Rows drawn before the grid stops: the reference's 46vh, so a tall window
/// shows more cards rather than a longer scroll.
const GRID_FRACTION: f32 = 0.46;

/// One card's worth of a theme: what it is called, who it belongs to, and the
/// five inks the swatch paints. Read off the registry once per frame.
#[derive(Clone)]
pub(crate) struct Card {
    pub name: String,
    pub label: SharedString,
    pub family: SharedString,
    pub surface: u32,
    pub line: u32,
    pub accent: u32,
    pub green: u32,
    pub red: u32,
    pub add: u32,
    pub del: u32,
    pub current: bool,
}

/// Whether a card survives the filter. Every whitespace-separated word must
/// appear somewhere in the label, the family or the name — so `git dark` finds
/// GitHub Dark and `bit` finds both Bitbuckets.
pub(crate) fn matches(query: &str, label: &str, family: &str, name: &str) -> bool {
    let label = label.to_lowercase();
    let family = family.to_lowercase();
    let name = name.to_lowercase();
    query
        .split_whitespace()
        .all(|word| label.contains(word) || family.contains(word) || name.contains(word))
}

impl crate::DevShell {
    /// Open the picker: fresh query, selection on the current theme, keyboard
    /// in the filter field. The field entity is built once and reused — the
    /// same reason the command palette's is.
    pub(crate) fn open_theme_picker(&mut self, cx: &mut Context<Self>) {
        if self.theme_picker_field.is_none() {
            let field = cx.new(|cx| input::Input::new("", "Search themes…", "", cx));
            // Embedded: the field draws as the reference's rounded search box.
            // No exit hints — the footer names the keys once, and a box that
            // repeated them would be the second place to read them.
            field.update(cx, |field, _| field.set_embedded());
            let sub = cx.subscribe(&field, |this: &mut Self, _, event, cx| {
                if let input::Event::Edited(text) = event {
                    this.theme_picker_query = text.clone();
                    this.theme_picker_sel = 0;
                    cx.notify();
                }
            });
            self.theme_picker_field = Some(field);
            self.theme_picker_sub = Some(sub);
        }
        if let Some(field) = self.theme_picker_field.clone() {
            field.update(cx, |field, cx| {
                field.set_text(String::new(), cx);
            });
        }
        self.theme_picker_query.clear();
        // The selection starts on what is already on screen, so a `T` to look
        // at the palette opens with the cursor where the eye already is.
        self.theme_picker_sel = self
            .theme_picker_cards(cx)
            .iter()
            .position(|c| c.current)
            .unwrap_or(0);
        self.theme_picker_open = true;
        cx.notify();
    }

    /// Close the picker and hand the keyboard back to the files pane — the
    /// same restoration every dialog in this window keeps.
    pub(crate) fn close_theme_picker(&mut self, cx: &mut Context<Self>) {
        self.theme_picker_open = false;
        self.theme_picker_sel = 0;
        self.focus_named("files", cx);
        cx.notify();
    }

    /// The registry as cards, filtered by the live query and in registry order.
    pub(crate) fn theme_picker_cards(&self, cx: &App) -> Vec<Card> {
        let host = config::host(cx);
        let query = self.theme_picker_query.trim().to_lowercase();
        let mut out = Vec::with_capacity(host.themes.len());
        for i in 0..host.themes.len() {
            let Some(theme) = host.themes.at(i) else {
                continue;
            };
            let label = match theme.label.is_empty() {
                true => theme.name.clone(),
                false => theme.label.clone(),
            };
            if !query.is_empty() && !matches(&query, &label, &theme.family, &theme.name) {
                continue;
            }
            out.push(Card {
                name: theme.name.clone(),
                label: label.into(),
                family: theme.family.clone().into(),
                surface: theme.chrome.bg,
                line: theme.chrome.border,
                accent: theme.chrome.accent,
                green: theme.diff.adds_fg,
                red: theme.diff.dels_fg,
                add: theme.diff.added_bg,
                del: theme.diff.removed_bg,
                current: theme.name == host.theme.name,
            });
        }
        out
    }

    pub(crate) fn theme_picker_step(&mut self, by: isize, cx: &mut Context<Self>) {
        let count = self.theme_picker_cards(cx).len();
        if count == 0 {
            return;
        }
        let next = (self.theme_picker_sel as isize + by).clamp(0, count as isize - 1) as usize;
        if next != self.theme_picker_sel {
            self.theme_picker_sel = next;
            cx.notify();
        }
    }

    /// Put the card at `index` on screen. The picker stays open — that is the
    /// point of it — and the card redraws with its `Current` mark.
    pub(crate) fn choose_theme_at(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(card) = self.theme_picker_cards(cx).get(index).cloned() else {
            return;
        };
        let name = card.name.clone();
        self.theme_picker_sel = index;
        // `set_theme` rebuilds the host and refreshes every window, which
        // repaints this dialog with the new palette and the new `Current`.
        self.set_theme(name, cx);
    }

    pub(crate) fn run_theme_picker_selection(&mut self, cx: &mut Context<Self>) {
        self.choose_theme_at(self.theme_picker_sel, cx);
    }

    pub(crate) fn render_theme_picker(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let host = config::host(cx);
        let c = &host.theme.chrome;
        // The filter owns the keyboard while the picker stands; focusing on
        // every render is idempotent and keeps typing alive across re-renders.
        if let Some(field) = self.theme_picker_field.clone() {
            window.focus(&field.read(cx).focus_handle(), cx);
        }
        let cards = self.theme_picker_cards(cx);
        let sel = self.theme_picker_sel.min(cards.len().saturating_sub(1));
        let me = cx.entity().downgrade();

        let close = modal::close_button(&host, "theme-picker-close")
            .on_click({
                let me = me.clone();
                move |_, _, cx| {
                    _ = me.update(cx, |this, cx| this.close_theme_picker(cx));
                }
            })
            .into_any_element();

        // Two cards to a row, the reference's grid. A short tail row keeps its
        // card at half width rather than stretching it across the dialog.
        let mut grid: Vec<AnyElement> = Vec::new();
        for (row_index, row) in cards.chunks(2).enumerate() {
            let mut line = div().flex().items_stretch().gap(px(4.0)).w_full();
            for (offset, card) in row.iter().enumerate() {
                let index = row_index * 2 + offset;
                let selected = index == sel;
                let me = me.clone();
                let inks = [card.accent, card.green, card.red, card.add, card.del];
                let bars = inks
                    .iter()
                    .map(|ink| div().w(px(5.0)).h(px(16.0)).rounded(px(2.0)).bg(rgb(*ink)));
                let swatch = div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(3.0))
                    .w(px(42.0))
                    .h(px(28.0))
                    .bg(rgb(card.surface))
                    .border_1()
                    .border_color(rgb(card.line))
                    .rounded(px(5.0))
                    .children(bars);
                let meta = div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .items_start()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(c.fg))
                            .child(card.label.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(9.0))
                            .text_color(rgb(c.dim))
                            .child(card.family.clone()),
                    );
                let card_element = div()
                    .id(SharedString::from(format!("theme-card-{index}")))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .p(px(8.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(match card.current {
                        true => c.accent,
                        false => match selected {
                            true => c.faint,
                            false => c.bg,
                        },
                    }))
                    .bg(rgb(match selected {
                        true => c.selection_bg,
                        false => c.bg,
                    }))
                    .cursor_pointer()
                    .child(swatch)
                    .child(meta)
                    .children(card.current.then(|| {
                        div()
                            .flex_none()
                            .ml_auto()
                            .text_size(px(9.0))
                            .text_color(rgb(c.accent))
                            .child("Current")
                    }))
                    .on_click(move |_, _, cx| {
                        _ = me.update(cx, |this, cx| this.choose_theme_at(index, cx));
                    });
                line = line.child(card_element);
            }
            if row.len() == 1 {
                line = line.child(div().flex_1().min_w_0());
            }
            grid.push(line.into_any_element());
        }

        let rows = div()
            .id("theme-grid")
            .flex()
            .flex_col()
            .gap(px(4.0))
            .max_h(px(f32::from(window.viewport_size().height) * GRID_FRACTION))
            .overflow_y_scroll()
            .children(grid);

        let body = match cards.is_empty() {
            true => div()
                .flex_none()
                .py(px(20.0))
                .text_color(rgb(c.dim))
                .child("No theme matches that.")
                .into_any_element(),
            false => rows.into_any_element(),
        };

        modal::centered(
            &host,
            modal::Width::Exact(560.0),
            vec![
                modal::heading(&host, "Themes", Some(close)).into_any_element(),
                self.theme_picker_field
                    .clone()
                    .map(|f| f.into_any_element())
                    .unwrap_or_else(|| div().into_any_element()),
                div().h(px(10.0)).flex_none().into_any_element(),
                body,
                modal::hint(
                    &host,
                    SharedString::from(format!(
                        "{} themes \u{00b7} enter to choose \u{00b7} esc to close",
                        cards.len()
                    )),
                ),
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn a_query_matches_the_label_the_family_or_the_name() {
        assert!(matches("dark", "GitHub Dark", "GitHub", "github-dark"));
        assert!(matches("git dark", "GitHub Dark", "GitHub", "github-dark"));
        assert!(matches(
            "jet",
            "IntelliJ Light",
            "JetBrains",
            "intellij-light"
        ));
        assert!(matches(
            "mocha",
            "Catppuccin Mocha",
            "Catppuccin",
            "catppuccin-mocha"
        ));
        // The registry name is searched too, so a palette the file named but
        // never labelled is still findable.
        assert!(matches("solarized", "solarized-ish", "", "solarized-ish"));
        assert!(!matches("dracula", "GitHub Dark", "GitHub", "github-dark"));
        assert!(!matches(
            "dark light",
            "GitHub Dark",
            "GitHub",
            "github-dark"
        ));
    }
}
