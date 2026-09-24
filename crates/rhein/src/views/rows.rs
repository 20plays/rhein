//! One row of the message list: optional day separator and "New messages" line, the reply
//! quote, header, rich text, attachments, link previews, reactions and hover actions.

use gpui::{
    AnyElement, App, Entity, FontWeight, Hsla, IntoElement, ObjectFit, ParentElement as _, Pixels, SharedString,
    Styled as _, StyledImage as _, WeakEntity, Window, div, img, prelude::*, px,
};
use rhein_proto::{Attachment, Message};
use rhein_store::embed::{Embed, EmbedKind};
use rhein_store::{Client, EmbedState, markdown};
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
const IMAGE_MAX_W: f32 = 400.;
const IMAGE_MAX_H: f32 = 300.;

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
        (v.revealed.clone(), v.highlight.as_ref().map(|(id, _)| id.clone()), v.fresh.contains(message_key(&message)))
    });
    let (revealed, highlighted, fresh) = view_state.unwrap_or_default();
    let highlighted = highlighted.as_deref() == Some(message.id.as_str());

    let doc = markdown::parse(&message.content);
    let mentions_me = me.as_deref().is_some_and(|me| !mine && markdown::mentions(&doc, me));

    // Previews: links outside code, looked up lazily.
    let links = if strip_embeds { Vec::new() } else { markdown::links_in(&message.content) };
    let mut missing = Vec::new();
    let mut cards = Vec::new();
    for url in &links {
        if is_invite(url) {
            cards.push(invite_card(ctx, ix, url, &t));
            continue;
        }
        match client.embed(channel_id, url) {
            Some(EmbedState::Ready(embed)) => cards.push(embed_card(ctx, ix, cards.len(), embed, &t)),
            Some(EmbedState::Loading) => {}
            Some(EmbedState::Empty { retry_at }) if std::time::Instant::now() < *retry_at => {}
            _ => missing.push(url.clone()),
        }
    }

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
        pending: pending && !failed,
    };
    let body_text = (!message.content.trim().is_empty()).then(|| message_text::render(&doc, &text_ctx, cx));

    let (images, files): (Vec<&Attachment>, Vec<&Attachment>) = message.attachments.iter().partition(|a| a.is_image());
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
                    .child(div().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_faint).child(format_time(message.created_at, prefs.clock_24h))),
            )
        })
        .children(body_text.map(|b| div().text_size(body_size).child(b)))
        .when(message.edited_at.is_some(), |d| d.child(div().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_faint).child("(edited)")))
        .children(images.into_iter().enumerate().map(|(i, a)| image_attachment(ctx, ix, i, a, &t)))
        .children(files.into_iter().enumerate().map(|(i, a)| file_chip(ix, i, a, &t)))
        .children(cards)
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

    let group = SharedString::from(format!("msg-{ix}"));
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
        Some(Hsla::from(t.warning).opacity(0.07))
    } else {
        None
    };
    let row = div()
        .relative()
        .group(group)
        .flex()
        .flex_col()
        .px_4()
        .when(grouped, |d| d.py_0p5())
        .when(!grouped, |d| if prefs.compact { d.pt_1p5().pb_0p5() } else { d.pt_3().pb_0p5() })
        .when_some(row_bg, |d, bg| d.bg(bg))
        .when(mentions_me, |d| d.border_l_2().border_color(t.warning))
        .when(row_bg.is_none(), |d| d.hover(|s| s.bg(t.bg_hover)))
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

    let element = div()
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
    let mut bar = div()
        .absolute()
        .top(px(2.))
        .right(px(12.))
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

/// Fits the picture into the preview box, keeping its aspect ratio when the size is known.
fn fit(width: Option<i64>, height: Option<i64>) -> (Pixels, Pixels) {
    match (width, height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => {
            let (w, h) = (w as f32, h as f32);
            let scale = (IMAGE_MAX_W / w).min(IMAGE_MAX_H / h).min(1.0);
            (px((w * scale).max(16.)), px((h * scale).max(16.)))
        }
        _ => (px(IMAGE_MAX_W), px(IMAGE_MAX_H * 0.75)),
    }
}

fn image_attachment(ctx: &RowCtx, ix: usize, i: usize, a: &Attachment, t: &Theme) -> AnyElement {
    let Some(url) = a.url.clone() else { return file_chip(ix, i, a, t).into_any_element() };
    let (w, h) = fit(a.width, a.height);
    let view = ctx.view.clone();
    let (open, name) = (url.clone(), a.filename.clone());
    let fallback_name = a.filename.clone();
    let faint = t.text_faint;
    let raised = t.bg_raised;
    div()
        .id(SharedString::from(format!("img-{ix}-{i}")))
        .w(w)
        .max_w_full()
        .h(h)
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
                .size_full()
                .object_fit(ObjectFit::Cover)
                .with_loading(move || div().size_full().bg(raised).into_any_element())
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
        .into_any_element()
}

fn file_chip(ix: usize, i: usize, a: &Attachment, t: &Theme) -> impl IntoElement {
    let url = a.url.clone();
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
        .on_click(move |_, _, cx| {
            if let Some(url) = &url {
                cx.open_url(url);
            }
        })
        .child(icon(if a.is_image() { Icon::Image } else { Icon::File }).text_color(t.text_muted))
        .child(div().flex_1().truncate().text_color(t.accent).child(a.filename.clone()))
        .child(div().text_size(text::XS).text_color(t.text_faint).child(format_size(a.size)))
}

fn embed_card(ctx: &RowCtx, ix: usize, n: usize, e: &Embed, t: &Theme) -> AnyElement {
    let view = ctx.view.clone();
    // Just a picture: show it like an attachment.
    if e.kind == EmbedKind::Image && e.title.is_none() && e.description.is_none() {
        let Some(image) = e.image.clone() else { return div().into_any_element() };
        let (open, name) = (image.clone(), e.url.clone());
        return div()
            .id(SharedString::from(format!("embed-{ix}-{n}")))
            .max_w(px(IMAGE_MAX_W))
            .w_full()
            .max_h(px(IMAGE_MAX_H))
            .rounded(radius::LG)
            .overflow_hidden()
            .cursor_pointer()
            .on_click(move |_, window, cx| {
                let (open, name) = (open.clone(), name.clone());
                view.update(cx, |v, cx| v.open_lightbox(open, name, window, cx)).ok();
            })
            .child(img(image).max_w_full().max_h(px(IMAGE_MAX_H)).object_fit(ObjectFit::Contain))
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
            .child(img(image).max_w(px(IMAGE_MAX_W)).max_h(px(IMAGE_MAX_H)).object_fit(ObjectFit::Contain))
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
                        .text_color(t.accent)
                        .cursor_pointer()
                        .hover(|s| s.underline())
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

fn format_hm(ms: i64, h24: bool) -> String {
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
