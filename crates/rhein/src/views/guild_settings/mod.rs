//! Guild settings: an overlay with the guild's pages on the left, as in the web client
//! (`web/src/views/guild-settings/`). Every page is a plain call to one server, since a guild
//! has exactly one authority; the list only shows the pages this account may use there, and the
//! server checks each call again.

mod automod;
mod bots;
mod channels;
mod expressions;
mod lists;
mod moderation;
mod overview;
mod roles;

use std::future::Future;

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Window, div, img, prelude::*, px,
};
use rhein_store::rhein_net::{Api, ApiResult};
use rhein_store::{Client, perms};
use rhein_ui::theme::{FONT_MONO, radius, text};
use rhein_ui::{ActiveTheme as _, Dismiss, Icon, IconButton, ListRow, TextInput, Theme};

use crate::views::crop::CropDialog;

pub const GUILD_SETTINGS_CONTEXT: &str = "GuildSettings";

/// A page of the guild settings, gated as the web client gates it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pane {
    Overview,
    Roles,
    Channels,
    Bots,
    Gates,
    AutoMod,
    Queue,
    Purge,
    Bans,
    Invites,
    Expressions,
    Voice,
    Audit,
}

impl Pane {
    pub const ALL: [Pane; 13] = [
        Pane::Overview,
        Pane::Roles,
        Pane::Channels,
        Pane::Bots,
        Pane::Gates,
        Pane::AutoMod,
        Pane::Queue,
        Pane::Purge,
        Pane::Bans,
        Pane::Invites,
        Pane::Expressions,
        Pane::Voice,
        Pane::Audit,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Pane::Overview => "Overview",
            Pane::Roles => "Roles",
            Pane::Channels => "Channel permissions",
            Pane::Bots => "Bots",
            Pane::Gates => "Join gate",
            Pane::AutoMod => "AutoMod",
            Pane::Queue => "Moderation queue",
            Pane::Purge => "Purge messages",
            Pane::Bans => "Bans",
            Pane::Invites => "Invites",
            Pane::Expressions => "Emoji & stickers",
            Pane::Voice => "Voice regions",
            Pane::Audit => "Audit log",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Pane::Overview => Icon::Info,
            Pane::Roles => Icon::Users,
            Pane::Channels => Icon::Lock,
            Pane::Bots => Icon::Bot,
            Pane::Gates => Icon::Shield,
            Pane::AutoMod => Icon::Ban,
            Pane::Queue => Icon::Inbox,
            Pane::Purge => Icon::Trash,
            Pane::Bans => Icon::Ban,
            Pane::Invites => Icon::Link,
            Pane::Expressions => Icon::Smile,
            Pane::Voice => Icon::Globe,
            Pane::Audit => Icon::ScrollText,
        }
    }

    /// The permission the page's routes check; the overview is readable by every member.
    fn needs(self) -> Option<u64> {
        Some(match self {
            Pane::Overview => return None,
            Pane::Roles | Pane::Channels => perms::MANAGE_ROLES,
            Pane::Bots | Pane::Gates | Pane::AutoMod | Pane::Invites | Pane::Voice => perms::MANAGE_GUILD,
            Pane::Queue => perms::MODERATE_MEMBERS,
            Pane::Purge => perms::MANAGE_MESSAGES,
            Pane::Bans => perms::BAN_MEMBERS,
            Pane::Expressions => perms::MANAGE_EXPRESSIONS,
            Pane::Audit => perms::VIEW_AUDIT_LOG,
        })
    }

    /// The pane named in the dev hook (`RHEIN_DEV_OPEN=guild-settings=roles`).
    #[cfg(debug_assertions)]
    pub fn from_key(key: &str) -> Option<Pane> {
        Pane::ALL.into_iter().find(|p| format!("{p:?}").eq_ignore_ascii_case(key))
    }
}

pub enum GuildSettingsEvent {
    Close,
}

pub struct GuildSettingsView {
    pub(crate) client: Entity<Client>,
    pub(crate) guild_id: String,
    pane: Pane,
    focus: FocusHandle,
    body_scroll: gpui::ScrollHandle,
    /// The last outcome to report: `(worked, what)`.
    pub(crate) status: Option<(bool, SharedString)>,
    pub(crate) overview: overview::State,
    pub(crate) roles: roles::State,
    pub(crate) channel_perms: channels::State,
    pub(crate) lists: lists::State,
    pub(crate) expressions: expressions::State,
    pub(crate) moderation: moderation::State,
    pub(crate) bots: bots::State,
    pub(crate) automod: automod::State,
    pub(crate) crop: Option<(Entity<CropDialog>, Subscription)>,
    crop_focus: bool,
    pub(crate) subs: Vec<Subscription>,
}

impl EventEmitter<GuildSettingsEvent> for GuildSettingsView {}

impl Focusable for GuildSettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl GuildSettingsView {
    pub fn new(client: Entity<Client>, guild_id: String, pane: Pane, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // The guild changes under the pages (a rename, a new role): draw again when it does.
        let subs = vec![cx.observe(&client, |_, _, cx| cx.notify())];
        // What the list from READY leaves out (when it was made, its address): ask for it.
        client.update(cx, |c, cx| c.refresh_guild(&guild_id, cx));
        let overview = overview::State::new(&client, &guild_id, cx);
        let roles = roles::State::new(cx);
        let channel_perms = channels::State::new(cx);
        let lists = lists::State::new(cx);
        let expressions = expressions::State::new();
        let moderation = moderation::State::new(cx);
        let bots = bots::State::new(cx);
        let automod = automod::State::new(cx);
        let mut view = Self {
            client,
            guild_id,
            pane: Pane::Overview,
            focus,
            body_scroll: gpui::ScrollHandle::new(),
            status: None,
            overview,
            roles,
            channel_perms,
            lists,
            expressions,
            moderation,
            bots,
            automod,
            crop: None,
            crop_focus: false,
            subs,
        };
        view.roles_subscriptions(cx);
        view.open(pane, cx);
        view
    }

    /// Whether this account may use a page here.
    fn allowed(&self, pane: Pane, cx: &App) -> bool {
        pane.needs().is_none_or(|bit| self.client.read(cx).guild_can(&self.guild_id, bit))
    }

    pub fn open(&mut self, pane: Pane, cx: &mut Context<Self>) {
        // A page this account may not use is a request, not an instruction: the overview opens.
        let pane = if self.allowed(pane, cx) { pane } else { Pane::Overview };
        if self.pane != pane {
            self.status = None;
            self.body_scroll.set_offset(gpui::Point::default());
        }
        self.pane = pane;
        self.load(pane, cx);
        cx.notify();
    }

    /// Fetches what a page shows, each time it is opened.
    fn load(&mut self, pane: Pane, cx: &mut Context<Self>) {
        match pane {
            Pane::Overview => self.overview.reset(&self.client, &self.guild_id, cx),
            Pane::Roles => self.load_roles(cx),
            Pane::Invites => self.load_invites(cx),
            Pane::Bans => self.load_bans(cx),
            Pane::Audit => self.load_audit(cx),
            Pane::Voice => self.load_regions(cx),
            Pane::Expressions => self.load_expressions(cx),
            Pane::Gates => self.load_gates(cx),
            Pane::AutoMod => self.load_automod(cx),
            Pane::Queue => self.load_queue(cx),
            Pane::Bots => self.load_bots(cx),
            _ => {}
        }
    }

    pub(crate) fn can(&self, bit: u64, cx: &App) -> bool {
        self.client.read(cx).guild_can(&self.guild_id, bit)
    }

    pub(crate) fn is_owner(&self, cx: &App) -> bool {
        self.client.read(cx).is_owner(&self.guild_id)
    }

    /// Calls the guild's server off the UI thread and hands the answer to `done`.
    pub(crate) fn call<T, F>(
        &mut self,
        cx: &mut Context<Self>,
        request: impl FnOnce(Api, String) -> F + Send + 'static,
        done: impl FnOnce(&mut Self, Result<T, String>, &mut Context<Self>) + 'static,
    ) where
        T: Send + 'static,
        F: Future<Output = ApiResult<T>> + Send + 'static,
    {
        let Some((api, io)) = self.client.read(cx).guild_api(&self.guild_id) else {
            done(self, Err("This guild's server can't be reached right now.".into()), cx);
            return;
        };
        let guild_id = self.guild_id.clone();
        cx.spawn(async move |this, cx| {
            let answer = io.spawn(async move { request(api, guild_id).await }).await;
            let answer = match answer {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(err)) => Err(err.to_string()),
                Err(err) => Err(err.to_string()),
            };
            this.update(cx, |v, cx| {
                done(v, answer, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Reports how something went, under the page's title.
    pub(crate) fn report(&mut self, worked: bool, what: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.status = Some((worked, what.into()));
        cx.notify();
    }

    /// Reports a result: `ok` when it worked, the server's reason when not.
    pub(crate) fn outcome<T>(&mut self, result: &Result<T, String>, ok: &str, cx: &mut Context<Self>) -> bool {
        match result {
            Ok(_) => self.report(true, ok.to_owned(), cx),
            Err(e) => self.report(false, e.clone(), cx),
        }
        result.is_ok()
    }

    pub(crate) fn show_crop(&mut self, dialog: Entity<CropDialog>, sub: Subscription, cx: &mut Context<Self>) {
        self.crop = Some((dialog, sub));
        self.crop_focus = true;
        cx.notify();
    }
}

// ------------------------------------------------------------------ shared pieces

/// A label above a control.
pub(crate) fn field(label: &'static str, control: impl IntoElement, t: &Theme) -> gpui::Div {
    div().flex().flex_col().gap_1p5().child(div().text_size(text::SM).text_color(t.text_muted).child(label)).child(control)
}

/// A group heading inside a page, as in the user settings.
pub(crate) fn heading(title: &'static str, t: &Theme) -> gpui::Div {
    div().pt_2().pb_1().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_faint).child(title.to_lowercase())
}

/// A line of explanation under a heading or a control.
pub(crate) fn hint(text_: impl Into<SharedString>, t: &Theme) -> gpui::Div {
    div().text_size(text::SM).text_color(t.text_faint).child(text_.into())
}

/// A bordered block, as the user settings draw theirs.
pub(crate) fn panel(t: &Theme) -> gpui::Div {
    div().p_4().rounded(radius::MD).border_1().border_color(t.border).flex().flex_col().gap_3()
}

/// A single-line text input, made once for a view.
pub(crate) fn input(placeholder: &'static str, text_: impl Into<SharedString>, cx: &mut Context<GuildSettingsView>) -> Entity<TextInput> {
    let text_ = text_.into();
    cx.new(|cx| TextInput::new(placeholder, cx).with_text(text_))
}

/// A guild's face: its icon, or its initial, in a circle.
pub(crate) fn guild_face(name: &str, url: Option<String>, size: f32, t: &Theme, window: &Window) -> AnyElement {
    let initial: String = name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
    let tile = div()
        .size(px(size))
        .flex_none()
        .rounded_full()
        .overflow_hidden()
        .bg(t.bg_selected)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(size * 0.42))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.text);
    match url {
        // Shrunk properly to the pixels it covers: GPUI's own sampling is jagged.
        Some(url) => tile.child(img(rhein_ui::images::scaled(url, px(size), window)).size_full().rounded_full().object_fit(gpui::ObjectFit::Cover)).into_any_element(),
        None => tile.child(initial).into_any_element(),
    }
}

/// When something happened, in the local time zone.
pub(crate) fn when(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .ok()
        .map(|ts| ts.to_zoned(jiff::tz::TimeZone::system()).strftime("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

impl Render for GuildSettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let client = self.client.read(cx);
        let Some(entry) = client.guild(&self.guild_id) else {
            // The guild went (deleted, left): nothing left to set.
            cx.emit(GuildSettingsEvent::Close);
            return div().into_any_element();
        };
        let guild_name = entry.guild.name.clone();
        let server = entry.server.clone();
        let icon_url = client.guild_icon_url(&self.guild_id);

        let mut nav = div().w(px(220.)).flex_none().flex().flex_col().gap_0p5().p_3().bg(t.bg_panel);
        nav = nav.child(
            div()
                .px_2()
                .pb_3()
                .flex()
                .items_center()
                .gap_2()
                .child(guild_face(&guild_name, icon_url, 24., &t, window))
                .child(div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(guild_name.clone())),
        );
        for pane in Pane::ALL {
            if !self.allowed(pane, cx) {
                continue;
            }
            nav = nav.child(
                ListRow::new(SharedString::from(format!("guild-settings-{pane:?}")), pane.label())
                    .icon(pane.icon())
                    .selected(self.pane == pane)
                    .on_click(cx.listener(move |v, _, _, cx| v.open(pane, cx))),
            );
        }
        nav = nav.child(div().flex_1()).child(
            div()
                .px_2()
                .pt_2()
                .flex()
                .flex_col()
                .child(div().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_faint).child("hosted by"))
                .child(div().text_size(text::SM).text_color(t.text_muted).truncate().child(server)),
        );

        let body = match self.pane {
            Pane::Overview => self.overview_page(&t, window, cx),
            Pane::Roles => self.roles_page(&t, cx),
            Pane::Channels => self.channels_page(&t, cx),
            Pane::Invites => self.invites_page(&t, cx),
            Pane::Bans => self.bans_page(&t, cx),
            Pane::Audit => self.audit_page(&t, cx),
            Pane::Purge => self.purge_page(&t, cx),
            Pane::Voice => self.voice_page(&t, cx),
            Pane::Expressions => self.expressions_page(&t, cx),
            Pane::Gates => self.gates_page(&t, cx),
            Pane::AutoMod => self.automod_page(&t, cx),
            Pane::Queue => self.queue_page(&t, cx),
            Pane::Bots => self.bots_page(&t, cx),
        };
        if let Some((dialog, _)) = &self.crop
            && std::mem::take(&mut self.crop_focus)
        {
            window.focus(&dialog.focus_handle(cx), cx);
        }
        let status = self.status.clone().map(|(ok, what)| {
            div().text_size(text::SM).text_color(if ok { t.success } else { t.danger }).child(what)
        });

        div()
            .id("guild-settings")
            .track_focus(&self.focus)
            .key_context(GUILD_SETTINGS_CONTEXT)
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(GuildSettingsEvent::Close)))
            .absolute()
            .inset_0()
            .occlude()
            .bg(t.bg_main)
            .flex()
            .text_color(t.text)
            .child(nav)
            .child(rhein_ui::smooth_scroll(
                "guild-settings-glide",
                rhein_ui::ScrollTarget::Handle(self.body_scroll.clone()),
                div()
                    .id("guild-settings-body")
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.body_scroll)
                    .px_10()
                    .py_8()
                    .child(
                        div()
                            .mx_auto()
                            .max_w(px(if matches!(self.pane, Pane::Roles | Pane::Channels) { 980. } else { 720. }))
                            .flex()
                            .flex_col()
                            .gap_5()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .child(div().flex_1().text_size(text::XL).font_weight(FontWeight::SEMIBOLD).child(self.pane.label()))
                                    .child(
                                        IconButton::new("guild-settings-close", Icon::X)
                                            .tooltip("Close (Esc)")
                                            .on_click(cx.listener(|_, _, _, cx| cx.emit(GuildSettingsEvent::Close))),
                                    ),
                            )
                            .children(status)
                            .child(body),
                    ),
            ))
            .children(self.crop.as_ref().map(|(dialog, _)| dialog.clone()))
            .into_any_element()
    }
}
