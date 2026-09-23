//! The message pane: history as a virtual list that sticks to the bottom, and the composer.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FollowMode, FontWeight, IntoElement, ListAlignment,
    ListState, ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Subscription,
    WeakEntity, Window, div, list, prelude::*, px,
};
use rhein_proto::{Attachment, Message};
use rhein_store::{Client, ClientEvent};

use rhein_ui::theme::{radius, size};
use rhein_ui::{ActiveTheme as _, Avatar, Icon, IconButton, InputEvent, TextInput, icon, tooltip};

/// Messages from the same author this close together share one header.
const GROUP_WINDOW_MS: i64 = 7 * 60 * 1000;
const TYPING_EVERY: Duration = Duration::from_secs(6);
const QUICK_REACTIONS: [&str; 4] = ["👍", "❤️", "😂", "🎉"];

pub struct ChatView {
    client: Entity<Client>,
    list: ListState,
    channel: Rc<RefCell<Option<String>>>,
    composer: Entity<TextInput>,
    editing: Option<String>,
    last_typing: Option<Instant>,
    members_visible: bool,
    _subs: Vec<Subscription>,
}

/// Asks the shell to show or hide the member list; the shell owns that panel.
pub struct ToggleMembers;

impl EventEmitter<ToggleMembers> for ChatView {}

impl ChatView {
    pub fn new(client: Entity<Client>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(0, ListAlignment::Bottom, px(400.));
        list.set_follow_mode(FollowMode::Tail);
        let channel: Rc<RefCell<Option<String>>> = Rc::default();
        {
            let client = client.downgrade();
            let channel = channel.clone();
            list.set_scroll_handler(move |event, _window, cx| {
                if event.visible_range.start > 2 {
                    return;
                }
                let Some(id) = channel.borrow().clone() else { return };
                client
                    .update(cx, |client, cx| {
                        let ready = client.logs.get(&id).is_some_and(|l| l.loaded && !l.reached_start);
                        if ready {
                            client.load_history(id, true, cx);
                        }
                    })
                    .ok();
            });
        }
        let composer = cx.new(|cx| TextInput::new("Message", cx));
        let subs = vec![
            cx.subscribe_in(&composer, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Submit => this.submit(window, cx),
                InputEvent::Changed => this.typed(cx),
            }),
            cx.subscribe(&client, |this, _, event: &ClientEvent, cx| this.on_client(event, cx)),
            cx.observe(&client, |this, _, cx| this.sync_channel(cx)),
        ];
        let mut this = Self {
            client,
            list,
            channel,
            composer,
            editing: None,
            last_typing: None,
            members_visible: true,
            _subs: subs,
        };
        this.sync_channel(cx);
        this
    }

    pub fn set_members_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.members_visible = visible;
        cx.notify();
    }

    pub fn composer_focus(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }

    fn count(&self, cx: &App) -> usize {
        let Some(id) = self.channel.borrow().clone() else { return 0 };
        self.client.read(cx).logs.get(&id).map_or(0, |l| l.messages.len())
    }

    /// Follows the client's selected channel; a switch resets the list to that channel's history.
    fn sync_channel(&mut self, cx: &mut Context<Self>) {
        let selected = self.client.read(cx).selected_channel().map(str::to_owned);
        if *self.channel.borrow() == selected {
            return;
        }
        *self.channel.borrow_mut() = selected.clone();
        self.editing = None;
        let name = selected
            .as_deref()
            .and_then(|id| self.client.read(cx).channel(id))
            .map(|c| format!("Message #{}", c.name))
            .unwrap_or_else(|| "Message".into());
        self.composer.update(cx, |c, cx| {
            c.clear(cx);
            c.set_placeholder(name, cx);
        });
        self.list.reset(self.count(cx));
        self.list.set_follow_mode(FollowMode::Tail);
        cx.notify();
    }

    fn on_client(&mut self, event: &ClientEvent, cx: &mut Context<Self>) {
        let current = self.channel.borrow().clone();
        let is_current = |id: &String| current.as_ref() == Some(id);
        match event {
            ClientEvent::MessagesReset { channel_id } if is_current(channel_id) => {
                self.list.reset(self.count(cx));
                self.list.set_follow_mode(FollowMode::Tail);
            }
            ClientEvent::MessagesPrepended { channel_id, count } if is_current(channel_id) => {
                self.list.splice(0..0, *count);
            }
            ClientEvent::MessageAppended { channel_id } if is_current(channel_id) => {
                let n = self.list.item_count();
                self.list.splice(n..n, 1);
            }
            ClientEvent::MessageChanged { channel_id, index } if is_current(channel_id) => {
                // The next row's header depends on this one, so re-measure both.
                let end = (*index + 2).min(self.list.item_count());
                self.list.splice(*index..end, end - *index);
            }
            ClientEvent::MessageRemoved { channel_id, index } if is_current(channel_id)
                && *index < self.list.item_count() => {
                    self.list.splice(*index..*index + 1, 0);
                }
            _ => {}
        }
        cx.notify();
    }

    fn submit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(channel) = self.channel.borrow().clone() else { return };
        let text = self.composer.read(cx).text().to_owned();
        if let Some(message_id) = self.editing.take() {
            if !text.trim().is_empty() {
                self.client.update(cx, |c, cx| c.edit_message(channel, message_id, text, cx));
            }
        } else {
            if text.trim().is_empty() {
                return;
            }
            self.client.update(cx, |c, cx| c.send_message(channel, text, Vec::new(), cx));
        }
        self.last_typing = None;
        self.composer.update(cx, |c, cx| c.clear(cx));
        self.list.set_follow_mode(FollowMode::Tail);
        cx.notify();
    }

    fn typed(&mut self, cx: &mut Context<Self>) {
        if self.editing.is_some() || self.composer.read(cx).text().is_empty() {
            return;
        }
        if self.last_typing.is_some_and(|at| at.elapsed() < TYPING_EVERY) {
            return;
        }
        self.last_typing = Some(Instant::now());
        let Some(channel) = self.channel.borrow().clone() else { return };
        self.client.update(cx, |c, _| c.send_typing(&channel));
    }

    fn start_edit(&mut self, message: &Message, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = Some(message.id.clone());
        let content = message.content.clone();
        self.composer.update(cx, |c, cx| c.set_text(content, cx));
        let focus = self.composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        self.composer.update(cx, |c, cx| c.clear(cx));
        cx.notify();
    }

    fn attach(&mut self, cx: &mut Context<Self>) {
        let Some(channel) = self.channel.borrow().clone() else { return };
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Send file".into()),
        });
        let caption = self.composer.read(cx).text().to_owned();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            this.update(cx, |this, cx| {
                this.composer.update(cx, |c, cx| c.clear(cx));
                this.client.update(cx, |c, cx| c.send_file(channel, path, caption, cx));
            })
            .ok();
        })
        .detach();
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let channel_id = self.channel.borrow().clone();
        let client = self.client.read(cx);
        let Some(channel_id) = channel_id else {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .bg(t.bg_main)
                .text_color(t.text_muted)
                .flex_col()
                .gap_2()
                .child(icon(if client.showing_home { Icon::MessageSquare } else { Icon::Hash }).text_color(t.text_faint))
                .child(if client.showing_home {
                    "Direct messages arrive with end-to-end encryption in a later version."
                } else if client.guilds.is_empty() {
                    "No guilds yet. Create one or join with an invite (+ on the left)."
                } else {
                    "Pick a channel."
                })
                .into_any_element();
        };
        let channel = client.channel(&channel_id).cloned();
        let guild_id = client.guild_of_channel(&channel_id).map(|g| g.guild.id.clone());
        let log = client.logs.get(&channel_id);
        let loading = log.is_some_and(|l| l.loading);
        let reached_start = log.is_some_and(|l| l.reached_start);
        let history_error = log.and_then(|l| l.error.clone());
        let typing: Vec<String> = client
            .typing_in(&channel_id)
            .iter()
            .map(|u| client.display_name(guild_id.as_deref(), u))
            .collect();
        let name = channel.as_ref().map(|c| c.name.clone()).unwrap_or_default();
        let topic = channel.as_ref().and_then(|c| c.topic.clone());

        let header = div()
            .h(size::HEADER)
            .flex_none()
            .px_4()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(t.border)
            .child(icon(Icon::Hash).text_color(t.text_faint))
            .child(div().font_weight(FontWeight::BOLD).child(name.clone()))
            .children(topic.map(|topic| {
                div()
                    .ml_2()
                    .pl_3()
                    .border_l_1()
                    .border_color(t.bg_selected)
                    .text_sm()
                    .text_color(t.text_muted)
                    .truncate()
                    .child(topic)
            }))
            .child(div().flex_1())
            .child(
                IconButton::new("toggle-members", Icon::Users)
                    .tooltip(if self.members_visible { "Hide members" } else { "Show members" })
                    .active(self.members_visible)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ToggleMembers))),
            );

        let top_note = if loading {
            Some("Loading messages…".to_owned())
        } else if let Some(err) = history_error {
            Some(format!("Couldn't load history: {err}"))
        } else if reached_start {
            Some(format!("This is the start of #{name}."))
        } else {
            None
        };

        let view = cx.weak_entity();
        let client_entity = self.client.clone();
        let chan = channel_id.clone();
        let messages = list(self.list.clone(), move |ix, _window, cx| {
            render_row(&client_entity, &view, &chan, ix, cx)
        })
        .flex_1()
        .size_full();

        let composer = {
            let editing = self.editing.is_some();
            div()
                .flex_none()
                .px_4()
                .pb_2()
                .flex()
                .flex_col()
                .gap_1()
                .when(editing, |d| {
                    d.child(
                        div()
                            .flex()
                            .gap_2()
                            .text_xs()
                            .text_color(t.text_muted)
                            .child("Editing message")
                            .child(
                                div()
                                    .id("cancel-edit")
                                    .text_color(t.accent)
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel_edit(cx)))
                                    .child("Cancel"),
                            ),
                    )
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(IconButton::new("attach", Icon::Paperclip).tooltip("Send a file").on_click(cx.listener(|this, _, _, cx| this.attach(cx))))
                        .child(div().flex_1().child(self.composer.clone())),
                )
                .child(
                    div().h(px(16.)).text_xs().text_color(t.text_muted).child(match typing.len() {
                        0 => String::new(),
                        1 => format!("{} is typing…", typing[0]),
                        2 => format!("{} and {} are typing…", typing[0], typing[1]),
                        _ => "Several people are typing…".into(),
                    }),
                )
        };

        let _ = window;
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(t.bg_main)
            .text_color(t.text)
            .child(header)
            .children(top_note.map(|note| {
                div().px_4().pt_3().text_sm().text_color(t.text_faint).child(note)
            }))
            .child(div().flex_1().min_h_0().child(messages))
            .child(composer)
            .into_any_element()
    }
}

fn render_row(
    client: &Entity<Client>,
    view: &WeakEntity<ChatView>,
    channel_id: &str,
    ix: usize,
    cx: &mut App,
) -> AnyElement {
    let t = cx.theme().clone();
    let client_ref = client.read(cx);
    let Some(log) = client_ref.logs.get(channel_id) else { return div().into_any_element() };
    let Some(message) = log.messages.get(ix).cloned() else { return div().into_any_element() };
    let prev = ix.checked_sub(1).and_then(|p| log.messages.get(p));
    let grouped = prev.is_some_and(|p| {
        p.author == message.author && message.created_at - p.created_at < GROUP_WINDOW_MS
    });
    let guild_id = client_ref.guild_of_channel(channel_id).map(|g| g.guild.id.clone());
    let author_name = client_ref.display_name(guild_id.as_deref(), &message.author);
    let mine = client_ref.user_id() == Some(message.author.as_str());
    let pending = message.id.starts_with("pending:");
    let reply_name = message
        .reply_author
        .as_deref()
        .map(|a| client_ref.display_name(guild_id.as_deref(), a));

    let group = SharedString::from(format!("msg-{ix}"));

    let body = div()
        .flex()
        .flex_col()
        .min_w_0()
        .flex_1()
        .when(!grouped, |d| {
            d.child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(author_name.clone()))
                    .child(div().text_xs().text_color(t.text_faint).child(format_time(message.created_at))),
            )
        })
        .children(reply_name.map(|n| div().text_xs().text_color(t.text_faint).child(format!("↪ Replying to {n}"))))
        .when(!message.content.is_empty(), |d| {
            d.child(
                div()
                    .text_color(if pending { t.text_faint } else { t.text })
                    .child(message.content.clone())
                    .children(message.edited_at.map(|_| {
                        div().text_xs().text_color(t.text_faint).child(" (edited)")
                    })),
            )
        })
        .children(message.attachments.iter().enumerate().map(|(i, a)| attachment_chip(ix, i, a, &t)))
        .when(!message.reactions.is_empty(), |d| {
            d.child(div().flex().flex_wrap().gap_1().pt_1().children(message.reactions.iter().enumerate().map(
                |(i, r)| {
                    let client = client.clone();
                    let (chan, mid, emoji) = (channel_id.to_owned(), message.id.clone(), r.emoji.clone());
                    div()
                        .id(SharedString::from(format!("react-{ix}-{i}")))
                        .px_2()
                        .rounded_md()
                        .border_1()
                        .border_color(if r.me { t.accent } else { t.border })
                        .bg(if r.me { t.accent_soft } else { t.bg_raised })
                        .text_sm()
                        .cursor_pointer()
                        .on_click(move |_, _, cx| {
                            let (chan, mid, emoji) = (chan.clone(), mid.clone(), emoji.clone());
                            client.update(cx, |c, cx| c.toggle_reaction(chan, mid, emoji, cx));
                        })
                        .child(format!("{} {}", r.emoji, r.count))
                },
            )))
        });

    let gutter = if grouped {
        div()
            .w(px(40.))
            .flex_none()
            .text_xs()
            .text_color(t.text_faint)
            .opacity(0.)
            .group_hover(group.clone(), |s| s.opacity(1.))
            .child(format_time(message.created_at))
    } else {
        div().w(px(40.)).flex_none().child(Avatar::new(message.author.clone(), author_name.clone()).size(px(40.)))
    };

    let actions = (!pending).then(|| {
        let mut bar = div()
            .absolute()
            .top(px(-12.))
            .right(px(16.))
            .flex()
            .gap_0p5()
            .p_0p5()
            .rounded_md()
            .bg(t.bg_raised)
            .border_1()
            .border_color(t.border)
            .opacity(0.)
            .group_hover(group.clone(), |s| s.opacity(1.));
        for (i, emoji) in QUICK_REACTIONS.iter().enumerate() {
            let client = client.clone();
            let (chan, mid) = (channel_id.to_owned(), message.id.clone());
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
                    .child(*emoji),
            );
        }
        if mine {
            let view = view.clone();
            let for_edit = message.clone();
            bar = bar.child(IconButton::new(SharedString::from(format!("edit-{ix}")), Icon::Pencil).tooltip("Edit").on_click(
                move |_, window, cx| {
                    view.update(cx, |v, cx| v.start_edit(&for_edit, window, cx)).ok();
                },
            ));
            let client = client.clone();
            let (chan, mid) = (channel_id.to_owned(), message.id.clone());
            bar = bar.child(IconButton::new(SharedString::from(format!("del-{ix}")), Icon::Trash).tooltip("Delete").danger(true).on_click(
                move |_, _, cx| {
                    let (chan, mid) = (chan.clone(), mid.clone());
                    client.update(cx, |c, cx| c.delete_message(chan, mid, cx));
                },
            ));
        }
        bar
    });

    div()
        .relative()
        .group(group)
        .flex()
        .gap_4()
        .px_4()
        .when(grouped, |d| d.py_0p5())
        .when(!grouped, |d| d.pt_4().pb_0p5())
        .hover(|s| s.bg(t.bg_hover))
        .child(gutter)
        .child(body)
        .children(actions)
        .into_any_element()
}

fn attachment_chip(ix: usize, i: usize, a: &Attachment, t: &rhein_ui::Theme) -> impl IntoElement {
    let url = a.url.clone();
    let glyph = if a.is_image() { Icon::Image } else { Icon::File };
    div()
        .id(SharedString::from(format!("att-{ix}-{i}")))
        .mt_1()
        .max_w(px(420.))
        .px_3()
        .py_2()
        .rounded_md()
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
        .child(icon(glyph).text_color(t.text_muted))
        .child(div().flex_1().truncate().text_color(t.accent).child(a.filename.clone()))
        .child(div().text_xs().text_color(t.text_faint).child(format_size(a.size)))
}

fn format_time(ms: i64) -> String {
    let Ok(ts) = jiff::Timestamp::from_millisecond(ms) else { return String::new() };
    let zoned = ts.to_zoned(jiff::tz::TimeZone::system());
    let today = jiff::Zoned::now().date();
    if zoned.date() == today {
        zoned.strftime("%H:%M").to_string()
    } else {
        zoned.strftime("%d.%m.%Y %H:%M").to_string()
    }
}

fn format_size(bytes: i64) -> String {
    let b = bytes as f64;
    if b < 1024.0 {
        format!("{bytes} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{:.1} KB", b / 1024.0)
    } else {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    }
}
