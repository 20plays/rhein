//! Settings: a full-window overlay with a section list on the left.

use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Subscription, Window, div, prelude::*,
    px,
};
use rhein_proto::Device;
use rhein_store::{Client, ServerStatus};

use crate::views::profile;
use rhein_ui::theme::{FONT_MONO, radius, text};
use rhein_ui::{ActiveTheme as _, Avatar, Button, Dismiss, Icon, IconButton, InputEvent, ListRow, Switch, TextInput, Theme, icon};

pub const SETTINGS_CONTEXT: &str = "Settings";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Account,
    Devices,
    Servers,
    Appearance,
    Activity,
    Encryption,
    Voice,
    Shortcuts,
    About,
}

impl Section {
    const ALL: [Section; 9] = [
        Section::Account,
        Section::Devices,
        Section::Servers,
        Section::Appearance,
        Section::Activity,
        Section::Encryption,
        Section::Voice,
        Section::Shortcuts,
        Section::About,
    ];

    fn label(self) -> &'static str {
        match self {
            Section::Account => "My account",
            Section::Devices => "Devices",
            Section::Servers => "Servers",
            Section::Appearance => "Appearance",
            Section::Activity => "Activity",
            Section::Encryption => "Encryption",
            Section::Voice => "Voice",
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
            Section::Activity => Icon::Gamepad,
            Section::Encryption => Icon::Lock,
            Section::Voice => Icon::Mic,
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
    banner_ref: Option<String>,
    banner_dirty: bool,
    accent: Option<u32>,
    theme: Option<[u32; 2]>,
    /// Accent or theme changed since the last save.
    look_dirty: bool,
    /// The profile's banner, accent and theme have been read in (they arrive with the profile).
    look_loaded: bool,
    saving: bool,
    status: Option<(bool, String)>,
    devices: Option<Result<Vec<Device>, String>>,
    /// Microphones and speakers, once listed.
    audio_devices: Option<(Vec<rhein_voice::audio::AudioDevice>, Vec<rhein_voice::audio::AudioDevice>)>,
    /// Redraws the level meter while the voice section is open.
    meter: Option<gpui::Task<()>>,
    add_server: Entity<TextInput>,
    unlock_password: Entity<TextInput>,
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
        let unlock_password = cx.new(|cx| TextInput::new("Your account password", cx).masked());
        let subs = vec![
            cx.observe(&client, |this, _, cx| {
                this.load_look(cx);
                cx.notify();
            }),
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
            banner_ref: None,
            banner_dirty: false,
            accent: None,
            theme: None,
            look_dirty: false,
            look_loaded: false,
            saving: false,
            status: None,
            devices: None,
            audio_devices: None,
            meter: None,
            add_server,
            unlock_password,
            focus,
            _subs: subs,
        }
    }

    /// Takes the banner, accent and theme from the profile once it has arrived, and whatever the
    /// profile had for pronouns and bio if those fields are still untouched.
    fn load_look(&mut self, cx: &mut Context<Self>) {
        if self.look_loaded {
            return;
        }
        let client = self.client.read(cx);
        let Some(me) = client.user_id().map(str::to_owned) else { return };
        let Some(profile) = client.cached_profile(&me).cloned() else { return };
        self.look_loaded = true;
        self.banner_ref = profile.banner_ref.filter(|r| !r.is_empty());
        self.accent = profile.accent_color;
        self.theme = profile.theme_colors;
        if self.pronouns.read(cx).text().is_empty() {
            let text = profile.pronouns.unwrap_or_default();
            self.pronouns.update(cx, |i, cx| i.set_text(text, cx));
        }
        if self.bio.read(cx).text().is_empty() {
            let text = profile.bio.unwrap_or_default();
            self.bio.update(cx, |i, cx| i.set_text(text, cx));
        }
    }

    /// Opens a section by its label ("Activity"), for debug start screens.
    #[cfg(debug_assertions)]
    pub fn open_named(&mut self, label: &str, cx: &mut Context<Self>) {
        if let Some(section) = Section::ALL.into_iter().find(|s| s.label().eq_ignore_ascii_case(label)) {
            self.open(section, cx);
        }
    }

    fn open(&mut self, section: Section, cx: &mut Context<Self>) {
        self.section = section;
        self.status = None;
        if section == Section::Devices {
            self.reload_devices(cx);
        }
        self.meter = None;
        if section == Section::Voice {
            self.load_audio_devices(cx);
            // The level meter moves while a call is up.
            self.meter = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(std::time::Duration::from_millis(50)).await;
                    if this.update(cx, |v, cx| if v.client.read(cx).call().is_some() { cx.notify() }).is_err() {
                        return;
                    }
                }
            }));
        }
        cx.notify();
    }

    fn load_audio_devices(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async { (rhein_voice::audio::input_devices(), rhein_voice::audio::output_devices()) })
                .await;
            this.update(cx, |v, cx| {
                v.audio_devices = Some(found);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Calls: which microphone and speakers, how sensitive, push-to-talk, processing.
    fn voice_section(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let client = self.client.read(cx);
        let voice = client.voice_prefs();
        let level = client.input_level();
        let row = |title: &'static str, detail: &'static str, control: AnyElement| {
            div()
                .p_3()
                .rounded(radius::MD)
                .border_1()
                .border_color(t.border)
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().font_weight(FontWeight::MEDIUM).child(title))
                                .child(div().text_size(text::SM).text_color(t.text_faint).child(detail)),
                        ),
                )
                .child(control)
        };
        let pill = |id: SharedString, label: String, active: bool, on_click: Box<dyn Fn(&mut gpui::App)>| {
            div()
                .id(id)
                .px_2()
                .py_1()
                .rounded(radius::MD)
                .border_1()
                .border_color(if active { t.accent } else { t.border_strong })
                .text_size(text::SM)
                .cursor_pointer()
                .on_click(move |_, _, cx| on_click(cx))
                .child(label)
        };
        let device_pills = |input: bool| -> AnyElement {
            let current = if input { voice.input.clone() } else { voice.output.clone() };
            let Some((inputs, outputs)) = &self.audio_devices else {
                return div().text_size(text::SM).text_color(t.text_faint).child("Looking for devices…").into_any_element();
            };
            let list = if input { inputs } else { outputs };
            let mut choices = vec![(None, "System default".to_owned())];
            choices.extend(list.iter().filter(|d| !d.id.ends_with(":default")).map(|d| (Some(d.id.clone()), d.name.clone())));
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(choices.into_iter().enumerate().map(|(i, (id, name))| {
                    let client = self.client.clone();
                    let active = id == current;
                    pill(
                        SharedString::from(format!("{}-{i}", if input { "in" } else { "out" })),
                        name,
                        active,
                        Box::new(move |cx| {
                            let id = id.clone();
                            client.update(cx, |c, cx| {
                                c.update_voice_prefs(cx, |v| if input { v.input = id } else { v.output = id })
                            })
                        }),
                    )
                }))
                .into_any_element()
        };
        let gates: [(Option<f32>, &str); 5] = [(None, "Off"), (Some(-60.), "-60 dB"), (Some(-50.), "-50 dB"), (Some(-40.), "-40 dB"), (Some(-30.), "-30 dB")];
        let gate_pills = div().flex().gap_1().children(gates.into_iter().enumerate().map(|(i, (gate, label))| {
            let client = self.client.clone();
            pill(
                SharedString::from(format!("gate-{i}")),
                label.to_owned(),
                voice.gate == gate,
                Box::new(move |cx| client.update(cx, |c, cx| c.update_voice_prefs(cx, |v| v.gate = gate))),
            )
        }));
        // The meter: how loud the microphone is, and where the gate cuts in (-70..0 dBFS).
        let meter = level.map(|level| {
            let db = 20. * level.max(1e-4).log10();
            let at = |db: f32| ((db + 70.) / 70.).clamp(0., 1.);
            let open = voice.gate.is_none_or(|g| db >= g);
            div()
                .relative()
                .h(px(8.))
                .w_full()
                .rounded_full()
                .bg(t.bg_selected)
                .child(div().absolute().left_0().top_0().h_full().rounded_full().w(gpui::relative(at(db))).bg(if open { t.success } else { t.text_faint }))
                .children(voice.gate.map(|g| div().absolute().top_0().h_full().w(px(2.)).left(gpui::relative(at(g))).bg(t.text)))
        });
        let sensitivity = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(gate_pills)
            .child(match meter {
                Some(meter) => meter.into_any_element(),
                None => div().text_size(text::SM).text_color(t.text_faint).child("Join a call to see your microphone's level here.").into_any_element(),
            });
        let (c1, c2) = (self.client.clone(), self.client.clone());
        // Call privacy in conversations: the copy is the web client's (`POSTURE_COPY`).
        let postures = [
            ("direct", "Direct", "Connect straight to the others when the network allows. The quickest path, but in a peer-to-peer call everyone in it can see your IP address."),
            ("relay", "Relay only", "Every connection goes through a TURN relay, so nobody in the call learns your address. Needs a relay on your server, or an SFU."),
            ("sfu", "Through an SFU", "Media goes through an SFU and is encrypted end to end, so the SFU cannot hear or see it. The SFU still sees your address and when you talk."),
        ];
        let current = if voice.posture.is_empty() { "direct" } else { voice.posture.as_str() };
        let explained = postures.iter().find(|(id, ..)| *id == current).map(|(.., copy)| *copy).unwrap_or_default();
        let posture_control = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().flex().gap_1().children(postures.iter().map(|(id, label, _)| {
                let (client, id) = (self.client.clone(), id.to_string());
                pill(
                    SharedString::from(format!("posture-{id}")),
                    label.to_string(),
                    current == id,
                    Box::new(move |cx| {
                        let id = id.clone();
                        client.update(cx, |c, cx| c.update_voice_prefs(cx, |v| v.posture = id))
                    }),
                )
            })))
            .child(div().text_size(text::SM).text_color(t.text_faint).child(explained));
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(row("Call privacy", "For calls in direct messages and group chats. A call is as private as its most cautious participant asks; no server learns your choice.", posture_control.into_any_element()))
            .child(row("Microphone", "Where your voice comes from.", device_pills(true)))
            .child(row("Speakers", "Where calls are played.", device_pills(false)))
            .child(row("Input sensitivity", "Below this level the microphone sends silence, so keyboards and fans stay out of the call.", sensitivity.into_any_element()))
            .child(row(
                "Push to talk",
                "Only send your voice while holding the backtick key (`) in Rhein.",
                Switch::new("ptt", voice.push_to_talk)
                    .on_toggle(move |on, _, cx| c1.update(cx, |c, cx| c.update_voice_prefs(cx, |v| v.push_to_talk = on)))
                    .into_any_element(),
            ))
            .child(row(
                "Voice processing",
                "Echo cancellation, noise suppression and automatic gain. Turn off for a studio microphone or music.",
                Switch::new("processing", !voice.raw_input)
                    .on_toggle(move |on, _, cx| c2.update(cx, |c, cx| c.update_voice_prefs(cx, |v| v.raw_input = !on)))
                    .into_any_element(),
            ))
            .into_any_element()
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
        if self.banner_dirty {
            patch["banner_ref"] = serde_json::json!(self.banner_ref);
        }
        if self.look_dirty {
            patch["accent_color"] = serde_json::json!(self.accent);
            patch["theme_colors"] = serde_json::json!(self.theme);
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
                        if result.is_ok() {
                            v.avatar_dirty = false;
                            v.banner_dirty = false;
                            v.look_dirty = false;
                        }
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
        self.pick_media(false, cx);
    }

    fn pick_banner(&mut self, cx: &mut Context<Self>) {
        self.pick_media(true, cx);
    }

    /// Uploads a picture for the avatar or the banner; it applies on save.
    fn pick_media(&mut self, banner: bool, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(if banner { "Choose a banner".into() } else { "Choose a picture".into() }),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            this.update(cx, |v, cx| {
                v.status = Some((true, "Uploading…".into()));
                let weak = cx.weak_entity();
                v.client.update(cx, |c, cx| {
                    c.upload_profile_media(
                        path,
                        move |result, cx| {
                            weak.update(cx, |v, cx| {
                                match result {
                                    Ok(reference) if banner => {
                                        v.banner_ref = Some(reference);
                                        v.banner_dirty = true;
                                        v.status = Some((true, "Banner uploaded – save to apply.".into()));
                                    }
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
            .child(self.look_editor(t, cx))
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

    /// Banner, accent and theme, with a live preview of the card as others will see it.
    fn look_editor(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let client = self.client.read(cx);
        let me = client.user_id().unwrap_or_default().to_owned();
        let name = Some(self.display_name.read(cx).text().trim().to_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| me.clone());
        let face_url = self.avatar_ref.as_deref().and_then(|r| client.home_media_url(r));
        let look = profile::Look {
            banner: self.banner_ref.as_deref().and_then(|r| client.home_media_url(r)),
            accent: self.accent,
            theme: self.theme,
        };
        let pronouns = self.pronouns.read(cx).text().trim().to_owned();
        let face = Avatar::new(me.clone(), name.clone()).size(px(profile::FACE_SIZE)).image(face_url.map(Into::into)).into_any_element();
        let preview = profile::frame(
            &look,
            t,
            div().flex().flex_col().child(profile::header(&look, face, t)).child(
                div()
                    .px_4()
                    .pb_4()
                    .flex()
                    .flex_col()
                    .child(div().text_size(text::LG).font_weight(FontWeight::SEMIBOLD).child(name))
                    .child(div().text_size(text::SM).text_color(t.text_faint).child(me))
                    .when(!pronouns.is_empty(), |d| d.child(div().text_size(text::SM).text_color(t.text_muted).child(pronouns))),
            ),
        );

        // A row of colour swatches with "none" first; `pick` applies a choice.
        let swatches = |id: &'static str, current: Option<u32>, pick: fn(&mut Self, Option<u32>), cx: &mut Context<Self>| {
            let mut row = div().flex().flex_wrap().gap_2();
            let choices = std::iter::once(None).chain(profile::ACCENTS.iter().map(|(_, c)| Some(*c)));
            for (i, choice) in choices.enumerate() {
                let selected = current == choice;
                let label = choice.map_or("None", |c| profile::ACCENTS.iter().find(|(_, x)| *x == c).map_or("", |(n, _)| *n));
                row = row.child(
                    div()
                        .id((id, i))
                        .size(px(28.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .tooltip(rhein_ui::tooltip(label))
                        .map(|d| match choice {
                            Some(c) => d.bg(profile::rgb(c)),
                            None => d.border_1().border_color(t.border_strong).child(icon(Icon::X).text_color(t.text_faint)),
                        })
                        .when(selected, |d| d.border_2().border_color(t.text))
                        .when(selected && choice.is_some(), |d| d.child(icon(Icon::Check).text_color(gpui::white())))
                        .on_click(cx.listener(move |v, _, _, cx| {
                            pick(v, choice);
                            v.look_dirty = true;
                            cx.notify();
                        })),
                );
            }
            row
        };
        let label = |title: &'static str, hint: &'static str| {
            div()
                .flex()
                .flex_col()
                .child(div().text_size(text::SM).text_color(t.text_muted).child(title))
                .child(div().text_size(text::XS).text_color(t.text_faint).child(hint))
        };
        let top = self.theme.map(|[a, _]| a);
        let bottom = self.theme.map(|[_, b]| b);
        let banner = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(label("Banner", "A wide picture across the top of your card."))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(Button::new("banner-change", "Change banner").icon(Icon::Image).on_click(cx.listener(|v, _, _, cx| v.pick_banner(cx))))
                    .when(self.banner_ref.is_some(), |d| {
                        d.child(Button::new("banner-remove", "Remove").ghost().on_click(cx.listener(|v, _, _, cx| {
                            v.banner_ref = None;
                            v.banner_dirty = true;
                            cx.notify();
                        })))
                    }),
            );
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .flex()
                    .gap_8()
                    .items_start()
                    .child(banner)
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(div().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_faint).child("preview"))
                            .child(preview),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(label("Accent", "Behind your name, wherever your card is drawn without a banner."))
                    .child(swatches("accent", self.accent, |v, c| v.accent = c, cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(label("Theme", "The border and background of your card, from top to bottom."))
                    // Picking one end fills the other if it was empty; "none" on either clears both.
                    .child(swatches("theme-top", top, |v, c| v.theme = c.map(|c| [c, v.theme.map_or(c, |[_, b]| b)]), cx))
                    .child(swatches("theme-bottom", bottom, |v, c| v.theme = c.map(|c| [v.theme.map_or(c, |[a, _]| a), c]), cx)),
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
                                Button::new(("revoke", i), "Sign out").small()
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
                        d.child(Button::new(("remove-server", i), "Remove").small().ghost().on_click(cx.listener(move |v, _, _, cx| {
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
                .border_1()
                .border_color(t.border)
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
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
        let (c1, c2, c3) = (self.client.clone(), self.client.clone(), self.client.clone());
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
            .child(row(
                "Reduce motion",
                "Show changes instantly: no fades, glides or blinking cursor.",
                Switch::new("reduce-motion", prefs.reduce_motion)
                    .on_toggle(move |on, _, cx| c3.update(cx, |c, cx| c.update_prefs(cx, |p| p.reduce_motion = on)))
                    .into_any_element(),
            ))
            .into_any_element()
    }

    /// Rich presence: what is shared, from which sources, and what is being shared right now.
    fn activity_section(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let client = self.client.read(cx);
        let prefs = client.prefs.clone();
        let status = client.rpc_status.clone();
        let found = client.local_activities.clone();
        let shared = client.shared_activities().to_vec();
        let row = |title: &'static str, detail: &'static str, control: AnyElement| {
            div()
                .p_3()
                .rounded(radius::MD)
                .border_1()
                .border_color(t.border)
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().font_weight(FontWeight::MEDIUM).child(title))
                        .child(div().text_size(text::SM).text_color(t.text_faint).child(detail)),
                )
                .child(control)
        };
        let toggle = |id: &'static str, on: bool, set: fn(&mut rhein_store::prefs::Prefs, bool)| {
            let client = self.client.clone();
            Switch::new(id, on).on_toggle(move |on, _, cx| client.update(cx, |c, cx| c.update_prefs(cx, |p| set(p, on)))).into_any_element()
        };
        let mono = |s: String| div().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_faint).child(s);

        let sources = if !prefs.share_activity {
            div().child(mono("sharing is off".into()))
        } else {
            let socket = status.socket.as_ref().map_or("not listening (another app holds every slot)".to_owned(), |p| p.display().to_string());
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(mono(format!("discord rpc   {socket}")))
                .child(mono(format!("websocket     {}", status.websocket_port.map_or("not listening".to_owned(), |p| format!("127.0.0.1:{p}")))))
                .child(mono(format!("rpc apps      {}", if status.clients.is_empty() { "none connected".to_owned() } else { status.clients.len().to_string() })))
                .child(mono(format!("games         {}", if status.detected.is_empty() { "none running".to_owned() } else { status.detected.join(", ") })))
                .child(mono(format!("media players {}", if status.players.is_empty() { "none".to_owned() } else { status.players.join(", ") })))
        };

        // Everything found, with whether it goes out; any of it can be hidden by name.
        let mut list = div().flex().flex_col().gap_2();
        if found.is_empty() && prefs.hidden_activities.is_empty() {
            list = list.child(div().text_size(text::SM).text_color(t.text_faint).child("Nothing detected right now."));
        }
        for (i, a) in found.iter().enumerate() {
            let is_shared = shared.contains(a);
            let name = a.name.clone();
            let client = self.client.clone();
            list = list.child(
                div()
                    .p_3()
                    .rounded(radius::MD)
                    .border_1()
                    .border_color(t.border)
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(icon(match a.kind { 2 => Icon::Music, 1 | 3 => Icon::Monitor, _ => Icon::Gamepad }).text_color(t.text_faint))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().child(a.summary()))
                            .child(mono(if is_shared { "shared".into() } else { "not shared".into() })),
                    )
                    .child(Button::new(("hide-activity", i), "Don't share").small().ghost().on_click(move |_, _, cx| {
                        let name = name.clone();
                        client.update(cx, |c, cx| {
                            c.update_prefs(cx, |p| {
                                if !p.hidden_activities.contains(&name) {
                                    p.hidden_activities.push(name);
                                }
                            })
                        });
                    })),
            );
        }
        for (i, name) in prefs.hidden_activities.iter().enumerate() {
            let (client, name) = (self.client.clone(), name.clone());
            list = list.child(
                div()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().text_size(text::SM).text_color(t.text_muted).child(format!("{name} is never shared")))
                    .child(Button::new(("unhide-activity", i), "Share again").small().ghost().on_click(move |_, _, cx| {
                        let name = name.clone();
                        client.update(cx, |c, cx| c.update_prefs(cx, |p| p.hidden_activities.retain(|n| *n != name)));
                    })),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(row(
                "Share my activity",
                "Show what you are playing, listening to or watching, like Discord. Apps using Discord Rich Presence report to Rhein directly.",
                toggle("share-activity", prefs.share_activity, |p, on| p.share_activity = on),
            ))
            .child(row("Games", "Games found running, and apps that report over Discord RPC.", toggle("share-games", prefs.share_games, |p, on| p.share_games = on)))
            .child(row("Music", "The song a media player is playing.", toggle("share-music", prefs.share_music, |p, on| p.share_music = on)))
            .child(row("Videos", "What a browser or video player is showing.", toggle("share-videos", prefs.share_videos, |p, on| p.share_videos = on)))
            .child(div().pt_4().pb_1().font_weight(FontWeight::SEMIBOLD).child("Sources"))
            .child(sources)
            .child(div().pt_4().pb_1().font_weight(FontWeight::SEMIBOLD).child("Right now"))
            .child(list)
            .into_any_element()
    }

    /// Encrypted DMs on this device: whether the recovery keyring is here, this device's key
    /// fingerprint, and whether the account's identity vouches for it.
    fn encryption_section(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let client = self.client.read(cx);
        let status = client.dm.clone();
        let error = client.dm_error.clone();
        let line = |label: &'static str, value: String, good: Option<bool>| {
            div()
                .flex()
                .gap_4()
                .py_1()
                .child(div().w(px(180.)).flex_none().text_color(t.text_muted).child(label))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(FONT_MONO)
                        .text_size(text::SM)
                        .text_color(match good {
                            Some(true) => t.success,
                            Some(false) => t.warning,
                            None => t.text,
                        })
                        .child(value),
                )
        };
        let mut body = div().flex().flex_col().gap_1().child(
            div()
                .pb_3()
                .text_size(text::SM)
                .text_color(t.text_faint)
                .child("Direct messages and group chats are end-to-end encrypted with MLS. Their keys never leave your devices; your home server keeps only a copy sealed with a key derived from your password."),
        );
        match &status {
            None => {
                body = body.child(line("Status", error.clone().unwrap_or_else(|| "starting…".into()), Some(false)));
            }
            Some(s) => {
                body = body
                    .child(line("Key recovery", s.vault_mode.replace('_', " "), None))
                    .child(line(
                        "Recovery key on this device",
                        s.locked.clone().map_or("available".into(), |why| format!("locked: {why}")),
                        Some(s.locked.is_none()),
                    ))
                    .child(line("This device's key", s.fingerprint(), None))
                    .child(line(
                        "Cross-signing",
                        match (&s.identity_key, s.cross_signed, &s.identity_problem) {
                            (_, _, Some(problem)) => format!("problem: {problem}"),
                            (None, _, _) => "no identity yet (set up by the Telium web client)".into(),
                            (Some(_), true, _) => "this device is vouched for".into(),
                            (Some(_), false, _) => "not vouched for yet".into(),
                        },
                        Some(s.identity_problem.is_none() && (s.identity_key.is_none() || s.cross_signed)),
                    ))
                    .children(s.immutable_id.clone().map(|id| line("Identity", id, None)));
                if let Some(err) = &error {
                    body = body.child(div().pt_2().text_size(text::SM).text_color(t.danger).child(err.clone()));
                }
            }
        }
        // A session restored without the password can't open the vault; proving it again does.
        if status.as_ref().is_none_or(|s| s.locked.is_some()) {
            let client = self.client.clone();
            let input = self.unlock_password.clone();
            body = body.child(
                div()
                    .pt_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().font_weight(FontWeight::MEDIUM).child("Unlock on this device"))
                    .child(div().text_size(text::SM).text_color(t.text_faint).child("Enter your password once. It is checked with OPAQUE, so it never leaves this device."))
                    .child(
                        div().flex().gap_2().child(div().flex_1().child(self.unlock_password.clone())).child(
                            Button::new("dm-unlock", "Unlock").primary().on_click(move |_, _, cx| {
                                let password = input.read(cx).text().to_owned();
                                if password.is_empty() {
                                    return;
                                }
                                input.update(cx, |i, cx| i.clear(cx));
                                client.update(cx, |c, cx| c.unlock_dm(password, cx));
                            }),
                        ),
                    ),
            );
        }
        body.into_any_element()
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
            .child("Bundled: Geist and Geist Mono (SIL Open Font License 1.1), Lucide icons (ISC licence), Unicode emoji data via the emojis crate.")
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
        nav = nav.child(div().px_2().pb_2().font_family(rhein_ui::theme::FONT_MONO).text_size(text::XS).text_color(t.text_faint).child("settings"));
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
            Section::Activity => self.activity_section(&t, cx),
            Section::Encryption => self.encryption_section(&t, cx),
            Section::Voice => self.voice_section(&t, cx),
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
