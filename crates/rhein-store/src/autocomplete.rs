//! Composer autocomplete triggers, ported from the web client's `Autocomplete.jsx::tokenAt`.

use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// `@` – people, roles, @everyone/@here.
    Mention,
    /// `#` – channels.
    Channel,
    /// `:` – emoji.
    Emoji,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub trigger: Trigger,
    /// What was typed after the trigger character.
    pub query: String,
    /// Byte offset of the trigger character; the completion replaces `start..caret`.
    pub start: usize,
}

static MENTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|[\s(\[{>])([@#])([^\s@#:]{0,32})$").unwrap());
static EMOJI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|[\s(\[{])(:)([a-zA-Z0-9_+-]{2,32})$").unwrap());

/// The trigger the caret is in, if any. Only at a word boundary: `a@b` is an address.
pub fn token_at(text: &str, caret: usize) -> Option<Token> {
    let before = text.get(..caret)?;
    if let Some(c) = MENTION.captures(before) {
        let trigger = if &c[1] == "@" { Trigger::Mention } else { Trigger::Channel };
        return Some(Token { trigger, query: c[2].to_owned(), start: c.get(1).unwrap().start() });
    }
    let c = EMOJI.captures(before)?;
    Some(Token { trigger: Trigger::Emoji, query: c[2].to_owned(), start: c.get(1).unwrap().start() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers() {
        let t = token_at("hey @an", 7).unwrap();
        assert_eq!((t.trigger, t.query.as_str(), t.start), (Trigger::Mention, "an", 4));
        assert_eq!(token_at("@", 1).unwrap().query, "");
        assert_eq!(token_at("see #gen", 8).unwrap().trigger, Trigger::Channel);
        assert_eq!(token_at("nice :thu", 9).unwrap().trigger, Trigger::Emoji);
        assert!(token_at("mail a@b", 8).is_none());
        assert!(token_at("time 10:3", 9).is_none());
        assert!(token_at("nice :t", 7).is_none());
        assert!(token_at("@anna done", 10).is_none());
    }
}
