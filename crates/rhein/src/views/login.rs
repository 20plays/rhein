use gpui::{
    AppContext as _, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, Window, div, prelude::*, px,
};
use rhein_store::{Client, Phase};
use rhein_ui::theme::{radius, text};
use rhein_ui::{ActiveTheme as _, Button, Icon, InputEvent, TextInput, Toast, ToastKind, icon};

pub struct LoginView {
    client: Entity<Client>,
    server: Entity<TextInput>,
    user: Entity<TextInput>,
    password: Entity<TextInput>,
    register: bool,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl LoginView {
    pub fn new(client: Entity<Client>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let default_server = std::env::var("RHEIN_SERVER").unwrap_or_else(|_| "localhost:8448".into());
        let server = cx.new(|cx| TextInput::new("chat.example.com", cx).with_text(default_server));
        let user = cx.new(|cx| TextInput::new("Username", cx));
        let password = cx.new(|cx| TextInput::new("Password", cx).masked());
        let subs = [&server, &user, &password]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Submit) {
                        this.submit(window, cx);
                    }
                })
            })
            .collect();
        let focus = cx.focus_handle();
        window.focus(&user.focus_handle(cx), cx);
        Self { client, server, user, password, register: false, focus, _subs: subs }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let server = self.server.read(cx).text().to_owned();
        let user = self.user.read(cx).text().to_owned();
        let password = self.password.read(cx).text().to_owned();
        let empty = [
            (server.trim().is_empty(), &self.server),
            (user.trim().is_empty(), &self.user),
            (password.is_empty(), &self.password),
        ];
        if let Some((_, input)) = empty.iter().find(|(missing, _)| *missing) {
            window.focus(&input.focus_handle(cx), cx);
            return;
        }
        let register = self.register;
        self.password.update(cx, |p, cx| p.clear(cx));
        self.client.update(cx, |client, cx| client.sign_in(server, user, password, register, cx));
    }
}

impl Focusable for LoginView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for LoginView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let client = self.client.read(cx);
        let busy = client.phase == Phase::SigningIn;
        let error = client.error.clone();
        let register = self.register;

        let field = |label: &'static str, input: &Entity<TextInput>| {
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(div().text_size(text::SM).text_color(t.text_muted).child(label))
                .child(input.clone())
        };

        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(t.bg_base)
            .child(
                div()
                    .w(px(380.))
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .size(px(40.))
                                    .mb_3()
                                    .rounded(radius::LG)
                                    .bg(t.accent)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(icon(Icon::MessagesSquare).text_color(t.text_on_accent)),
                            )
                            .child(
                                div().text_size(text::XXL).font_weight(FontWeight::SEMIBOLD).child(if register {
                                    "Create an account"
                                } else {
                                    "Welcome back"
                                }),
                            )
                            .child(div().text_color(t.text_muted).child(if register {
                                "Register on a Telium server of your choice."
                            } else {
                                "Sign in with your Telium account."
                            })),
                    )
                    .children(error.map(|e| Toast::new(e, ToastKind::Error)))
                    .child(field("Home server", &self.server))
                    .child(field("Username", &self.user))
                    .child(field("Password", &self.password))
                    .child(
                        Button::new(
                            "login-submit",
                            match (busy, register) {
                                (true, _) => "One moment…",
                                (false, true) => "Create account",
                                (false, false) => "Sign in",
                            },
                        )
                        .primary()
                        .full_width()
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    )
                    // Adding an account: the one we came from is a click away.
                    .children(self.client.read(cx).returning_to.clone().map(|(id, name)| {
                        let client = self.client.clone();
                        Button::new("login-back", format!("Back to {name}"))
                            .ghost()
                            .full_width()
                            .on_click(move |_, _, cx| client.update(cx, |c, cx| c.switch_account(&id, cx)))
                    }))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_size(text::SM)
                            .child(
                                div()
                                    .id("login-toggle")
                                    .text_color(t.accent)
                                    .cursor_pointer()
                                    .hover(|s| s.text_color(t.accent_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.register = !this.register;
                                        cx.notify();
                                    }))
                                    .child(if register { "I already have an account" } else { "Create an account" }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_color(t.text_faint)
                                    .child(icon(Icon::Lock).text_color(t.text_faint))
                                    .child("Password stays on this device"),
                            ),
                    ),
            )
    }
}
