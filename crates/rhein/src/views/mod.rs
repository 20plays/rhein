pub mod chat;
pub mod emoji_picker;
pub mod login;
pub mod message_text;
pub mod panels;
pub mod rows;
pub mod settings;
pub mod shell;
pub mod switcher;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Subscription, Window,
    div,
};
use rhein_store::{Client, Phase};
use rhein_ui::ActiveTheme as _;
use rhein_ui::theme::{FONT_UI, text};

use login::LoginView;
use shell::Shell;

/// Swaps between the login screen and the signed-in shell as the client's phase changes.
pub struct Root {
    client: Entity<Client>,
    login: Option<Entity<LoginView>>,
    shell: Option<Entity<Shell>>,
    _sub: Subscription,
}

impl Root {
    pub fn new(client: Entity<Client>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe_in(&client, window, |this, _, window, cx| this.sync(window, cx));
        let mut this = Self { client, login: None, shell: None, _sub: sub };
        this.sync(window, cx);
        this
    }

    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let signed_in = self.client.read(cx).phase == Phase::SignedIn;
        if signed_in && self.shell.is_none() {
            let client = self.client.clone();
            self.shell = Some(cx.new(|cx| Shell::new(client, window, cx)));
            self.login = None;
        } else if !signed_in && self.login.is_none() && self.client.read(cx).phase != Phase::Restoring {
            let client = self.client.clone();
            self.login = Some(cx.new(|cx| LoginView::new(client, window, cx)));
            self.shell = None;
        }
        cx.notify();
    }
}

impl Render for Root {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        let root = div()
            .size_full()
            .bg(t.bg_main)
            .text_color(t.text)
            .font_family(FONT_UI)
            .text_size(text::MD);
        if let Some(shell) = &self.shell {
            root.child(shell.clone())
        } else if let Some(login) = &self.login {
            root.child(login.clone())
        } else {
            root.flex().items_center().justify_center().text_color(t.text_faint).child("Starting Rhein…")
        }
    }
}
