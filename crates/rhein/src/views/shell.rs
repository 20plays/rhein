//! The signed-in window: guild rail | channel sidebar | chat | member list, plus its modals.

use std::collections::HashSet;

use gpui::{
    EventEmitter,
    AnyElement, AppContext as _, Context, Entity, FocusHandle, Focusable as _, FontWeight, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div, point,
    prelude::*, px,
};
use rhein_store::{Client, ServerStatus};
use rhein_ui::theme::{radius, size, text};
use rhein_ui::{
    ActiveTheme as _, Avatar, Button, CountBadge, Icon, IconButton, InputEvent, ListRow, MenuEntry, MenuHost, Modal,
    Presence, Tabs, Tag, TextInput, Toast, ToastKind, icon, icon_sized, section_label, tooltip,
};

use crate::views::chat::{ChatEvent, ChatView, PanelKind};
use crate::views::panels::{PanelEvent, PinsPanel, SearchPanel, ThreadsPanel};

#[derive(Clone, Copy, PartialEq, Eq)]
enum DialogTab {
    Join,
    Create,
    Server,
}

impl DialogTab {
    const ALL: [DialogTab; 3] = [DialogTab::Join, DialogTab::Create, DialogTab::Server];

    fn label(self) -> &'static str {
        match self {
            DialogTab::Join => "Join",
            DialogTab::Create => "Create",
            DialogTab::Server => "Add server",
        }
    }
}

/// What occupies the right-hand slot. Members is the default and stays remembered via
/// `show_members`; the others replace it while open.
enum Panel {
    None,
    Pins { channel: String, view: Entity<PinsPanel> },
    Threads { channel: String, view: Entity<ThreadsPanel> },
    Search { guild: String, view: Entity<SearchPanel> },
    Thread { view: Entity<ChatView> },
}

pub struct Shell {
    client: Entity<Client>,
    chat: Entity<ChatView>,
    dialog: Option<DialogTab>,
    dialog_input: Entity<TextInput>,
    create_on: Option<String>,
    show_members: bool,
    panel: Panel,
    panel_subs: Vec<Subscription>,
    /// Categories the user folded away.
    collapsed: HashSet<String>,
    confirm_sign_out: bool,
    modal_focus: FocusHandle,
    menu: MenuHost,
    _subs: Vec<Subscription>,
}

impl Shell {
    pub fn new(client: Entity<Client>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat = cx.new(|cx| ChatView::new(client.clone(), window, cx));
        let dialog_input = cx.new(|cx| TextInput::new("", cx));
        let subs = vec![
            cx.observe_in(&client, window, |this, _, window, cx| {
                this.follow_channel(window, cx);
                cx.notify();
            }),
            cx.subscribe_in(&chat, window, |this, _, event: &ChatEvent, window, cx| match event {
                ChatEvent::TogglePanel(kind) => this.toggle_panel(*kind, window, cx),
                ChatEvent::OpenThread(id) => this.open_thread(id.clone(), window, cx),
                ChatEvent::ClosePanel => this.close_panel(cx),
            }),
            cx.subscribe_in(&dialog_input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Submit) {
                    this.confirm_dialog(window, cx);
                }
            }),
        ];
        let focus = chat.read(cx).composer_focus(cx);
        window.focus(&focus, cx);
        Self {
            client,
            chat,
            dialog: None,
            dialog_input,
            create_on: None,
            show_members: true,
            panel: Panel::None,
            panel_subs: Vec::new(),
            collapsed: HashSet::new(),
            confirm_sign_out: false,
            modal_focus: cx.focus_handle(),
            menu: MenuHost::default(),
            _subs: subs,
        }
    }

    fn active_kind(&self) -> Option<PanelKind> {
        match self.panel {
            Panel::None if self.show_members => Some(PanelKind::Members),
            Panel::Pins { .. } => Some(PanelKind::Pins),
            Panel::Threads { .. } => Some(PanelKind::Threads),
            Panel::Search { .. } => Some(PanelKind::Search),
            _ => None,
        }
    }

    fn sync_header(&self, cx: &mut Context<Self>) {
        let kind = self.active_kind();
        self.chat.update(cx, |c, cx| c.set_active_panel(kind, cx));
    }

    fn toggle_panel(&mut self, kind: PanelKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_kind() == Some(kind) {
            if kind == PanelKind::Members {
                self.show_members = false;
            }
            self.close_panel(cx);
            return;
        }
        self.open_panel(kind, window, cx);
    }

    fn open_panel(&mut self, kind: PanelKind, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client.read(cx);
        let Some(channel) = client.selected_channel().map(str::to_owned) else { return };
        let guild = client.guild_of_channel(&channel).map(|g| g.guild.id.clone());
        let chat_view = self.chat.downgrade();
        self.panel_subs.clear();
        self.panel = match kind {
            PanelKind::Members => {
                self.show_members = true;
                Panel::None
            }
            PanelKind::Pins => {
                let client = self.client.clone();
                let ch = channel.clone();
                let view = cx.new(|cx| PinsPanel::new(client, chat_view, ch, cx));
                self.panel_subs.push(self.panel_events(&view, window, cx));
                Panel::Pins { channel, view }
            }
            PanelKind::Threads => {
                let client = self.client.clone();
                let ch = channel.clone();
                let view = cx.new(|cx| ThreadsPanel::new(client, ch, window, cx));
                self.panel_subs.push(self.panel_events(&view, window, cx));
                Panel::Threads { channel, view }
            }
            PanelKind::Search => {
                let Some(guild) = guild else { return };
                let client = self.client.clone();
                let (g, ch) = (guild.clone(), channel.clone());
                let view = cx.new(|cx| SearchPanel::new(client, chat_view, g, ch, window, cx));
                self.panel_subs.push(self.panel_events(&view, window, cx));
                Panel::Search { guild, view }
            }
        };
        self.sync_header(cx);
        cx.notify();
    }

    fn panel_events<P: EventEmitter<PanelEvent>>(&self, view: &Entity<P>, window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe_in(view, window, |this, _, event: &PanelEvent, window, cx| match event {
            PanelEvent::Close => this.close_panel(cx),
            PanelEvent::OpenThread(id) => this.open_thread(id.clone(), window, cx),
        })
    }

    fn open_thread(&mut self, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let view = cx.new(|cx| ChatView::fixed(client, thread_id, window, cx));
        self.panel_subs.clear();
        self.panel_subs.push(cx.subscribe_in(&view, window, |this, _, event: &ChatEvent, window, cx| match event {
            ChatEvent::ClosePanel => this.close_panel(cx),
            ChatEvent::OpenThread(id) => this.open_thread(id.clone(), window, cx),
            ChatEvent::TogglePanel(kind) => this.toggle_panel(*kind, window, cx),
        }));
        let focus = view.read(cx).composer_focus(cx);
        window.focus(&focus, cx);
        self.panel = Panel::Thread { view };
        self.sync_header(cx);
        cx.notify();
    }

    fn close_panel(&mut self, cx: &mut Context<Self>) {
        self.panel = Panel::None;
        self.panel_subs.clear();
        self.sync_header(cx);
        cx.notify();
    }

    /// Pins and threads belong to a channel, search to a guild: follow the selection, and close a
    /// thread that belongs to another guild.
    fn follow_channel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client.read(cx);
        let channel = client.selected_channel().map(str::to_owned);
        let guild = channel.as_deref().and_then(|c| client.guild_of_channel(c)).map(|g| g.guild.id.clone());
        let reopen = match &self.panel {
            Panel::Pins { channel: c, .. } if Some(c) != channel.as_ref() => Some(PanelKind::Pins),
            Panel::Threads { channel: c, .. } if Some(c) != channel.as_ref() => Some(PanelKind::Threads),
            Panel::Search { guild: g, .. } if Some(g) != guild.as_ref() => Some(PanelKind::Search),
            _ => None,
        };
        match reopen {
            Some(_) if channel.is_none() => self.close_panel(cx),
            Some(kind) => self.open_panel(kind, window, cx),
            None => {}
        }
    }

    fn render_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = cx.theme().clone();
        let (content, width): (AnyElement, gpui::Pixels) = match &self.panel {
            Panel::None => return self.members(cx),
            Panel::Pins { view, .. } => (view.clone().into_any_element(), px(320.)),
            Panel::Threads { view, .. } => (view.clone().into_any_element(), px(320.)),
            Panel::Search { view, .. } => (view.clone().into_any_element(), px(380.)),
            Panel::Thread { view } => (view.clone().into_any_element(), px(420.)),
        };
        Some(
            div()
                .w(width)
                .h_full()
                .flex_none()
                .flex()
                .flex_col()
                .bg(t.bg_panel)
                .border_l_1()
                .border_color(t.border)
                .child(content)
                .into_any_element(),
        )
    }

    fn open_dialog(&mut self, tab: DialogTab, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog = Some(tab);
        let placeholder = match tab {
            DialogTab::Join => "server/CODE or https://server/invite/CODE",
            DialogTab::Create => "Guild name",
            DialogTab::Server => "chat.example.com",
        };
        self.dialog_input.update(cx, |i, cx| {
            i.clear(cx);
            i.set_placeholder(placeholder, cx);
        });
        if self.create_on.is_none() {
            self.create_on = self.client.read(cx).home.as_ref().map(|h| h.doc.server_name.clone());
        }
        window.focus(&self.dialog_input.focus_handle(cx), cx);
        cx.notify();
    }

    fn ask_sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog = None;
        self.confirm_sign_out = true;
        window.focus(&self.modal_focus, cx);
        cx.notify();
    }

    fn sign_out(&mut self, cx: &mut Context<Self>) {
        self.confirm_sign_out = false;
        self.client.update(cx, |c, cx| c.sign_out(cx));
    }

    fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog = None;
        self.confirm_sign_out = false;
        let focus = self.chat.read(cx).composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn confirm_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.dialog else { return };
        let text = self.dialog_input.read(cx).text().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let client = self.client.read(cx);
        let default_server = client
            .selected_guild
            .as_ref()
            .and_then(|g| client.guild(g))
            .map(|g| g.server.clone())
            .or_else(|| client.home.as_ref().map(|h| h.doc.server_name.clone()));
        let create_on = self.create_on.clone();
        self.client.update(cx, |c, cx| match tab {
            DialogTab::Join => c.join_invite(text, default_server, cx),
            DialogTab::Create => {
                if let Some(server) = create_on {
                    c.create_guild(server, text, cx)
                }
            }
            DialogTab::Server => c.add_server(text, cx),
        });
        self.close_dialog(window, cx);
    }

    fn guild_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(gid) = self.client.read(cx).selected_guild.clone() else { return };
        let client = self.client.clone();
        let this = cx.weak_entity();
        let this2 = this.clone();
        let entries = vec![
            MenuEntry::item("Invite people", move |_, cx| {
                let gid = gid.clone();
                client.update(cx, |c, cx| c.create_invite(gid, cx));
            })
            .with_icon(Icon::UserPlus),
            MenuEntry::Separator,
            MenuEntry::item("Join a guild", move |window, cx| {
                this.update(cx, |v, cx| v.open_dialog(DialogTab::Join, window, cx)).ok();
            })
            .with_icon(Icon::LogIn),
            MenuEntry::item("Create a guild", move |window, cx| {
                this2.update(cx, |v, cx| v.open_dialog(DialogTab::Create, window, cx)).ok();
            })
            .with_icon(Icon::Plus),
        ];
        let at = point(size::RAIL + px(12.), size::HEADER);
        self.menu.show(at, entries, |v: &mut Shell| &mut v.menu, window, cx);
    }

    fn rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme().clone();
        let client = self.client.read(cx);
        let home = client.showing_home;
        let selected = client.selected_guild.clone();

        // One rail item: a pill marker on the left edge (tall when open, a dot when unread),
        // the round button, and a mention badge in its corner.
        let item = |id: SharedString, active: bool, unread: bool, mentions: u32, tip: String, body: AnyElement| {
            div()
                .relative()
                .w_full()
                .flex()
                .justify_center()
                .when(active || unread, |d| {
                    d.child(
                        div()
                            .absolute()
                            .left_0()
                            .top(if active { px(8.) } else { px(16.) })
                            .w(px(3.))
                            .h(if active { px(24.) } else { px(8.) })
                            .rounded_r(px(2.))
                            .bg(t.text),
                    )
                })
                .child(
                    div()
                        .id(id)
                        .relative()
                        .size(px(40.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(if active { radius::LG } else { px(20.) })
                        .bg(if active { t.accent } else { t.bg_raised })
                        .text_color(if active { t.text_on_accent } else { t.text_muted })
                        .text_size(text::MD)
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .hover(|s| s.rounded(radius::LG).text_color(t.text))
                        .tooltip(tooltip(tip))
                        .child(body)
                        .when(mentions > 0, |d| {
                            d.child(div().absolute().bottom(px(-3.)).right(px(-3.)).child(CountBadge(mentions)))
                        }),
                )
        };

        let home_item = {
            let client = self.client.clone();
            item(
                "rail-home".into(),
                home,
                false,
                0,
                "Direct messages".into(),
                icon(Icon::House).text_color(if home { t.text_on_accent } else { t.text_muted }).into_any_element(),
            )
            .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| client.update(cx, |c, cx| c.show_home(cx)))
        };

        let mut rail = div()
            .id("rail")
            .w(size::RAIL)
            .h_full()
            .flex_none()
            .bg(t.bg_base)
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .py_3()
            .overflow_y_scroll()
            .child(home_item)
            .child(div().w(px(24.)).h(px(1.)).my_1().bg(t.border_strong));

        let mut last_server: Option<&str> = None;
        for entry in &client.guilds {
            if last_server.is_some_and(|s| s != entry.server) {
                rail = rail.child(div().w(px(24.)).h(px(1.)).my_1().bg(t.border_strong));
            }
            last_server = Some(&entry.server);
            let id = entry.guild.id.clone();
            let active = !home && selected.as_deref() == Some(id.as_str());
            let (unread, mentions) = client.guild_unread(&id);
            let client = self.client.clone();
            rail = rail.child(
                item(
                    SharedString::from(format!("guild-{id}")),
                    active,
                    unread,
                    mentions,
                    format!("{} · {}", entry.guild.name, entry.server),
                    initials(&entry.guild.name).into_any_element(),
                )
                .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                    let id = id.clone();
                    client.update(cx, |c, cx| c.select_guild(id, cx));
                }),
            );
        }
        rail.child(
            div()
                .id("add-guild")
                .size(px(40.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(20.))
                .bg(t.bg_raised)
                .cursor_pointer()
                .hover(|s| s.rounded(radius::LG))
                .tooltip(tooltip("Join or create a guild"))
                .on_click(cx.listener(|this, _, window, cx| this.open_dialog(DialogTab::Join, window, cx)))
                .child(icon(Icon::Plus).text_color(t.success)),
        )
        .into_any_element()
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme().clone();
        let client = self.client.read(cx);
        let home = client.showing_home;
        let entry = if home { None } else { client.selected_guild.as_ref().and_then(|g| client.guild(g)) };
        let selected_channel = client.selected_channel().map(str::to_owned);

        let header = div()
            .id("guild-header")
            .h(size::HEADER)
            .flex_none()
            .px_4()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(t.border)
            .when(entry.is_some(), |d| {
                d.cursor_pointer()
                    .hover(|s| s.bg(t.bg_hover))
                    .on_click(cx.listener(|this, _, window, cx| this.guild_menu(window, cx)))
            })
            .child(
                div().flex_1().min_w_0().font_weight(FontWeight::SEMIBOLD).truncate().child(match (home, entry) {
                    (true, _) => "Direct messages".to_owned(),
                    (false, Some(e)) => e.guild.name.clone(),
                    (false, None) => "Rhein".to_owned(),
                }),
            )
            .when(entry.is_some(), |d| d.child(icon(Icon::ChevronDown).text_color(t.text_muted)));

        let mut channels = div().id("channels").flex_1().overflow_y_scroll().px_2().pb_2().pt_2();
        if home {
            channels = channels.child(
                div()
                    .px_2()
                    .pt_6()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .text_center()
                    .text_size(text::SM)
                    .text_color(t.text_faint)
                    .child(icon(Icon::Lock).text_color(t.text_faint))
                    .child("End-to-end encrypted direct messages are coming in a later version."),
            );
        } else if let Some(entry) = entry {
            if let Some(server) = client.server(&entry.server).filter(|s| s.status != ServerStatus::Ready) {
                channels = channels.child(
                    div()
                        .px_2()
                        .pb_2()
                        .text_size(text::SM)
                        .text_color(t.warning)
                        .child(format!("{}: {}", server.name, server.status.label())),
                );
            }
            let gid = entry.guild.id.clone();
            for ch in entry.ordered_channels() {
                if ch.is_category() {
                    let collapsed = self.collapsed.contains(&ch.id);
                    let cat = ch.id.clone();
                    channels = channels.child(
                        div()
                            .id(SharedString::from(format!("cat-{}", ch.id)))
                            .flex()
                            .items_center()
                            .gap_1()
                            .pl_1()
                            .pt_4()
                            .pb_1()
                            .text_size(text::XS)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text_faint)
                            .cursor_pointer()
                            .hover(|s| s.text_color(t.text_muted))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.collapsed.remove(&cat) {
                                    this.collapsed.insert(cat.clone());
                                }
                                cx.notify();
                            }))
                            .child(icon_sized(if collapsed { Icon::ChevronRight } else { Icon::ChevronDown }, px(12.)))
                            .child(ch.name.to_uppercase()),
                    );
                    continue;
                }
                let active = selected_channel.as_deref() == Some(ch.id.as_str());
                let mark = client.read_mark(&ch.id);
                // A collapsed category still shows its open channel and anything with news.
                let hidden = ch.parent_id.as_ref().is_some_and(|p| self.collapsed.contains(p));
                if hidden && !active && !mark.is_unread() && mark.mentions == 0 {
                    continue;
                }
                let glyph = match ch.kind.as_str() {
                    "voice" => Icon::Volume,
                    "announcement" => Icon::Megaphone,
                    "forum" => Icon::MessagesSquare,
                    _ => Icon::Hash,
                };
                let mut row = ListRow::new(SharedString::from(format!("chan-{}", ch.id)), ch.name.clone())
                    .icon(glyph)
                    .selected(active)
                    .strong(mark.is_unread())
                    .muted(mark.muted && !active)
                    .when(mark.mentions > 0, |r| r.trailing(CountBadge(mark.mentions)));
                if ch.is_textual() {
                    let id = ch.id.clone();
                    let client = self.client.clone();
                    row = row.on_click(move |_, _, cx| {
                        let id = id.clone();
                        client.update(cx, |c, cx| c.select_channel(id, cx));
                    });
                } else if ch.is_voice() {
                    row = row.tooltip("Voice is coming soon");
                }
                channels = channels.child(row);
                if ch.is_voice() {
                    for user in client.voice_members(&gid, &ch.id) {
                        let name = client.display_name(Some(&gid), &user);
                        channels = channels.child(
                            div()
                                .pl(px(30.))
                                .py_0p5()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(text::SM)
                                .text_color(t.text_muted)
                                .child(Avatar::new(user.clone(), name.clone()).size(px(20.)))
                                .child(div().truncate().child(name)),
                        );
                    }
                }
            }
        } else {
            for server in &client.servers {
                channels = channels.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_size(text::SM)
                        .text_color(if server.status == ServerStatus::Ready { t.text_faint } else { t.warning })
                        .child(format!("{}: {}", server.name, server.status.label()))
                        .children(server.error.clone().map(|e| div().text_color(t.danger).child(e))),
                );
            }
        }

        let me = client.user_id().unwrap_or_default().to_owned();
        let my_name = client.display_name(None, &me);
        let user_panel = div()
            .h(px(56.))
            .flex_none()
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .bg(t.bg_base)
            .child(Avatar::new(me.clone(), my_name.clone()).size(px(32.)).presence(Some(Presence::Online)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().text_size(text::MD).font_weight(FontWeight::SEMIBOLD).truncate().child(my_name))
                    .child(div().text_size(text::XS).text_color(t.text_faint).truncate().child(me)),
            )
            .child(
                IconButton::new("sign-out", Icon::LogOut)
                    .tooltip("Sign out")
                    .on_click(cx.listener(|this, _, window, cx| this.ask_sign_out(window, cx))),
            );

        div()
            .w(size::SIDEBAR)
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(t.bg_panel)
            .child(header)
            .child(channels)
            .child(user_panel)
            .into_any_element()
    }

    fn members(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.show_members {
            return None;
        }
        let t = cx.theme().clone();
        let client = self.client.read(cx);
        if client.showing_home {
            return None;
        }
        let gid = client.selected_guild.clone()?;
        let members = client.members.get(&gid)?;
        let (online, offline): (Vec<_>, Vec<_>) = members.iter().partition(|m| m.status() != "offline");
        let row = |m: &rhein_proto::Member| {
            let name = m.shown_name().to_owned();
            let presence = Presence::parse(m.status());
            ListRow::new(SharedString::from(format!("member-{}", m.user_id)), name.clone())
                .leading(Avatar::new(m.user_id.clone(), name).size(px(28.)).presence(Some(presence)))
                .muted(presence == Presence::Offline)
                .when(m.is_bot, |r| r.trailing(Tag("BOT")))
        };
        let online_label = section_label(format!("Online — {}", online.len()), cx);
        let offline_label = (!offline.is_empty()).then(|| section_label(format!("Offline — {}", offline.len()), cx));
        Some(
            div()
                .id("members")
                .w(size::PANEL)
                .h_full()
                .flex_none()
                .bg(t.bg_panel)
                .px_2()
                .overflow_y_scroll()
                .child(online_label)
                .children(online.into_iter().map(row))
                .children(offline_label)
                .children(offline.into_iter().map(row))
                .into_any_element(),
        )
    }

    fn dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tab = self.dialog?;
        let t = cx.theme().clone();
        let servers: Vec<String> =
            self.client.read(cx).servers.iter().filter(|s| s.api.is_some()).map(|s| s.name.clone()).collect();
        let explanation = match tab {
            DialogTab::Join => {
                "Join a guild with an invite. If it lives on another server, Rhein signs you in there with your account; you don't need a new one."
            }
            DialogTab::Create => {
                "A guild belongs entirely to the server it lives on: channels, roles, messages and moderation. Choose the server deliberately."
            }
            DialogTab::Server => "Connect another Telium server to see its guilds. Your account stays on your home server.",
        };
        let server_picker = (tab == DialogTab::Create).then(|| {
            let current = self.create_on.clone();
            div().flex().flex_wrap().gap_1().children(servers.into_iter().map(|name| {
                let active = current.as_deref() == Some(name.as_str());
                let pick = name.clone();
                div()
                    .id(SharedString::from(format!("pick-{name}")))
                    .px_2()
                    .py_1()
                    .rounded(radius::MD)
                    .flex()
                    .items_center()
                    .gap_1()
                    .border_1()
                    .border_color(if active { t.accent } else { t.border_strong })
                    .text_size(text::SM)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.create_on = Some(pick.clone());
                        cx.notify();
                    }))
                    .child(icon(Icon::Server).text_color(t.text_muted))
                    .child(name)
            }))
        });
        let selected = DialogTab::ALL.iter().position(|d| *d == tab).unwrap_or(0);
        let this = cx.weak_entity();
        let tabs = Tabs::new("dialog-tabs", DialogTab::ALL.map(DialogTab::label)).selected(selected).on_select(
            move |i, window, cx| {
                this.update(cx, |v, cx| v.open_dialog(DialogTab::ALL[i], window, cx)).ok();
            },
        );
        let this = cx.weak_entity();
        Some(
            Modal::new("add-dialog")
                .on_dismiss(move |window, cx| {
                    this.update(cx, |v, cx| v.close_dialog(window, cx)).ok();
                })
                .child(tabs)
                .child(div().text_color(t.text_muted).child(explanation))
                .children(server_picker)
                .child(self.dialog_input.clone())
                .footer(
                    Button::new("dialog-cancel", "Cancel")
                        .ghost()
                        .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))),
                )
                .footer(
                    Button::new("dialog-ok", tab.label())
                        .primary()
                        .on_click(cx.listener(|this, _, window, cx| this.confirm_dialog(window, cx))),
                )
                .into_any_element(),
        )
    }

    /// Signing out drops this device's session, so it asks first.
    fn sign_out_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.confirm_sign_out {
            return None;
        }
        let t = cx.theme().clone();
        let me = self.client.read(cx).user_id().unwrap_or_default().to_owned();
        let this = cx.weak_entity();
        Some(
            Modal::new("sign-out")
                .title("Sign out?")
                .width(px(400.))
                .focus(&self.modal_focus)
                .on_dismiss(move |window, cx| {
                    this.update(cx, |v, cx| v.close_dialog(window, cx)).ok();
                })
                .child(div().text_color(t.text_muted).child(format!(
                    "You'll be signed out of {me} on this device. Your account, guilds and \
                     messages stay on your servers; you can sign in again at any time."
                )))
                .footer(
                    Button::new("sign-out-cancel", "Cancel")
                        .ghost()
                        .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))),
                )
                .footer(
                    Button::new("sign-out-confirm", "Sign out")
                        .danger()
                        .icon(Icon::LogOut)
                        .on_click(cx.listener(|this, _, _, cx| this.sign_out(cx))),
                )
                .into_any_element(),
        )
    }

    fn toast(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let client = self.client.read(cx);
        let (text, kind) = match (&client.error, &client.notice) {
            (Some(e), _) => (e.clone(), ToastKind::Error),
            (None, Some(n)) => (n.clone(), ToastKind::Info),
            _ => return None,
        };
        let client = self.client.clone();
        Some(
            div()
                .absolute()
                .bottom(px(84.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(Toast::new(text, kind).on_dismiss(move |_, _, cx| client.update(cx, |c, cx| c.dismiss(cx))))
                .into_any_element(),
        )
    }
}

fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let out: String = match words.as_slice() {
        [] => "?".into(),
        [one] => one.chars().take(2).collect(),
        [first, .., last] => first.chars().take(1).chain(last.chars().take(1)).collect(),
    };
    if out.is_empty() { "?".into() } else { out }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rail = self.rail(cx);
        let sidebar = self.sidebar(cx);
        let members = self.render_panel(cx);
        let dialog = self.dialog(cx);
        let sign_out = self.sign_out_dialog(cx);
        let toast = self.toast(cx);
        div()
            .relative()
            .size_full()
            .flex()
            .child(rail)
            .child(sidebar)
            .child(self.chat.clone())
            .children(members)
            .children(toast)
            .children(dialog)
            .children(sign_out)
            .children(self.menu.element())
    }
}
