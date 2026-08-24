//! Native text input shared by every shell prompt.
//!
//! GPUI key events are not text input: they do not carry IME composition,
//! candidate windows, UTF-16 selection ranges or the platform character
//! palette. This block implements [`EntityInputHandler`] and leaves insertion
//! to the operating system. Only accepting and cancelling are named commands;
//! local cursor and clipboard actions are ordinary text-field mechanics.

use crate::config;
use gpui::prelude::*;
use gpui::*;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation as _;

pub const MODE: &str = "input";
const KEY_CONTEXT: &str = "GittenInput";

actions!(
    gitten_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        Home,
        End,
        SelectHome,
        SelectEnd,
        SelectAll,
        Paste,
        Copy,
        Cut,
        CharacterPalette,
    ]
);

/// Installs platform editing keys. Scoped to [`KEY_CONTEXT`], so these never
/// compete with the shell's named command path outside a focused input.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some(KEY_CONTEXT)),
        KeyBinding::new("delete", Delete, Some(KEY_CONTEXT)),
        KeyBinding::new("left", Left, Some(KEY_CONTEXT)),
        KeyBinding::new("right", Right, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-left", SelectLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-right", SelectRight, Some(KEY_CONTEXT)),
        KeyBinding::new("home", Home, Some(KEY_CONTEXT)),
        KeyBinding::new("end", End, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-left", Home, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-right", End, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-home", SelectHome, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-end", SelectEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-left", SelectHome, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-right", SelectEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-a", SelectAll, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-a", SelectAll, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-v", Paste, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-v", Paste, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-c", Copy, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-c", Copy, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-x", Cut, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-x", Cut, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-cmd-space", CharacterPalette, Some(KEY_CONTEXT)),
    ]);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Accepted(String),
    Cancelled,
}

pub struct Input {
    focus: FocusHandle,
    label: SharedString,
    placeholder: SharedString,
    content: String,
    selected: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
}

impl Input {
    pub fn new(
        label: impl Into<SharedString>,
        placeholder: impl Into<SharedString>,
        initial: impl Into<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let content = initial.into();
        let end = content.len();
        Self {
            focus: cx.focus_handle(),
            label: label.into(),
            placeholder: placeholder.into(),
            content,
            selected: end..end,
            reversed: false,
            marked: None,
            last_layout: None,
            last_bounds: None,
            selecting: false,
        }
    }

    /// The text as it stands — what a prompt's consumer reads on accept.
    pub fn value(&self) -> &str {
        &self.content
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn selected_text(&self) -> Option<String> {
        (!self.selected.is_empty()).then(|| self.content[self.selected.clone()].to_string())
    }

    pub fn select_all_text(&mut self, select: bool, cx: &mut Context<Self>) {
        match select {
            true => self.selected = 0..self.content.len(),
            false => {
                let cursor = self.cursor();
                self.selected = cursor..cursor;
            }
        }
        self.reversed = false;
        self.marked = None;
        cx.notify();
    }

    pub fn accept(&mut self, cx: &mut Context<Self>) {
        self.marked = None;
        cx.emit(Event::Accepted(self.content.clone()));
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.marked = None;
        cx.emit(Event::Cancelled);
    }

    fn cursor(&self) -> usize {
        if self.reversed {
            self.selected.start
        } else {
            self.selected.end
        }
    }

    fn move_to(&mut self, offset: usize) {
        let offset = floor_boundary(&self.content, offset);
        self.selected = offset..offset;
        self.reversed = false;
        self.marked = None;
    }

    fn select_to(&mut self, offset: usize) {
        let offset = floor_boundary(&self.content, offset);
        if self.reversed {
            self.selected.start = offset;
        } else {
            self.selected.end = offset;
        }
        if self.selected.end < self.selected.start {
            self.reversed = !self.reversed;
            self.selected = self.selected.end..self.selected.start;
        }
        self.marked = None;
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(at, _)| (at < offset).then_some(at))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(at, _)| (at > offset).then_some(at))
            .unwrap_or(self.content.len())
    }

    fn replace(&mut self, range_utf16: Option<Range<usize>>, text: &str) {
        let range = range_utf16
            .as_ref()
            .map(|range| range_from_utf16(&self.content, range))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selected.clone());
        self.content.replace_range(range.clone(), text);
        let cursor = range.start + text.len();
        self.selected = cursor..cursor;
        self.reversed = false;
        self.marked = None;
    }

    fn replace_marked(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| range_from_utf16(&self.content, range))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selected.clone());
        let start = range.start;
        self.content.replace_range(range, text);
        self.marked = (!text.is_empty()).then_some(start..start + text.len());
        self.selected = selected_utf16
            .as_ref()
            .map(|range| range_from_utf16(text, range))
            .map(|range| start + range.start..start + range.end)
            .unwrap_or_else(|| start + text.len()..start + text.len());
        self.reversed = false;
    }

    fn index_for_point(&self, point: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(line)) = (&self.last_bounds, &self.last_layout) else {
            return 0;
        };
        if line.text.as_ref() != self.content {
            return self.cursor();
        }
        if point.y < bounds.top() {
            return 0;
        }
        if point.y > bounds.bottom() {
            return self.content.len();
        }
        line.closest_index_for_x(point.x - bounds.left())
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        let at = match self.selected.is_empty() {
            true => self.previous_boundary(self.cursor()),
            false => self.selected.start,
        };
        self.move_to(at);
        cx.notify();
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        let at = match self.selected.is_empty() {
            true => self.next_boundary(self.cursor()),
            false => self.selected.end,
        };
        self.move_to(at);
        cx.notify();
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor()));
        cx.notify();
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor()));
        cx.notify();
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0);
        cx.notify();
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len());
        cx.notify();
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0);
        cx.notify();
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len());
        cx.notify();
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.select_all_text(true, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            let at = self.previous_boundary(self.cursor());
            if at == self.cursor() {
                window.play_system_bell();
                return;
            }
            self.select_to(at);
        }
        self.replace(None, "");
        cx.notify();
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            let at = self.next_boundary(self.cursor());
            if at == self.cursor() {
                window.play_system_bell();
                return;
            }
            self.select_to(at);
        }
        self.replace(None, "");
        cx.notify();
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace(None, &text.replace(['\r', '\n'], " "));
            cx.notify();
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected.clone()].to_string(),
            ));
            self.replace(None, "");
            cx.notify();
        }
    }

    fn character_palette(
        &mut self,
        _: &CharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.selecting = true;
        let at = self.index_for_point(event.position);
        if event.modifiers.shift {
            self.select_to(at);
        } else {
            self.move_to(at);
        }
        cx.notify();
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.select_to(self.index_for_point(event.position));
            cx.notify();
        }
    }
}

impl EventEmitter<Event> for Input {}

impl Focusable for Input {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for Input {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = range_from_utf16(&self.content, &range_utf16);
        actual_range.replace(range_to_utf16(&self.content, &range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: range_to_utf16(&self.content, &self.selected),
            reversed: self.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|range| range_to_utf16(&self.content, range))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace(range_utf16, text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_marked(range_utf16, text, selected_utf16);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.last_layout.as_ref()?;
        if line.text.as_ref() != self.content {
            return None;
        }
        let range = range_from_utf16(&self.content, &range_utf16);
        Some(Bounds::from_corners(
            point(bounds.left() + line.x_for_index(range.start), bounds.top()),
            point(bounds.left() + line.x_for_index(range.end), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(offset_to_utf16(&self.content, self.index_for_point(point)))
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected = range_from_utf16(&self.content, &range_utf16);
        self.reversed = false;
        self.marked = None;
        cx.notify();
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.content.encode_utf16().count())
    }
}

struct TextElement {
    input: Entity<Input>,
    cursor: Rgba,
    selection: Rgba,
}

struct Prepaint {
    line: ShapedLine,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let style = window.text_style();
        let (text, color) = match input.content.is_empty() {
            true => (input.placeholder.clone(), style.color.opacity(0.55)),
            false => (SharedString::from(input.content.clone()), style.color),
        };
        let base = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = match input.marked.as_ref() {
            Some(marked) if !input.content.is_empty() => vec![
                TextRun {
                    len: marked.start,
                    ..base.clone()
                },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle {
                        color: Some(color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..base.clone()
                },
                TextRun {
                    len: text.len() - marked.end,
                    ..base
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect(),
            _ => vec![base],
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(text, font_size, &runs, None);
        let (selection, cursor) = if input.selected.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(
                            bounds.left() + line.x_for_index(input.cursor()),
                            bounds.top(),
                        ),
                        size(px(1.0), bounds.size.height),
                    ),
                    self.cursor,
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(input.selected.start),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(input.selected.end),
                            bounds.bottom(),
                        ),
                    ),
                    self.selection,
                )),
                None,
            )
        };
        Prepaint {
            line,
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        state: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = state.selection.take() {
            window.paint_quad(selection);
        }
        state
            .line
            .paint(
                bounds.origin,
                window.line_height(),
                TextAlign::Left,
                None,
                window,
                cx,
            )
            .expect("input text is shapeable");
        if focus.is_focused(window) {
            if let Some(cursor) = state.cursor.take() {
                window.paint_quad(cursor);
            }
        }
        self.input.update(cx, |input, _| {
            input.last_layout = Some(state.line.clone());
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for Input {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = config::host(cx);
        let chrome = host.theme.chrome;
        div()
            .id("input")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .h(px(34.0))
            .px_4()
            .bg(rgb(chrome.status_bg))
            .border_t_1()
            .border_color(rgb(chrome.border))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::character_palette))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .child(
                div()
                    .flex_none()
                    .text_color(rgb(chrome.accent))
                    .child(self.label.clone()),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_grow(1.0)
                    .overflow_hidden()
                    .text_color(rgb(chrome.fg))
                    .child(TextElement {
                        input: cx.entity(),
                        cursor: rgb(chrome.accent),
                        selection: rgb(chrome.selection_bg),
                    }),
            )
    }
}

fn floor_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn offset_from_utf16(text: &str, target: usize) -> usize {
    let mut utf16 = 0;
    let mut utf8 = 0;
    for ch in text.chars() {
        let next = utf16 + ch.len_utf16();
        if next > target {
            break;
        }
        utf16 = next;
        utf8 += ch.len_utf8();
    }
    utf8
}

fn offset_to_utf16(text: &str, offset: usize) -> usize {
    text[..floor_boundary(text, offset)].encode_utf16().count()
}

fn range_from_utf16(text: &str, range: &Range<usize>) -> Range<usize> {
    let start = offset_from_utf16(text, range.start);
    let end = offset_from_utf16(text, range.end);
    start.min(end)..start.max(end)
}

fn range_to_utf16(text: &str, range: &Range<usize>) -> Range<usize> {
    offset_to_utf16(text, range.start)..offset_to_utf16(text, range.end)
}

#[cfg(test)]
mod tests {
    use super::{offset_from_utf16, range_from_utf16, range_to_utf16, Input};
    use gpui::{AppContext as _, Entity, TestAppContext};

    fn input(text: &str, cx: &mut TestAppContext) -> Entity<Input> {
        cx.new(|cx| Input::new("message", "type", text, cx))
    }

    #[gpui::test]
    fn utf16_ranges_never_split_unicode(cx: &mut TestAppContext) {
        let text = "a😀éz";
        assert_eq!(offset_from_utf16(text, 0), 0);
        assert_eq!(offset_from_utf16(text, 1), 1);
        assert_eq!(offset_from_utf16(text, 2), 1, "inside the surrogate pair");
        assert_eq!(offset_from_utf16(text, 3), 5);
        assert_eq!(range_to_utf16(text, &(1..5)), 1..3);

        let input = input(text, cx);
        input.update(cx, |input, _| {
            input.selected = range_from_utf16(&input.content, &(1..3));
            assert_eq!(&input.content[input.selected.clone()], "😀");
        });
    }

    #[gpui::test]
    fn composition_replaces_the_marked_text_and_keeps_its_relative_selection(
        cx: &mut TestAppContext,
    ) {
        let input = input("ab", cx);
        input.update(cx, |input, _| {
            input.move_to(1);
            input.replace_marked(None, "😀x", Some(2..3));
            assert_eq!(input.value(), "a😀xb");
            assert_eq!(input.marked, Some(1..6));
            assert_eq!(input.selected, 5..6);

            input.replace(None, "é");
            assert_eq!(input.value(), "aéb");
            assert_eq!(input.marked, None);
            assert_eq!(input.selected, 3..3);
        });
    }

    #[gpui::test]
    fn cursor_motion_and_deletion_follow_graphemes(cx: &mut TestAppContext) {
        let input = input("éx", cx);
        input.update(cx, |input, _| {
            input.move_to(input.content.len());
            let before_x = input.previous_boundary(input.cursor());
            input.move_to(before_x);
            assert_eq!(input.cursor(), "é".len());
            let before_combined = input.previous_boundary(input.cursor());
            assert_eq!(before_combined, 0, "the accent stayed with its base");
            input.selected = before_combined..input.cursor();
            input.replace(None, "");
            assert_eq!(input.value(), "x");
        });
    }
}
