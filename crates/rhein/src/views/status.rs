//! The status line along the bottom of the window: who you are, where you are, and how each server
//! connection is doing.

use std::rc::Rc;

use gpui::{
    App, ClickEvent, Entity, IntoElement, ParentElement as _, Pixels, Point, RenderOnce, SharedString, Styled as _,
    Window, div, prelude::*, px,
};
use rhein_store::{Client, ServerStatus};
use rhein_ui::theme::{FONT_MONO, size, text};
use rhein_ui::{ActiveTheme as _, Avatar, Icon, IconButton, Presence, tooltip};

use crate::views::chat::typing_label;

type At = Rc<dyn Fn(Point<Pixels>, &mut Window, &mut App)>;
type Click = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct StatusLine {
    client: Entity<Client>,
    on_status: At,
    on_settings: Click,
    on_sign_out: Click,
}

impl StatusLine {
    pub fn new(
        client: Entity<Client>,
        on_status: impl Fn(Point<Pixels>, &mut Window, &mut App) + 'static,
        on_settings: impl Fn(&mut Window, &mut App) + 'static,
        on_sign_out: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self { client, on_status: Rc::new(on_status), on_settings: Rc::new(on_settings), on_sign_out: Rc::new(on_sign_out) }
    }
}

impl RenderOnce for StatusLine {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
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

        // Where you are, then what's happening there.
        let (location, typing) = if client.showing_home {
            let place = match client.home_conversation.as_deref().and_then(|id| client.conversations.iter().find(|c| c.id == id)) {
                Some(c) => format!("direct messages › {}", client.conversation_title(c)),
                None => "direct messages › friends".into(),
            };
            (place, None)
        } else {
            let channel = client.selected_channel().and_then(|id| client.channel(id).map(|c| (id.to_owned(), c.name.clone())));
            let entry = channel.as_ref().and_then(|(id, _)| client.guild_of_channel(id));
            let place = match (entry, &channel) {
                (Some(e), Some((_, ch))) => format!("{} › {} › #{ch}", e.server, e.guild.name),
                (Some(e), None) => format!("{} › {}", e.server, e.guild.name),
                _ => String::new(),
            };
            let typing = channel.as_ref().and_then(|(id, _)| {
                let gid = entry.map(|e| e.guild.id.as_str());
                let names: Vec<String> = client.typing_in(id).iter().map(|u| client.display_name(gid, u)).collect();
                typing_label(&names)
            });
            (place, typing)
        };
        let mentions = client.total_mentions();

        let servers: Vec<(String, ServerStatus, Option<u128>)> =
            client.servers.iter().map(|s| (s.name.clone(), s.status, s.latency.map(|d| d.as_millis()))).collect();

        let on_status = self.on_status.clone();
        let (on_settings, on_sign_out) = (self.on_settings.clone(), self.on_sign_out.clone());

        div()
            .h(size::STATUS)
            .flex_none()
            .w_full()
            .px_2()
            .flex()
            .items_center()
            .gap_3()
            .border_t_1()
            .border_color(t.border)
            .bg(t.bg_base)
            .font_family(FONT_MONO)
            .text_size(text::XS)
            .text_color(t.text_faint)
            .child(
                div()
                    .id("status-me")
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .px_1()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.bg_hover))
                    .tooltip(tooltip("Set status"))
                    .on_click(move |e: &ClickEvent, window, cx| on_status(e.position(), window, cx))
                    .child(Avatar::new(me.clone(), name.clone()).size(px(16.)).image(avatar.map(Into::into)).presence(Some(presence)))
                    .child(div().text_color(t.text_muted).child(name))
                    .child(div().max_w(px(160.)).truncate().child(status)),
            )
            .child(div().text_color(t.border_strong).child("│"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().min_w_0().truncate().child(location))
                    .when_some(typing, |d, typing| d.child(div().flex_none().text_color(t.text_muted).child(typing)))
                    .when(mentions > 0, |d| {
                        let label = if mentions == 1 { "1 mention".to_owned() } else { format!("{mentions} mentions") };
                        d.child(div().flex_none().text_color(t.danger).child(label))
                    }),
            )
            .children(servers.into_iter().map(|(name, status, ms)| {
                let color = match status {
                    ServerStatus::Ready => t.success,
                    ServerStatus::Connecting | ServerStatus::Reconnecting | ServerStatus::Renewing => t.warning,
                    ServerStatus::Incompatible | ServerStatus::Failed => t.danger,
                };
                let detail = match (status, ms) {
                    (ServerStatus::Ready, Some(ms)) => format!("{ms}ms"),
                    (ServerStatus::Ready, None) => String::new(),
                    (other, _) => other.label().to_lowercase(),
                };
                div()
                    .id(SharedString::from(format!("status-server-{name}")))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .tooltip(tooltip(format!("{name}: {}", status.label())))
                    .child(div().size(px(6.)).rounded_full().bg(color))
                    .child(div().text_color(t.text_muted).child(name))
                    .when(!detail.is_empty(), |d| d.child(detail))
            }))
            .child(div().text_color(t.border_strong).child("│"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        IconButton::new("open-settings", Icon::Settings)
                            .tooltip("Settings (Ctrl+,)")
                            .on_click(move |_, window, cx| on_settings(window, cx)),
                    )
                    .child(
                        IconButton::new("sign-out", Icon::LogOut)
                            .tooltip("Sign out")
                            .on_click(move |_, window, cx| on_sign_out(window, cx)),
                    ),
            )
    }
}
