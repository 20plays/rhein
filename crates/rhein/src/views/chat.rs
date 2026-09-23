//! The message pane: history as a virtual list that sticks to the bottom, and the composer with
//! replies, edits and staged attachments.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, ClipboardEntry, ClipboardItem, Context, Entity, EventEmitter, ExternalPaths, FocusHandle,
    Focusable, FollowMode, FontWeight, ImageFormat, IntoElement, KeyBinding, ListAlignment, ListState, ObjectFit,
    ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, StyledImage as _, Subscription,
    Window, div, img, list, prelude::*, px,
};
use rhein_proto::Message;
use rhein_store::{Client, ClientEvent, StagedState};
use rhein_ui::theme::{radius, size, text};
use rhein_ui::{ActiveTheme as _, Button, Dismiss, Icon, IconButton, InputEvent, TextInput, icon};

use crate::views::rows::{RowCtx, format_size, render_row};

const TYPING_EVERY: Duration = Duration::from_secs(6);
const HIGHLIGHT_FOR: Duration = Duration::from_millis(1600);
const COMPOSER_LINES: usize = 10;
const LIGHTBOX_CONTEXT: &str = "Lightbox";

pub fn bindings() -> Vec<KeyBinding> {
    vec![KeyBinding::new("escape", Dismiss, Some(LIGHTBOX_CONTEXT))]
}

pub struct ChatView {
    client: Entity<Client>,
    list: ListState,
    channel: Rc<RefCell<Option<String>>>,
    composer: Entity<TextInput>,
    editing: Option<String>,
    replying: Option<String>,
    last_typing: Option<Instant>,
    members_visible: bool,
    /// Spoilers opened by the user, per (message id, spoiler index).
    pub(crate) revealed: HashSet<(String, usize)>,
    /// A message briefly highlighted after a jump.
    pub(crate) highlight: Option<(String, Instant)>,
    lightbox: Option<(String, String)>,
    lightbox_focus: FocusHandle,
    _subs: Vec<Subscription>,
}

/// Asks the shell to show or hide the member list; the shell owns that panel.
pub struct ToggleMembers;

impl EventEmitter<ToggleMembers> for ChatView {}

impl ChatView {
    pub fn new(client: Entity<Client>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(0, ListAlignment::Bottom, px(600.));
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
        let composer = cx.new(|cx| TextInput::new("Message", cx).multiline(COMPOSER_LINES));
        let subs = vec![
            cx.subscribe_in(&composer, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Submit => this.submit(window, cx),
                InputEvent::Changed => this.typed(cx),
                InputEvent::UpAtStart => this.edit_last(window, cx),
                InputEvent::Cancel => this.cancel(cx),
                InputEvent::PasteFiles(item) => this.paste_files(item.clone(), cx),
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
            replying: None,
            last_typing: None,
            members_visible: true,
            revealed: HashSet::new(),
            highlight: None,
            lightbox: None,
            lightbox_focus: cx.focus_handle(),
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

    fn current(&self) -> Option<String> {
        self.channel.borrow().clone()
    }

    fn count(&self, cx: &App) -> usize {
        let Some(id) = self.current() else { return 0 };
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
        self.replying = None;
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
        let current = self.current();
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
                if *index < end {
                    self.list.splice(*index..end, end - *index);
                }
            }
            ClientEvent::MessageRemoved { channel_id, index } if is_current(channel_id) && *index < self.list.item_count() => {
                self.list.splice(*index..*index + 1, 0);
            }
            ClientEvent::Jump { channel_id, index } if is_current(channel_id) => {
                let id = self
                    .client
                    .read(cx)
                    .logs
                    .get(channel_id)
                    .and_then(|l| l.messages.get(*index))
                    .map(|m| m.id.clone());
                self.list.pause_following_tail();
                self.list.scroll_to_reveal_item(*index);
                if let Some(id) = id {
                    self.flash(id, cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    fn flash(&mut self, message_id: String, cx: &mut Context<Self>) {
        self.highlight = Some((message_id, Instant::now()));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HIGHLIGHT_FOR).await;
            this.update(cx, |this, cx| {
                if this.highlight.as_ref().is_some_and(|(_, at)| at.elapsed() >= HIGHLIGHT_FOR) {
                    this.highlight = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn submit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(channel) = self.current() else { return };
        let text = self.composer.read(cx).text().to_owned();
        if let Some(message_id) = self.editing.take() {
            if !text.trim().is_empty() {
                self.client.update(cx, |c, cx| c.edit_message(channel, message_id, text, cx));
            }
        } else {
            let has_files = !self.client.read(cx).staged(&channel).is_empty();
            if text.trim().is_empty() && !has_files {
                return;
            }
            let reply = self.replying.take();
            self.client.update(cx, |c, cx| c.send_message(channel, text, reply, cx));
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
        let Some(channel) = self.current() else { return };
        self.client.update(cx, |c, _| c.send_typing(&channel));
    }

    pub(crate) fn start_edit(&mut self, message: &Message, window: &mut Window, cx: &mut Context<Self>) {
        self.replying = None;
        self.editing = Some(message.id.clone());
        let content = message.content.clone();
        self.composer.update(cx, |c, cx| c.set_text(content, cx));
        let focus = self.composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Up on an empty composer edits your newest message, as in the web client.
    fn edit_last(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_some() || !self.composer.read(cx).text().is_empty() {
            return;
        }
        let Some(channel) = self.current() else { return };
        let Some(message) = self.client.read(cx).last_own_message(&channel).cloned() else { return };
        self.start_edit(&message, window, cx);
    }

    pub(crate) fn start_reply(&mut self, message_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            self.editing = None;
            self.composer.update(cx, |c, cx| c.clear(cx));
        }
        self.replying = Some(message_id);
        let focus = self.composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            self.composer.update(cx, |c, cx| c.clear(cx));
        }
        self.replying = None;
        cx.notify();
    }

    pub(crate) fn reveal_spoiler(&mut self, key: (String, usize), cx: &mut Context<Self>) {
        self.revealed.insert(key);
        cx.notify();
    }

    pub(crate) fn open_lightbox(&mut self, url: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.lightbox = Some((url, name));
        window.focus(&self.lightbox_focus, cx);
        cx.notify();
    }

    fn close_lightbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.lightbox = None;
        let focus = self.composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn attach(&mut self, cx: &mut Context<Self>) {
        let Some(channel) = self.current() else { return };
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach files".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            this.update(cx, |this, cx| this.stage_paths(channel, paths, cx)).ok();
        })
        .detach();
    }

    fn stage_paths(&mut self, channel: String, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        self.client.update(cx, |c, cx| {
            for path in paths.into_iter().filter(|p| p.is_file()) {
                c.stage_path(channel.clone(), path, cx);
            }
        });
    }

    fn paste_files(&mut self, item: ClipboardItem, cx: &mut Context<Self>) {
        let Some(channel) = self.current() else { return };
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    let (mime, ext) = image_type(image.format());
                    let bytes = image.bytes().to_vec();
                    let preview = Some(Arc::new(image.clone()));
                    let channel = channel.clone();
                    self.client.update(cx, |c, cx| {
                        c.stage_bytes(channel, format!("pasted-image.{ext}"), mime.into(), bytes, preview, cx)
                    });
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    self.stage_paths(channel.clone(), paths.paths().to_vec(), cx);
                }
                ClipboardEntry::String(_) => {}
            }
        }
    }

    fn render_composer(&self, channel_id: &str, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = cx.theme().clone();
        let client = self.client.read(cx);
        let guild_id = client.guild_of_channel(channel_id).map(|g| g.guild.id.clone());
        let typing: Vec<String> =
            client.typing_in(channel_id).iter().map(|u| client.display_name(guild_id.as_deref(), u)).collect();

        let context_bar = if self.editing.is_some() {
            Some(("Editing message".to_owned(), None))
        } else if let Some(reply) = &self.replying {
            client.find_message(channel_id, reply).map(|m| {
                let name = client.display_name(guild_id.as_deref(), &m.author);
                (format!("Replying to {name}"), Some(snippet(&m.content)))
            })
        } else {
            None
        };
        let context_bar = context_bar.map(|(label, detail)| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1p5()
                .rounded_t(radius::MD)
                .bg(t.bg_panel)
                .text_size(text::SM)
                .text_color(t.text_muted)
                .child(icon(if self.editing.is_some() { Icon::Pencil } else { Icon::Reply }).text_color(t.text_faint))
                .child(div().font_weight(FontWeight::MEDIUM).child(label))
                .children(detail.map(|d| div().flex_1().min_w_0().truncate().text_color(t.text_faint).child(d)))
                .child(div().flex_1())
                .child(
                    IconButton::new("composer-cancel", Icon::X)
                        .tooltip("Cancel (Esc)")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                )
        });

        let staged = client.staged(channel_id);
        let chips = (!staged.is_empty()).then(|| {
            div().flex().flex_wrap().gap_2().pb_2().children(staged.iter().map(|s| {
                let (status, color) = match &s.state {
                    StagedState::Uploading => ("Uploading…".to_owned(), t.text_faint),
                    StagedState::Ready(_) => (format_size(s.size as i64), t.text_faint),
                    StagedState::Failed(err) => (format!("Failed: {err}"), t.danger),
                };
                let thumb: Option<gpui::AnyElement> = match (&s.preview, &s.state) {
                    (Some(image), _) => Some(img(image.clone()).size(px(40.)).rounded(radius::SM).object_fit(ObjectFit::Cover).into_any_element()),
                    (None, StagedState::Ready(a)) if a.is_image() => a.url.clone().map(|u| {
                        img(u).size(px(40.)).rounded(radius::SM).object_fit(ObjectFit::Cover).into_any_element()
                    }),
                    _ => None,
                };
                let client = self.client.clone();
                let (chan, id) = (channel_id.to_owned(), s.id);
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_1()
                    .pr_2()
                    .rounded(radius::MD)
                    .bg(t.bg_raised)
                    .border_1()
                    .border_color(t.border)
                    .child(thumb.unwrap_or_else(|| {
                        div()
                            .size(px(40.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(Icon::File).text_color(t.text_muted))
                            .into_any_element()
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .max_w(px(180.))
                            .child(div().text_size(text::SM).truncate().child(s.name.clone()))
                            .child(div().text_size(text::XS).text_color(color).truncate().child(status)),
                    )
                    .child(
                        IconButton::new(SharedString::from(format!("unstage-{}", s.id)), Icon::X)
                            .tooltip("Remove")
                            .on_click(move |_, _, cx| client.update(cx, |c, cx| c.unstage(&chan, id, cx))),
                    )
            }))
        });

        div()
            .flex_none()
            .px_4()
            .pb_2()
            .flex()
            .flex_col()
            .children(chips)
            .children(context_bar)
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div().pt_1().child(
                            IconButton::new("attach", Icon::Paperclip)
                                .tooltip("Attach files")
                                .on_click(cx.listener(|this, _, _, cx| this.attach(cx))),
                        ),
                    )
                    .child(div().flex_1().min_w_0().child(self.composer.clone())),
            )
            .child(div().h(px(18.)).pt_0p5().text_size(text::XS).text_color(t.text_muted).child(match typing.len() {
                0 => String::new(),
                1 => format!("{} is typing…", typing[0]),
                2 => format!("{} and {} are typing…", typing[0], typing[1]),
                _ => "Several people are typing…".into(),
            }))
    }

    fn render_lightbox(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let (url, name) = self.lightbox.clone()?;
        let t = cx.theme().clone();
        let open = url.clone();
        Some(
            div()
                .id("lightbox")
                .track_focus(&self.lightbox_focus)
                .key_context(LIGHTBOX_CONTEXT)
                .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.close_lightbox(window, cx)))
                .absolute()
                .inset_0()
                .bg(t.overlay)
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .p_8()
                .on_click(cx.listener(|this, _, window, cx| this.close_lightbox(window, cx)))
                .child(img(url).max_w_full().max_h(gpui::relative(0.85)).object_fit(ObjectFit::Contain))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .text_size(text::SM)
                        .text_color(t.text_muted)
                        .child(name)
                        .child(
                            Button::new("lightbox-open", "Open in browser")
                                .ghost()
                                .icon(Icon::ExternalLink)
                                .on_click(move |_, _, cx| cx.open_url(&open)),
                        ),
                )
                .into_any_element(),
        )
    }
}

fn snippet(content: &str) -> String {
    let line = content.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut out: String = line.chars().take(120).collect();
    if line.chars().count() > 120 {
        out.push('…');
    }
    out
}

fn image_type(format: ImageFormat) -> (&'static str, &'static str) {
    match format {
        ImageFormat::Png => ("image/png", "png"),
        ImageFormat::Jpeg => ("image/jpeg", "jpg"),
        ImageFormat::Webp => ("image/webp", "webp"),
        ImageFormat::Gif => ("image/gif", "gif"),
        ImageFormat::Svg => ("image/svg+xml", "svg"),
        ImageFormat::Bmp => ("image/bmp", "bmp"),
        ImageFormat::Tiff => ("image/tiff", "tiff"),
        _ => ("application/octet-stream", "bin"),
    }
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let channel_id = self.current();
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
        let log = client.logs.get(&channel_id);
        let loading = log.is_some_and(|l| l.loading);
        let reached_start = log.is_some_and(|l| l.reached_start);
        let history_error = log.and_then(|l| l.error.clone());
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
            .child(div().font_weight(FontWeight::SEMIBOLD).child(name.clone()))
            .children(topic.map(|topic| {
                div()
                    .ml_2()
                    .pl_3()
                    .border_l_1()
                    .border_color(t.bg_selected)
                    .text_size(text::SM)
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

        let row_ctx = Rc::new(RowCtx {
            client: self.client.clone(),
            view: cx.weak_entity(),
            channel_id: channel_id.clone(),
        });
        let messages = list(self.list.clone(), move |ix, window, cx| render_row(&row_ctx, ix, window, cx)).size_full();

        let behind = !self.list.is_following_tail() && self.count(cx) > 0;
        let jump = behind.then(|| {
            div().absolute().bottom(px(8.)).right(px(16.)).child(
                Button::new("jump-present", "Jump to present")
                    .icon(Icon::ArrowDown)
                    .primary()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.list.scroll_to_end();
                        this.list.set_follow_mode(FollowMode::Tail);
                        cx.notify();
                    })),
            )
        });

        let composer = self.render_composer(&channel_id, cx);
        let lightbox = self.render_lightbox(cx);
        let drop_channel = channel_id.clone();

        div()
            .id("chat")
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(t.bg_main)
            .text_color(t.text)
            .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(t.bg_panel))
            .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                this.stage_paths(drop_channel.clone(), paths.paths().to_vec(), cx);
            }))
            .child(header)
            .children(top_note.map(|note| div().px_4().pt_3().text_size(text::SM).text_color(t.text_faint).child(note)))
            .child(div().relative().flex_1().min_h_0().child(messages).children(jump))
            .child(composer)
            .children(lightbox)
            .into_any_element()
    }
}
