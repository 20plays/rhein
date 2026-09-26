//! One row of the message list: optional day separator and "New messages" line, the reply
//! quote, header, rich text, attachments, link previews, reactions and hover actions.

use gpui::{
    AnyElement, App, Entity, FontWeight, Hsla, IntoElement, ObjectFit, ParentElement as _, Pixels, SharedString,
    Styled as _, StyledImage as _, WeakEntity, Window, div, img, prelude::*, px,
};
use rhein_proto::{Attachment, Message};
use rhein_store::embed::{Embed, EmbedKind};
use rhein_store::client::GifShare;
use rhein_store::gif_links::MediaKind;
use rhein_store::MediaProbe;
use rhein_store::{Client, EmbedState, gif_links, markdown};
use rhein_ui::theme::{FONT_EMOJI, FONT_MONO, radius, size, text};
use rhein_ui::{ActiveTheme as _, motion, Avatar, Button, Icon, IconButton, Theme, icon, icon_sized, tooltip};

use crate::views::chat::ChatView;
use crate::views::message_text;

/// Messages from the same author this close together share one header.
const GROUP_WINDOW_MS: i64 = 7 * 60 * 1000;
const QUICK_REACTIONS: [&str; 1] = ["👍"];
/// Room kept free on the right of every message for the hover action bar, so the bar never
/// covers text: one quick reaction, add reaction, reply and "more", at 28 px each, plus padding.
const ACTION_BAR_W: f32 = 4. * 28. + 14.;
/// Link previews and embed cards.
const IMAGE_MAX_W: f32 = 400.;
/// Every picture in the chat (an attachment, a GIF, a link to a picture, a preview's image) is
/// shown whole and at most this high: a larger one is scaled down to it, keeping its shape, a
/// smaller one keeps its own size.
const PICTURE_MAX_H: f32 = 300.;
/// A picture of unknown size, until it loads.
const PICTURE_UNKNOWN: (f32, f32) = (400., 300.);

pub struct RowCtx {
    pub client: Entity<Client>,
    pub view: WeakEntity<ChatView>,
    pub channel_id: String,
}

pub fn render_row(ctx: &RowCtx, ix: usize, _window: &mut Window, cx: &mut App) -> AnyElement {
    let t = cx.theme().clone();
    let client = ctx.client.read(cx);
    let channel_id = ctx.channel_id.as_str();
    let Some(log) = client.logs.get(channel_id) else { return div().into_any_element() };
    let Some(message) = log.messages.get(ix).cloned() else { return div().into_any_element() };
    let prev = ix.checked_sub(1).and_then(|p| log.messages.get(p));
    let divider_here = client.divider_index(channel_id) == Some(ix);
    let new_day = prev.is_none_or(|p| day_of(p.created_at) != day_of(message.created_at));
    let grouped = !new_day
        && !divider_here
        && message.reply_to.is_none()
        && prev.is_some_and(|p| p.author == message.author && message.created_at - p.created_at < GROUP_WINDOW_MS);

    let guild_id = client.guild_of_channel(channel_id).map(|g| g.guild.id.clone());
    let author_name = client.display_name(guild_id.as_deref(), &message.author);
    let author_bot = guild_id
        .as_deref()
        .and_then(|g| client.members.get(g))
        .and_then(|l| l.iter().find(|m| m.user_id == message.author))
        .is_some_and(|m| m.is_bot);
    let me = client.user_id().map(str::to_owned);
    let mine = me.as_deref() == Some(message.author.as_str());
    let pending = message.id.starts_with("pending:");
    let failed = client.is_failed(channel_id, &message.id);
    let strip_embeds = client.channel(channel_id).is_some_and(|c| c.strip_embeds);
    let avatar_url = client.avatar_url(guild_id.as_deref(), &message.author);
    let prefs = client.prefs.clone();
    let avatar_size = if prefs.compact { 28. } else { 40. };
    let body_size = text::MD * (prefs.text_scale.clamp(80, 150) as f32 / 100.);

    let view_state = ctx.view.upgrade().map(|v| {
        let v = v.read(cx);
        let selected = v.selection.as_ref().filter(|s| s.message == message.id).map(|s| (s.block, s.range()));
        let hovered = v.hovered_link.as_ref().filter(|(m, _, _)| *m == message.id).map(|(_, b, r)| (*b, r.clone()));
        (v.revealed.clone(), v.highlight.as_ref().map(|(id, _)| id.clone()), v.fresh.contains(message_key(&message)), (selected, hovered))
    });
    let (revealed, highlighted, fresh, (selected, hovered)) = view_state.unwrap_or_default();
    let highlighted = highlighted.as_deref() == Some(message.id.as_str());

    let doc = markdown::parse(&message.content);
    // Called on: by name, @everyone, a role we hold, or a reply to one of our messages.
    let mentions_me = me.as_deref().is_some_and(|me| {
        let roles = guild_id.as_deref().map(|g| client.my_roles(g)).unwrap_or_default();
        let replied = message.reply_author.as_deref().or_else(|| {
            message.reply_to.as_deref().and_then(|p| client.find_message(channel_id, p)).map(|p| p.author.as_str())
        }) == Some(me);
        !mine && (replied || markdown::mentions_me(&doc, me, &roles))
    });

    // Previews: links outside code, looked up lazily.
    let links = if strip_embeds { Vec::new() } else { markdown::links_in(&message.content) };
    let mut missing = Vec::new();
    let mut media_missing = Vec::new();
    let mut cards = Vec::new();
    for url in &links {
        if is_invite(url) {
            cards.push(invite_card(ctx, ix, url, &t));
            continue;
        }
        // GIFs are shown below, straight from their host, and never unfurled.
        if gif_links::is_gif(url) {
            continue;
        }
        // Telium media says what it is only when asked: a GIF or a picture is shown below as
        // itself; anything else gets the usual preview.
        if gif_links::is_telium_media(url) {
            match client.media_kind(url) {
                Some(MediaProbe::Known(MediaKind::Other)) => {}
                Some(_) => continue,
                None => {
                    media_missing.push(url.clone());
                    continue;
                }
            }
        }
        match client.embed(channel_id, url) {
            Some(EmbedState::Ready(embed)) => cards.push(embed_card(ctx, ix, cards.len(), embed, &t, cx)),
            Some(EmbedState::Loading) => {}
            Some(EmbedState::Empty { retry_at }) if std::time::Instant::now() < *retry_at => {}
            _ => missing.push(url.clone()),
        }
    }

    let seen = client.dm_seen(channel_id) == Some(message.id.as_str());

    // GIF links: the picture itself (a share page once its provider has named it).
    let mut shares_missing = Vec::new();
    let mut gifs = Vec::new();
    for (n, url) in client.gif_links_in(&message.content).into_iter().enumerate() {
        match gif_links::share(&url) {
            None => gifs.push(gif_tile(ctx, ix, n, url.clone(), url, true, &t, cx)),
            Some(share) => match client.gif_share(&url) {
                Some(GifShare::Ready(media)) => gifs.push(gif_tile(ctx, ix, n, media, url, true, &t, cx)),
                Some(GifShare::Loading) => {}
                Some(GifShare::Failed) => gifs.push(gif_share_link(ix, n, url, share.provider(), &t)),
                None => shares_missing.push(url),
            },
        }
    }
    // Telium media that proved to be a still picture: shown as one, without the GIF star.
    for url in gif_links::telium_media_in(&message.content) {
        if client.media_kind(&url) == Some(MediaProbe::Known(MediaKind::Image)) {
            let n = gifs.len() + 100;
            gifs.push(gif_tile(ctx, ix, n, url.clone(), url, false, &t, cx));
        }
    }
    let gif_only = client.is_gif_only(&message.content);

    let reply_quote = message.reply_to.as_ref().map(|parent_id| {
        let parent = client.find_message(channel_id, parent_id);
        let label = match parent {
            Some(p) => {
                let name = client.display_name(guild_id.as_deref(), &p.author);
                let snippet: String = p.content.lines().next().unwrap_or("").chars().take(100).collect();
                format!("{name}: {}", if snippet.is_empty() { "(attachment)".to_owned() } else { snippet })
            }
            None => "Replying to an older message".to_owned(),
        };
        let client = ctx.client.clone();
        let (chan, target) = (ctx.channel_id.clone(), parent_id.clone());
        div()
            .id(SharedString::from(format!("quote-{ix}")))
            .ml(px(20.))
            .mb_0p5()
            .flex()
            .items_center()
            .gap_1()
            .text_size(text::SM)
            .text_color(t.text_faint)
            .cursor_pointer()
            .hover(|s| s.text_color(t.text_muted))
            .on_click(move |_, _, cx| {
                let (chan, target) = (chan.clone(), target.clone());
                client.update(cx, |c, cx| c.jump_to(chan, target, cx));
            })
            .child(icon_sized(Icon::Reply, px(14.)).text_color(t.text_faint))
            .child(div().truncate().child(label))
    });

    // A message that started a thread links to it (not inside the thread itself).
    let thread_chip = message.thread_id.clone().filter(|t| t != channel_id).map(|thread_id| {
        let title = client.thread(&thread_id).map(|t| (t.title.clone(), t.message_count));
        let label = match title {
            Some((title, count)) => format!("{title} · {count} message{}", if count == 1 { "" } else { "s" }),
            None => "View thread".to_owned(),
        };
        let view = ctx.view.clone();
        div()
            .id(SharedString::from(format!("thread-chip-{ix}")))
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded(radius::MD)
            .bg(t.bg_panel)
            .text_size(text::SM)
            .text_color(t.accent)
            .cursor_pointer()
            .hover(|s| s.bg(t.bg_selected))
            .on_click(move |_, _, cx| {
                let id = thread_id.clone();
                view.update(cx, |v, cx| v.open_thread(id, cx)).ok();
            })
            .child(icon_sized(Icon::MessagesSquare, px(14.)).text_color(t.accent))
            .child(label)
    });

    let text_ctx = message_text::Context {
        client: &ctx.client,
        view: &ctx.view,
        message_id: &message.id,
        guild_id: guild_id.as_deref(),
        me: me.as_deref(),
        revealed: &revealed,
        selected,
        hovered,
        pending: pending && !failed,
    };
    let body_text = (!message.content.trim().is_empty() && !gif_only).then(|| message_text::render(&doc, &text_ctx, cx));

    // Indexed by position in the message, which is how an encrypted attachment is found again.
    type Indexed<'a> = Vec<(usize, &'a Attachment)>;
    let (images, files): (Indexed, Indexed) = message.attachments.iter().enumerate().partition(|(_, a)| a.is_image());
    let body = div()
        .flex()
        .flex_col()
        .gap_1()
        .min_w_0()
        .flex_1()
        .when(!grouped, |d| {
            d.child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child({
                        let (v1, v2, u1, u2) = (ctx.view.clone(), ctx.view.clone(), message.author.clone(), message.author.clone());
                        div()
                            .id(SharedString::from(format!("author-{ix}")))
                            .font_weight(FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .hover(|s| s.underline())
                            .on_click(move |e, _, cx| {
                                let (u, at) = (u1.clone(), e.position());
                                v1.update(cx, |v, cx| v.show_profile(u, at, cx)).ok();
                            })
                            .on_mouse_down(gpui::MouseButton::Right, move |e, _, cx| {
                                cx.stop_propagation();
                                let (u, at) = (u2.clone(), e.position);
                                v2.update(cx, |v, cx| v.member_menu(u, at, cx)).ok();
                            })
                            .child(author_name.clone())
                    })
                    .when(author_bot, |d| d.child(rhein_ui::Tag("BOT")))
                    .child(div().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_faint).child(format_time(message.created_at, prefs.clock_24h))),
            )
        })
        .children(body_text.map(|b| div().text_size(body_size).child(b)))
        .when(message.edited_at.is_some(), |d| d.child(div().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_faint).child("(edited)")))
        .children(images.into_iter().map(|(i, a)| image_attachment(ctx, ix, i, &message.id, a, &t, cx)))
        .children(files.into_iter().map(|(i, a)| {
            if rhein_voice::playback::playable(&a.filename, &a.mime) {
                sound_chip(ctx, ix, i, &message.id, a, &t, cx)
            } else {
                file_chip(ctx, ix, i, &message.id, a, &t).into_any_element()
            }
        }))
        .when(!gifs.is_empty(), |d| d.child(div().flex().flex_wrap().gap_2().children(gifs)))
        .children(cards)
        // Your newest message the other side has read (read receipts on).
        .when(seen, |d| d.child(div().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_faint).child("seen")))
        .when(failed, |d| {
            let (c1, c2) = (ctx.client.clone(), ctx.client.clone());
            let (ch1, id1, ch2, id2) = (ctx.channel_id.clone(), message.id.clone(), ctx.channel_id.clone(), message.id.clone());
            d.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(text::SM)
                    .text_color(t.danger)
                    .child(icon(Icon::CircleAlert).text_color(t.danger))
                    .child("Not sent.")
                    .child(Button::new(SharedString::from(format!("retry-{ix}")), "Retry").small().ghost().on_click(move |_, _, cx| {
                        let (ch, id) = (ch1.clone(), id1.clone());
                        c1.update(cx, |c, cx| c.retry_failed(ch, id, cx));
                    }))
                    .child(Button::new(SharedString::from(format!("discard-{ix}")), "Discard").small().ghost().on_click(move |_, _, cx| {
                        let (ch, id) = (ch2.clone(), id2.clone());
                        c2.update(cx, |c, cx| c.discard_failed(ch, id, cx));
                    })),
            )
        })
        .when(!message.reactions.is_empty(), |d| d.child(reactions(ctx, ix, &message, &t)))
        .children(thread_chip);

    let group = message_group(ix);
    let gutter = if grouped {
        div()
            .w(px(avatar_size))
            .flex_none()
            .pt_0p5()
            .text_size(text::XS)
            .text_color(t.text_faint)
            .opacity(0.)
            .group_hover(group.clone(), |s| s.opacity(1.))
            .child(format_hm(message.created_at, prefs.clock_24h))
            .into_any_element()
    } else {
        let (v1, u1) = (ctx.view.clone(), message.author.clone());
        div()
            .id(SharedString::from(format!("avatar-{ix}")))
            .w(px(avatar_size))
            .flex_none()
            .cursor_pointer()
            .on_click(move |e, _, cx| {
                let (u, at) = (u1.clone(), e.position());
                v1.update(cx, |v, cx| v.show_profile(u, at, cx)).ok();
            })
            .child(Avatar::new(message.author.clone(), author_name.clone()).size(px(avatar_size)).image(avatar_url.map(Into::into)))
            .into_any_element()
    };

    let actions = (!pending).then(|| hover_actions(ctx, ix, &message, &group, &t));

    let row_bg: Option<Hsla> = if highlighted {
        Some(t.accent_soft.into())
    } else if mentions_me {
        Some(Hsla::from(t.warning).opacity(0.10))
    } else {
        None
    };
    let row = div()
        .relative()
        .w_full()
        .group(group)
        .flex()
        .flex_col()
        .px_4()
        .when(grouped, |d| d.py_0p5())
        .when(!grouped, |d| if prefs.compact { d.pt_1p5().pb_0p5() } else { d.pt_3().pb_0p5() })
        .when_some(row_bg, |d, bg| d.bg(bg))
        // The bar lies over the row's edge rather than taking room, so the text stays in line
        // with its neighbours.
        .when(mentions_me, |d| d.child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(t.warning)))
        .when(row_bg.is_none(), |d| d.hover(|s| s.bg(t.bg_hover)))
        .when(mentions_me && !highlighted, |d| {
            let lit = Hsla::from(t.warning).opacity(0.16);
            d.hover(move |s| s.bg(lit))
        })
        .children(reply_quote)
        .child(div().flex().gap_4().pr(px(ACTION_BAR_W)).child(gutter).child(body))
        .children(actions)
        .when(!pending, |d| {
            let (view, m) = (ctx.view.clone(), message.clone());
            d.on_mouse_down(gpui::MouseButton::Right, move |e, window, cx| {
                let (m, at) = (m.clone(), e.position);
                view.update(cx, |v, cx| v.open_message_menu(m, at, window, cx)).ok();
            })
        });

    // As wide as the list: a highlight or the hover spans the row, and the actions sit at its
    // right edge rather than on the text.
    let element = div()
        .w_full()
        .flex()
        .flex_col()
        .when(new_day, |d| d.child(day_separator(message.created_at, &t)))
        .when(divider_here, |d| d.child(new_messages_line(&t)));
    // A message that just arrived rises into place; history renders as it is.
    let element = if fresh {
        let key = SharedString::from(format!("fresh-{}", message_key(&message)));
        element.child(motion::rise_in(row, key, 6., motion::BASE)).into_any_element()
    } else {
        element.child(row).into_any_element()
    };
    // Unknown previews are asked for after this frame, never during it.
    if !media_missing.is_empty() {
        let client = ctx.client.clone();
        cx.defer(move |cx| {
            client.update(cx, |c, cx| {
                for url in &media_missing {
                    c.request_media_kind(url, cx);
                }
            })
        });
    }
    if !shares_missing.is_empty() {
        let client = ctx.client.clone();
        cx.defer(move |cx| {
            client.update(cx, |c, cx| {
                for url in &shares_missing {
                    c.request_gif_share(url, cx);
                }
            })
        });
    }
    if !missing.is_empty() {
        let client = ctx.client.clone();
        let chan = ctx.channel_id.clone();
        cx.defer(move |cx| {
            client.update(cx, |c, cx| {
                for url in &missing {
                    c.request_embed(&chan, url, cx);
                }
            })
        });
    }
    element
}

fn hover_actions(ctx: &RowCtx, ix: usize, message: &Message, group: &SharedString, t: &Theme) -> AnyElement {
    // Astride the row's top edge, as in Discord, so it covers a short message's neighbours
    // least; the first row keeps it inside, where the list would clip it.
    let mut bar = div()
        .absolute()
        .top(px(if ix == 0 { 2. } else { -16. }))
        .right(px(16.))
        .flex()
        .gap_0p5()
        .p_0p5()
        .rounded(radius::MD)
        .bg(t.bg_raised)
        .border_1()
        .border_color(t.border_strong)
        .shadow_md()
        .opacity(0.)
        .group_hover(group.clone(), |s| s.opacity(1.));
    for (i, emoji) in QUICK_REACTIONS.iter().enumerate() {
        let client = ctx.client.clone();
        let (chan, mid) = (ctx.channel_id.clone(), message.id.clone());
        bar = bar.child(
            div()
                .id(SharedString::from(format!("qr-{ix}-{i}")))
                .size(size::ICON_BUTTON)
                .flex()
                .items_center()
                .justify_center()
                .rounded(radius::MD)
                .cursor_pointer()
                .hover(|s| s.bg(t.bg_hover))
                .tooltip(tooltip(format!("React with {emoji}")))
                .on_click(move |_, _, cx| {
                    let (chan, mid) = (chan.clone(), mid.clone());
                    client.update(cx, |c, cx| c.toggle_reaction(chan, mid, emoji.to_string(), cx));
                })
                .child(div().font_family(FONT_EMOJI).child(*emoji)),
        );
    }
    let view = ctx.view.clone();
    let react_id = message.id.clone();
    bar = bar.child(IconButton::new(SharedString::from(format!("react-pick-{ix}")), Icon::SmilePlus).tooltip("Add reaction").on_click(
        move |e, window, cx| {
            let (id, at) = (react_id.clone(), e.position());
            view.update(cx, |v, cx| v.react_picker(at, id, window, cx)).ok();
        },
    ));
    let view = ctx.view.clone();
    let reply_id = message.id.clone();
    bar = bar.child(IconButton::new(SharedString::from(format!("reply-{ix}")), Icon::Reply).tooltip("Reply").on_click(
        move |_, window, cx| {
            let id = reply_id.clone();
            view.update(cx, |v, cx| v.start_reply(id, window, cx)).ok();
        },
    ));
    let view = ctx.view.clone();
    let for_menu = message.clone();
    bar = bar.child(IconButton::new(SharedString::from(format!("more-{ix}")), Icon::Ellipsis).tooltip("More").on_click(
        move |e, window, cx| {
            let (m, at) = (for_menu.clone(), e.position());
            view.update(cx, |v, cx| v.open_message_menu(m, at, window, cx)).ok();
        },
    ));
    bar.into_any_element()
}

fn reactions(ctx: &RowCtx, ix: usize, message: &Message, t: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_wrap()
        .gap_1()
        .children(message.reactions.iter().enumerate().map(|(i, r)| {
            let client = ctx.client.clone();
            let (chan, mid, emoji) = (ctx.channel_id.clone(), message.id.clone(), r.emoji.clone());
            div()
                .id(SharedString::from(format!("react-{ix}-{i}")))
                .px_2()
                .py_0p5()
                .rounded(radius::MD)
                .border_1()
                .border_color(if r.me { t.accent } else { t.border_strong })
                .bg(if r.me { t.accent_soft } else { t.bg_raised })
                .text_size(text::SM)
                .cursor_pointer()
                .hover(|s| s.border_color(t.text_faint))
                .on_click(move |_, _, cx| {
                    let (chan, mid, emoji) = (chan.clone(), mid.clone(), emoji.clone());
                    client.update(cx, |c, cx| c.toggle_reaction(chan, mid, emoji, cx));
                })
                .flex()
                .items_center()
                .gap_1()
                .child(div().font_family(FONT_EMOJI).child(r.emoji.clone()))
                .child(div().text_color(if r.me { t.accent } else { t.text_muted }).child(r.count.to_string()))
        }))
        .into_any_element()
}

/// The natural size of a picture, scaled down to the height limit; its aspect ratio when known.
fn fit(width: Option<i64>, height: Option<i64>) -> (Pixels, Option<f32>) {
    match (width, height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => {
            let (w, h) = (w as f32, h as f32);
            let scale = (PICTURE_MAX_H / h).min(1.0);
            (px((w * scale).max(16.)), Some(w / h))
        }
        _ => (px(PICTURE_UNKNOWN.0), None),
    }
}

/// Whether a link is a GIF (a .gif, or a GIF provider's).
/// A GIF from a link: `media` is the picture, `link` what the message said (a share page, or the
/// picture again).
/// A picture straight from its link: a GIF (with its favourite star) or a still picture.
#[allow(clippy::too_many_arguments)]
fn gif_tile(ctx: &RowCtx, ix: usize, n: usize, media: String, link: String, starred: bool, t: &Theme, cx: &App) -> AnyElement {
    let group = SharedString::from(format!("gif-group-{ix}-{n}"));
    let star = starred.then(|| gif_star(ctx, format!("gif-star-{ix}-{n}"), message_group(ix), media.clone(), t, cx));
    let view = ctx.view.clone();
    let (open, raised, faint) = (media.clone(), t.bg_raised, t.text_faint);
    div()
        .id(SharedString::from(format!("gif-{ix}-{n}")))
        .group(group)
        .relative()
        .max_w_full()
        .max_h(px(PICTURE_MAX_H))
        .rounded(radius::LG)
        .overflow_hidden()
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            let (open, name) = (open.clone(), link.clone());
            view.update(cx, |v, cx| v.open_lightbox(open, name, window, cx)).ok();
        })
        .child(
            img(media)
                .max_w_full()
                .max_h(px(PICTURE_MAX_H))
                .object_fit(ObjectFit::Contain)
                .with_loading(move || div().w(px(PICTURE_UNKNOWN.0 / 2.)).h(px(PICTURE_UNKNOWN.1 / 2.)).bg(raised).into_any_element())
                .with_fallback(move || {
                    div().p_3().text_size(text::SM).text_color(faint).child("This picture is no longer available").into_any_element()
                }),
        )
        .children(star)
        .into_any_element()
}

/// A share page whose provider had no GIF for it: a link to open it there.
fn gif_share_link(ix: usize, n: usize, url: String, provider: &str, t: &Theme) -> AnyElement {
    div()
        .id(SharedString::from(format!("gif-link-{ix}-{n}")))
        .text_size(text::SM)
        .text_color(t.link)
        .cursor_pointer()
        .hover(|s| s.text_color(t.link_hover).underline())
        .on_click(move |_, _, cx| cx.open_url(&url))
        .child(format!("Open GIF on {provider}"))
        .into_any_element()
}

/// The hover group of the message at `ix`.
fn message_group(ix: usize) -> SharedString {
    SharedString::from(format!("msg-{ix}"))
}

/// The star over a GIF (shown while the pointer is on its message, favourite or not): adds it
/// to the favourites or takes it out.
fn gif_star(ctx: &RowCtx, id: String, group: SharedString, url: String, t: &Theme, cx: &App) -> AnyElement {
    let starred = ctx.client.read(cx).is_gif_favorite(&url);
    let client = ctx.client.clone();
    div()
        .absolute()
        .top_1p5()
        .left_1p5()
        .rounded(radius::MD)
        .bg(t.bg_base)
        .invisible()
        .group_hover(group, |s| s.visible())
        .child(
            IconButton::new(SharedString::from(id), if starred { Icon::StarFilled } else { Icon::Star })
                .active(starred)
                .tooltip(if starred { "Remove from favourite GIFs" } else { "Add to favourite GIFs" })
                .on_click(move |_, _, cx| client.update(cx, |c, cx| c.toggle_gif_favorite(&url, cx))),
        )
        .into_any_element()
}

fn image_attachment(ctx: &RowCtx, ix: usize, i: usize, message_id: &str, a: &Attachment, t: &Theme, cx: &App) -> AnyElement {
    let Some(url) = a.url.clone() else { return file_chip(ctx, ix, i, message_id, a, t).into_any_element() };
    let (w, aspect) = fit(a.width, a.height);
    let group = SharedString::from(format!("img-group-{ix}-{i}"));
    let star = (a.mime == "image/gif").then(|| gif_star(ctx, format!("img-star-{ix}-{i}"), message_group(ix), url.clone(), t, cx));
    let view = ctx.view.clone();
    let (open, name) = (url.clone(), a.filename.clone());
    let fallback_name = a.filename.clone();
    let faint = t.text_faint;
    let raised = t.bg_raised;
    div()
        .id(SharedString::from(format!("img-{ix}-{i}")))
        .group(group)
        .relative()
        .max_w_full()
        // Known size: its natural width, keeping its shape when the column is narrower. Unknown:
        // the picture lays itself out once it has loaded.
        .map(|d| match aspect {
            Some(ratio) => d.w(w).aspect_ratio(ratio),
            None => d,
        })
        .rounded(radius::LG)
        .overflow_hidden()
        .bg(t.bg_raised)
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            let (open, name) = (open.clone(), name.clone());
            view.update(cx, |v, cx| v.open_lightbox(open, name, window, cx)).ok();
        })
        .child(
            img(url)
                .map(|i| if aspect.is_some() { i.size_full() } else { i.max_w_full().max_h(px(PICTURE_MAX_H)) })
                .object_fit(ObjectFit::Contain)
                .with_loading(move || {
                    let placeholder = div().bg(raised);
                    if aspect.is_some() { placeholder.size_full() } else { placeholder.w(px(PICTURE_UNKNOWN.0)).h(px(PICTURE_UNKNOWN.1)) }.into_any_element()
                })
                .with_fallback(move || {
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(text::SM)
                        .text_color(faint)
                        .child(format!("Couldn't load {fallback_name}"))
                        .into_any_element()
                }),
        )
        .children(star)
        .into_any_element()
}

/// A sound file: play and pause, where it is (click the bar to jump), how long it is.
fn sound_chip(ctx: &RowCtx, ix: usize, i: usize, message_id: &str, a: &Attachment, t: &Theme, cx: &App) -> AnyElement {
    let key = format!("{message_id}#{i}");
    let state = ctx.client.read(cx).sound(&key);
    let (playing, loading) = state.as_ref().map_or((false, false), |s| (s.playing, s.loading));
    let (position, duration) = state.as_ref().map_or((0., None), |s| (s.position, s.duration));
    let fraction = duration.filter(|d| *d > 0.).map_or(0., |d| (position / d).clamp(0., 1.)) as f32;
    let clock = |secs: f64| format!("{}:{:02}", (secs / 60.) as u64, secs as u64 % 60);
    let times = match (state.as_ref(), duration) {
        (Some(s), _) if s.error.is_some() => s.error.clone().unwrap_or_default(),
        (Some(_), Some(d)) => format!("{} / {}", clock(position), clock(d)),
        (Some(s), None) if s.loading => "Loading…".to_owned(),
        _ => format_size(a.size),
    };
    let source = match &a.url {
        Some(url) => rhein_store::SoundSource::Url(url.clone()),
        None => rhein_store::SoundSource::Dm { conversation: ctx.channel_id.clone(), message: message_id.to_owned(), index: i },
    };
    let (client, toggle_key, name) = (ctx.client.clone(), key.clone(), a.filename.clone());
    let (seek_client, seek_key) = (ctx.client.clone(), key);
    // Where the bar is on screen, for turning a click into a position.
    let bar = std::rc::Rc::new(std::cell::Cell::new(None::<gpui::Bounds<Pixels>>));
    let measured = bar.clone();
    div()
        .id(SharedString::from(format!("sound-{ix}-{i}")))
        .w(px(420.))
        .max_w_full()
        .px_3()
        .py_2()
        .rounded(radius::MD)
        .bg(t.bg_raised)
        .border_1()
        .border_color(t.border)
        .flex()
        .gap_3()
        .items_center()
        .child(
            IconButton::new(SharedString::from(format!("sound-play-{ix}-{i}")), if playing { Icon::Pause } else { Icon::Play })
                .tooltip(if playing { "Pause" } else { "Play" })
                .on_click(move |_, _, cx| {
                    let (key, source, name) = (toggle_key.clone(), source.clone(), name.clone());
                    client.update(cx, |c, cx| c.toggle_sound(key, source, name, cx));
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(div().flex_1().min_w_0().truncate().text_size(text::SM).text_color(t.text).child(a.filename.clone()))
                        .child(div().flex_none().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_faint).child(times)),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("sound-bar-{ix}-{i}")))
                        .relative()
                        .h(px(6.))
                        .w_full()
                        .rounded_full()
                        .bg(t.bg_selected)
                        .when(state.is_some() && !loading, |d| d.cursor_pointer())
                        .child(div().absolute().left_0().top_0().h_full().rounded_full().bg(t.accent).w(gpui::relative(fraction)))
                        .child(
                            gpui::canvas(move |bounds, _, _| measured.set(Some(bounds)), |_, _, _, _| {})
                                .absolute()
                                .inset_0(),
                        )
                        .on_mouse_down(gpui::MouseButton::Left, move |e, _, cx| {
                            let Some(bounds) = bar.get() else { return };
                            let fraction = ((e.position.x - bounds.left()) / bounds.size.width) as f64;
                            seek_client.update(cx, |c, cx| c.seek_sound(&seek_key, fraction, cx));
                        }),
                ),
        )
        .into_any_element()
}

fn file_chip(ctx: &RowCtx, ix: usize, i: usize, message_id: &str, a: &Attachment, t: &Theme) -> impl IntoElement {
    let url = a.url.clone();
    // An encrypted DM attachment has no URL to open: the client fetches and opens it.
    let (client, conversation, message) = (ctx.client.clone(), ctx.channel_id.clone(), message_id.to_owned());
    div()
        .id(SharedString::from(format!("att-{ix}-{i}")))
        .max_w(px(420.))
        .px_3()
        .py_2()
        .rounded(radius::MD)
        .bg(t.bg_raised)
        .border_1()
        .border_color(t.border)
        .flex()
        .gap_2()
        .items_center()
        .cursor_pointer()
        .hover(|s| s.bg(t.bg_selected))
        .on_click(move |_, _, cx| match &url {
            Some(url) => cx.open_url(url),
            None => client.update(cx, |c, cx| c.open_dm_attachment(&conversation, &message, i, cx)),
        })
        .child(icon(if a.is_image() { Icon::Image } else { Icon::File }).text_color(t.text_muted))
        .child(div().flex_1().truncate().text_color(t.accent).child(a.filename.clone()))
        .child(div().text_size(text::XS).text_color(t.text_faint).child(format_size(a.size)))
}

fn embed_card(ctx: &RowCtx, ix: usize, n: usize, e: &Embed, t: &Theme, cx: &App) -> AnyElement {
    let view = ctx.view.clone();
    // Just a picture: show it like an attachment.
    if e.kind == EmbedKind::Image && e.title.is_none() && e.description.is_none() {
        let Some(image) = e.image.clone() else { return div().into_any_element() };
        let (open, name) = (image.clone(), e.url.clone());
        // A GIF is kept by its picture's own link, which draws anywhere.
        let group = SharedString::from(format!("embed-group-{ix}-{n}"));
        let star = (gif_links::is_gif(&image) || gif_links::is_gif(&e.url)).then(|| gif_star(ctx, format!("embed-star-{ix}-{n}"), message_group(ix), image.clone(), t, cx));
        return div()
            .id(SharedString::from(format!("embed-{ix}-{n}")))
            .group(group)
            .relative()
            .max_w_full()
            .max_h(px(PICTURE_MAX_H))
            .rounded(radius::LG)
            .overflow_hidden()
            .cursor_pointer()
            .on_click(move |_, window, cx| {
                let (open, name) = (open.clone(), name.clone());
                view.update(cx, |v, cx| v.open_lightbox(open, name, window, cx)).ok();
            })
            .child(img(image).max_w_full().max_h(px(PICTURE_MAX_H)).object_fit(ObjectFit::Contain))
            .children(star)
            .into_any_element();
    }
    let link = e.url.clone();
    let title = e.title.clone().unwrap_or_else(|| e.url.clone());
    let video = e.video.clone();
    let media = e.image.clone().map(|image| {
        let is_video = video.is_some();
        let target = video.clone().unwrap_or_else(|| image.clone());
        let name = e.url.clone();
        let view = view.clone();
        div()
            .id(SharedString::from(format!("embed-media-{ix}-{n}")))
            .relative()
            .mt_2()
            .max_w(px(IMAGE_MAX_W))
            .rounded(radius::MD)
            .overflow_hidden()
            .cursor_pointer()
            .on_click(move |_, window, cx| {
                if is_video {
                    cx.open_url(&target);
                } else {
                    let (open, name) = (target.clone(), name.clone());
                    view.update(cx, |v, cx| v.open_lightbox(open, name, window, cx)).ok();
                }
            })
            .child(img(image).max_w(px(IMAGE_MAX_W)).max_h(px(PICTURE_MAX_H)).object_fit(ObjectFit::Contain))
            .when(is_video, |d| {
                d.child(
                    div().absolute().inset_0().flex().items_center().justify_center().child(
                        div()
                            .size(px(48.))
                            .rounded_full()
                            .bg(t.overlay)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon_sized(Icon::Play, px(22.)).text_color(t.text)),
                    ),
                )
            })
    });
    div()
        .max_w(px(IMAGE_MAX_W + 32.))
        .min_w_0()
        .flex()
        .rounded(radius::MD)
        .bg(t.bg_panel)
        .overflow_hidden()
        .child(div().w(px(3.)).flex_none().bg(t.accent))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .p_3()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_size(text::XS)
                        .text_color(t.text_faint)
                        .child(icon_sized(Icon::Globe, px(12.)).text_color(t.text_faint))
                        .child(e.site.clone()),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("embed-title-{ix}-{n}")))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.link)
                        .cursor_pointer()
                        .hover(|s| s.text_color(t.link_hover).underline())
                        .on_click(move |_, _, cx| cx.open_url(&link))
                        .child(title),
                )
                .children(e.description.clone().map(|d| {
                    div().text_size(text::SM).text_color(t.text_muted).line_clamp(4).child(d)
                }))
                .children(media),
        )
        .into_any_element()
}

fn is_invite(url: &str) -> bool {
    url.contains("/invite/")
}

fn invite_card(ctx: &RowCtx, ix: usize, url: &str, t: &Theme) -> AnyElement {
    let client = ctx.client.clone();
    let invite = url.to_owned();
    let (server, code) = rhein_store::client::parse_invite(url, None).unwrap_or_default();
    div()
        .max_w(px(IMAGE_MAX_W))
        .min_w_0()
        .p_3()
        .rounded(radius::MD)
        .bg(t.bg_panel)
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .size(px(40.))
                .rounded(radius::LG)
                .bg(t.accent_soft)
                .flex()
                .items_center()
                .justify_center()
                .child(icon(Icon::UserPlus).text_color(t.accent)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_size(text::XS).text_color(t.text_faint).child("You've been invited to a guild"))
                .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(format!("{server} · {code}"))),
        )
        .child(Button::new(SharedString::from(format!("invite-join-{ix}")), "Join").primary().on_click(move |_, _, cx| {
            let invite = invite.clone();
            client.update(cx, |c, cx| c.join_invite(invite, None, cx));
        }))
        .into_any_element()
}

fn day_separator(ms: i64, t: &Theme) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .pt_4()
        .pb_1()
        .child(div().flex_1().h(px(1.)).bg(t.border))
        .child(div().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_faint).child(day_label(ms).to_lowercase()))
        .child(div().flex_1().h(px(1.)).bg(t.border))
        .into_any_element()
}

fn new_messages_line(t: &Theme) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_4()
        .pt_2()
        .child(div().flex_1().h(px(1.)).bg(t.danger))
        .child(
            div()
                .px_1p5()
                .rounded(px(3.))
                .font_family(FONT_MONO)
                .text_size(text::XS)
                .text_color(t.danger)
                .child("new"),
        )
        .into_any_element()
}

fn zoned(ms: i64) -> Option<jiff::Zoned> {
    jiff::Timestamp::from_millisecond(ms).ok().map(|ts| ts.to_zoned(jiff::tz::TimeZone::system()))
}

fn day_of(ms: i64) -> Option<jiff::civil::Date> {
    zoned(ms).map(|z| z.date())
}

fn day_label(ms: i64) -> String {
    let Some(z) = zoned(ms) else { return String::new() };
    let today = jiff::Zoned::now().date();
    if z.date() == today {
        "Today".into()
    } else if today.yesterday().ok() == Some(z.date()) {
        "Yesterday".into()
    } else {
        z.strftime("%B %-d, %Y").to_string()
    }
}

fn clock(z: &jiff::Zoned, h24: bool) -> String {
    if h24 { z.strftime("%H:%M").to_string() } else { z.strftime("%-I:%M %p").to_string() }
}

pub(crate) fn format_hm(ms: i64, h24: bool) -> String {
    zoned(ms).map(|z| clock(&z, h24)).unwrap_or_default()
}

fn format_time(ms: i64, h24: bool) -> String {
    let Some(z) = zoned(ms) else { return String::new() };
    let today = jiff::Zoned::now().date();
    if z.date() == today {
        format!("Today at {}", clock(&z, h24))
    } else if today.yesterday().ok() == Some(z.date()) {
        format!("Yesterday at {}", clock(&z, h24))
    } else {
        format!("{} {}", z.strftime("%d/%m/%Y"), clock(&z, h24))
    }
}

pub fn format_size(bytes: i64) -> String {
    let b = bytes as f64;
    if b < 1024.0 {
        format!("{bytes} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{:.1} KB", b / 1024.0)
    } else {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    }
}

/// Identifies a message across the swap from its pending copy to the server's: the nonce when we
/// sent it, the id otherwise.
pub fn message_key(message: &Message) -> &str {
    message.nonce.as_deref().unwrap_or(&message.id)
}

/// What a row's height still waits on: previews and GIF share pages not asked for yet, the
/// pictures whose size is only known once loaded, and a fingerprint of how far the previews
/// have got (it changes whenever one arrives and the row grows).
#[derive(Default)]
pub struct RowMedia {
    pub embeds: Vec<String>,
    pub shares: Vec<String>,
    /// Telium media links whose kind is not known yet.
    pub probes: Vec<String>,
    pub pictures: Vec<String>,
    pub progress: u64,
}

pub fn row_media(client: &Client, channel_id: &str, message: &Message) -> RowMedia {
    use std::hash::{Hash, Hasher};
    let mut media = RowMedia::default();
    let mut progress = std::collections::hash_map::DefaultHasher::new();
    for a in message.attachments.iter().filter(|a| a.is_image()) {
        if let (Some(url), (_, None)) = (&a.url, fit(a.width, a.height)) {
            media.pictures.push(url.clone());
        }
    }
    let strip_embeds = client.channel(channel_id).is_some_and(|c| c.strip_embeds);
    let links = if strip_embeds { Vec::new() } else { markdown::links_in(&message.content) };
    for url in links.iter().filter(|u| !is_invite(u) && !gif_links::is_gif(u)) {
        if gif_links::is_telium_media(url) {
            match client.media_kind(url) {
                Some(MediaProbe::Known(MediaKind::Other)) => {}
                Some(MediaProbe::Known(_)) => {
                    5u8.hash(&mut progress);
                    media.pictures.push(url.clone());
                    continue;
                }
                Some(MediaProbe::Loading) => {
                    4u8.hash(&mut progress);
                    continue;
                }
                None => {
                    media.probes.push(url.clone());
                    continue;
                }
            }
        }
        match client.embed(channel_id, url) {
            Some(EmbedState::Ready(embed)) => {
                1u8.hash(&mut progress);
                media.pictures.extend(embed.image.clone());
            }
            Some(EmbedState::Loading) => 2u8.hash(&mut progress),
            Some(EmbedState::Empty { .. }) => 3u8.hash(&mut progress),
            None => media.embeds.push(url.clone()),
        }
    }
    for url in gif_links::gifs_in(&message.content) {
        if gif_links::share(&url).is_none() {
            media.pictures.push(url);
            continue;
        }
        match client.gif_share(&url) {
            Some(GifShare::Ready(picture)) => {
                1u8.hash(&mut progress);
                media.pictures.push(picture);
            }
            Some(state) => (state == GifShare::Loading).hash(&mut progress),
            None => media.shares.push(url),
        }
    }
    media.progress = progress.finish();
    media
}
