//! Message text → a small document tree, with the same grammar as the Telium web client
//! (`web/src/lib/markdown.jsx`), so a message reads the same in both clients.
//!
//! The grammar is deliberately small: fenced code, `> ` quotes, and inline tokens found by one
//! leftmost-first scan. Emphasis tokens forbid their own delimiter inside themselves and hand their
//! contents back to the scan, so they can hold code, mentions and spoilers. There is no escaping.

use std::sync::LazyLock;

use regex::Regex;

const ULID: &str = "[A-Z0-9]{26}";
const USER_ID: &str = r"@[a-z0-9._-]{1,32}:[a-zA-Z0-9-]+(?:\.[a-zA-Z0-9-]+)*(?::[0-9]{2,5})?";
const CUSTOM_EMOJI: &str = r"<[a-zA-Z0-9_.-]{1,32}:[A-Z0-9]{26}(?:@[a-zA-Z0-9.-]+)?>";

/// One alternation, in the web client's priority order. Group numbers matter below.
static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*]+\*)|(~~[^~]+~~)|(\|\|[^|]+\|\|)|(<@&{ULID}>)|(<#{ULID}>)|({CUSTOM_EMOJI})|(<{USER_ID}>|{USER_ID})|(@(?:everyone|here))|(https?://\S+)"
    ))
    .expect("token regex")
});

static FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"```(?:[a-zA-Z0-9+-]*)\n?((?s:.*?))```").expect("fence regex"));

static FORWARD_GUILD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^telium://forward/([A-Za-z0-9_-]+)/([A-Za-z0-9_-]+)/([A-Za-z0-9_-]+)[ \t]*(?:\n|$)").unwrap()
});
static FORWARD_CONVO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^telium://forward/convo/([A-Za-z0-9_-]+)/([A-Za-z0-9_-]+)[ \t]*(?:\n|$)").unwrap());
static FORWARD_BARE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^telium://forwarded[ \t]*(?:\n|$)").unwrap());

static CUSTOM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<([a-zA-Z0-9_.-]{1,32}):([A-Z0-9]{26})(?:@([a-zA-Z0-9.-]+))?>").expect("emoji regex")
});

static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s<>"']+"#).expect("link regex"));
static INLINE_CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`[^`]*`").expect("code regex"));

/// Previews per message, as in the web client.
pub const MAX_EMBEDS: usize = 3;
const JUMBO_MAX: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    Text(String),
    Code(String),
    Bold(Vec<Span>),
    Italic(Vec<Span>),
    Strike(Vec<Span>),
    Spoiler(Vec<Span>),
    Role(String),
    Channel(String),
    Emoji { name: String, id: String, origin: Option<String> },
    /// A user id such as `@alice:a.example`.
    User(String),
    /// `@everyone` or `@here`.
    Broadcast(String),
    Link(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// Lines of prose; newlines stay inside the text spans.
    Text(Vec<Span>),
    /// A run of consecutive `> ` lines.
    Quote(Vec<Span>),
    CodeBlock(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forward {
    Guild { guild_id: String, channel_id: String, message_id: String },
    Conversation { conversation_id: String, message_id: String },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Document {
    pub forwarded: Option<Forward>,
    pub blocks: Vec<Block>,
    /// The body is 1–6 emoji and nothing else: draw them large.
    pub jumbo: bool,
}

pub fn parse(content: &str) -> Document {
    let (forwarded, body) = take_forwards(content);
    let mut blocks = Vec::new();
    let mut last = 0;
    for m in FENCE.captures_iter(body) {
        let whole = m.get(0).unwrap();
        if whole.start() > last {
            inline(&body[last..whole.start()], &mut blocks);
        }
        blocks.push(Block::CodeBlock(m[1].to_owned()));
        last = whole.end();
    }
    if last < body.len() {
        inline(&body[last..], &mut blocks);
    }
    Document { forwarded, blocks, jumbo: is_jumbo(body) }
}

fn take_forwards(content: &str) -> (Option<Forward>, &str) {
    let mut rest = content;
    let mut origin = None;
    loop {
        if let Some(c) = FORWARD_GUILD.captures(rest) {
            origin.get_or_insert(Forward::Guild {
                guild_id: c[1].to_owned(),
                channel_id: c[2].to_owned(),
                message_id: c[3].to_owned(),
            });
            rest = &rest[c.get(0).unwrap().end()..];
        } else if let Some(c) = FORWARD_CONVO.captures(rest) {
            origin.get_or_insert(Forward::Conversation {
                conversation_id: c[1].to_owned(),
                message_id: c[2].to_owned(),
            });
            rest = &rest[c.get(0).unwrap().end()..];
        } else if let Some(c) = FORWARD_BARE.find(rest) {
            origin.get_or_insert(Forward::Unknown);
            rest = &rest[c.end()..];
        } else {
            return (origin, rest);
        }
    }
}

/// Splits prose into text and quote blocks; consecutive `> ` lines form one quote.
fn inline(text: &str, blocks: &mut Vec<Block>) {
    let mut prose: Vec<Span> = Vec::new();
    let mut quote: Option<Vec<Span>> = None;
    let mut first_prose_line = true;
    for line in text.split('\n') {
        if let Some(rest) = line.strip_prefix("> ") {
            if !prose.is_empty() {
                blocks.push(Block::Text(std::mem::take(&mut prose)));
            }
            first_prose_line = true;
            let q = quote.get_or_insert_with(Vec::new);
            if !q.is_empty() {
                push_text(q, "\n");
            }
            extend(q, spans(rest));
        } else {
            if let Some(q) = quote.take() {
                blocks.push(Block::Quote(q));
                first_prose_line = true;
            }
            if !first_prose_line {
                push_text(&mut prose, "\n");
            }
            first_prose_line = false;
            extend(&mut prose, spans(line));
        }
    }
    if let Some(q) = quote {
        blocks.push(Block::Quote(q));
    }
    // A fence boundary leaves a lone newline behind; it carries nothing.
    let meaningful = prose.iter().any(|s| !matches!(s, Span::Text(t) if t.trim().is_empty()));
    if meaningful {
        blocks.push(Block::Text(prose));
    }
}

/// Appends, merging a leading text span into a trailing one.
fn extend(out: &mut Vec<Span>, more: Vec<Span>) {
    for span in more {
        match span {
            Span::Text(t) => push_text(out, &t),
            other => out.push(other),
        }
    }
}

fn push_text(out: &mut Vec<Span>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(Span::Text(last)) = out.last_mut() {
        last.push_str(text);
    } else {
        out.push(Span::Text(text.to_owned()));
    }
}

/// The inline scan. `@everyone`/`@here` must not be followed by `[\w.:@-]` (a lookahead in the
/// web client's regex); when it is, those characters are plain text and the scan moves on.
pub fn spans(line: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut last = 0;
    let mut at = 0;
    while let Some(c) = TOKEN.captures_at(line, at) {
        let whole = c.get(0).unwrap();
        let raw = whole.as_str();
        if c.get(10).is_some() {
            let next = line[whole.end()..].chars().next();
            if next.is_some_and(|ch| ch.is_alphanumeric() || "_.:@-".contains(ch)) {
                at = whole.start() + 1;
                continue;
            }
        }
        push_text(&mut out, &line[last..whole.start()]);
        let inner = |open: usize, close: usize| &raw[open..raw.len() - close];
        let span = if c.get(1).is_some() {
            Span::Code(inner(1, 1).to_owned())
        } else if c.get(2).is_some() {
            Span::Bold(spans(inner(2, 2)))
        } else if c.get(3).is_some() {
            Span::Italic(spans(inner(1, 1)))
        } else if c.get(4).is_some() {
            Span::Strike(spans(inner(2, 2)))
        } else if c.get(5).is_some() {
            Span::Spoiler(spans(inner(2, 2)))
        } else if c.get(6).is_some() {
            Span::Role(inner(3, 1).to_owned())
        } else if c.get(7).is_some() {
            Span::Channel(inner(2, 1).to_owned())
        } else if c.get(8).is_some() {
            let e = CUSTOM.captures(raw).unwrap();
            Span::Emoji { name: e[1].to_owned(), id: e[2].to_owned(), origin: e.get(3).map(|o| o.as_str().to_owned()) }
        } else if c.get(9).is_some() {
            Span::User(raw.trim_start_matches('<').trim_end_matches('>').to_owned())
        } else if c.get(10).is_some() {
            Span::Broadcast(raw.to_owned())
        } else {
            Span::Link(raw.to_owned())
        };
        out.push(span);
        last = whole.end();
        at = whole.end();
    }
    push_text(&mut out, &line[last..]);
    out
}

/// 1–6 emoji (custom or unicode) and whitespace, nothing else.
fn is_jumbo(body: &str) -> bool {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return false;
    }
    let mut count = CUSTOM.find_iter(trimmed).count();
    let rest = CUSTOM.replace_all(trimmed, " ");
    for grapheme in unicode_segmentation::UnicodeSegmentation::graphemes(rest.as_ref(), true) {
        if grapheme.trim().is_empty() {
            continue;
        }
        if !is_emoji_grapheme(grapheme) {
            return false;
        }
        count += 1;
    }
    (1..=JUMBO_MAX).contains(&count)
}

fn is_emoji_grapheme(g: &str) -> bool {
    let mut chars = g.chars();
    let Some(first) = chars.next() else { return false };
    let cp = first as u32;
    // Keycaps: a digit, # or * followed by U+20E3.
    if (first.is_ascii_digit() || first == '#' || first == '*') && g.contains('\u{20E3}') {
        return true;
    }
    matches!(cp,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2300..=0x23FF | 0x2B00..=0x2BFF
        | 0x2190..=0x21FF | 0x25A0..=0x25FF | 0x2934..=0x2935 | 0x3030 | 0x303D | 0x3297 | 0x3299
        | 0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x24C2)
}

/// URLs worth a preview, like the web client's `linksIn`: outside code, trailing punctuation
/// trimmed, deduplicated, at most [`MAX_EMBEDS`].
pub fn links_in(content: &str) -> Vec<String> {
    let without_fences = FENCE.replace_all(content, " ");
    let without_code = INLINE_CODE.replace_all(&without_fences, " ");
    let mut out: Vec<String> = Vec::new();
    for m in LINK.find_iter(&without_code) {
        let url = m.as_str().trim_end_matches([')', ']', '}', '>', '.', ',', ';', ':', '!', '?', '\'', '"']);
        if url.len() > "https://".len() && !out.iter().any(|u| u == url) {
            out.push(url.to_owned());
            if out.len() == MAX_EMBEDS {
                break;
            }
        }
    }
    out
}

/// Every user id the text mentions (for highlighting a message that mentions you).
pub fn mentions(doc: &Document, user_id: &str) -> bool {
    fn walk(spans: &[Span], user_id: &str) -> bool {
        spans.iter().any(|s| match s {
            Span::User(u) => u == user_id,
            Span::Broadcast(_) => true,
            Span::Bold(i) | Span::Italic(i) | Span::Strike(i) | Span::Spoiler(i) => walk(i, user_id),
            _ => false,
        })
    }
    doc.blocks.iter().any(|b| match b {
        Block::Text(s) | Block::Quote(s) => walk(s, user_id),
        Block::CodeBlock(_) => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Span {
        Span::Text(s.into())
    }

    #[test]
    fn plain_and_newlines() {
        let d = parse("hello\nworld");
        assert_eq!(d.blocks, vec![Block::Text(vec![text("hello\nworld")])]);
        assert!(!d.jumbo);
    }

    #[test]
    fn emphasis_nests_tokens() {
        let s = spans("**bold `code` <@alice:a.x>** *it* ~~no~~ ||secret||");
        assert_eq!(
            s,
            vec![
                Span::Bold(vec![text("bold "), Span::Code("code".into()), text(" "), Span::User("@alice:a.x".into())]),
                text(" "),
                Span::Italic(vec![text("it")]),
                text(" "),
                Span::Strike(vec![text("no")]),
                text(" "),
                Span::Spoiler(vec![text("secret")]),
            ]
        );
    }

    #[test]
    fn code_is_literal() {
        assert_eq!(spans("`**x**`"), vec![Span::Code("**x**".into())]);
    }

    #[test]
    fn mentions_and_tokens() {
        let role = "<@&01ABCDEFGHJKMNPQRSTVWXYZ00>";
        let chan = "<#01ABCDEFGHJKMNPQRSTVWXYZ00>";
        let s = spans(&format!("{role} {chan} <wave:01ABCDEFGHJKMNPQRSTVWXYZ00@b.x> @bob:b.x @everyone https://x.y/z"));
        assert_eq!(s[0], Span::Role("01ABCDEFGHJKMNPQRSTVWXYZ00".into()));
        assert_eq!(s[2], Span::Channel("01ABCDEFGHJKMNPQRSTVWXYZ00".into()));
        assert_eq!(
            s[4],
            Span::Emoji { name: "wave".into(), id: "01ABCDEFGHJKMNPQRSTVWXYZ00".into(), origin: Some("b.x".into()) }
        );
        assert_eq!(s[6], Span::User("@bob:b.x".into()));
        assert_eq!(s[8], Span::Broadcast("@everyone".into()));
        assert_eq!(s[10], Span::Link("https://x.y/z".into()));
    }

    #[test]
    fn broadcast_needs_a_boundary() {
        assert_eq!(spans("mail @everyone.else"), vec![text("mail @everyone.else")]);
        assert_eq!(spans("@here!"), vec![Span::Broadcast("@here".into()), text("!")]);
    }

    #[test]
    fn fences_and_quotes() {
        let d = parse("before\n```rust\nfn main() {}\n```\n> quoted\n> more\nafter");
        assert_eq!(
            d.blocks,
            vec![
                Block::Text(vec![text("before\n")]),
                Block::CodeBlock("fn main() {}\n".into()),
                Block::Quote(vec![text("quoted\nmore")]),
                Block::Text(vec![text("after")]),
            ]
        );
    }

    #[test]
    fn forwards() {
        let d = parse("telium://forward/g1/c1/m1\ntelium://forwarded\nhi");
        assert_eq!(
            d.forwarded,
            Some(Forward::Guild { guild_id: "g1".into(), channel_id: "c1".into(), message_id: "m1".into() })
        );
        assert_eq!(d.blocks, vec![Block::Text(vec![text("hi")])]);
    }

    #[test]
    fn jumbo() {
        assert!(parse("🎉").jumbo);
        assert!(parse("👍 ❤️ <x:01ABCDEFGHJKMNPQRSTVWXYZ00>").jumbo);
        assert!(!parse("🎉 yay").jumbo);
        assert!(!parse("🎉🎉🎉🎉🎉🎉🎉").jumbo);
    }

    #[test]
    fn links() {
        assert_eq!(
            links_in("see https://a.x/p). and `https://code.x` and https://a.x/p again, https://b.x!"),
            vec!["https://a.x/p".to_string(), "https://b.x".to_string()]
        );
        assert_eq!(links_in("```\nhttps://in.fence\n```").len(), 0);
        assert_eq!(links_in("https://1.x https://2.x https://3.x https://4.x").len(), 3);
    }

    #[test]
    fn mention_detection() {
        assert!(mentions(&parse("hey <@me:a.x>"), "@me:a.x"));
        assert!(mentions(&parse("**@here** look"), "@me:a.x"));
        assert!(!mentions(&parse("`@me:a.x`"), "@me:a.x"));
    }
}
