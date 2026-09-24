//! Which parts of a text are emoji, so they can be drawn with the colour emoji font. UI fonts
//! such as Geist carry monochrome glyphs for some emoji (❤ ✈ ☀), and GPUI prefers those.

use std::ops::Range;

use gpui::{Font, TextRun};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::theme::FONT_EMOJI;

/// Anything in the Unicode emoji set, except plain ASCII/Latin-1 characters such as `#`, `1` or
/// `©` that are only emoji with an explicit presentation selector (U+FE0F) or a keycap.
pub fn is_emoji(grapheme: &str) -> bool {
    let Some(first) = grapheme.chars().next() else { return false };
    if (first as u32) < 0x2000 && !grapheme.contains('\u{FE0F}') && !grapheme.contains('\u{20E3}') {
        return false;
    }
    emojis::get(grapheme).is_some() || (first as u32) >= 0x1F000
}

/// Byte ranges of the emoji in `text`, adjacent emoji merged into one range.
pub fn emoji_ranges(text: &str) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for (at, g) in text.grapheme_indices(true) {
        if is_emoji(g) {
            match out.last_mut() {
                Some(last) if last.end == at => last.end = at + g.len(),
                _ => out.push(at..at + g.len()),
            }
        }
    }
    out
}

/// Splits `run` (covering all of `text`) so every emoji range uses the emoji font.
pub fn runs_with_emoji(text: &str, run: &TextRun) -> Vec<TextRun> {
    let emoji_font = Font { family: FONT_EMOJI.into(), ..run.font.clone() };
    let mut out = Vec::new();
    let mut at = 0;
    for r in emoji_ranges(text) {
        if at < r.start {
            out.push(TextRun { len: r.start - at, ..run.clone() });
        }
        out.push(TextRun { len: r.end - r.start, font: emoji_font.clone(), ..run.clone() });
        at = r.end;
    }
    if at < text.len() || out.is_empty() {
        out.push(TextRun { len: text.len() - at, ..run.clone() });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_emoji_but_not_plain_symbols() {
        for e in ["❤️", "❤", "👍🏽", "👩‍💻", "🇩🇪", "✈️", "⚽", "1️⃣"] {
            assert!(is_emoji(e), "{e}");
        }
        for t in ["a", "1", "#", "©", "é", "-"] {
            assert!(!is_emoji(t), "{t}");
        }
        assert_eq!(emoji_ranges("hi 👍👍 ok ❤️"), vec![3..11, 15..21]);
    }

    #[test]
    fn runs_cover_text_and_switch_font_for_emoji() {
        let base = TextRun {
            len: 0,
            font: gpui::font("Geist"),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let text = "ok ❤️ ✈️";
        let runs = runs_with_emoji(text, &base);
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), text.len());
        let families: Vec<&str> = runs.iter().map(|r| r.font.family.as_ref()).collect();
        assert_eq!(families, vec!["Geist", FONT_EMOJI, "Geist", FONT_EMOJI]);
        assert_eq!(runs_with_emoji("", &base).len(), 1);
    }
}
