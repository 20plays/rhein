//! Renders a parsed message (`rhein_store::markdown`) with GPUI rich text.
//!
//! Inline spans are flattened into one string per block with non-overlapping highlight ranges;
//! nested emphasis combines into one style per leaf. Clickable leaves (links, channel mentions,
//! spoilers) become `InteractiveText` click ranges.

use std::collections::HashSet;
use std::ops::Range;

use gpui::{
    AnyElement, App, Entity, FontStyle, FontWeight, HighlightStyle, Hsla, InteractiveText, IntoElement,
    ParentElement as _, SharedString, StrikethroughStyle, Styled as _, StyledText, UnderlineStyle, WeakEntity,
    div, img, prelude::*, px,
};
use rhein_store::Client;
use rhein_store::markdown::{Block, Document, Forward, Span};
use rhein_ui::theme::{FONT_EMOJI, FONT_MONO, radius, text};
use rhein_ui::{ActiveTheme as _, Icon, Theme, icon};

use crate::views::chat::ChatView;

/// What a click on a range does.
#[derive(Clone)]
enum Action {
    Open(String),
    Channel(String),
    Spoiler(usize),
}

/// Everything the renderer needs to resolve names and remember revealed spoilers.
pub struct Context<'a> {
    pub client: &'a Entity<Client>,
    pub view: &'a WeakEntity<ChatView>,
    pub message_id: &'a str,
    pub guild_id: Option<&'a str>,
    pub me: Option<&'a str>,
    pub revealed: &'a HashSet<(String, usize)>,
    pub pending: bool,
}

struct Flat {
    text: String,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    mono: Vec<(Range<usize>, SharedString)>,
    clicks: Vec<(Range<usize>, Action)>,
    spoilers: usize,
}

pub fn render(doc: &Document, ctx: &Context, cx: &App) -> AnyElement {
    let t = cx.theme().clone();
    let mut col = div().flex().flex_col().gap_1().min_w_0();
    if let Some(forward) = &doc.forwarded {
        col = col.child(forward_chip(forward, ctx, &t, cx));
    }
    if doc.jumbo {
        return col.child(jumbo(doc, ctx, &t, cx)).into_any_element();
    }
    let mut spoiler_base = 0;
    for (i, block) in doc.blocks.iter().enumerate() {
        match block {
            Block::Text(spans) => {
                let (el, used) = inline(spans, ctx, &t, cx, (ctx.message_id, i), spoiler_base);
                spoiler_base += used;
                col = col.child(el);
            }
            Block::Quote(spans) => {
                let (el, used) = inline(spans, ctx, &t, cx, (ctx.message_id, i), spoiler_base);
                spoiler_base += used;
                col = col.child(
                    div().flex().child(div().w(px(3.)).flex_none().rounded(px(2.)).bg(t.border_strong)).child(
                        div().pl_3().min_w_0().text_color(t.text_muted).child(el),
                    ),
                );
            }
            Block::CodeBlock(code) => {
                col = col.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded(radius::MD)
                        .bg(t.bg_base)
                        .border_1()
                        .border_color(t.border)
                        .font_family(FONT_MONO)
                        .text_size(text::SM)
                        .text_color(t.text)
                        .child(code.trim_end_matches('\n').to_owned()),
                );
            }
        }
    }
    col.into_any_element()
}

fn inline(
    spans: &[Span],
    ctx: &Context,
    t: &Theme,
    cx: &App,
    id: (&str, usize),
    spoiler_base: usize,
) -> (AnyElement, usize) {
    let mut flat = Flat { text: String::new(), highlights: Vec::new(), mono: Vec::new(), clicks: Vec::new(), spoilers: 0 };
    let base = HighlightStyle { color: Some(if ctx.pending { t.text_faint } else { t.text }.into()), ..Default::default() };
    flatten(spans, base, ctx, t, cx, &mut flat, spoiler_base);
    let used = flat.spoilers;
    // Font overrides must be sorted and must not overlap (code spans hold no emoji ranges).
    flat.mono.sort_by_key(|(r, _)| r.start);
    let styled = StyledText::new(flat.text).with_highlights(flat.highlights).with_font_family_overrides(flat.mono);
    if flat.clicks.is_empty() {
        return (styled.into_any_element(), used);
    }
    let ranges: Vec<Range<usize>> = flat.clicks.iter().map(|(r, _)| r.clone()).collect();
    let actions: Vec<Action> = flat.clicks.into_iter().map(|(_, a)| a).collect();
    let client = ctx.client.clone();
    let view = ctx.view.clone();
    let message_id = ctx.message_id.to_owned();
    let element_id = SharedString::from(format!("text-{}-{}", id.0, id.1));
    let interactive = InteractiveText::new(element_id, styled).on_click(ranges, move |ix, _window, cx| {
        match &actions[ix] {
            Action::Open(url) => cx.open_url(url),
            Action::Channel(channel) => {
                let channel = channel.clone();
                client.update(cx, |c, cx| c.select_channel(channel, cx));
            }
            Action::Spoiler(n) => {
                let key = (message_id.clone(), *n);
                view.update(cx, |v, cx| v.reveal_spoiler(key, cx)).ok();
            }
        }
    });
    (interactive.into_any_element(), used)
}

fn merge(base: HighlightStyle, over: HighlightStyle) -> HighlightStyle {
    HighlightStyle {
        color: over.color.or(base.color),
        font_weight: over.font_weight.or(base.font_weight),
        font_style: over.font_style.or(base.font_style),
        background_color: over.background_color.or(base.background_color),
        underline: over.underline.or(base.underline),
        strikethrough: over.strikethrough.or(base.strikethrough),
        fade_out: over.fade_out.or(base.fade_out),
    }
}

fn push(flat: &mut Flat, text: &str, style: HighlightStyle, click: Option<Action>) -> Range<usize> {
    let start = flat.text.len();
    flat.text.push_str(text);
    let range = start..flat.text.len();
    // GPUI only swaps the font of a run that lies wholly inside an override, so every emoji gets
    // a highlight run of its own (same style) and an emoji-font override on exactly that run.
    let mut at = start;
    for r in rhein_store::emoji::emoji_ranges(text) {
        let emoji = start + r.start..start + r.end;
        if at < emoji.start {
            flat.highlights.push((at..emoji.start, style));
        }
        flat.highlights.push((emoji.clone(), style));
        flat.mono.push((emoji.clone(), FONT_EMOJI.into()));
        at = emoji.end;
    }
    if at < range.end {
        flat.highlights.push((at..range.end, style));
    }
    if !range.is_empty()
        && let Some(action) = click
    {
        flat.clicks.push((range.clone(), action));
    }
    range
}

fn flatten(
    spans: &[Span],
    style: HighlightStyle,
    ctx: &Context,
    t: &Theme,
    cx: &App,
    flat: &mut Flat,
    spoiler_base: usize,
) {
    let client = ctx.client.read(cx);
    let pill = |color: Hsla, bg: Hsla| HighlightStyle { color: Some(color), background_color: Some(bg), ..Default::default() };
    for span in spans {
        match span {
            Span::Text(text) => {
                push(flat, text, style, None);
            }
            Span::Code(code) => {
                let s = merge(style, HighlightStyle { background_color: Some(t.bg_base.into()), ..Default::default() });
                let r = push(flat, code, s, None);
                flat.mono.retain(|(m, _)| m.end <= r.start || m.start >= r.end);
                flat.mono.push((r, FONT_MONO.into()));
            }
            Span::Bold(inner) => flatten(
                inner,
                merge(style, HighlightStyle { font_weight: Some(FontWeight::BOLD), ..Default::default() }),
                ctx,
                t,
                cx,
                flat,
                spoiler_base,
            ),
            Span::Italic(inner) => flatten(
                inner,
                merge(style, HighlightStyle { font_style: Some(FontStyle::Italic), ..Default::default() }),
                ctx,
                t,
                cx,
                flat,
                spoiler_base,
            ),
            Span::Strike(inner) => flatten(
                inner,
                merge(
                    style,
                    HighlightStyle {
                        strikethrough: Some(StrikethroughStyle { thickness: px(1.), color: None }),
                        ..Default::default()
                    },
                ),
                ctx,
                t,
                cx,
                flat,
                spoiler_base,
            ),
            Span::Spoiler(inner) => {
                let n = spoiler_base + flat.spoilers;
                flat.spoilers += 1;
                let revealed = ctx.revealed.contains(&(ctx.message_id.to_owned(), n));
                if revealed {
                    let s = merge(style, HighlightStyle { background_color: Some(t.bg_selected.into()), ..Default::default() });
                    flatten(inner, s, ctx, t, cx, flat, spoiler_base);
                } else {
                    // Covered: text and background in the same colour until clicked.
                    let cover: Hsla = t.border_strong.into();
                    let s = merge(style, HighlightStyle { color: Some(cover), background_color: Some(cover), ..Default::default() });
                    let start = flat.text.len();
                    let mut hidden = Flat { text: String::new(), highlights: Vec::new(), mono: Vec::new(), clicks: Vec::new(), spoilers: 0 };
                    flatten(inner, s, ctx, t, cx, &mut hidden, spoiler_base);
                    push(flat, &hidden.text, s, None);
                    flat.clicks.push((start..flat.text.len(), Action::Spoiler(n)));
                }
            }
            Span::Role(id) => {
                let name = ctx
                    .guild_id
                    .and_then(|g| client.guild(g))
                    .and_then(|g| g.guild.roles.iter().find(|r| &r.id == id))
                    .map(|r| format!("@{}", r.name))
                    .unwrap_or_else(|| "@unknown-role".into());
                push(flat, &name, merge(style, pill(t.accent.into(), t.accent_soft.into())), None);
            }
            Span::Channel(id) => {
                let channel = client.channel(id).filter(|c| !c.is_category());
                match channel {
                    Some(c) => {
                        let label = format!("#{}", c.name);
                        push(flat, &label, merge(style, pill(t.accent.into(), t.accent_soft.into())), Some(Action::Channel(id.clone())));
                    }
                    None => {
                        push(flat, "#unknown-channel", merge(style, pill(t.text_muted.into(), t.bg_raised.into())), None);
                    }
                }
            }
            Span::Emoji { name, .. } => {
                push(flat, &format!(":{name}:"), merge(style, HighlightStyle { color: Some(t.text_muted.into()), ..Default::default() }), None);
            }
            Span::User(user) => {
                let label = format!("@{}", client.display_name(ctx.guild_id, user));
                let mine = ctx.me == Some(user.as_str());
                let s = if mine { pill(t.warning.into(), gpui::Hsla::from(t.warning).opacity(0.18)) } else { pill(t.accent.into(), t.accent_soft.into()) };
                push(flat, &label, merge(style, s), None);
            }
            Span::Broadcast(b) => {
                push(flat, b, merge(style, pill(t.warning.into(), gpui::Hsla::from(t.warning).opacity(0.18))), None);
            }
            Span::Link(url) => {
                let s = merge(
                    style,
                    HighlightStyle {
                        color: Some(t.accent.into()),
                        underline: Some(UnderlineStyle { thickness: px(1.), color: None, wavy: false }),
                        ..Default::default()
                    },
                );
                push(flat, url, s, Some(Action::Open(url.clone())));
            }
        }
    }
}

/// Emoji-only messages: custom emoji as large images, unicode emoji as large text.
fn jumbo(doc: &Document, ctx: &Context, t: &Theme, cx: &App) -> AnyElement {
    let client = ctx.client.read(cx);
    let mut row = div().flex().flex_wrap().items_center().gap_1().text_size(px(40.)).text_color(t.text);
    for block in &doc.blocks {
        let (Block::Text(spans) | Block::Quote(spans)) = block else { continue };
        for span in spans {
            match span {
                Span::Emoji { name, id, origin } => {
                    match client.emoji_url(ctx.guild_id, id, origin.as_deref()) {
                        Some(url) => {
                            row = row.child(img(url).size(px(48.)).with_fallback({
                                let name = name.clone();
                                move || div().text_size(text::MD).child(format!(":{name}:")).into_any_element()
                            }))
                        }
                        None => row = row.child(div().text_size(text::MD).child(format!(":{name}:"))),
                    }
                }
                Span::Text(text) => {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        row = row.child(div().font_family(FONT_EMOJI).child(trimmed.to_owned()));
                    }
                }
                _ => {}
            }
        }
    }
    row.into_any_element()
}

fn forward_chip(forward: &Forward, ctx: &Context, t: &Theme, cx: &App) -> AnyElement {
    let label = match forward {
        Forward::Guild { channel_id, .. } => match ctx.client.read(cx).channel(channel_id) {
            Some(c) => format!("Forwarded from #{}", c.name),
            None => "Forwarded".to_owned(),
        },
        Forward::Conversation { .. } => "Forwarded from a conversation".to_owned(),
        Forward::Unknown => "Forwarded".to_owned(),
    };
    div()
        .flex()
        .items_center()
        .gap_1()
        .text_size(text::SM)
        .text_color(t.text_faint)
        .child(icon(Icon::Reply).text_color(t.text_faint))
        .child(label)
        .into_any_element()
}
