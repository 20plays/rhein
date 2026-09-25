//! Selecting a message's text, as in a browser but within one text block of one message: press
//! and drag over it, double-click a word, triple-click the whole block; Ctrl+C copies. The
//! selection is drawn as one more highlight on the block's own text (`overlay`), so it needs no
//! painting of its own.

use std::ops::Range;

use gpui::{
    AnyElement, App, Bounds, CursorStyle, DispatchPhase, Element, ElementId, GlobalElementId, HighlightStyle, Hitbox, HitboxBehavior,
    InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, SharedString,
    TextLayout, WeakEntity, Window,
};

use crate::views::chat::ChatView;

/// Which block of which message is selected, and how far.
#[derive(Clone, Debug, PartialEq)]
pub struct TextSelection {
    pub message: String,
    pub block: usize,
    pub anchor: usize,
    pub head: usize,
    pub text: SharedString,
    pub dragging: bool,
}

impl TextSelection {
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    /// The selected text, if any is.
    pub fn selected(&self) -> Option<&str> {
        let r = self.range();
        (!r.is_empty()).then(|| self.text.get(r)).flatten()
    }
}

/// The word around byte `at` (letters, digits and underscores), or the character there.
pub fn word_at(text: &str, at: usize) -> Range<usize> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let at = at.min(text.len());
    let start = text[..at].char_indices().rev().take_while(|(_, c)| is_word(*c)).last().map_or(at, |(i, _)| i);
    let end = text[at..].char_indices().find(|(_, c)| !is_word(*c)).map_or(text.len(), |(i, _)| at + i);
    if start == end {
        // Not in a word: the one character under the pointer.
        let next = text[at..].chars().next().map_or(at, |c| at + c.len_utf8());
        return at..next;
    }
    start..end
}

/// `highlights` (sorted, not overlapping) with `style` laid over `range`, still sorted and not
/// overlapping: the pieces inside `range` get both.
pub fn overlay(highlights: Vec<(Range<usize>, HighlightStyle)>, range: Range<usize>, style: HighlightStyle) -> Vec<(Range<usize>, HighlightStyle)> {
    if range.is_empty() {
        return highlights;
    }
    let mut cuts: Vec<usize> = vec![range.start, range.end];
    for (r, _) in &highlights {
        cuts.push(r.start);
        cuts.push(r.end);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::new();
    for pair in cuts.windows(2) {
        let piece = pair[0]..pair[1];
        let own = highlights.iter().find(|(r, _)| r.start <= piece.start && piece.end <= r.end).map(|(_, s)| *s);
        let selected = range.start <= piece.start && piece.end <= range.end;
        match (own, selected) {
            (Some(s), true) => out.push((piece, HighlightStyle { background_color: style.background_color, color: style.color.or(s.color), ..s })),
            (Some(s), false) => out.push((piece, s)),
            (None, true) => out.push((piece, style)),
            (None, false) => {}
        }
    }
    out
}

/// A text block that can be selected: `child` is its text element, `layout` that element's
/// layout (to find the character under the pointer).
pub struct Selectable {
    id: ElementId,
    message: String,
    block: usize,
    text: SharedString,
    layout: TextLayout,
    view: WeakEntity<ChatView>,
    child: Option<AnyElement>,
}

pub fn selectable(message: &str, block: usize, text: SharedString, layout: TextLayout, view: WeakEntity<ChatView>, child: AnyElement) -> Selectable {
    Selectable {
        id: ElementId::Name(format!("select-{message}-{block}").into()),
        message: message.to_owned(),
        block,
        text,
        layout,
        view,
        child: Some(child),
    }
}

impl IntoElement for Selectable {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Selectable {
    type RequestLayoutState = AnyElement;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState) {
        let mut child = self.child.take().expect("laid out once");
        let layout = child.request_layout(window, cx);
        (layout, child)
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, child: &mut Self::RequestLayoutState, window: &mut Window, cx: &mut App) -> Self::PrepaintState {
        child.prepaint(window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, child: &mut Self::RequestLayoutState, hitbox: &mut Self::PrepaintState, window: &mut Window, cx: &mut App) {
        child.paint(window, cx);
        window.set_cursor_style(CursorStyle::IBeam, hitbox);
        let index = {
            let layout = self.layout.clone();
            move |position| match layout.index_for_position(position) {
                Ok(i) | Err(i) => i,
            }
        };
        let (view, message, block, text) = (self.view.clone(), self.message.clone(), self.block, self.text.clone());
        let (hit, at) = (hitbox.clone(), index.clone());
        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || e.button != MouseButton::Left || !hit.is_hovered(window) {
                return;
            }
            let i = at(e.position).min(text.len());
            let (anchor, head) = match e.click_count {
                1 => (i, i),
                2 => {
                    let w = word_at(&text, i);
                    (w.start, w.end)
                }
                _ => (0, text.len()),
            };
            let selection = TextSelection { message: message.clone(), block, anchor, head, text: text.clone(), dragging: e.click_count == 1 };
            view.update(cx, |v, cx| {
                v.set_selection(Some(selection), cx);
                window.focus(&v.select_focus, cx);
            })
            .ok();
        });
        let (view, message, block) = (self.view.clone(), self.message.clone(), self.block);
        window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
            if phase != DispatchPhase::Bubble || e.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let i = index(e.position);
            view.update(cx, |v, cx| v.drag_selection(&message, block, i, cx)).ok();
        });
        let view = self.view.clone();
        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |v, cx| v.end_selection(cx)).ok();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use gpui::{HighlightStyle, hsla};

    use super::*;

    #[test]
    fn words_and_selections() {
        let text = "hello there, world";
        assert_eq!(&text[word_at(text, 7)], "there");
        // Right after a word still takes the word; between two separators, the one character.
        assert_eq!(&text[word_at(text, 11)], "there");
        assert_eq!(&text[word_at(text, 12)], " ");
        let s = TextSelection { message: "m".into(), block: 0, anchor: 12, head: 6, text: text.into(), dragging: false };
        assert_eq!(s.selected(), Some("there,"));
    }

    #[test]
    fn a_selection_overlays_the_text_styles() {
        let bold = HighlightStyle { font_weight: Some(gpui::FontWeight::BOLD), ..Default::default() };
        let sel = HighlightStyle { background_color: Some(hsla(0.7, 0.5, 0.5, 0.4)), ..Default::default() };
        let out = overlay(vec![(0..5, bold), (6..10, bold)], 3..8, sel);
        let ranges: Vec<_> = out.iter().map(|(r, _)| r.clone()).collect();
        assert_eq!(ranges, vec![0..3, 3..5, 5..6, 6..8, 8..10]);
        assert!(out[1].1.background_color.is_some() && out[1].1.font_weight.is_some());
        assert!(out[0].1.background_color.is_none());
        assert!(out[2].1.background_color.is_some() && out[2].1.font_weight.is_none());
    }
}
