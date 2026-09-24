//! A single-line text field, adapted from GPUI's `examples/input.rs` (Apache-2.0).
//!
//! Adds what the example leaves out: a masked mode for passwords, `Submit`/`Changed` events for
//! the owning view, programmatic `set_text`/`clear`, and theme colours.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardEntry, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ShapedLine, SharedString, Style, TextRun, WrappedLine, UTF16Selection, UnderlineStyle, Window, actions, div,
    fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::theme::{ActiveTheme as _, radius, text};

actions!(
    rhein_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
        Submit,
        Up,
        Down,
        Newline,
        Cancel,
        Tab,
    ]
);

pub const CONTEXT: &str = "TextInput";
const MASK: char = '•';

pub fn bindings() -> Vec<KeyBinding> {
    let ctx = Some(CONTEXT);
    let mut out = vec![
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new("enter", Submit, ctx),
        KeyBinding::new("shift-enter", Newline, ctx),
        KeyBinding::new("up", Up, ctx),
        KeyBinding::new("down", Down, ctx),
        KeyBinding::new("escape", Cancel, ctx),
        KeyBinding::new("tab", Tab, ctx),
    ];
    for m in ["ctrl", "cmd"] {
        out.push(KeyBinding::new(&format!("{m}-a"), SelectAll, ctx));
        out.push(KeyBinding::new(&format!("{m}-v"), Paste, ctx));
        out.push(KeyBinding::new(&format!("{m}-c"), Copy, ctx));
        out.push(KeyBinding::new(&format!("{m}-x"), Cut, ctx));
    }
    out
}

#[derive(Debug, Clone)]
pub enum InputEvent {
    Submit,
    Changed,
    /// Up was pressed with the cursor on the first line (e.g. "edit my last message").
    UpAtStart,
    /// Escape; the event also keeps propagating so an enclosing modal can close.
    Cancel,
    /// The clipboard held files or an image rather than text.
    PasteFiles(ClipboardItem),
    /// While navigation is captured (an open suggestion list), keys that would move the cursor
    /// or submit are reported instead.
    Navigate(Nav),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    Accept,
    Close,
}

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    masked: bool,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    multiline: bool,
    max_lines: usize,
    multi: Option<MultiLayout>,
    scroll_top: Pixels,
    capture_nav: bool,
}

/// The last painted multi-line layout: one wrapped line per paragraph, with its byte offset
/// in the content and its y offset from the top of the text (before scrolling).
struct MultiLayout {
    paragraphs: Vec<(usize, WrappedLine, Pixels)>,
    line_height: Pixels,
    total: Pixels,
}

impl EventEmitter<InputEvent> for TextInput {}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: SharedString::default(),
            placeholder: placeholder.into(),
            masked: false,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
            multiline: false,
            max_lines: 1,
            multi: None,
            scroll_top: px(0.),
            capture_nav: false,
        }
    }

    /// Wraps text and grows up to `max_lines` before scrolling; Shift+Enter inserts a newline.
    pub fn multiline(mut self, max_lines: usize) -> Self {
        self.multiline = true;
        self.max_lines = max_lines.max(1);
        self
    }

    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    pub fn with_text(mut self, text: impl Into<SharedString>) -> Self {
        self.content = text.into();
        self.selected_range = self.content.len()..self.content.len();
        self
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn set_placeholder(&mut self, placeholder: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    pub fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.content = text.into();
        self.selected_range = self.content.len()..self.content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    /// Byte offset of the caret.
    pub fn cursor(&self) -> usize {
        self.cursor_offset()
    }

    /// Replaces `range` (byte offsets) and puts the caret after the new text.
    pub fn replace_range(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        let range = range.start.min(self.content.len())..range.end.min(self.content.len());
        self.content = (self.content[..range.start].to_owned() + text + &self.content[range.end..]).into();
        let at = range.start + text.len();
        self.selected_range = at..at;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    /// Inserts at the caret, replacing any selection.
    pub fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let range = self.selected_range.clone();
        self.replace_range(range, text, cx);
    }

    /// Route Up/Down/Enter/Tab/Escape to `InputEvent::Navigate` (for a suggestion popup).
    pub fn capture_navigation(&mut self, capture: bool) {
        self.capture_nav = capture;
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.set_text("", cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        if self.capture_nav {
            cx.emit(InputEvent::Navigate(Nav::Accept));
            return;
        }
        cx.emit(InputEvent::Submit);
    }

    fn tab(&mut self, _: &Tab, _: &mut Window, cx: &mut Context<Self>) {
        if self.capture_nav {
            cx.emit(InputEvent::Navigate(Nav::Accept));
        } else {
            cx.propagate();
        }
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            self.replace_text_in_range(None, "\n", window, cx);
        }
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if self.capture_nav {
            cx.emit(InputEvent::Navigate(Nav::Close));
            return;
        }
        cx.emit(InputEvent::Cancel);
        cx.propagate();
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if self.capture_nav {
            cx.emit(InputEvent::Navigate(Nav::Up));
            return;
        }
        self.vertical(-1.0, cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if self.capture_nav {
            cx.emit(InputEvent::Navigate(Nav::Down));
            return;
        }
        self.vertical(1.0, cx);
    }

    /// Moves the cursor one visual line up or down, keeping its x position.
    fn vertical(&mut self, direction: f32, cx: &mut Context<Self>) {
        let here = self.multi_position(self.cursor_offset());
        let Some((here, lh)) = here.zip(self.multi.as_ref().map(|m| m.line_height)) else {
            if direction < 0.0 {
                cx.emit(InputEvent::UpAtStart);
            }
            return;
        };
        let target_y = here.y + lh * direction + lh / 2.0;
        if target_y < px(0.) {
            if direction < 0.0 {
                cx.emit(InputEvent::UpAtStart);
            }
            self.move_to(0, cx);
            return;
        }
        let total = self.multi.as_ref().map_or(px(0.), |m| m.total);
        if target_y > total {
            self.move_to(self.content.len(), cx);
            return;
        }
        let index = self.multi_index(point(here.x, target_y));
        self.move_to(index, cx);
    }

    /// Cursor position relative to the top-left of the text (unscrolled), multi-line only.
    fn multi_position(&self, index: usize) -> Option<Point<Pixels>> {
        let layout = self.multi.as_ref()?;
        let (start, line, y) = layout
            .paragraphs
            .iter()
            .rev()
            .find(|(start, _, _)| *start <= index)?;
        let local = line.position_for_index(index - start, layout.line_height)?;
        Some(point(local.x, local.y + *y))
    }

    fn multi_index(&self, at: Point<Pixels>) -> usize {
        let Some(layout) = self.multi.as_ref() else { return 0 };
        let Some((start, line, y)) = layout
            .paragraphs
            .iter()
            .rev()
            .find(|(_, _, y)| *y <= at.y)
            .or(layout.paragraphs.first())
        else {
            return 0;
        };
        let local = point(at.x.max(px(0.)), (at.y - *y).max(px(0.)));
        let offset = match line.closest_index_for_position(local, layout.line_height) {
            Ok(i) | Err(i) => i,
        };
        (start + offset).min(self.content.len())
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                return;
            }
            self.select_to(prev, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                return;
            }
            self.select_to(next, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else { return };
        let has_files = item
            .entries()
            .iter()
            .any(|e| matches!(e, ClipboardEntry::Image(_) | ClipboardEntry::ExternalPaths(_)));
        if has_files {
            cx.emit(InputEvent::PasteFiles(item));
            return;
        }
        if let Some(text) = item.text() {
            let text = if self.multiline { text } else { text.replace('\n', " ") };
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.masked {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.masked {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    /// Content byte offset → offset in what is drawn (the same, unless masked).
    fn display_offset(&self, offset: usize) -> usize {
        if self.masked {
            self.content[..offset].chars().count() * MASK.len_utf8()
        } else {
            offset
        }
    }

    fn content_offset(&self, offset: usize) -> usize {
        if self.masked {
            let chars = offset / MASK.len_utf8();
            self.content.char_indices().nth(chars).map_or(self.content.len(), |(i, _)| i)
        } else {
            offset
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        if self.multiline {
            let Some(bounds) = self.last_bounds else { return 0 };
            let local = point(position.x - bounds.left(), position.y - bounds.top() + self.scroll_top);
            return self.multi_index(local);
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref()) else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        self.content_offset(line.closest_index_for_x(position.x - bounds.left()))
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        self.marked_range = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        if self.multiline {
            let lh = self.multi.as_ref()?.line_height;
            let start = self.multi_position(range.start)?;
            let top = bounds.top() + start.y - self.scroll_top;
            return Some(Bounds::new(point(bounds.left() + start.x, top), size(px(2.), lh)));
        }
        let last_layout = self.last_layout.as_ref()?;
        Some(Bounds::from_corners(
            point(bounds.left() + last_layout.x_for_index(self.display_offset(range.start)), bounds.top()),
            point(bounds.left() + last_layout.x_for_index(self.display_offset(range.end)), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        if self.multiline {
            let bounds = self.last_bounds?;
            let local = gpui::point(point.x - bounds.left(), point.y - bounds.top() + self.scroll_top);
            return Some(self.offset_to_utf16(self.multi_index(local)));
        }
        let line_point = self.last_bounds?.localize(&point)?;
        let last_layout = self.last_layout.as_ref()?;
        let index = last_layout.index_for_x(point.x - line_point.x)?;
        Some(self.offset_to_utf16(self.content_offset(index)))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
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
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.display_offset(input.selected_range.start)..input.display_offset(input.selected_range.end);
        let cursor = input.display_offset(input.cursor_offset());
        let style = window.text_style();
        let t = cx.theme().clone();

        let (display_text, text_color): (SharedString, gpui::Hsla) = if content.is_empty() {
            (input.placeholder.clone(), t.text_faint.into())
        } else if input.masked {
            (MASK.to_string().repeat(content.chars().count()).into(), style.color)
        } else {
            (content, style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = match input.marked_range.as_ref().filter(|_| !input.masked && !input.content.is_empty()) {
            Some(marked) => vec![
                TextRun { len: marked.start, ..run.clone() },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle { color: Some(run.color), thickness: px(1.0), wavy: false }),
                    ..run.clone()
                },
                TextRun { len: display_text.len() - marked.end, ..run },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect(),
            None => vec![run],
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window.text_system().shape_line(display_text, font_size, &runs, None);

        let (selection, cursor) = if selected_range.is_empty() {
            let x = if input.content.is_empty() { px(0.) } else { line.x_for_index(cursor) };
            (
                None,
                Some(fill(
                    Bounds::new(point(bounds.left() + x, bounds.top()), size(px(2.), bounds.bottom() - bounds.top())),
                    t.accent,
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(bounds.left() + line.x_for_index(selected_range.start), bounds.top()),
                        point(bounds.left() + line.x_for_index(selected_range.end), bounds.bottom()),
                    ),
                    t.selection,
                )),
                None,
            )
        };
        PrepaintState { line: Some(line), cursor, selection }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.input.clone()), cx);
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection)
        }
        let line = prepaint.line.take().unwrap();
        line.paint(bounds.origin, window.line_height(), gpui::TextAlign::Left, None, window, cx)
            .ok();
        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
        let empty = self.input.read(cx).content.is_empty();
        self.input.update(cx, |input, _cx| {
            // With the placeholder showing, the layout is not the content's.
            input.last_layout = (!empty).then_some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let focused = self.focus_handle.is_focused(window);
        div()
            .flex()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::tab))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .w_full()
            .px_3()
            .py_2()
            .rounded(radius::MD)
            .bg(t.bg_raised)
            .border_1()
            .border_color(if focused { t.accent } else { t.border })
            .text_size(text::MD)
            .text_color(t.text)
            .overflow_hidden()
            .map(|d| {
                if self.multiline {
                    d.child(MultiLineElement { input: cx.entity() })
                } else {
                    d.child(TextElement { input: cx.entity() })
                }
            })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// The multi-line field: one wrapped line per paragraph, measured to its content height up to
/// `max_lines`, scrolled so the cursor stays visible.
struct MultiLineElement {
    input: Entity<TextInput>,
}

struct MultiPrepaint {
    paragraphs: Vec<(usize, WrappedLine, Pixels)>,
    line_height: Pixels,
    total: Pixels,
    scroll: Pixels,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
}

impl IntoElement for MultiLineElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// What gets shaped: the content, or the placeholder in the faint colour when empty.
fn multi_source(input: &TextInput, window: &Window, cx: &App) -> (SharedString, TextRun, Pixels) {
    let style = window.text_style();
    let t = cx.theme();
    let (text, color) = if input.content.is_empty() {
        (input.placeholder.clone(), t.text_faint.into())
    } else {
        (input.content.clone(), style.color)
    };
    let run = TextRun {
        len: 0,
        font: style.font(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    (text, run, style.font_size.to_pixels(window.rem_size()))
}

fn shape_paragraphs(
    text: &str,
    run: &TextRun,
    font_size: Pixels,
    width: Pixels,
    line_height: Pixels,
    window: &Window,
) -> (Vec<(usize, WrappedLine, Pixels)>, Pixels) {
    let mut out = Vec::new();
    let mut y = px(0.);
    let mut start = 0;
    for paragraph in text.split('\n') {
        let runs = [TextRun { len: paragraph.len(), ..run.clone() }];
        let shaped = window
            .text_system()
            .shape_text(paragraph.to_owned().into(), font_size, &runs, Some(width), None)
            .ok()
            .and_then(|mut lines| lines.pop());
        if let Some(line) = shaped {
            let height = line.size(line_height).height.max(line_height);
            out.push((start, line, y));
            y += height;
        }
        start += paragraph.len() + 1;
    }
    (out, y)
}

impl Element for MultiLineElement {
    type RequestLayoutState = ();
    type PrepaintState = MultiPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let input = self.input.read(cx);
        let (text, run, font_size) = multi_source(input, window, cx);
        let line_height = window.line_height();
        let max_height = line_height * input.max_lines as f32;
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let layout = window.request_measured_layout(style, move |known, available, window, _cx| {
            let width = known.width.unwrap_or(match available.width {
                gpui::AvailableSpace::Definite(w) => w,
                _ => px(400.),
            });
            let (_, total) = shape_paragraphs(&text, &run, font_size, width, line_height, window);
            size(width, total.max(line_height).min(max_height))
        });
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let t = cx.theme().clone();
        let (text, run, font_size) = multi_source(input, window, cx);
        let line_height = window.line_height();
        let (paragraphs, total) = shape_paragraphs(&text, &run, font_size, bounds.size.width, line_height, window);
        let empty = input.content.is_empty();

        let position = |index: usize| -> Point<Pixels> {
            if empty {
                return point(px(0.), px(0.));
            }
            paragraphs
                .iter()
                .rev()
                .find(|(start, _, _)| *start <= index)
                .and_then(|(start, line, y)| {
                    line.position_for_index(index - start, line_height).map(|p| point(p.x, p.y + *y))
                })
                .unwrap_or(point(px(0.), px(0.)))
        };

        // Keep the cursor inside the visible window.
        let cursor_at = position(input.cursor_offset());
        let visible = bounds.size.height;
        let mut scroll = input.scroll_top;
        if cursor_at.y < scroll {
            scroll = cursor_at.y;
        }
        if cursor_at.y + line_height > scroll + visible {
            scroll = cursor_at.y + line_height - visible;
        }
        scroll = scroll.max(px(0.)).min((total - visible).max(px(0.)));

        let origin = point(bounds.left(), bounds.top() - scroll);
        let range = input.selected_range.clone();
        let mut selection = Vec::new();
        let cursor = if range.is_empty() {
            Some(fill(
                Bounds::new(point(origin.x + cursor_at.x, origin.y + cursor_at.y), size(px(2.), line_height)),
                t.accent,
            ))
        } else {
            let a = position(range.start);
            let b = position(range.end);
            let mut row = a.y;
            while row <= b.y + px(0.5) {
                let left = if (row - a.y).abs() < px(0.5) { a.x } else { px(0.) };
                let right = if (row - b.y).abs() < px(0.5) { b.x } else { bounds.size.width };
                if right > left {
                    selection.push(fill(
                        Bounds::from_corners(
                            point(origin.x + left, origin.y + row),
                            point(origin.x + right, origin.y + row + line_height),
                        ),
                        t.selection,
                    ));
                }
                row += line_height;
            }
            None
        };
        MultiPrepaint { paragraphs, line_height, total, scroll, cursor, selection }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.input.clone()), cx);
        let scroll = prepaint.scroll;
        let line_height = prepaint.line_height;
        let paragraphs = std::mem::take(&mut prepaint.paragraphs);
        let selection = std::mem::take(&mut prepaint.selection);
        let cursor = prepaint.cursor.take();
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for quad in selection {
                window.paint_quad(quad);
            }
            for (_, line, y) in &paragraphs {
                let at = point(bounds.left(), bounds.top() + *y - scroll);
                line.paint(at, line_height, gpui::TextAlign::Left, None, window, cx).ok();
            }
            if focus_handle.is_focused(window)
                && let Some(cursor) = cursor
            {
                window.paint_quad(cursor);
            }
        });
        let empty = self.input.read(cx).content.is_empty();
        let total = prepaint.total;
        self.input.update(cx, |input, _cx| {
            input.multi = (!empty).then_some(MultiLayout { paragraphs, line_height, total });
            input.scroll_top = scroll;
            input.last_bounds = Some(bounds);
        });
    }
}
