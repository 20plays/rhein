//! Settings: a full-window overlay with a section list on the left.

use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Subscription, Window, div, prelude::*,
    px,
};
use rhein_proto::Device;
use rhein_store::{Client, ServerStatus};
use rhein_ui::theme::{radius, text};
use rhein_ui::{ActiveTheme as _, Avatar, Button, Dismiss, Icon, IconButton, InputEvent, ListRow, Switch, TextInput, Theme, icon};

pub const SETTINGS_CONTEXT: &str = "Settings";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Account,
    Devices,
    Servers,
    Appearance,
    Shortcuts,
    About,
}

impl Section {
    const ALL: [Section; 6] = [Section::Account, Section::Devices, Section::Servers, Section::Appearance, Section::Shortcuts, Section::About];

    fn label(self) -> &'static str {
        match self {
            Section::Account => "My account",
            Section::Devices => "Devices",
            Section::Servers => "Servers",
            Section::Appearance => "Appearance",
            Section::Shortcuts => "Keyboard shortcuts",
            Section::About => "About Rhein",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Section::Account => Icon::User,
            Section::Devices => Icon::Monitor,
            Section::Servers => Icon::Server,
            Section::Appearance => Icon::Palette,
            Section::Shortcuts => Icon::Keyboard,
            Section::About => Icon::Info,
        }
    }
}

pub enum SettingsEvent {
    Close,
    SignOut,
}

/// Every shortcut, for the read-only list.
pub const SHORTCUTS: [(&str, &str); 16] = [
    ("Ctrl+K", "Quick switcher"),
    ("Ctrl+,", "Settings"),
    ("Alt+↑ / Alt+↓", "Previous / next channel"),
    ("Alt+Shift+↑ / ↓", "Previous / next unread channel"),
    ("Esc", "Mark channel read and jump to the present"),
    ("Enter", "Send message"),
    ("Shift+Enter", "New line"),
    ("↑ (empty message)", "Edit your last message"),
    ("Tab / Enter", "Accept a suggestion"),
    ("Ctrl+V", "Paste text or an image"),
    ("Home / End", "Start / end of the line (with Ctrl: of the whole text)"),
    ("Shift + arrows, Home, End", "Select"),
    ("Ctrl+← / Ctrl+→", "Previous / next word (with Shift: select)"),
    ("Ctrl+Backspace / Ctrl+Delete", "Delete the word before / after the cursor"),
    ("Esc (while replying or editing)", "Cancel"),
    ("Ctrl+Q", "Quit"),
];

pub struct SettingsView {
    client: Entity<Client>,
    section: Section,
    display_name: Entity<TextInput>,
    pronouns: Entity<TextInput>,
    bio: Entity<TextInput>,
    avatar_ref: Option<String>,
    avatar_dirty: bool,
    saving: bool,
    status: Option<(bool, String)>,
    devices: Option<Result<Vec<Device>, String>>,
    add_server: Entity<TextInput>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SettingsView {
    pub fn new(client: Entity<Client>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (name, avatar_ref, profile) = {
            let c = client.read(cx);
            let home = c.home.as_ref();
            (
                home.and_then(|h| h.display_name.clone()).unwrap_or_default(),
                home.and_then(|h| h.avatar_ref.clone()),
                home.map(|h| h.user_id.clone()),
            )
        };
        let cached = profile.and_then(|id| client.update(cx, |c, cx| c.profile(&id, cx)));
        let display_name = cx.new(|cx| TextInput::new("Display name", cx).with_text(name));
        let pronouns = cx.new(|cx| {
            TextInput::new("Pronouns", cx).with_text(cached.as_ref().and_then(|p| p.pronouns.clone()).unwrap_or_default())
        });
        let bio = cx.new(|cx| {
            TextInput::new("About me", cx).multiline(5).with_text(cached.as_ref().and_then(|p| p.bio.clone()).unwrap_or_default())
        });
        let add_server = cx.new(|cx| TextInput::new("chat.example.com", cx));
        let subs = vec![
            cx.observe(&client, |_, _, cx| cx.notify()),
            cx.subscribe_in(&add_server, window, |this, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Submit) {
                    let text = input.read(cx).text().trim().to_owned();
                    if !text.is_empty() {
                        this.client.update(cx, |c, cx| c.add_server(text, cx));
                        input.update(cx, |i, cx| i.clear(cx));
                    }
                }
            }),
        ];
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            client,
            section: Section::Account,
            display_name,
            pronouns,
            bio,
            avatar_ref,
            avatar_dirty: false,
            saving: false,
            status: None,
            devices: None,
            add_server,
            focus,
            _subs: subs,
        }
    }

    fn open(&mut self, section: Section, cx: &mut Context<Self>) {
        self.section = section;
        self.status = None;
        if section == Section::Devices {
            self.reload_devices(cx);
        }
        cx.notify();
    }

    fn reload_devices(&mut self, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        self.client.update(cx, |c, cx| {
            c.load_devices(
                move |result, cx| {
                    this.update(cx, |v, cx| {
                        v.devices = Some(result);
                        cx.notify();
                    })
                    .ok();
                },
                cx,
            )
        });
    }

    fn save_profile(&mut self, cx: &mut Context<Self>) {
        let mut patch = serde_json::json!({
            "display_name": self.display_name.read(cx).text().trim(),
            "pronouns": self.pronouns.read(cx).text().trim(),
            "bio": self.bio.read(cx).text().trim(),
        });
        if self.avatar_dirty {
            patch["avatar_ref"] = serde_json::json!(self.avatar_ref);
        }
        self.saving = true;
        self.status = None;
        let this = cx.weak_entity();
        self.client.update(cx, |c, cx| {
            c.save_profile(
                patch,
                move |result, cx| {
                    this.update(cx, |v, cx| {
                        v.saving = false;
                        v.avatar_dirty = false;
                        v.status = Some(match result {
                            Ok(()) => (true, "Saved.".into()),
                            Err(e) => (false, format!("Couldn't save: {e}")),
                        });
                        cx.notify();
                    })
                    .ok();
                },
                cx,
            )
        });
        cx.notify();
    }

    fn pick_avatar(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a picture".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            this.update(cx, |v, cx| {
                v.status = Some((true, "Uploading…".into()));
                let weak = cx.weak_entity();
                v.client.update(cx, |c, cx| {
                    c.upload_avatar(
                        path,
                        move |result, cx| {
                            weak.update(cx, |v, cx| {
                                match result {
                                    Ok(reference) => {
                                        v.avatar_ref = Some(reference);
                                        v.avatar_dirty = true;
                                        v.status = Some((true, "Picture uploaded – save to apply.".into()));
                                    }
                                    Err(e) => v.status = Some((false, format!("Upload failed: {e}"))),
                                }
                                cx.notify();
                            })
                            .ok();
                        },
                        cx,
                    )
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn account(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let client = self.client.read(cx);
        let me = client.user_id().unwrap_or_default().to_owned();
        let name = self.display_name.read(cx).text().to_owned();
        let preview = self.avatar_ref.as_deref().and_then(|r| client.home_media_url(r));
        let field = |label: &'static str, input: &Entity<TextInput>| {
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(div().text_size(text::SM).text_color(t.text_muted).child(label))
                .child(input.clone())
        };
        let copy_id = me.clone();
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(Avatar::new(me.clone(), if name.is_empty() { me.clone() } else { name }).size(px(72.)).image(preview.map(Into::into)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_color(t.text_muted)
                                    .child(me.clone())
                                    .child(
                                        IconButton::new("copy-user-id", Icon::Copy)
                                            .tooltip("Copy user ID")
                                            .on_click(move |_, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_id.clone()))),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(Button::new("avatar-change", "Change picture").icon(Icon::Image).on_click(cx.listener(|v, _, _, cx| v.pick_avatar(cx))))
                                    .when(self.avatar_ref.is_some(), |d| {
                                        d.child(Button::new("avatar-remove", "Remove").ghost().on_click(cx.listener(|v, _, _, cx| {
                                            v.avatar_ref = None;
                                            v.avatar_dirty = true;
                                            cx.notify();
                                        })))
                                    }),
                            ),
                    ),
            )
            .child(field("Display name", &self.display_name))
            .child(field("Pronouns", &self.pronouns))
            .child(field("About me", &self.bio))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        Button::new("profile-save", if self.saving { "Saving…" } else { "Save changes" })
                            .primary()
                            .disabled(self.saving)
                            .on_click(cx.listener(|v, _, _, cx| v.save_profile(cx))),
                    )
                    .children(self.status.clone().map(|(ok, msg)| {
                        div().text_size(text::SM).text_color(if ok { t.success } else { t.danger }).child(msg)
                    })),
            )
            .into_any_element()
    }

    fn devices_section(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div().flex().flex_col().gap_2();
        match &self.devices {
            None => list = list.child(div().text_color(t.text_faint).child("Loading…")),
            Some(Err(e)) => list = list.child(div().text_color(t.danger).child(format!("Couldn't load devices: {e}"))),
            Some(Ok(devices)) => {
                for (i, d) in devices.iter().filter(|d| d.active).enumerate() {
                    let id = d.device_id.clone();
                    let seen = d.last_seen.map(|ms| format!("Last active {}", date(ms))).unwrap_or_default();
                    list = list.child(
                        div()
                            .p_3()
                            .rounded(radius::MD)
                            .bg(t.bg_panel)
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(icon(Icon::Monitor).text_color(t.text_muted))
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .child(div().font_weight(FontWeight::MEDIUM).child(d.name.clone().unwrap_or_else(|| "Unnamed device".into())))
                                    .child(div().text_size(text::XS).text_color(t.text_faint).child(format!("{} · {seen}", d.device_id))),
                            )
                            .child(if d.current {
                                div().text_size(text::SM).text_color(t.success).child("This device").into_any_element()
                            } else {
                                Button::new(("revoke", i), "Sign out")
                                    .ghost()
                                    .on_click(cx.listener(move |v, _, _, cx| {
                                        let id = id.clone();
                                        v.client.update(cx, |c, cx| c.revoke_device(id, cx));
                                        v.reload_devices(cx);
                                    }))
                                    .into_any_element()
                            }),
                    );
                }
            }
        }
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_color(t.text_muted).child("Everywhere this account is signed in. Signing a device out ends its session immediately."))
            .child(list)
            .into_any_element()
    }

    fn servers_section(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let client = self.client.read(cx);
        let home = client.home.as_ref().map(|h| h.doc.server_name.clone());
        let mut list = div().flex().flex_col().gap_2();
        for (i, s) in client.servers.iter().enumerate() {
            let is_home = home.as_deref() == Some(s.name.as_str());
            let guilds = client.guilds.iter().filter(|g| g.server == s.name).count();
            let color = if s.status == ServerStatus::Ready { t.success } else { t.warning };
            let name = s.name.clone();
            list = list.child(
                div()
                    .p_3()
                    .rounded(radius::MD)
                    .bg(t.bg_panel)
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().size(px(8.)).rounded_full().bg(color))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(div().font_weight(FontWeight::MEDIUM).child(s.name.clone()))
                                    .when(is_home, |d| d.child(div().text_size(text::XS).text_color(t.accent).child("HOME"))),
                            )
                            .child(div().text_size(text::XS).text_color(t.text_faint).child(format!(
                                "{} · {} · {guilds} guild{}",
                                s.doc.as_ref().map(|d| d.api.clone()).unwrap_or_else(|| s.input.clone()),
                                s.status.label(),
                                if guilds == 1 { "" } else { "s" }
                            ))),
                    )
                    .when(!is_home, |d| {
                        d.child(Button::new(("remove-server", i), "Remove").ghost().on_click(cx.listener(move |v, _, _, cx| {
                            let name = name.clone();
                            v.client.update(cx, |c, cx| c.remove_server(&name, cx));
                        })))
                    }),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_color(t.text_muted).child("Your account lives on your home server. Other servers host guilds you joined; Rhein signs in to them with your account."))
            .child(list)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(div().flex_1().child(self.add_server.clone()))
                    .child(Button::new("add-server", "Add server").on_click(cx.listener(|v, _, _, cx| {
                        let text = v.add_server.read(cx).text().trim().to_owned();
                        if !text.is_empty() {
                            v.client.update(cx, |c, cx| c.add_server(text, cx));
                            v.add_server.update(cx, |i, cx| i.clear(cx));
                        }
                    }))),
            )
            .into_any_element()
    }

    fn appearance(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let prefs = self.client.read(cx).prefs.clone();
        let row = |title: &'static str, detail: &'static str, control: AnyElement| {
            div()
                .p_3()
                .rounded(radius::MD)
                .bg(t.bg_panel)
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(div().font_weight(FontWeight::MEDIUM).child(title))
                        .child(div().text_size(text::SM).text_color(t.text_faint).child(detail)),
                )
                .child(control)
        };
        let client = self.client.clone();
        let size_control = div()
            .flex()
            .gap_1()
            .children([80u16, 90, 100, 115, 130].into_iter().map(|scale| {
                let active = prefs.text_scale == scale;
                let client = client.clone();
                div()
                    .id(("scale", scale as usize))
                    .px_2()
                    .py_1()
                    .rounded(radius::MD)
                    .border_1()
                    .border_color(if active { t.accent } else { t.border_strong })
                    .text_size(text::SM)
                    .cursor_pointer()
                    .on_click(move |_, _, cx| client.update(cx, |c, cx| c.update_prefs(cx, |p| p.text_scale = scale)))
                    .child(format!("{scale}%"))
            }))
            .into_any_element();
        let (c1, c2) = (self.client.clone(), self.client.clone());
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(row("Message text size", "How large message text is drawn.", size_control))
            .child(row(
                "Compact messages",
                "Smaller avatars and less space between messages.",
                Switch::new("compact", prefs.compact)
                    .on_toggle(move |on, _, cx| c1.update(cx, |c, cx| c.update_prefs(cx, |p| p.compact = on)))
                    .into_any_element(),
            ))
            .child(row(
                "24-hour clock",
                "Show times as 14:05 instead of 2:05 PM.",
                Switch::new("clock24", prefs.clock_24h)
                    .on_toggle(move |on, _, cx| c2.update(cx, |c, cx| c.update_prefs(cx, |p| p.clock_24h = on)))
                    .into_any_element(),
            ))
            .into_any_element()
    }

    fn shortcuts(&self, t: &Theme) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(SHORTCUTS.iter().map(|(keys, what)| {
                div()
                    .px_3()
                    .py_2()
                    .rounded(radius::MD)
                    .bg(t.bg_panel)
                    .flex()
                    .justify_between()
                    .child(div().text_color(t.text_muted).child(*what))
                    .child(
                        div()
                            .px_2()
                            .rounded(radius::SM)
                            .bg(t.bg_raised)
                            .font_family(rhein_ui::theme::FONT_MONO)
                            .text_size(text::SM)
                            .child(*keys),
                    )
            }))
            .into_any_element()
    }

    fn about(&self, t: &Theme) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .text_color(t.text_muted)
            .child(div().text_size(text::XL).font_weight(FontWeight::SEMIBOLD).text_color(t.text).child("Rhein"))
            .child(format!("Version {} – a native client for Telium, written in Rust with GPUI.", env!("CARGO_PKG_VERSION")))
            .child("Licensed under the Apache License 2.0.")
            .child("Bundled: Inter and JetBrains Mono (SIL Open Font License 1.1), Lucide icons (ISC licence), Unicode emoji data via the emojis crate.")
            .into_any_element()
    }
}

fn date(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .ok()
        .map(|ts| ts.to_zoned(jiff::tz::TimeZone::system()).strftime("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let mut nav = div().w(px(220.)).flex_none().flex().flex_col().gap_0p5().p_3().bg(t.bg_panel);
        nav = nav.child(div().px_2().pb_2().text_size(text::XS).font_weight(FontWeight::SEMIBOLD).text_color(t.text_faint).child("SETTINGS"));
        for section in Section::ALL {
            nav = nav.child(
                ListRow::new(SharedString::from(format!("settings-{}", section.label())), section.label())
                    .icon(section.icon())
                    .selected(self.section == section)
                    .on_click(cx.listener(move |v, _, _, cx| v.open(section, cx))),
            );
        }
        nav = nav.child(div().flex_1()).child(
            ListRow::new("settings-sign-out", "Sign out")
                .icon(Icon::LogOut)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::SignOut))),
        );
        let body = match self.section {
            Section::Account => self.account(&t, cx),
            Section::Devices => self.devices_section(&t, cx),
            Section::Servers => self.servers_section(&t, cx),
            Section::Appearance => self.appearance(&t, cx),
            Section::Shortcuts => self.shortcuts(&t),
            Section::About => self.about(&t),
        };
        div()
            .id("settings")
            .track_focus(&self.focus)
            .key_context(SETTINGS_CONTEXT)
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(SettingsEvent::Close)))
            .absolute()
            .inset_0()
            .bg(t.bg_main)
            .flex()
            .text_color(t.text)
            .child(nav)
            .child(
                div()
                    .id("settings-body")
                    .flex_1()
                    .overflow_y_scroll()
                    .px_10()
                    .py_8()
                    .child(
                        div()
                            .max_w(px(640.))
                            .flex()
                            .flex_col()
                            .gap_5()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .child(div().flex_1().text_size(text::XL).font_weight(FontWeight::SEMIBOLD).child(self.section.label()))
                                    .child(
                                        IconButton::new("settings-close", Icon::X)
                                            .tooltip("Close (Esc)")
                                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Close))),
                                    ),
                            )
                            .child(body),
                    ),
            )
    }
}
