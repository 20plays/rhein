//! The home view's main area: the friends page, or a private conversation (whose messages are
//! end-to-end encrypted and arrive with milestone M3).

use gpui::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window, div, prelude::*, px,
};
use rhein_proto::Friend;
use rhein_store::{Client, FriendAction};
use rhein_ui::theme::{radius, size, text};
use rhein_ui::{ActiveTheme as _, Avatar, Button, Icon, InputEvent, Presence, Tabs, TextInput, Theme, icon};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Online,
    All,
    Pending,
    Blocked,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Online, Tab::All, Tab::Pending, Tab::Blocked];

    fn label(self) -> &'static str {
        match self {
            Tab::Online => "Online",
            Tab::All => "All",
            Tab::Pending => "Pending",
            Tab::Blocked => "Blocked",
        }
    }

    fn shows(self, f: &Friend) -> bool {
        match self {
            Tab::Online => f.state == "accepted" && f.status() != "offline",
            Tab::All => f.state == "accepted",
            Tab::Pending => f.state == "pending_in" || f.state == "pending_out",
            Tab::Blocked => f.state == "blocked",
        }
    }
}

pub struct HomeView {
    client: Entity<Client>,
    tab: Tab,
    add: Entity<TextInput>,
    _subs: Vec<Subscription>,
}

impl HomeView {
    pub fn new(client: Entity<Client>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let add = cx.new(|cx| TextInput::new("@someone:example.org", cx));
        let subs = vec![
            cx.observe(&client, |_, _, cx| cx.notify()),
            cx.subscribe_in(&add, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Submit) {
                    this.send_request(cx);
                }
            }),
        ];
        Self { client, tab: Tab::Online, add, _subs: subs }
    }

    fn send_request(&mut self, cx: &mut Context<Self>) {
        let mut id = self.add.read(cx).text().trim().to_owned();
        if id.is_empty() {
            return;
        }
        if !id.starts_with('@') {
            id.insert(0, '@');
        }
        if !id.contains(':') {
            self.client.update(cx, |c, cx| {
                c.error = Some("Use the full address, like @name:server.".into());
                cx.notify();
            });
            return;
        }
        self.add.update(cx, |i, cx| i.clear(cx));
        self.client.update(cx, |c, cx| c.friend_action(FriendAction::Request, id, cx));
    }

    fn friend_row(&self, i: usize, f: &Friend, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let client = self.client.read(cx);
        let name = f.display_name.clone().unwrap_or_else(|| client.friend_or_local_name(&f.user_id));
        let avatar = client.friend_avatar(f);
        let presence = Presence::parse(f.status());
        let detail = match f.state.as_str() {
            "pending_in" => "Incoming friend request".to_owned(),
            "pending_out" => "Outgoing friend request".to_owned(),
            "blocked" => "Blocked".to_owned(),
            _ => f.user_id.clone(),
        };
        let action = |label: &'static str, id: String, what: FriendAction, primary: bool, cx: &mut Context<Self>| {
            let b = Button::new(SharedString::from(format!("{label}-{i}")), label);
            let b = if primary { b.primary() } else { b.ghost() };
            b.on_click(cx.listener(move |this, _, _, cx| {
                let id = id.clone();
                this.client.update(cx, |c, cx| c.friend_action(what, id, cx));
            }))
        };
        let id = f.user_id.clone();
        let mut actions = div().flex().gap_1();
        actions = match f.state.as_str() {
            "pending_in" => actions
                .child(action("Accept", id.clone(), FriendAction::Accept, true, cx))
                .child(action("Decline", id, FriendAction::Remove, false, cx)),
            "pending_out" => actions.child(action("Cancel", id, FriendAction::Remove, false, cx)),
            "blocked" => actions.child(action("Unblock", id, FriendAction::Unblock, false, cx)),
            _ => actions
                .child(action("Remove", id.clone(), FriendAction::Remove, false, cx))
                .child(action("Block", id, FriendAction::Block, false, cx)),
        };
        div()
            .px_3()
            .py_2()
            .rounded(radius::MD)
            .flex()
            .items_center()
            .gap_3()
            .hover(|s| s.bg(t.bg_hover))
            .child(Avatar::new(f.user_id.clone(), name.clone()).size(px(36.)).image(avatar.map(Into::into)).presence(Some(presence)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().font_weight(FontWeight::MEDIUM).truncate().child(name))
                    .child(div().text_size(text::SM).text_color(t.text_faint).truncate().child(detail)),
            )
            .child(actions)
            .into_any_element()
    }

    fn friends_page(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let friends: Vec<Friend> = self.client.read(cx).friends.iter().filter(|f| self.tab.shows(f)).cloned().collect();
        let pending = self.client.read(cx).incoming_friend_requests();
        let this = cx.weak_entity();
        let labels: Vec<String> = Tab::ALL
            .iter()
            .map(|tab| match tab {
                Tab::Pending if pending > 0 => format!("Pending ({pending})"),
                _ => tab.label().to_owned(),
            })
            .collect();
        let selected = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        let tabs = Tabs::new("friend-tabs", labels).selected(selected).on_select(move |i, _, cx| {
            this.update(cx, |v, cx| {
                v.tab = Tab::ALL[i];
                cx.notify();
            })
            .ok();
        });
        let empty = match self.tab {
            Tab::Online => "No friends online right now.",
            Tab::All => "No friends yet. Add someone by their address below.",
            Tab::Pending => "No pending requests.",
            Tab::Blocked => "You haven't blocked anyone.",
        };
        let mut list = div().id("friends-list").flex_1().min_h_0().overflow_y_scroll().px_4().flex().flex_col().gap_0p5();
        if friends.is_empty() {
            list = list.child(div().p_6().text_center().text_color(t.text_faint).child(empty));
        }
        for (i, f) in friends.iter().enumerate() {
            list = list.child(self.friend_row(i, f, t, cx));
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(size::HEADER)
                    .flex_none()
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(t.border)
                    .child(icon(Icon::Users).text_color(t.text_faint))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Friends"))
                    .child(tabs),
            )
            .child(
                div()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_size(text::SM).font_weight(FontWeight::SEMIBOLD).child("Add a friend"))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(div().flex_1().child(self.add.clone()))
                            .child(Button::new("friend-send", "Send request").primary().on_click(cx.listener(|v, _, _, cx| v.send_request(cx)))),
                    ),
            )
            .child(list)
            .into_any_element()
    }

    fn conversation_page(&self, id: &str, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let client = self.client.read(cx);
        let Some(conversation) = client.conversations.iter().find(|c| c.id == id).cloned() else {
            return div().p_6().text_color(t.text_faint).child("This conversation is gone.").into_any_element();
        };
        let title = client.conversation_title(&conversation);
        let me = client.user_id().unwrap_or_default().to_owned();
        let members: Vec<(String, String)> = conversation
            .members
            .iter()
            .map(|m| (m.user_id.clone(), client.friend_or_local_name(&m.user_id)))
            .collect();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(size::HEADER)
                    .flex_none()
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(t.border)
                    .child(icon(if conversation.kind == "gc" { Icon::Users } else { Icon::AtSign }).text_color(t.text_faint))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                    .when(conversation.request_state.as_deref() == Some("pending"), |d| {
                        d.child(div().text_size(text::XS).px_1p5().rounded(radius::SM).bg(t.accent_soft).text_color(t.accent).child("REQUEST"))
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .p_8()
                    .child(
                        div()
                            .flex()
                            .children(members.iter().filter(|(u, _)| *u != me).take(5).map(|(u, n)| {
                                div().ml(px(-8.)).child(Avatar::new(u.clone(), n.clone()).size(px(56.)))
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_color(t.text_muted)
                            .child(icon(Icon::Lock).text_color(t.text_faint))
                            .child("Messages here are end-to-end encrypted."),
                    )
                    .child(
                        div()
                            .max_w(px(460.))
                            .text_center()
                            .text_size(text::SM)
                            .text_color(t.text_faint)
                            .child("Rhein can read and send them once encrypted direct messages land (milestone M3). Until then, use the Telium web client for this conversation."),
                    )
                    .child(
                        div()
                            .text_size(text::XS)
                            .text_color(t.text_faint)
                            .child(format!("Members: {}", members.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", "))),
                    ),
            )
            .into_any_element()
    }
}

impl Render for HomeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let open = self.client.read(cx).home_conversation.clone();
        let page = match open {
            Some(id) => self.conversation_page(&id, &t, cx),
            None => self.friends_page(&t, cx),
        };
        div().flex_1().min_w_0().h_full().bg(t.bg_main).text_color(t.text).child(page)
    }
}
