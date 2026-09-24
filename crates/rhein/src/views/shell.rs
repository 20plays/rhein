//! The signed-in window: sidebar tree with your profile | chat over a status line | side panel,
//! plus its modals.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable as _, FontWeight, IntoElement, KeyBinding, MouseButton, ParentElement as _, Pixels, Point, Render,
    SharedString, Styled as _, Subscription, Window, actions, anchored, deferred, div, prelude::*, px,
};
use rhein_store::{Client, ServerStatus, perms};
use rhein_ui::theme::{FONT_MONO, radius, size, text};
use rhein_ui::{
    ActiveTheme as _, motion, Avatar, Button, CountBadge, Icon, IconButton, InputEvent, ListRow, MenuEntry, MenuHost, Modal,
    Presence, Tabs, Tag, TextInput, Toast, ToastKind, UnreadDot, icon, icon_sized, section_label, tooltip,
};

use crate::views::chat::{ChatEvent, ChatView, PanelKind};
use crate::views::home::HomeView;
use crate::views::profile;
use crate::views::panels::{PanelEvent, PinsPanel, SearchPanel, ThreadsPanel};
use crate::views::settings::{SETTINGS_CONTEXT, SettingsEvent, SettingsView};
use crate::views::status::StatusLine;
use crate::views::switcher::QuickSwitcher;

actions!(rhein_shell, [OpenSwitcher, OpenSettings, PrevChannel, NextChannel, PrevUnread, NextUnread]);

const SHELL_CONTEXT: &str = "Shell";

/// The status line (40) + the composer's bottom padding (12) + the composer box (42).
const PROFILE_STRIP_H: Pixels = px(94.);

pub fn bindings() -> Vec<KeyBinding> {
    let ctx = Some(SHELL_CONTEXT);
    let mut out = vec![
        KeyBinding::new("alt-up", PrevChannel, ctx),
        KeyBinding::new("alt-down", NextChannel, ctx),
        KeyBinding::new("alt-shift-up", PrevUnread, ctx),
        KeyBinding::new("alt-shift-down", NextUnread, ctx),
        KeyBinding::new("escape", rhein_ui::Dismiss, Some(SETTINGS_CONTEXT)),
    ];
    for m in ["ctrl", "cmd"] {
        out.push(KeyBinding::new(&format!("{m}-k"), OpenSwitcher, ctx));
        out.push(KeyBinding::new(&format!("{m}-,"), OpenSettings, ctx));
    }
    out
}

type Action = Rc<dyn Fn(&mut App)>;
type PromptAction = Rc<dyn Fn(Vec<String>, &mut App)>;

/// The shell's own dialogs (confirmations and small forms).
enum ShellModal {
    Confirm { title: String, body: String, label: String, danger: bool, action: Action },
    Prompt { title: String, fields: Vec<(String, Entity<TextInput>)>, label: String, action: PromptAction },
    CreateChannel { guild_id: String, name: Entity<TextInput>, kind: usize, category: Option<String> },
}

const CHANNEL_KINDS: [(&str, &str); 3] = [("text", "Text"), ("voice", "Voice"), ("announcement", "Announcement")];

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
    home: Entity<HomeView>,
    dialog: Option<DialogTab>,
    dialog_input: Entity<TextInput>,
    create_on: Option<String>,
    show_members: bool,
    panel: Panel,
    panel_subs: Vec<Subscription>,
    /// Categories the user folded away.
    collapsed: HashSet<String>,
    /// Guilds folded or unfolded by hand in the sidebar tree.
    folded_guilds: HashSet<String>,
    unfolded_guilds: HashSet<String>,
    confirm_sign_out: bool,
    modal_focus: FocusHandle,
    menu: MenuHost,
    modal: Option<ShellModal>,
    settings: Option<(Entity<SettingsView>, Subscription)>,
    switcher: Option<(Entity<QuickSwitcher>, Subscription)>,
    profile_card: Option<(String, Option<String>, Point<Pixels>)>,
    _subs: Vec<Subscription>,
}

impl Shell {
    pub fn new(client: Entity<Client>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat = cx.new(|cx| ChatView::new(client.clone(), window, cx));
        let dialog_input = cx.new(|cx| TextInput::new("", cx));
        let home = cx.new(|cx| HomeView::new(client.clone(), window, cx));
        let subs = vec![
            cx.observe_in(&client, window, |this, _, window, cx| {
                this.follow_channel(window, cx);
                cx.notify();
            }),
            cx.subscribe_in(&chat, window, |this, _, event: &ChatEvent, window, cx| match event {
                ChatEvent::TogglePanel(kind) => this.toggle_panel(*kind, window, cx),
                ChatEvent::OpenThread(id) => this.open_thread(id.clone(), window, cx),
                ChatEvent::ClosePanel => this.close_panel(cx),
                ChatEvent::Profile { user_id, at } => this.show_profile(user_id.clone(), *at, cx),
                ChatEvent::MemberMenu { user_id, at } => this.member_menu(user_id.clone(), *at, window, cx),
            }),
            cx.subscribe_in(&dialog_input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Submit) {
                    this.confirm_dialog(window, cx);
                }
            }),
        ];
        let focus = chat.read(cx).composer_focus(cx);
        window.focus(&focus, cx);
        // Debug builds can start on a given screen, for screenshots without input:
        // RHEIN_DEV_OPEN=home|settings|join|switcher|signout|profile=@user:server.
        #[cfg(debug_assertions)]
        if let Ok(what) = std::env::var("RHEIN_DEV_OPEN") {
            cx.defer_in(window, move |this: &mut Self, window, cx| match what.as_str() {
                // After READY, which selects a guild of its own.
                "home" => {
                    let client = this.client.clone();
                    cx.spawn(async move |_, cx| {
                        cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
                        client.update(cx, |c, cx| c.show_home(cx));
                    })
                    .detach();
                }
                "settings" => this.open_settings(window, cx),
                "join" => this.open_dialog(DialogTab::Join, window, cx),
                "switcher" => this.open_switcher(window, cx),
                "signout" => this.ask_sign_out(window, cx),
                // profile=@user:server opens that card once members are in.
                other => {
                    if let Some(user) = other.strip_prefix("profile=").map(str::to_owned) {
                        cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
                            this.update(cx, |v, cx| v.show_profile(user, gpui::point(px(420.), px(90.)), cx)).ok();
                        })
                        .detach();
                    }
                }
            });
        }
        Self {
            client,
            chat,
            home,
            dialog: None,
            dialog_input,
            create_on: None,
            show_members: true,
            panel: Panel::None,
            panel_subs: Vec::new(),
            collapsed: HashSet::new(),
            folded_guilds: HashSet::new(),
            unfolded_guilds: HashSet::new(),
            confirm_sign_out: false,
            modal_focus: cx.focus_handle(),
            menu: MenuHost::default(),
            modal: None,
            settings: None,
            switcher: None,
            profile_card: None,
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
            ChatEvent::Profile { user_id, at } => this.show_profile(user_id.clone(), *at, cx),
            ChatEvent::MemberMenu { user_id, at } => this.member_menu(user_id.clone(), *at, window, cx),
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

    /// Everything you can do with a guild; its `…` button and a right-click on its row.
    fn show_guild_menu(&mut self, gid: String, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client.read(cx);
        let Some(entry) = client.guild(&gid) else { return };
        let name = entry.guild.name.clone();
        let owner = client.is_owner(&gid);
        let manage_channels = client.guild_can(&gid, perms::MANAGE_CHANNELS);
        let manage_guild = client.guild_can(&gid, perms::MANAGE_GUILD);
        let this = cx.weak_entity();
        let mut entries = Vec::new();
        {
            let (c, g) = (self.client.clone(), gid.clone());
            entries.push(MenuEntry::item("Invite people", move |_, cx| {
                let g = g.clone();
                c.update(cx, |c, cx| c.create_invite(g, cx));
            }).with_icon(Icon::UserPlus));
            let (c, g) = (self.client.clone(), gid.clone());
            entries.push(MenuEntry::item("Mark as read", move |_, cx| c.update(cx, |c, cx| c.mark_guild_read(&g, cx))).with_icon(Icon::Check));
        }
        if manage_channels {
            let (v, g) = (this.clone(), gid.clone());
            entries.push(MenuEntry::item("Create channel", move |window, cx| {
                let g = g.clone();
                v.update(cx, |v, cx| v.open_create_channel(g, window, cx)).ok();
            }).with_icon(Icon::Plus));
        }
        if manage_guild {
            let (v, g, n) = (this.clone(), gid.clone(), name.clone());
            entries.push(MenuEntry::item("Rename guild", move |window, cx| {
                let (g, n) = (g.clone(), n.clone());
                v.update(cx, |v, cx| {
                    let client = v.client.clone();
                    v.prompt("Rename guild", vec![("Name".into(), n)], "Save", move |values, cx| {
                        let name = values[0].trim().to_owned();
                        if !name.is_empty() {
                            client.update(cx, |c, cx| c.rename_guild(g.clone(), name, cx));
                        }
                    }, window, cx)
                }).ok();
            }).with_icon(Icon::Pencil));
        }
        entries.push(MenuEntry::Separator);
        {
            let g = gid.clone();
            entries.push(MenuEntry::item("Copy guild ID", move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(g.clone()))).with_icon(Icon::Copy));
        }
        {
            let v = this.clone();
            entries.push(MenuEntry::item("Join a guild", move |window, cx| {
                v.update(cx, |v, cx| v.open_dialog(DialogTab::Join, window, cx)).ok();
            }).with_icon(Icon::LogIn));
            let v = this.clone();
            entries.push(MenuEntry::item("Create a guild", move |window, cx| {
                v.update(cx, |v, cx| v.open_dialog(DialogTab::Create, window, cx)).ok();
            }).with_icon(Icon::Plus));
        }
        if !owner {
            entries.push(MenuEntry::Separator);
            let (v, g) = (this.clone(), gid.clone());
            entries.push(MenuEntry::item("Leave guild", move |window, cx| {
                let (g, n) = (g.clone(), name.clone());
                v.update(cx, |v, cx| {
                    let client = v.client.clone();
                    v.confirm(
                        format!("Leave {n}?"),
                        "You can come back with a new invite.".into(),
                        "Leave".into(),
                        true,
                        move |cx| client.update(cx, |c, cx| c.leave_guild(g.clone(), cx)),
                        window,
                        cx,
                    )
                }).ok();
            }).with_icon(Icon::LogOut).danger());
        }
        self.menu.show(at, entries, |v: &mut Shell| &mut v.menu, window, cx);
    }

    fn channel_menu(&mut self, channel_id: String, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client.read(cx);
        let Some(channel) = client.channel(&channel_id).cloned() else { return };
        let manage = client.can(&channel_id, perms::MANAGE_CHANNELS);
        let this = cx.weak_entity();
        let mut entries = Vec::new();
        if channel.is_textual() {
            let (c, id) = (self.client.clone(), channel_id.clone());
            entries.push(MenuEntry::item("Mark as read", move |_, cx| {
                let id = id.clone();
                c.update(cx, |c, cx| c.mark_read(id, cx));
            }).with_icon(Icon::Check));
        }
        {
            let id = channel_id.clone();
            entries.push(MenuEntry::item("Copy channel ID", move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(id.clone()))).with_icon(Icon::Copy));
        }
        if manage {
            entries.push(MenuEntry::Separator);
            let (v, id, name, topic) = (this.clone(), channel_id.clone(), channel.name.clone(), channel.topic.clone().unwrap_or_default());
            entries.push(MenuEntry::item("Edit channel", move |window, cx| {
                let (id, name, topic) = (id.clone(), name.clone(), topic.clone());
                v.update(cx, |v, cx| {
                    let client = v.client.clone();
                    v.prompt("Edit channel", vec![("Name".into(), name), ("Topic".into(), topic)], "Save", move |values, cx| {
                        let (name, topic) = (values[0].clone(), values[1].clone());
                        if !name.trim().is_empty() {
                            client.update(cx, |c, cx| c.edit_channel(id.clone(), name, topic, cx));
                        }
                    }, window, cx)
                }).ok();
            }).with_icon(Icon::Pencil));
            let (v, id, name) = (this.clone(), channel_id.clone(), channel.name.clone());
            entries.push(MenuEntry::item("Delete channel", move |window, cx| {
                let (id, name) = (id.clone(), name.clone());
                v.update(cx, |v, cx| {
                    let client = v.client.clone();
                    v.confirm(
                        format!("Delete #{name}?"),
                        "The channel and its messages are removed for everyone.".into(),
                        "Delete".into(),
                        true,
                        move |cx| client.update(cx, |c, cx| c.delete_channel(id.clone(), cx)),
                        window,
                        cx,
                    )
                }).ok();
            }).with_icon(Icon::Trash).danger());
        }
        self.menu.show(at, entries, |v: &mut Shell| &mut v.menu, window, cx);
    }

    fn member_menu(&mut self, user_id: String, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client.read(cx);
        let Some(gid) = client.selected_guild.clone() else { return };
        let me = client.user_id().map(str::to_owned);
        let myself = me.as_deref() == Some(user_id.as_str());
        let name = client.display_name(Some(&gid), &user_id);
        let target_owner = client.guild(&gid).is_some_and(|g| g.guild.owner.as_deref() == Some(user_id.as_str()));
        let moderate = !myself && !target_owner && client.guild_can(&gid, perms::MODERATE_MEMBERS);
        let kick = !myself && !target_owner && client.guild_can(&gid, perms::KICK_MEMBERS);
        let ban = !myself && !target_owner && client.guild_can(&gid, perms::BAN_MEMBERS);
        let this = cx.weak_entity();
        let mut entries = Vec::new();
        {
            let (v, u) = (this.clone(), user_id.clone());
            entries.push(MenuEntry::item("View profile", move |window, cx| {
                let at = window.mouse_position();
                let u = u.clone();
                v.update(cx, |v, cx| v.show_profile(u, at, cx)).ok();
            }).with_icon(Icon::User));
            let (v, u) = (this.clone(), user_id.clone());
            entries.push(MenuEntry::item("Mention", move |window, cx| {
                let u = u.clone();
                v.update(cx, |v, cx| v.chat.update(cx, |c, cx| c.insert_text(&format!("<{u}> "), window, cx))).ok();
            }).with_icon(Icon::AtSign));
            let u = user_id.clone();
            entries.push(MenuEntry::item("Copy user ID", move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(u.clone()))).with_icon(Icon::Copy));
        }
        if !myself && client.friend_state(&user_id).is_none() {
            let (c, u) = (self.client.clone(), user_id.clone());
            entries.push(MenuEntry::item("Add friend", move |_, cx| {
                let u = u.clone();
                c.update(cx, |c, cx| c.friend_action(rhein_store::FriendAction::Request, u, cx));
            }).with_icon(Icon::UserPlus));
        }
        if myself {
            let (v, g, u) = (this.clone(), gid.clone(), user_id.clone());
            let current = client.members.get(&gid).and_then(|l| l.iter().find(|m| m.user_id == user_id)).and_then(|m| m.nickname.clone()).unwrap_or_default();
            entries.push(MenuEntry::item("Change nickname", move |window, cx| {
                let (g, u, current) = (g.clone(), u.clone(), current.clone());
                v.update(cx, |v, cx| {
                    let client = v.client.clone();
                    v.prompt("Nickname in this guild", vec![("Nickname".into(), current)], "Save", move |values, cx| {
                        let nick = values[0].clone();
                        client.update(cx, |c, cx| {
                            c.set_nickname(g.clone(), u.clone(), nick, cx);
                            c.load_members(g.clone(), cx);
                        });
                    }, window, cx)
                }).ok();
            }).with_icon(Icon::Pencil));
        }
        if moderate || kick || ban {
            entries.push(MenuEntry::Separator);
            entries.push(MenuEntry::Label("Moderation".into()));
        }
        if moderate {
            for (label, minutes) in [("Time out for 10 minutes", 10), ("Time out for 1 hour", 60), ("Time out for 1 day", 1440)] {
                let (v, g, u, n) = (this.clone(), gid.clone(), user_id.clone(), name.clone());
                entries.push(MenuEntry::item(label, move |window, cx| {
                    let (g, u, n) = (g.clone(), u.clone(), n.clone());
                    v.update(cx, |v, cx| {
                        let client = v.client.clone();
                        v.confirm(
                            format!("Time out {n}?"),
                            "They can read but not write, react or talk until it ends.".into(),
                            "Time out".into(),
                            true,
                            move |cx| client.update(cx, |c, cx| c.timeout_member(g.clone(), u.clone(), minutes, cx)),
                            window,
                            cx,
                        )
                    }).ok();
                }));
            }
        }
        for (enabled, label, verb, ban_it) in [(kick, "Kick", "kick", false), (ban, "Ban", "ban", true)] {
            if !enabled {
                continue;
            }
            let (v, g, u, n) = (this.clone(), gid.clone(), user_id.clone(), name.clone());
            entries.push(MenuEntry::item(format!("{label} {name}"), move |window, cx| {
                let (g, u, n) = (g.clone(), u.clone(), n.clone());
                v.update(cx, |v, cx| {
                    let client = v.client.clone();
                    let body = if ban_it { "They are removed and cannot rejoin until unbanned." } else { "They are removed and can rejoin with an invite." };
                    v.confirm(
                        format!("{} {n}?", if ban_it { "Ban" } else { "Kick" }),
                        body.into(),
                        verb[..1].to_uppercase() + &verb[1..],
                        true,
                        move |cx| client.update(cx, |c, cx| {
                            if ban_it { c.ban_member(g.clone(), u.clone(), cx) } else { c.kick_member(g.clone(), u.clone(), cx) }
                            c.load_members(g.clone(), cx);
                        }),
                        window,
                        cx,
                    )
                }).ok();
            }).danger());
        }
        self.menu.show(at, entries, |v: &mut Shell| &mut v.menu, window, cx);
    }

    fn status_menu(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let custom = self.client.read(cx).prefs.custom_status.clone();
        let mut entries = Vec::new();
        for (status, label) in [("online", "Online"), ("idle", "Idle"), ("dnd", "Do not disturb"), ("invisible", "Invisible")] {
            let (c, keep) = (self.client.clone(), custom.clone());
            entries.push(MenuEntry::item(label, move |_, cx| c.update(cx, |c, cx| c.set_presence(status.into(), keep.clone(), cx))).with_icon(Icon::Circle));
        }
        entries.push(MenuEntry::Separator);
        let this = cx.weak_entity();
        entries.push(MenuEntry::item("Set a custom status…", move |window, cx| {
            this.update(cx, |v, cx| {
                let client = v.client.clone();
                let current = client.read(cx).prefs.custom_status.clone().unwrap_or_default();
                v.prompt("Custom status", vec![("What are you up to?".into(), current)], "Save", move |values, cx| {
                    let text = values[0].clone();
                    client.update(cx, |c, cx| {
                        let status = c.prefs.status.clone();
                        c.set_presence(status, Some(text), cx)
                    });
                }, window, cx)
            }).ok();
        }).with_icon(Icon::Pencil));
        if custom.is_some() {
            let c = self.client.clone();
            entries.push(MenuEntry::item("Clear custom status", move |_, cx| c.update(cx, |c, cx| {
                let status = c.prefs.status.clone();
                c.set_presence(status, None, cx)
            })).with_icon(Icon::X));
        }
        self.menu.show(at, entries, |v: &mut Shell| &mut v.menu, window, cx);
    }

    // ---------------------------------------------------------------- modals

    // A confirmation is its title, text, button and consequence; bundling them would only move
    // the same arguments into a struct literal at every call site.
    #[allow(clippy::too_many_arguments)]
    fn confirm(&mut self, title: String, body: String, label: String, danger: bool, action: impl Fn(&mut App) + 'static, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = Some(ShellModal::Confirm { title, body, label, danger, action: Rc::new(action) });
        window.focus(&self.modal_focus, cx);
        cx.notify();
    }

    fn prompt(&mut self, title: &str, fields: Vec<(String, String)>, label: &str, action: impl Fn(Vec<String>, &mut App) + 'static, window: &mut Window, cx: &mut Context<Self>) {
        let fields: Vec<(String, Entity<TextInput>)> = fields
            .into_iter()
            .map(|(label, value)| {
                let input = cx.new(|cx| TextInput::new(label.clone(), cx).with_text(value));
                (label, input)
            })
            .collect();
        for (_, input) in &fields {
            let sub = cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Submit => this.submit_modal(window, cx),
                InputEvent::Cancel => this.close_modal(window, cx),
                _ => {}
            });
            self._subs.push(sub);
        }
        if let Some((_, first)) = fields.first() {
            window.focus(&first.focus_handle(cx), cx);
        }
        self.modal = Some(ShellModal::Prompt { title: title.into(), fields, label: label.into(), action: Rc::new(action) });
        cx.notify();
    }

    fn open_create_channel(&mut self, guild_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| TextInput::new("new-channel", cx));
        let sub = cx.subscribe_in(&name, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Submit => this.submit_modal(window, cx),
            InputEvent::Cancel => this.close_modal(window, cx),
            _ => {}
        });
        self._subs.push(sub);
        window.focus(&name.focus_handle(cx), cx);
        self.modal = Some(ShellModal::CreateChannel { guild_id, name, kind: 0, category: None });
        cx.notify();
    }

    fn submit_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.modal.take() {
            Some(ShellModal::Confirm { action, .. }) => action(cx),
            Some(ShellModal::Prompt { fields, action, .. }) => {
                let values = fields.iter().map(|(_, i)| i.read(cx).text().to_owned()).collect();
                action(values, cx);
            }
            Some(ShellModal::CreateChannel { guild_id, name, kind, category }) => {
                let name = name.read(cx).text().trim().to_owned();
                if name.is_empty() {
                    return;
                }
                let kind = CHANNEL_KINDS[kind].0.to_owned();
                self.client.update(cx, |c, cx| c.create_channel(guild_id, name, kind, category, cx));
            }
            None => {}
        }
        self.close_modal(window, cx);
    }

    fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        let focus = self.chat.read(cx).composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn render_modal(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let modal = self.modal.as_ref()?;
        let t = cx.theme().clone();
        let this = cx.weak_entity();
        let dismiss = move |window: &mut Window, cx: &mut App| {
            this.update(cx, |v, cx| v.close_modal(window, cx)).ok();
        };
        let cancel = Button::new("modal-cancel", "Cancel").ghost().on_click(cx.listener(|v, _, window, cx| v.close_modal(window, cx)));
        let element = match modal {
            ShellModal::Confirm { title, body, label, danger, .. } => Modal::new("shell-confirm")
                .title(title.clone())
                .width(px(420.))
                .focus(&self.modal_focus)
                .on_dismiss(dismiss)
                .child(div().text_color(t.text_muted).child(body.clone()))
                .footer(cancel)
                .footer(Button::new("modal-ok", label.clone()).kind(if *danger { rhein_ui::ButtonKind::Danger } else { rhein_ui::ButtonKind::Primary })
                    .on_click(cx.listener(|v, _, window, cx| v.submit_modal(window, cx)))),
            ShellModal::Prompt { title, fields, label, .. } => Modal::new("shell-prompt")
                .title(title.clone())
                .width(px(420.))
                .on_dismiss(dismiss)
                .children(fields.iter().map(|(label, input)| {
                    div().flex().flex_col().gap_1p5()
                        .child(div().text_size(text::SM).text_color(t.text_muted).child(label.clone()))
                        .child(input.clone())
                }))
                .footer(cancel)
                .footer(Button::new("modal-ok", label.clone()).primary().on_click(cx.listener(|v, _, window, cx| v.submit_modal(window, cx)))),
            ShellModal::CreateChannel { guild_id, name, kind, category } => {
                let client = self.client.read(cx);
                let categories: Vec<(String, String)> = client
                    .guild(guild_id)
                    .map(|g| g.guild.channels.iter().filter(|c| c.is_category()).map(|c| (c.id.clone(), c.name.clone())).collect())
                    .unwrap_or_default();
                let chip = |id: SharedString, label: String, active: bool| {
                    div().id(id).px_2().py_1().rounded(radius::MD).border_1()
                        .border_color(if active { t.accent } else { t.border_strong })
                        .text_size(text::SM).cursor_pointer().child(label)
                };
                let kinds = div().flex().gap_1().children(CHANNEL_KINDS.iter().enumerate().map(|(i, (_, label))| {
                    chip(SharedString::from(format!("kind-{i}")), (*label).into(), *kind == i).on_click(cx.listener(move |v, _, _, cx| {
                        if let Some(ShellModal::CreateChannel { kind, .. }) = v.modal.as_mut() { *kind = i; }
                        cx.notify();
                    }))
                }));
                let mut cats = div().flex().flex_wrap().gap_1().child(
                    chip("cat-none".into(), "No category".into(), category.is_none()).on_click(cx.listener(|v, _, _, cx| {
                        if let Some(ShellModal::CreateChannel { category, .. }) = v.modal.as_mut() { *category = None; }
                        cx.notify();
                    })),
                );
                for (id, label) in categories {
                    let active = category.as_deref() == Some(id.as_str());
                    cats = cats.child(chip(SharedString::from(format!("cat-{id}")), label, active).on_click(cx.listener(move |v, _, _, cx| {
                        if let Some(ShellModal::CreateChannel { category, .. }) = v.modal.as_mut() { *category = Some(id.clone()); }
                        cx.notify();
                    })));
                }
                Modal::new("create-channel")
                    .title("Create channel")
                    .width(px(440.))
                    .on_dismiss(dismiss)
                    .child(div().flex().flex_col().gap_1p5().child(div().text_size(text::SM).text_color(t.text_muted).child("Type")).child(kinds))
                    .child(div().flex().flex_col().gap_1p5().child(div().text_size(text::SM).text_color(t.text_muted).child("Name")).child(name.clone()))
                    .child(div().flex().flex_col().gap_1p5().child(div().text_size(text::SM).text_color(t.text_muted).child("Category")).child(cats))
                    .footer(cancel)
                    .footer(Button::new("modal-ok", "Create").primary().on_click(cx.listener(|v, _, window, cx| v.submit_modal(window, cx))))
            }
        };
        Some(element.into_any_element())
    }

    // ---------------------------------------------------------------- profile card

    fn show_profile(&mut self, user_id: String, at: Point<Pixels>, cx: &mut Context<Self>) {
        let guild = self.client.read(cx).selected_guild.clone();
        let uid = user_id.clone();
        self.client.update(cx, |c, cx| {
            c.profile(&uid, cx);
        });
        self.profile_card = Some((user_id, guild, at));
        cx.notify();
    }

    fn render_profile_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (user_id, guild_id, at) = self.profile_card.clone()?;
        let t = cx.theme().clone();
        let profile = self.client.update(cx, |c, cx| c.profile(&user_id, cx));
        let client = self.client.read(cx);
        let name = client.display_name(guild_id.as_deref(), &user_id);
        let avatar = client.avatar_url(guild_id.as_deref(), &user_id);
        let member = guild_id.as_deref().and_then(|g| client.members.get(g)).and_then(|l| l.iter().find(|m| m.user_id == user_id)).cloned();
        let roles: Vec<String> = match (&member, guild_id.as_deref().and_then(|g| client.guild(g))) {
            (Some(m), Some(g)) => g.guild.roles.iter().filter(|r| m.roles.contains(&r.id)).map(|r| r.name.clone()).collect(),
            _ => Vec::new(),
        };
        let presence = member.as_ref().map(|m| rhein_ui::Presence::parse(m.status()));
        let look = profile::Look {
            banner: profile.as_ref().and_then(|p| p.banner_ref.as_deref()).and_then(|r| client.banner_url(&user_id, r)),
            accent: profile.as_ref().and_then(|p| p.accent_color),
            theme: profile.as_ref().and_then(|p| p.theme_colors),
        };
        let face = Avatar::new(user_id.clone(), name.clone())
            .size(px(profile::FACE_SIZE))
            .image(avatar.map(Into::into))
            .presence(presence)
            .into_any_element();
        let body = div()
            .px_4()
            .pb_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div().flex().flex_col()
                    .child(div().text_size(text::LG).font_weight(FontWeight::SEMIBOLD).child(name))
                    .child(div().text_size(text::SM).text_color(t.text_faint).child(user_id.clone()))
                    .children(profile.as_ref().and_then(|p| p.pronouns.clone()).filter(|p| !p.is_empty()).map(|p| div().text_size(text::SM).text_color(t.text_muted).child(p))),
            )
            .children(profile.as_ref().and_then(|p| p.bio.clone()).filter(|b| !b.is_empty()).map(|bio| {
                div().flex().flex_col().gap_1()
                    .child(div().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_faint).child("about me"))
                    .child(div().text_size(text::SM).child(bio))
            }))
            .children({
                let me = client.user_id().map(str::to_owned);
                let state = client.friend_state(&user_id).map(str::to_owned);
                (me.as_deref() != Some(user_id.as_str())).then(|| {
                    let (c, u) = (self.client.clone(), user_id.clone());
                    let label = match state.as_deref() {
                        Some("accepted") => "Friends",
                        Some("pending_out") => "Request sent",
                        Some("pending_in") => "Accept friend request",
                        Some("blocked") => "Blocked",
                        _ => "Add friend",
                    };
                    let action = match state.as_deref() {
                        None => Some(rhein_store::FriendAction::Request),
                        Some("pending_in") => Some(rhein_store::FriendAction::Accept),
                        _ => None,
                    };
                    Button::new("profile-friend", label)
                        .icon(Icon::UserPlus)
                        .disabled(action.is_none())
                        .on_click(move |_, _, cx| {
                            if let Some(a) = action {
                                let u = u.clone();
                                c.update(cx, |c, cx| c.friend_action(a, u, cx));
                            }
                        })
                })
            })
            .when(!roles.is_empty(), |d| {
                d.child(div().flex().flex_col().gap_1()
                    .child(div().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_faint).child("roles"))
                    .child(div().flex().flex_wrap().gap_1().children(roles.into_iter().map(|r| {
                        div().px_2().py_0p5().rounded(radius::SM).bg(t.bg_raised).text_size(text::XS).child(r)
                    }))))
            });
        let card = profile::frame(&look, &t, div().flex().flex_col().child(profile::header(&look, face, &t)).child(body))
            .occlude()
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                v.profile_card = None;
                cx.notify();
            }));
        Some(deferred(anchored().position(at).snap_to_window_with_margin(px(8.)).child(motion::pop_in(card.relative(), "profile-card-in"))).with_priority(2).into_any_element())
    }

    // ---------------------------------------------------------------- settings & switcher

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            return;
        }
        let client = self.client.clone();
        let view = cx.new(|cx| SettingsView::new(client, window, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, event: &SettingsEvent, window, cx| match event {
            SettingsEvent::Close => this.close_settings(window, cx),
            SettingsEvent::SignOut => {
                this.close_settings(window, cx);
                this.ask_sign_out(window, cx);
            }
        });
        self.settings = Some((view, sub));
        cx.notify();
    }

    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = None;
        let focus = self.chat.read(cx).composer_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn open_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let view = cx.new(|cx| QuickSwitcher::new(client, window, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, _: &DismissEvent, window, cx| {
            this.switcher = None;
            let focus = this.chat.read(cx).composer_focus(cx);
            window.focus(&focus, cx);
            cx.notify();
        });
        self.switcher = Some((view, sub));
        cx.notify();
    }

    /// Whether a guild's channels are showing in the sidebar tree: the open guild unless folded
    /// away, any other guild once unfolded.
    fn guild_expanded(&self, gid: &str, cx: &App) -> bool {
        let client = self.client.read(cx);
        let open = !client.showing_home && client.selected_guild.as_deref() == Some(gid);
        !self.folded_guilds.contains(gid) && (open || self.unfolded_guilds.contains(gid))
    }

    fn toggle_guild(&mut self, gid: &str, cx: &mut Context<Self>) {
        if self.guild_expanded(gid, cx) {
            self.unfolded_guilds.remove(gid);
            self.folded_guilds.insert(gid.to_owned());
        } else {
            self.folded_guilds.remove(gid);
            self.unfolded_guilds.insert(gid.to_owned());
        }
        cx.notify();
    }

    /// The one sidebar: direct messages on top, then every server with its guilds, each guild
    /// unfolding into its channels.
    fn sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme().clone();
        let client = self.client.read(cx);
        let home = client.showing_home;
        let selected = client.selected_guild.clone();

        let mut tree = div().id("sidebar-tree").flex_1().overflow_y_scroll().px_2().pb_2().flex().flex_col();

        // Direct messages: the row sits in a strip as tall as the chat header, so both share one
        // line underneath; its conversations open below it.
        let mut top = div()
            .h(size::HEADER)
            .flex_none()
            .px_2()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(t.border);
        {
            let requests = client.incoming_friend_requests() as u32;
            let dm_unread = client.conversations.iter().any(|c| client.conversation_unread(&c.id));
            let c = self.client.clone();
            top = top.child(div().flex_1().child(
                ListRow::new("tree-home", "Direct messages")
                    .icon(Icon::MessagesSquare)
                    // Friends and conversations below carry the selection; this row just marks home.
                    .strong(dm_unread || home)
                    .when(requests > 0, |r| r.trailing(CountBadge(requests)))
                    .on_click(move |_, _, cx| c.update(cx, |c, cx| c.show_home(cx))),
            ));
            if home {
                tree = tree.child(div().h_2());
                for row in self.conversation_rows(cx) {
                    tree = tree.child(row);
                }
            }
        }

        // Servers and their guilds.
        for server in &client.servers {
            let dot = match server.status {
                ServerStatus::Ready => t.success,
                ServerStatus::Connecting | ServerStatus::Reconnecting | ServerStatus::Renewing => t.warning,
                ServerStatus::Incompatible | ServerStatus::Failed => t.danger,
            };
            tree = tree.child(
                div()
                    .px_2()
                    .pt_4()
                    .pb_1()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .font_family(FONT_MONO)
                    .text_size(text::XS)
                    .text_color(t.text_faint)
                    .child(div().size(px(6.)).flex_none().rounded_full().bg(dot))
                    .child(div().min_w_0().truncate().child(server.name.clone())),
            );
            if server.status != ServerStatus::Ready {
                tree = tree.child(
                    div()
                        .px_2()
                        .pb_1()
                        .font_family(FONT_MONO)
                        .text_size(text::XS)
                        .text_color(if matches!(server.status, ServerStatus::Failed | ServerStatus::Incompatible) { t.danger } else { t.warning })
                        .child(server.status.label().to_lowercase())
                        .children(server.error.clone().map(|e| div().text_color(t.danger).child(e))),
                );
            }
            for entry in client.guilds.iter().filter(|g| g.server == server.name) {
                let gid = entry.guild.id.clone();
                let active = !home && selected.as_deref() == Some(gid.as_str());
                let expanded = self.guild_expanded(&gid, cx);
                let (unread, mentions) = client.guild_unread(&gid);
                let (select_id, chevron_id, menu_id, more_id) = (gid.clone(), gid.clone(), gid.clone(), gid.clone());
                tree = tree.child(
                    div()
                        .id(SharedString::from(format!("tree-guild-{gid}")))
                        .group("guild-row")
                        .h(px(30.))
                        .pl_1()
                        .pr_1()
                        .rounded(radius::MD)
                        .flex()
                        .items_center()
                        .gap_1()
                        .cursor_pointer()
                        .text_color(if active || unread { t.text } else { t.text_muted })
                        .when(unread, |d| d.font_weight(FontWeight::SEMIBOLD))
                        .when(active && !expanded, |d| d.bg(t.bg_selected))
                        .hover(|s| s.bg(t.bg_hover).text_color(t.text))
                        .tooltip(tooltip(format!("{} · {}", entry.guild.name, entry.server)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let open = {
                                let c = this.client.read(cx);
                                !c.showing_home && c.selected_guild.as_deref() == Some(select_id.as_str())
                            };
                            if open {
                                this.toggle_guild(&select_id, cx);
                            } else {
                                this.folded_guilds.remove(&select_id);
                                let id = select_id.clone();
                                this.client.update(cx, |c, cx| c.select_guild(id, cx));
                            }
                        }))
                        .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &gpui::MouseDownEvent, window, cx| {
                            this.show_guild_menu(menu_id.clone(), e.position, window, cx)
                        }))
                        .child(
                            div()
                                .id(SharedString::from(format!("tree-chevron-{gid}")))
                                .size(px(20.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(radius::SM)
                                .text_color(t.text_faint)
                                .hover(|s| s.text_color(t.text))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.toggle_guild(&chevron_id, cx);
                                }))
                                .child(icon_sized(if expanded { Icon::ChevronDown } else { Icon::ChevronRight }, px(12.)).text_color(t.text_faint)),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(entry.guild.name.clone()))
                        .when(unread && mentions == 0 && !expanded, |d| d.child(UnreadDot))
                        .when(mentions > 0, |d| d.child(CountBadge(mentions)))
                        .child(
                            div()
                                .id(SharedString::from(format!("tree-more-{gid}")))
                                .invisible()
                                .group_hover("guild-row", |s| s.visible())
                                .size(px(20.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(radius::SM)
                                .text_color(t.text_faint)
                                .hover(|s| s.text_color(t.text).bg(t.bg_selected))
                                .on_click(cx.listener(move |this, e: &gpui::ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.show_guild_menu(more_id.clone(), e.position(), window, cx)
                                }))
                                .child(icon_sized(Icon::Ellipsis, px(14.)).text_color(t.text_muted)),
                        ),
                );
                if expanded {
                    tree = tree.child(div().pl_3().flex().flex_col().children(self.channel_rows(&gid, cx)));
                }
            }
        }

        tree = tree.child(
            div().pt_3().child(
                ListRow::new("tree-add", "Join or create")
                    .icon(Icon::Plus)
                    .muted(true)
                    .on_click(cx.listener(|this, _, window, cx| this.open_dialog(DialogTab::Join, window, cx))),
            ),
        );

        div()
            .w(size::SIDEBAR)
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(t.bg_panel)
            .border_r_1()
            .border_color(t.border)
            .child(top)
            .child(tree)
            .child(self.profile_strip(cx))
            .into_any_element()
    }

    /// You, at the foot of the sidebar: avatar, name and status (a click sets it), settings and
    /// sign-out.
    fn profile_strip(&self, cx: &Context<Self>) -> AnyElement {
        let t = cx.theme().clone();
        let client = self.client.read(cx);
        let me = client.user_id().unwrap_or_default().to_owned();
        let name = client.display_name(None, &me);
        let avatar = client.avatar_url(None, &me);
        let prefs = &client.prefs;
        let presence = match prefs.status.as_str() {
            "idle" => Presence::Idle,
            "dnd" => Presence::Dnd,
            "invisible" => Presence::Offline,
            _ => Presence::Online,
        };
        let status = prefs.custom_status.clone().unwrap_or_else(|| match prefs.status.as_str() {
            "idle" => "idle".into(),
            "dnd" => "do not disturb".into(),
            "invisible" => "invisible".into(),
            _ => "online".into(),
        });
        // As tall as the composer box plus the status line below it, so its top edge runs level
        // with the composer's.
        div()
            .h(PROFILE_STRIP_H)
            .flex_none()
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .border_t_1()
            .border_color(t.border)
            .child(
                div()
                    .id("profile-strip")
                    .flex_1()
                    .min_w_0()
                    .h(px(52.))
                    .px_1()
                    .rounded(radius::MD)
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .hover(|s| s.bg(t.bg_hover))
                    .tooltip(tooltip("Set status"))
                    .on_click(cx.listener(|this, e: &gpui::ClickEvent, window, cx| this.status_menu(e.position(), window, cx)))
                    .child(Avatar::new(me.clone(), name.clone()).size(px(36.)).image(avatar.map(Into::into)).presence(Some(presence)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(div().text_size(text::MD).font_weight(FontWeight::SEMIBOLD).truncate().child(name))
                            .child(div().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_faint).truncate().child(status)),
                    ),
            )
            .child(
                IconButton::new("open-settings", Icon::Settings)
                    .tooltip("Settings (Ctrl+,)")
                    .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx))),
            )
            .child(
                IconButton::new("sign-out", Icon::LogOut)
                    .tooltip("Sign out")
                    .on_click(cx.listener(|this, _, window, cx| this.ask_sign_out(window, cx))),
            )
            .into_any_element()
    }

    /// Friends and the private conversations, shown under "Direct messages".
    fn conversation_rows(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let t = cx.theme().clone();
        let client = self.client.read(cx);
        let mut rows = Vec::new();
        let pending = client.incoming_friend_requests();
        let c = self.client.clone();
        rows.push(
            ListRow::new("home-friends", "Friends")
                .icon(Icon::Users)
                .selected(client.home_conversation.is_none())
                .when(pending > 0, |r| r.trailing(CountBadge(pending as u32)))
                .on_click(move |_, _, cx| c.update(cx, |c, cx| c.open_conversation(None, cx)))
                .into_any_element(),
        );
        if client.conversations.is_empty() {
            rows.push(
                div().px_2().py_1().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_faint).child("no conversations yet").into_any_element(),
            );
        }
        for conversation in &client.conversations {
            let title = client.conversation_title(conversation);
            let me = client.user_id().unwrap_or_default();
            let other = conversation.members.iter().find(|m| m.user_id != me).map(|m| m.user_id.clone()).unwrap_or_default();
            let selected = client.home_conversation.as_deref() == Some(conversation.id.as_str());
            let unread = client.conversation_unread(&conversation.id);
            let request = conversation.request_state.as_deref() == Some("pending");
            let (c, id) = (self.client.clone(), conversation.id.clone());
            let leading = if conversation.kind == "gc" {
                div().size(px(20.)).rounded_full().bg(t.bg_selected).flex().items_center().justify_center().child(icon_sized(Icon::Users, px(12.))).into_any_element()
            } else {
                Avatar::new(other.clone(), title.clone()).size(px(20.)).into_any_element()
            };
            rows.push(
                ListRow::new(SharedString::from(format!("conv-{}", conversation.id)), title)
                    .leading(leading)
                    .selected(selected)
                    .strong(unread)
                    .when(request, |r| r.trailing(Tag("REQUEST")))
                    .on_click(move |_, _, cx| {
                        let id = id.clone();
                        c.update(cx, |c, cx| c.open_conversation(Some(id), cx));
                    })
                    .into_any_element(),
            );
        }
        rows.into_iter().map(|r| div().pl_3().child(r).into_any_element()).collect()
    }

    /// One guild's channels in order, with categories, voice occupants and unread state.
    fn channel_rows(&self, gid: &str, cx: &Context<Self>) -> Vec<AnyElement> {
        let t = cx.theme().clone();
        let client = self.client.read(cx);
        let Some(entry) = client.guild(gid) else { return Vec::new() };
        let selected_channel = if client.showing_home { None } else { client.selected_channel().map(str::to_owned) };
        let mut rows = Vec::new();
        for ch in entry.ordered_channels() {
            if ch.is_category() {
                let collapsed = self.collapsed.contains(&ch.id);
                let cat = ch.id.clone();
                rows.push(
                    div()
                        .id(SharedString::from(format!("cat-{}", ch.id)))
                        .flex()
                        .items_center()
                        .gap_1()
                        .pl_1()
                        .pt_3()
                        .pb_1()
                        .font_family(FONT_MONO)
                        .text_size(text::XS)
                        .text_color(t.text_faint)
                        .cursor_pointer()
                        .hover(|s| s.text_color(t.text_muted))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.collapsed.remove(&cat) {
                                this.collapsed.insert(cat.clone());
                            }
                            cx.notify();
                        }))
                        .child(icon_sized(if collapsed { Icon::ChevronRight } else { Icon::ChevronDown }, px(12.)).text_color(t.text_faint))
                        .child(ch.name.to_lowercase())
                        .into_any_element(),
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
            if ch.is_textual() || ch.is_voice() {
                let id = ch.id.clone();
                let client = self.client.clone();
                row = row.on_click(move |_, _, cx| {
                    let id = id.clone();
                    client.update(cx, |c, cx| c.select_channel(id, cx));
                });
            }
            let menu_id = ch.id.clone();
            row = row.on_secondary(cx.listener(move |this, e: &gpui::MouseDownEvent, window, cx| {
                this.channel_menu(menu_id.clone(), e.position, window, cx)
            }));
            rows.push(row.into_any_element());
            if ch.is_voice() {
                for state in client.voice_members(gid, &ch.id) {
                    let name = client.display_name(Some(gid), &state.user_id);
                    let avatar = client.avatar_url(Some(gid), &state.user_id);
                    let muted = state.self_mute || state.server_mute;
                    let deaf = state.self_deaf || state.server_deaf;
                    rows.push(
                        div()
                            .pl(px(30.))
                            .pr_2()
                            .py_0p5()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(text::SM)
                            .text_color(t.text_muted)
                            .child(Avatar::new(state.user_id.clone(), name.clone()).size(px(20.)).image(avatar.map(Into::into)))
                            .child(div().flex_1().min_w_0().truncate().child(name))
                            .when(state.streaming, |d| d.child(icon_sized(Icon::ScreenShare, px(12.)).text_color(t.danger)))
                            .when(state.self_video, |d| d.child(icon_sized(Icon::Video, px(12.)).text_color(t.text_faint)))
                            .when(muted, |d| d.child(icon_sized(Icon::MicOff, px(12.)).text_color(if state.server_mute { t.danger } else { t.text_faint })))
                            .when(deaf, |d| d.child(icon_sized(Icon::HeadphoneOff, px(12.)).text_color(if state.server_deaf { t.danger } else { t.text_faint })))
                            .into_any_element(),
                    );
                }
            }
        }
        rows
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
        let this = cx.weak_entity();
        let row = |m: &rhein_proto::Member| {
            let name = m.shown_name().to_owned();
            let presence = Presence::parse(m.status());
            let (uid, uid2) = (m.user_id.clone(), m.user_id.clone());
            let (weak, weak2) = (this.clone(), this.clone());
            ListRow::new(SharedString::from(format!("member-{}", m.user_id)), name.clone())
                .leading(Avatar::new(m.user_id.clone(), name).size(px(28.)).image(client.avatar_url(Some(&gid), &m.user_id).map(Into::into)).presence(Some(presence)))
                .muted(presence == Presence::Offline)
                .when(m.is_bot, |r| r.trailing(Tag("BOT")))
                .on_click(move |e, _, cx| {
                    let (u, at) = (uid.clone(), e.position());
                    weak.update(cx, |v, cx| v.show_profile(u, at, cx)).ok();
                })
                .on_secondary(move |e, window, cx| {
                    let (u, at) = (uid2.clone(), e.position);
                    weak2.update(cx, |v, cx| v.member_menu(u, at, window, cx)).ok();
                })
        };
        let online_label = section_label(format!("Online — {}", online.len()), cx).pt_2();
        let offline_label = (!offline.is_empty()).then(|| section_label(format!("Offline — {}", offline.len()), cx));
        Some(
            div()
                .w(size::PANEL)
                .h_full()
                .flex_none()
                .flex()
                .flex_col()
                .bg(t.bg_panel)
                .border_l_1()
                .border_color(t.border)
                .child(
                    div()
                        .h(size::HEADER)
                        .flex_none()
                        .px_4()
                        .flex()
                        .items_center()
                        .border_b_1()
                        .border_color(t.border)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Members"),
                )
                .child(
                    div()
                        .id("members")
                        .flex_1()
                        .min_h_0()
                        .px_2()
                        .overflow_y_scroll()
                        .child(online_label)
                        .children(online.into_iter().map(row))
                        .children(offline_label)
                        .children(offline.into_iter().map(row)),
                )
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

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar = self.sidebar(cx);
        let status = StatusLine::new(self.client.clone());
        let members = self.render_panel(cx);
        let dialog = self.dialog(cx);
        let sign_out = self.sign_out_dialog(cx);
        let toast = self.toast(cx);
        let modal = self.render_modal(cx);
        let card = self.render_profile_card(cx);
        let switcher = self.switcher.as_ref().map(|(view, _)| {
            motion::fade_in(
                div()
                    .absolute()
                    .inset_0()
                    .bg(cx.theme().overlay)
                    .flex()
                    .justify_center()
                    .pt(px(120.))
                    .child(motion::pop_in(div().relative().child(view.clone()), "switcher-in")),
                "switcher-fade",
                motion::FAST,
            )
        });
        let settings = self
            .settings
            .as_ref()
            .map(|(view, _)| motion::fade_in(div().absolute().inset_0().child(view.clone()), "settings-in", motion::FAST));
        div()
            .key_context(SHELL_CONTEXT)
            .on_action(cx.listener(|this, _: &OpenSwitcher, window, cx| this.open_switcher(window, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)))
            .on_action(cx.listener(|this, _: &PrevChannel, _, cx| this.client.update(cx, |c, cx| c.step_channel(-1, false, cx))))
            .on_action(cx.listener(|this, _: &NextChannel, _, cx| this.client.update(cx, |c, cx| c.step_channel(1, false, cx))))
            .on_action(cx.listener(|this, _: &PrevUnread, _, cx| this.client.update(cx, |c, cx| c.step_channel(-1, true, cx))))
            .on_action(cx.listener(|this, _: &NextUnread, _, cx| this.client.update(cx, |c, cx| c.step_channel(1, true, cx))))
            .relative()
            .size_full()
            .flex()
            .child(sidebar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .map(|d| if self.client.read(cx).showing_home { d.child(self.home.clone()) } else { d.child(self.chat.clone()) }),
                    )
                    .child(status),
            )
            // Side panels run the full height; the status line belongs to the middle column.
            .children(members)
            .children(toast)
            .children(dialog)
            .children(sign_out)
            .children(settings)
            .children(switcher)
            .children(modal)
            .children(card)
            .children(self.menu.element())
    }
}
