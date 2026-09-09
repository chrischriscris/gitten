//! Native text input shared by every shell prompt.
//!
//! GPUI key events are not text input: they do not carry IME composition,
//! candidate windows, UTF-16 selection ranges or the platform character
//! palette. This block implements [`EntityInputHandler`] and leaves insertion
//! to the operating system. Only accepting and cancelling are named commands;
//! local cursor and clipboard actions are ordinary text-field mechanics.

use crate::chrome::{gap_m, gap_s};
use crate::config;
use gitten_core::host::Host;
use gitten_core::theme::ChromePalette;
use gpui::prelude::*;
use gpui::*;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation as _;

pub const MODE: &str = "input";
const KEY_CONTEXT: &str = "GittenInput";

/// The input band's height: [`crate::chrome::STATUS_H`] plus 8. Taller than
/// the 36px bottom strip on purpose — a text field is a *target*, not a label:
/// it is where typing lands, and a target earns the extra air. Named from the
/// strip so the two stay in one chrome rhythm.
const INPUT_H: f32 = crate::chrome::STATUS_H + 8.0;

actions!(
    gitten_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
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
        // Vertical moves only a multiline field answers: a prompt is one
        // line, where up and down never meant anything, and the handler
        // below no-ops there — bound so a multiline field's keyboard is
        // complete, not so a prompt gains a move it cannot make.
        KeyBinding::new("up", Up, Some(KEY_CONTEXT)),
        KeyBinding::new("down", Down, Some(KEY_CONTEXT)),
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
    /// The text changed — typed, pasted, cut, deleted or replaced by the
    /// platform. Emitted from the two places content can mutate, so a live
    /// consumer (a search prompt filtering as you type) sees every edit
    /// without polling a field it does not own.
    Edited(String),
    Accepted(String),
    Cancelled,
}

pub struct Input {
    focus: FocusHandle,
    /// The prompt's name, stored already uppercased: every other chrome label
    /// — the section headings, the `PROMPT` badge this field sits above — is
    /// uppercase, and a label that renders lowercase reads as a third kind of
    /// text. Uppercased once, here, so the call sites stay lowercase data and
    /// the frame never pays for the conversion.
    label: SharedString,
    placeholder: SharedString,
    content: String,
    selected: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    /// One painted line per visual row, in order — the multiline field's
    /// hit-testing and IME answers. Each entry is the line's bounds with
    /// the shaped line and its byte range in `content` at paint time.
    /// Empty for single-line fields, which keep answering from
    /// `last_layout`/`last_bounds` exactly as before.
    last_lines: Vec<(Bounds<Pixels>, ShapedLine, usize, usize)>,
    /// Whether the field holds line breaks: the inspector's Description.
    /// Single-line fields keep every existing behavior — paste folds
    /// newlines to spaces, Enter is accept's, the render is one row.
    multiline: bool,
    selecting: bool,
    /// The two ways out, resolved once when the shell opened the prompt: the
    /// live key for `input.accept` and for `input.cancel`, `None` until then.
    /// An empty string means that exit has no live key and draws nothing —
    /// naming a key that would not fire is the one lie a prompt of keys must
    /// not tell, and this is the highest-stakes text entry in the app.
    exits: Option<(SharedString, SharedString)>,
    embedded: bool,
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
            label: label.into().to_uppercase().into(),
            placeholder: placeholder.into(),
            content,
            selected: end..end,
            reversed: false,
            marked: None,
            last_layout: None,
            last_bounds: None,
            last_lines: Vec::new(),
            multiline: false,
            selecting: false,
            exits: None,
            embedded: false,
        }
    }

    /// Turns the field multiline — the Description's shape. Newlines survive
    /// paste, Enter (as `input.newline`, routed by the shell) breaks the
    /// line, and the render grows one row per visual line.
    pub fn set_embedded(&mut self) {
        self.embedded = true;
    }

    pub fn set_multiline(&mut self, multiline: bool) {
        self.multiline = multiline;
    }

    pub fn is_multiline(&self) -> bool {
        self.multiline
    }

    /// Replaces the whole text — a draft store refilling its field on a
    /// repository switch. The cursor parks at the end; no event fires,
    /// because the caller already wrote the draft this mirrors.
    pub fn set_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.content = text;
        let end = self.content.len();
        self.selected = end..end;
        self.reversed = false;
        self.marked = None;
        cx.notify();
    }

    /// The `input.newline` answer for a focused multiline field: a line
    /// break at the cursor. Single-line fields refuse it — their Enter is
    /// accept's, and a break inside one would be content no render shows.
    pub fn insert_newline(&mut self, cx: &mut Context<Self>) {
        if !self.multiline {
            return;
        }
        self.replace(None, "\n", cx);
        cx.notify();
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

    /// Names the two exits, resolved by the shell at open time — the mode
    /// stack the prompt will run under exists only there, and a key resolved
    /// per frame would re-walk the keymap for a field whose keyboard does not
    /// change while it holds it. `None` leaves the field hintless, which is
    /// the honest state for a test-built field: no shell, no live keys.
    pub fn set_exits(&mut self, accept: Option<String>, cancel: Option<String>) {
        self.exits = match (accept, cancel) {
            (None, None) => None,
            (accept, cancel) => Some((
                accept.map(SharedString::from).unwrap_or_default(),
                cancel.map(SharedString::from).unwrap_or_default(),
            )),
        };
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

    /// Byte starts of every visual line, in order — the multiline cursor's
    /// address book. Pure over the text, so moves compute without paint.
    fn line_starts(content: &str) -> Vec<usize> {
        let mut starts = vec![0];
        for (at, ch) in content.char_indices() {
            if ch == '\n' {
                starts.push(at + 1);
            }
        }
        starts
    }

    /// The visual line holding `at`, clamping a past-the-end cursor onto
    /// the last line.
    fn line_of(content: &str, at: usize) -> usize {
        let starts = Self::line_starts(content);
        starts.iter().rposition(|&s| s <= at).unwrap_or(0)
    }

    /// A visual line's byte range, end exclusive and without its break.
    fn line_range(content: &str, line: usize) -> (usize, usize) {
        let starts = Self::line_starts(content);
        let start = starts.get(line).copied().unwrap_or(content.len());
        let end = starts
            .get(line + 1)
            .map(|next| next.saturating_sub(1))
            .unwrap_or(content.len());
        (start, end.min(content.len()))
    }

    /// Moves the cursor one visual line down (`delta` +1) or up (-1),
    /// keeping the grapheme column where the target line is long enough.
    /// A no-op on single-line fields and past either edge.
    fn move_line(&mut self, delta: isize) {
        if !self.multiline {
            return;
        }
        let line = Self::line_of(&self.content, self.cursor());
        let next = line as isize + delta;
        if next < 0 {
            return;
        }
        let (start, _) = Self::line_range(&self.content, line);
        let column = self.content[start..self.cursor()].graphemes(true).count();
        let (nstart, nend) = Self::line_range(&self.content, next as usize);
        if nstart >= self.content.len() && next as usize >= Self::line_starts(&self.content).len() {
            return;
        }
        let mut at = nstart;
        for _ in 0..column {
            let next_at = self.next_boundary(at);
            if next_at > nend {
                break;
            }
            at = next_at;
        }
        self.move_to(at);
    }

    /// Replaces the selected (or marked, or given) range. The one choke point
    /// for every edit that is not composition state — backspace, delete,
    /// paste, cut and the platform's own `replace_text_in_range` all arrive
    /// here — which is why the [`Event::Edited`] emission lives at this one
    /// place and nowhere else.
    ///
    /// Crate-visible so a test can drive an edit the way the platform would,
    /// without a window to deliver one through.
    pub(crate) fn replace(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        cx: &mut Context<Self>,
    ) {
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
        cx.emit(Event::Edited(self.content.clone()));
    }

    /// The composition half of the same story: IME text lands through here,
    /// and it edits content just as much as a keystroke does, so it says so
    /// with the same event.
    fn replace_marked(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
        cx: &mut Context<Self>,
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
        cx.emit(Event::Edited(self.content.clone()));
    }

    fn index_for_point(&self, point: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        if self.multiline {
            return self.multiline_index_for_point(point);
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

    /// A point into a multiline field: the row by its painted bounds, then
    /// the column inside that row's shaped line, shifted by the row's byte
    /// start. A point past the last row names the end of the text; a stale
    /// paint (rows shaped for older content) keeps the cursor, because a
    /// click resolved against yesterday's rows is worse than no move.
    fn multiline_index_for_point(&self, point: Point<Pixels>) -> usize {
        let Some((bounds, line, start, _)) = self
            .last_lines
            .iter()
            .find(|(bounds, _, _, _)| point.y >= bounds.top() && point.y <= bounds.bottom())
        else {
            return match self.last_lines.last() {
                Some((bounds, _, _, _)) if point.y > bounds.bottom() => self.content.len(),
                _ => self.cursor(),
            };
        };
        let expected = &self.content[*start..];
        let row_text = line.text.as_ref();
        if !expected.starts_with(row_text) {
            return self.cursor();
        }
        start + line.closest_index_for_x(point.x - bounds.left())
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
        // Across the break, in a field that has breaks: the grapheme
        // after a line's last character is the next line's first, and
        // `next_boundary` stops at the break itself.
        let at =
            match self.multiline && at < self.content.len() && self.content[at..].starts_with('\n')
            {
                true => at + 1,
                false => at,
            };
        self.move_to(at);
        cx.notify();
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if !self.multiline {
            return;
        }
        self.move_line(-1);
        cx.notify();
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if !self.multiline {
            return;
        }
        self.move_line(1);
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
        // The line's start in a field that has lines, the field's in one
        // that does not.
        let at = match self.multiline {
            true => Self::line_range(&self.content, Self::line_of(&self.content, self.cursor())).0,
            false => 0,
        };
        self.move_to(at);
        cx.notify();
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let at = match self.multiline {
            true => Self::line_range(&self.content, Self::line_of(&self.content, self.cursor())).1,
            false => self.content.len(),
        };
        self.move_to(at);
        cx.notify();
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        let at = match self.multiline {
            true => Self::line_range(&self.content, Self::line_of(&self.content, self.cursor())).0,
            false => 0,
        };
        self.select_to(at);
        cx.notify();
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        let at = match self.multiline {
            true => Self::line_range(&self.content, Self::line_of(&self.content, self.cursor())).1,
            false => self.content.len(),
        };
        self.select_to(at);
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
        self.replace(None, "", cx);
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
        self.replace(None, "", cx);
        cx.notify();
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            // A multiline field keeps its breaks — carriage returns still
            // go, because a pasted CRLF is one break, not two characters.
            // A prompt folds both to a space, as before.
            let text = match self.multiline {
                true => text.replace('\r', ""),
                false => text.replace(['\r', '\n'], " "),
            };
            self.replace(None, &text, cx);
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
            self.replace(None, "", cx);
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
        self.replace(range_utf16, text, cx);
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
        self.replace_marked(range_utf16, text, selected_utf16, cx);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The cursor's own row in a multiline field — the single shaped
        // line everywhere else. A range outside the row it lands on answers
        // nothing rather than a box on the wrong row.
        if self.multiline {
            let cursor = self.cursor();
            let (row_bounds, line, start, end) = self
                .last_lines
                .iter()
                .find(|(_, _, s, e)| *s <= cursor && cursor <= *e)?;
            let range = range_from_utf16(&self.content, &range_utf16);
            if range.end < *start || range.start > *end {
                return None;
            }
            let len = line.text.len();
            let local =
                |at: usize| line.x_for_index(at.max(*start).saturating_sub(*start).min(len));
            return Some(Bounds::from_corners(
                point(row_bounds.left() + local(range.start), row_bounds.top()),
                point(row_bounds.left() + local(range.end), row_bounds.bottom()),
            ));
        }
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
    /// Which visual line this element draws. Single-line fields draw line
    /// 0 over the whole content; a multiline field draws one element per
    /// row, each shaping only its own slice.
    line: usize,
}

struct Prepaint {
    line: ShapedLine,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    /// This element's byte range in the content at shape time — what paint
    /// files into `last_lines` for hit-testing.
    start: usize,
    end: usize,
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
        // The slice this element shapes: the whole content on line 0 of a
        // single-line field, one visual row of a multiline one. The
        // placeholder shows on the first row of an empty field only.
        let (start, end) = match input.multiline {
            true => Input::line_range(&input.content, self.line),
            false => (0, input.content.len()),
        };
        let (text, color) = match input.content.is_empty() {
            true if self.line == 0 => (input.placeholder.clone(), style.color.opacity(0.55)),
            true => (SharedString::from(""), style.color),
            false => (
                SharedString::from(input.content[start..end].to_string()),
                style.color,
            ),
        };
        let base = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        // Composition underlines ride in this element's own coordinates:
        // a multiline row intersects the content-global marked range with
        // its slice, so an IME composing across a break underlines both
        // rows rather than neither.
        let runs = match input.marked.as_ref() {
            Some(marked) if !input.content.is_empty() => {
                let local = marked.start.max(start).saturating_sub(start)
                    ..marked.end.max(start).saturating_sub(start);
                let local = local.start.min(text.len())..local.end.min(text.len());
                match local.is_empty() {
                    true => vec![base],
                    false => vec![
                        TextRun {
                            len: local.start,
                            ..base.clone()
                        },
                        TextRun {
                            len: local.end - local.start,
                            underline: Some(UnderlineStyle {
                                color: Some(color),
                                thickness: px(1.0),
                                wavy: false,
                            }),
                            ..base.clone()
                        },
                        TextRun {
                            len: text.len() - local.end,
                            ..base
                        },
                    ]
                    .into_iter()
                    .filter(|run| run.len > 0)
                    .collect(),
                }
            }
            _ => vec![base],
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let text_len = text.len();
        let line = window
            .text_system()
            .shape_line(text, font_size, &runs, None);
        // The cursor and the selection in this row's coordinates: intersect
        // the content-global range with the shaped slice. A cursor on
        // another row draws nothing here; a selection crossing the row
        // paints the overlap. Single-line rows span the whole content, so
        // the intersection is the range itself.
        let at = input
            .cursor()
            .max(start)
            .saturating_sub(start)
            .min(text_len);
        let sel = input
            .selected
            .start
            .max(start)
            .saturating_sub(start)
            .min(text_len)
            ..input
                .selected
                .end
                .max(start)
                .saturating_sub(start)
                .min(text_len);
        // The cursor belongs to exactly one row: the row holding it, or
        // the last row when it stands at the very end of the text.
        let has_cursor = (start <= input.cursor() && input.cursor() < end)
            || (input.cursor() == input.content.len() && end == input.content.len());
        let (selection, cursor) = if input.selected.is_empty() {
            match has_cursor {
                true => (
                    None,
                    Some(fill(
                        Bounds::new(
                            point(bounds.left() + line.x_for_index(at), bounds.top()),
                            size(px(1.0), bounds.size.height),
                        ),
                        self.cursor,
                    )),
                ),
                false => (None, None),
            }
        } else {
            match sel.is_empty() {
                true => (None, None),
                false => (
                    Some(fill(
                        Bounds::from_corners(
                            point(bounds.left() + line.x_for_index(sel.start), bounds.top()),
                            point(bounds.left() + line.x_for_index(sel.end), bounds.bottom()),
                        ),
                        self.selection,
                    )),
                    None,
                ),
            }
        };
        Prepaint {
            line,
            cursor,
            selection,
            start,
            end,
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
        // The platform delivers text to the focused handler: register the
        // row the cursor stands on, so IME positioning follows it. One
        // registration per field — every row registering would leave the
        // platform no single answer about where the text goes.
        let (focus, cursor_line) = {
            let input = self.input.read(cx);
            (
                input.focus.clone(),
                Input::line_of(&input.content, input.cursor()),
            )
        };
        if self.line == cursor_line {
            window.handle_input(
                &focus,
                ElementInputHandler::new(bounds, self.input.clone()),
                cx,
            );
        }
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
            match input.multiline {
                // Filed in paint order, which is row order: render clears
                // the vec, then every row paints exactly once. A row
                // arriving out of sequence replaces its slot; anything
                // stranger clears, and the staleness guards in the
                // hit-testing hold until the next frame repopulates.
                true => {
                    if input.last_lines.len() == self.line {
                        input
                            .last_lines
                            .push((bounds, state.line.clone(), state.start, state.end));
                    } else if input.last_lines.len() > self.line {
                        input.last_lines[self.line] =
                            (bounds, state.line.clone(), state.start, state.end);
                    } else {
                        input.last_lines.clear();
                    }
                }
                false => {
                    input.last_layout = Some(state.line.clone());
                    input.last_bounds = Some(bounds);
                }
            }
        });
    }
}

impl Render for Input {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = config::host(cx);
        let chrome = host.theme.chrome;
        if self.multiline {
            return self
                .render_multiline(window, &host, chrome, cx)
                .into_any_element();
        }
        div()
            .id("input")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .flex_none()
            .flex()
            .items_center()
            .gap(gap_m(&host.font))
            .h(px(INPUT_H))
            // The status strip's inset, not the row pad: the `PROMPT` badge
            // sits directly below this field and the two are one column — a
            // prompt at a third inset, one [`gap_m`] step off the bar that
            // names it.
            .px(gap_m(&host.font))
            .bg(rgb(chrome.status_bg))
            .when(self.embedded, |d| {
                d.bg(rgb(chrome.title_bg)).border_1().rounded(px(5.0))
            })
            .border_t_1()
            .border_color(rgb(chrome.border))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
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
                        selection: rgb(chrome.selected_bg),
                        line: 0,
                    }),
            )
            .child(exit_hints(&host, chrome, &self.exits))
            .into_any_element()
    }
}

/// The multiline field's own render: a labeled box that grows one row per
/// visual line, rather than the prompt's fixed-height band. No exit hints —
/// an embedded field's keyboard belongs to the shell's named routing (see
/// the inspector), not to a prompt slot, so naming prompt exits here would
/// be the panel-of-keys lie.
impl Input {
    /// Most rows a pathological paste may spend. Past the cap the field
    /// still holds every line — content is never truncated — but only rows
    /// through the cursor's own paint, so one frame never shapes ten
    /// thousand rows for a field nobody is reading past.
    const MAX_ROWS: usize = 64;

    fn render_multiline(
        &mut self,
        window: &mut Window,
        host: &Host,
        chrome: ChromePalette,
        cx: &mut Context<Self>,
    ) -> Div {
        // Cleared here, repopulated in paint order (row order) below: every
        // frame re-files every row it draws, so hit-testing never reads a
        // row shaped for older content.
        self.last_lines.clear();
        let rows = self.content.split('\n').count().max(1);
        let cursor_line = Self::line_of(&self.content, self.cursor());
        let shown = rows.max(cursor_line + 1).min(Self::MAX_ROWS);
        let entity = cx.entity();
        let field = div()
            .id("input-multiline")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .flex_none()
            .flex()
            .flex_col()
            .px(gap_m(&host.font))
            .py(px(f32::from(window.line_height()) * 0.4))
            // Three rows at rest: a description is usually one line, and a
            // one-row box reads as a second summary field rather than the
            // longer text it invites.
            .min_h(px(f32::from(window.line_height()) * 3.0))
            .bg(rgb(chrome.raised))
            .when(self.embedded, |d| {
                d.bg(rgb(chrome.title_bg)).min_h(px(100.0))
            })
            .border_1()
            .border_color(rgb(chrome.border))
            .rounded(px(crate::chrome::RADIUS))
            .cursor(CursorStyle::IBeam)
            .text_color(rgb(chrome.fg))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
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
            .children((0..shown).map(|line| TextElement {
                input: entity.clone(),
                cursor: rgb(chrome.accent),
                selection: rgb(chrome.selected_bg),
                line,
            }))
            .children((shown < rows).then(|| {
                div()
                    .text_color(rgb(host.theme.dim_on(gitten_core::theme::Surface::Context)))
                    .child(SharedString::from(format!("… {} more lines", rows - shown)))
            }));
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap_y(px(2.0))
            .when(!self.embedded, |d| {
                d.child(
                    div()
                        .text_color(rgb(host.theme.dim_on(gitten_core::theme::Surface::Context)))
                        .child(self.label.clone()),
                )
            })
            .child(field)
    }
}

/// The prompt's exits, at the field's right edge: `enter accept · esc cancel`.
///
/// While a prompt stands, the status hints are blanked on the reasoning that
/// the field owns the keyboard and speaks for itself — but the field said
/// nothing, and the two keys that matter most in the app were drawn nowhere.
/// So the field speaks: the key bright and the verb dim, the same pairing the
/// status bar draws its hints in, because that bar is where the eye already
/// reads keys. The keys arrive resolved against the live mode stack (see
/// [`Input::set_exits`]); an exit with no live key draws nothing — the
/// panel-of-keys rule — and a `·` between the two, the separator the help
/// heading uses, so the row reads as two exits and not one sentence.
fn exit_hints(host: &Host, c: ChromePalette, exits: &Option<(SharedString, SharedString)>) -> Div {
    let Some((accept, cancel)) = exits else {
        return div();
    };
    let pair = |key: &SharedString, label: &'static str| {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(gap_s(&host.font))
            .child(div().flex_none().text_color(rgb(c.fg)).child(key.clone()))
            .child(div().flex_none().text_color(rgb(c.dim)).child(label))
    };
    let mut row = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(gap_m(&host.font));
    if !accept.is_empty() {
        row = row.child(pair(accept, "accept"));
    }
    if !accept.is_empty() && !cancel.is_empty() {
        row = row.child(div().flex_none().text_color(rgb(c.faint)).child("·"));
    }
    if !cancel.is_empty() {
        row = row.child(pair(cancel, "cancel"));
    }
    row
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
    use std::cell::RefCell;
    use std::rc::Rc;

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
        input.update(cx, |input, cx| {
            input.move_to(1);
            input.replace_marked(None, "😀x", Some(2..3), cx);
            assert_eq!(input.value(), "a😀xb");
            assert_eq!(input.marked, Some(1..6));
            assert_eq!(input.selected, 5..6);

            input.replace(None, "é", cx);
            assert_eq!(input.value(), "aéb");
            assert_eq!(input.marked, None);
            assert_eq!(input.selected, 3..3);
        });
    }

    #[gpui::test]
    fn every_edit_from_either_choke_point_is_announced(cx: &mut TestAppContext) {
        // A live consumer — the search prompt filtering as you type — sees
        // each edit through [`Event::Edited`], from both places content can
        // change, without polling a field it does not own.
        let input = input("ab", cx);
        let seen: Rc<RefCell<Vec<String>>> = Rc::default();
        let sink = seen.clone();
        input.update(cx, |_, cx| {
            cx.subscribe(&input, move |_, _, event: &super::Event, _| {
                if let super::Event::Edited(text) = event {
                    sink.borrow_mut().push(text.clone());
                }
            })
            .detach();
        });
        input.update(cx, |input, cx| {
            input.move_to(input.content.len());
            input.replace(None, "c", cx); // the keystroke path
            input.replace_marked(None, "de", None, cx); // the composition path
        });
        assert_eq!(*seen.borrow(), vec!["abc".to_string(), "abcde".to_string()]);
    }

    #[gpui::test]
    fn cursor_motion_and_deletion_follow_graphemes(cx: &mut TestAppContext) {
        let input = input("éx", cx);
        input.update(cx, |input, cx| {
            input.move_to(input.content.len());
            let before_x = input.previous_boundary(input.cursor());
            input.move_to(before_x);
            assert_eq!(input.cursor(), "é".len());
            let before_combined = input.previous_boundary(input.cursor());
            assert_eq!(before_combined, 0, "the accent stayed with its base");
            input.selected = before_combined..input.cursor();
            input.replace(None, "", cx);
            assert_eq!(input.value(), "x");
        });
    }
}
