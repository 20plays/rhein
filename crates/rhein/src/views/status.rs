//! The status line under the chat: where you are, what's happening there, and how each server
//! connection is doing.

use gpui::{App, Entity, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div, prelude::*, px};
use rhein_store::{Client, ServerStatus};
use rhein_ui::theme::{FONT_MONO, size, text};
use rhein_ui::{ActiveTheme as _, tooltip};


#[derive(IntoElement)]
pub struct StatusLine {
    client: Entity<Client>,
}

impl StatusLine {
    pub fn new(client: Entity<Client>) -> Self {
        Self { client }
    }
}

impl RenderOnce for StatusLine {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.theme().clone();
        let client = self.client.read(cx);

        // Where you are (who is typing shows above the message bar).
        let location = if client.showing_home {
            match client.home_conversation.as_deref().and_then(|id| client.conversations.iter().find(|c| c.id == id)) {
                Some(c) => format!("direct messages › {}", client.conversation_title(c)),
                None => "direct messages › friends".into(),
            }
        } else {
            let channel = client.selected_channel().and_then(|id| client.channel(id).map(|c| (id.to_owned(), c.name.clone())));
            let entry = channel.as_ref().and_then(|(id, _)| client.guild_of_channel(id));
            match (entry, &channel) {
                (Some(e), Some((_, ch))) => format!("{} › {} › #{ch}", e.server, e.guild.name),
                (Some(e), None) => format!("{} › {}", e.server, e.guild.name),
                _ => String::new(),
            }
        };
        let mentions = client.total_mentions();

        let servers: Vec<(String, ServerStatus, Option<u128>)> =
            client.servers.iter().map(|s| (s.name.clone(), s.status, s.latency.map(|d| d.as_millis()))).collect();

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
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().min_w_0().truncate().child(location))
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
    }
}
