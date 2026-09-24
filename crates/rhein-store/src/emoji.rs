//! The Unicode emoji catalogue (from the `emojis` crate) grouped and searchable like the web
//! client's picker, with skin tones.

pub use emojis::{Emoji, Group};

/// Picker categories in display order, with the emoji used as each tab's icon.
pub const GROUPS: [(Group, &str, &str); 9] = [
    (Group::SmileysAndEmotion, "Smileys & emotion", "😀"),
    (Group::PeopleAndBody, "People & body", "👋"),
    (Group::AnimalsAndNature, "Animals & nature", "🌿"),
    (Group::FoodAndDrink, "Food & drink", "🍕"),
    (Group::TravelAndPlaces, "Travel & places", "✈️"),
    (Group::Activities, "Activities", "⚽"),
    (Group::Objects, "Objects", "💡"),
    (Group::Symbols, "Symbols", "🔣"),
    (Group::Flags, "Flags", "🏁"),
];

/// Looks up a single emoji by its characters.
pub fn get(s: &str) -> Option<&'static Emoji> {
    emojis::get(s)
}

/// Whether a grapheme should be drawn with the colour emoji font. Anything in the Unicode emoji
/// set qualifies, except plain ASCII/Latin-1 characters such as `#`, `1` or `©` that are only
/// emoji with an explicit emoji presentation selector (U+FE0F) or a keycap.
pub fn is_emoji(grapheme: &str) -> bool {
    let Some(first) = grapheme.chars().next() else { return false };
    if (first as u32) < 0x2000 && !grapheme.contains('\u{FE0F}') && !grapheme.contains('\u{20E3}') {
        return false;
    }
    emojis::get(grapheme).is_some() || (first as u32) >= 0x1F000
}

/// Byte ranges of the emoji in `text`, adjacent emoji merged into one range.
pub fn emoji_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    for (at, g) in unicode_segmentation::UnicodeSegmentation::grapheme_indices(text, true) {
        if is_emoji(g) {
            match out.last_mut() {
                Some(last) if last.end == at => last.end = at + g.len(),
                _ => out.push(at..at + g.len()),
            }
        }
    }
    out
}

/// Skin tones offered in the picker: 0 = default, 1–5 = light … dark.
pub const TONES: [&str; 6] = ["✋", "✋🏻", "✋🏼", "✋🏽", "✋🏾", "✋🏿"];

/// The emoji of a group, base forms only (tones are applied when shown).
pub fn group(group: Group) -> impl Iterator<Item = &'static Emoji> {
    group.emojis().filter(|e| e.skin_tone().is_none_or(|t| t == emojis::SkinTone::Default))
}

/// Applies a skin tone (1–5) when the emoji has variants.
pub fn with_tone(emoji: &'static Emoji, tone: u8) -> &'static Emoji {
    let tone = match tone {
        1 => emojis::SkinTone::Light,
        2 => emojis::SkinTone::MediumLight,
        3 => emojis::SkinTone::Medium,
        4 => emojis::SkinTone::MediumDark,
        5 => emojis::SkinTone::Dark,
        _ => return emoji,
    };
    emoji.with_skin_tone(tone).unwrap_or(emoji)
}

/// Case-insensitive match on name and shortcodes; shortcode prefix matches first.
pub fn search(query: &str, limit: usize) -> Vec<&'static Emoji> {
    let q = query.trim().trim_matches(':').to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut prefix = Vec::new();
    let mut contains = Vec::new();
    for e in emojis::iter().filter(|e| e.skin_tone().is_none_or(|t| t == emojis::SkinTone::Default)) {
        if e.shortcodes().any(|s| s.starts_with(&q)) {
            prefix.push(e);
        } else if e.name().to_lowercase().contains(&q) || e.shortcodes().any(|s| s.contains(&q)) {
            contains.push(e);
        }
    }
    prefix.extend(contains);
    prefix.truncate(limit);
    prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_by_shortcode_and_name() {
        let found = search(":thumbs", 5);
        assert!(found.iter().any(|e| e.as_str() == "👍"));
        assert_eq!(search("tada", 1)[0].as_str(), "🎉");
        assert!(search("", 5).is_empty());
    }

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
    fn groups_and_tones() {
        assert!(group(Group::SmileysAndEmotion).count() > 50);
        let wave = emojis::get("👋").unwrap();
        assert_eq!(with_tone(wave, 5).as_str(), "👋🏿");
        assert_eq!(with_tone(wave, 0).as_str(), "👋");
    }
}
