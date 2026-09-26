//! Channel permissions: for one channel and one role (or member), each permission allowed,
//! denied or left to the roles, as the web client sets them. Channels come from the guild the
//! client already holds; CHANNEL_UPDATE keeps their overwrites current.

use std::collections::HashMap;

use gpui::{AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _, div, prelude::*, px};
use rhein_store::perms;
use rhein_ui::theme::{radius, text};
use rhein_ui::{Button, Icon, TextInput, Theme, icon_sized};

use super::{GuildSettingsView, heading, hint, input, panel};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Inherit,
    Allow,
    Deny,
}

pub(crate) struct State {
    channel: Option<String>,
    /// `(target_type, target_id)`.
    target: Option<(String, String)>,
    draft: HashMap<u64, Choice>,
    member: Entity<TextInput>,
    saving: bool,
}

impl State {
    pub(crate) fn new(cx: &mut Context<GuildSettingsView>) -> Self {
        Self { channel: None, target: None, draft: HashMap::new(), member: input("@member:example.org", "", cx), saving: false }
    }
}

impl GuildSettingsView {
    /// Opens a channel's overwrite for a target, as the server has it.
    fn pick_overwrite(&mut self, channel: String, target: (String, String), cx: &mut Context<Self>) {
        let existing = self
            .client
            .read(cx)
            .guild(&self.guild_id)
            .and_then(|g| g.guild.channels.iter().find(|c| c.id == channel).cloned())
            .and_then(|c| c.permission_overwrites.into_iter().find(|o| o.target_type == target.0 && o.target_id == target.1));
        let (allow, deny) = existing.map_or((0, 0), |o| (perms::parse_bits(&o.allow), perms::parse_bits(&o.deny)));
        self.channel_perms.draft = perms::ALL
            .iter()
            .map(|i| (i.bit, if allow & i.bit != 0 { Choice::Allow } else if deny & i.bit != 0 { Choice::Deny } else { Choice::Inherit }))
            .collect();
        self.channel_perms.channel = Some(channel);
        self.channel_perms.target = Some(target);
        cx.notify();
    }

    fn save_overwrite(&mut self, cx: &mut Context<Self>) {
        let (Some(channel), Some((kind, id))) = (self.channel_perms.channel.clone(), self.channel_perms.target.clone()) else { return };
        let (mut allow, mut deny) = (0u64, 0u64);
        for (bit, choice) in &self.channel_perms.draft {
            match choice {
                Choice::Allow => allow |= bit,
                Choice::Deny => deny |= bit,
                Choice::Inherit => {}
            }
        }
        self.channel_perms.saving = true;
        self.call(
            cx,
            move |api, _| async move { api.set_overwrite(&channel, &kind, &id, allow, deny).await },
            |v, r, cx| {
                v.channel_perms.saving = false;
                v.outcome(&r, "Overwrite saved.", cx);
            },
        );
    }

    pub(crate) fn channels_page(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let client = self.client.read(cx);
        let Some(entry) = client.guild(&self.guild_id) else { return div().into_any_element() };
        let channels: Vec<_> = entry.ordered_channels().into_iter().cloned().collect();
        let mut roles = entry.guild.roles.clone();
        roles.sort_by(|a, b| b.position.cmp(&a.position));
        let names: HashMap<String, String> = entry.guild.roles.iter().map(|r| (r.id.clone(), r.name.clone())).collect();
        let state = &self.channel_perms;

        // The channels, under their categories.
        let mut list = div().w(px(240.)).flex_none().flex().flex_col().gap_0p5();
        for ch in &channels {
            if ch.is_category() {
                list = list.child(heading_owned(ch.name.to_lowercase(), t));
                continue;
            }
            let id = ch.id.clone();
            let active = state.channel.as_deref() == Some(ch.id.as_str());
            let everyone = (String::from("role"), self.guild_id.clone());
            list = list.child(
                div()
                    .id(SharedString::from(format!("perm-chan-{id}")))
                    .px_2()
                    .h(px(30.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(radius::MD)
                    .cursor_pointer()
                    .when(active, |d| d.bg(t.bg_selected))
                    .hover(|s| s.bg(t.bg_hover))
                    .on_click(cx.listener(move |v, _, _, cx| v.pick_overwrite(id.clone(), everyone.clone(), cx)))
                    .child(icon_sized(if ch.is_voice() { Icon::Volume } else { Icon::Hash }, px(14.)).text_color(t.text_faint))
                    .child(div().truncate().child(ch.name.clone()))
                    .when(!ch.permission_overwrites.is_empty(), |d| {
                        d.child(div().flex_1()).child(div().text_size(text::XS).text_color(t.text_faint).child(ch.permission_overwrites.len().to_string()))
                    }),
            );
        }

        let Some(channel) = state.channel.as_deref().and_then(|id| channels.iter().find(|c| c.id == id)).cloned() else {
            return div()
                .flex()
                .items_start()
                .gap_6()
                .child(list)
                .child(hint("Pick a channel. Its overwrites allow or deny permissions for a role or a member here, over what their roles give.", t))
                .into_any_element();
        };

        // Who has an overwrite here, and the roles to add one for.
        let mut targets = div().flex().flex_wrap().gap_2();
        let mut seen: Vec<(String, String)> = vec![("role".into(), self.guild_id.clone())];
        seen.extend(channel.permission_overwrites.iter().map(|o| (o.target_type.clone(), o.target_id.clone())));
        for r in &roles {
            let key = ("role".to_owned(), r.id.clone());
            if !seen.contains(&key) {
                seen.push(key);
            }
        }
        for (kind, id) in seen {
            let label = if kind == "member" {
                id.clone()
            } else if id == self.guild_id {
                "@everyone".to_owned()
            } else {
                names.get(&id).cloned().unwrap_or_else(|| "unknown role".into())
            };
            let has = channel.permission_overwrites.iter().any(|o| o.target_type == kind && o.target_id == id);
            let active = state.target.as_ref() == Some(&(kind.clone(), id.clone()));
            let (chan, key) = (channel.id.clone(), (kind.clone(), id.clone()));
            targets = targets.child(
                div()
                    .id(SharedString::from(format!("perm-target-{kind}-{id}")))
                    .px_2p5()
                    .py_1()
                    .rounded(radius::MD)
                    .border_1()
                    .border_color(if active { t.text_muted } else { t.border })
                    .when(active, |d| d.bg(t.bg_selected))
                    .when(!has, |d| d.text_color(t.text_faint))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.bg_hover))
                    .text_size(text::SM)
                    .on_click(cx.listener(move |v, _, _, cx| v.pick_overwrite(chan.clone(), key.clone(), cx)))
                    .child(label),
            );
        }
        let add_member = {
            let chan = channel.id.clone();
            div()
                .flex()
                .gap_2()
                .child(div().w(px(260.)).child(state.member.clone()))
                .child(Button::new("perm-add-member", "Add member").small().on_click(cx.listener(move |v, _, _, cx| {
                    let id = v.channel_perms.member.read(cx).text().trim().to_owned();
                    if id.starts_with('@') && id.contains(':') {
                        v.channel_perms.member.update(cx, |i, cx| i.clear(cx));
                        v.pick_overwrite(chan.clone(), ("member".into(), id), cx);
                    } else {
                        v.report(false, "Use the member's full address, like @name:server.", cx);
                    }
                })))
        };

        let mut editor = panel(t)
            .child(div().text_size(text::LG).font_weight(FontWeight::SEMIBOLD).child(format!("#{}", channel.name)))
            .child(heading("for", t))
            .child(targets)
            .child(add_member);

        if state.target.is_some() {
            editor = editor.child(hint("Allowed or denied here overrides the roles; inherit leaves it to them.", t));
            for info in perms::ALL.iter().filter(|i| i.bit != perms::ADMINISTRATOR) {
                let now = state.draft.get(&info.bit).copied().unwrap_or(Choice::Inherit);
                let bit = info.bit;
                let button = |choice: Choice, label: &'static str, cx: &mut Context<Self>| {
                    let on = now == choice;
                    let colour = match choice {
                        Choice::Allow => t.success,
                        Choice::Deny => t.danger,
                        Choice::Inherit => t.text_muted,
                    };
                    div()
                        .id(SharedString::from(format!("perm-{bit}-{label}")))
                        .px_2()
                        .h(px(24.))
                        .flex()
                        .items_center()
                        .rounded(radius::SM)
                        .text_size(text::XS)
                        .cursor_pointer()
                        .when(on, |d| d.bg(t.bg_selected).text_color(colour).font_weight(FontWeight::SEMIBOLD))
                        .when(!on, |d| d.text_color(t.text_faint).hover(|s| s.text_color(t.text)))
                        .on_click(cx.listener(move |v, _, _, cx| {
                            v.channel_perms.draft.insert(bit, choice);
                            cx.notify();
                        }))
                        .child(label)
                };
                editor = editor.child(
                    div()
                        .py_1p5()
                        .flex()
                        .items_center()
                        .gap_3()
                        .border_b_1()
                        .border_color(t.border)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().child(info.label))
                                .child(div().text_size(text::XS).text_color(t.text_faint).child(info.hint)),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_0p5()
                                .p_0p5()
                                .rounded(radius::MD)
                                .border_1()
                                .border_color(t.border)
                                .child(button(Choice::Deny, "Deny", cx))
                                .child(button(Choice::Inherit, "Inherit", cx))
                                .child(button(Choice::Allow, "Allow", cx)),
                        ),
                );
            }
            editor = editor.child(
                div().flex().child(
                    Button::new("perm-save", if state.saving { "Saving…" } else { "Save overwrite" })
                        .primary()
                        .disabled(state.saving)
                        .on_click(cx.listener(|v, _, _, cx| v.save_overwrite(cx))),
                ),
            );
        }

        div().flex().items_start().gap_6().child(list).child(div().flex_1().min_w_0().child(editor)).into_any_element()
    }
}

fn heading_owned(title: String, t: &Theme) -> gpui::Div {
    heading("", t).child(title)
}
