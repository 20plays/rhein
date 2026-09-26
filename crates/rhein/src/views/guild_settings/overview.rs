//! Overview: the guild's face, name, address and file lifetime, and for the owner the two ways
//! out (handing it on, deleting it). Every member may read it; changing it needs MANAGE_GUILD.

use gpui::{AnyElement, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _, PathPromptOptions, Styled as _, Window, div, prelude::*};
use rhein_store::{Client, perms};
use rhein_ui::{Button, Icon, Switch, Tabs, TextInput, Theme};

use super::{GuildSettingsView, field, guild_face, heading, hint, input, panel, when};
use crate::views::crop::{self, CropDialog, CropEvent};

/// How long files may be kept, as the web client offers it: `None` is "as long as the server
/// keeps them".
const EXPIRY: [(Option<i64>, &str); 7] = [
    (None, "Unlimited"),
    (Some(3_600), "1 hour"),
    (Some(21_600), "6 hours"),
    (Some(86_400), "1 day"),
    (Some(259_200), "3 days"),
    (Some(604_800), "1 week"),
    (Some(2_592_000), "30 days"),
];

pub(crate) struct State {
    name: Entity<TextInput>,
    alias_on: bool,
    alias: Entity<TextInput>,
    expiry: usize,
    transfer_to: Entity<TextInput>,
    confirm_transfer: bool,
    delete_name: Entity<TextInput>,
    icon_busy: bool,
}

impl State {
    pub(crate) fn new(client: &Entity<Client>, guild_id: &str, cx: &mut Context<GuildSettingsView>) -> Self {
        let mut state = Self {
            name: input("Guild name", "", cx),
            alias_on: false,
            alias: input("linuxcommunity", "", cx),
            expiry: 0,
            transfer_to: input("@member:example.org", "", cx),
            confirm_transfer: false,
            delete_name: input("Type the name to confirm", "", cx),
            icon_busy: false,
        };
        state.reset(client, guild_id, cx);
        state
    }

    /// Takes the guild's current values into the fields.
    pub(crate) fn reset(&mut self, client: &Entity<Client>, guild_id: &str, cx: &mut Context<GuildSettingsView>) {
        let Some(g) = client.read(cx).guild(guild_id).map(|e| e.guild.clone()) else { return };
        self.name.update(cx, |i, cx| i.set_text(g.name.clone(), cx));
        let local = alias_localpart(g.alias.as_deref());
        self.alias_on = g.alias.is_some();
        self.alias.update(cx, |i, cx| i.set_text(local, cx));
        self.expiry = EXPIRY.iter().position(|(secs, _)| *secs == g.media_expiry_limit_secs).unwrap_or(0);
        self.transfer_to.update(cx, |i, cx| i.clear(cx));
        self.delete_name.update(cx, |i, cx| i.clear(cx));
        self.confirm_transfer = false;
    }
}

/// `#name:server` → `name`.
fn alias_localpart(address: Option<&str>) -> String {
    address.map(|a| a.trim_start_matches('#').split(':').next().unwrap_or_default().to_owned()).unwrap_or_default()
}

impl GuildSettingsView {
    pub(crate) fn overview_page(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let client = self.client.read(cx);
        let Some(g) = client.guild(&self.guild_id).map(|e| e.guild.clone()) else { return div().into_any_element() };
        let icon_url = client.guild_icon_url(&self.guild_id);
        let manage = self.can(perms::MANAGE_GUILD, cx);
        let owner = self.is_owner(cx);
        let state = &self.overview;

        let members = match g.member_count {
            Some(1) => "1 member.".to_owned(),
            Some(n) => format!("{n} members."),
            None => "Member count unknown.".to_owned(),
        };
        let created = g.created_at.map_or("Created a while ago.".to_owned(), |at| format!("Created {}.", when(at)));

        // Face, name.
        let mut identity = panel(t).child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(guild_face(&g.name, icon_url.clone(), 64., t, window))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap_1p5()
                        .when(manage, |d| {
                            d.child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        Button::new("guild-icon-choose", if state.icon_busy { "Saving icon…" } else { "Choose icon" })
                                            .icon(Icon::Image)
                                            .disabled(state.icon_busy)
                                            .on_click(cx.listener(|v, _, _, cx| v.pick_icon(cx))),
                                    )
                                    .when(icon_url.is_some(), |d| {
                                        d.child(
                                            Button::new("guild-icon-remove", "Remove")
                                                .ghost()
                                                .disabled(state.icon_busy)
                                                .on_click(cx.listener(|v, _, _, cx| v.set_icon(String::new(), cx))),
                                        )
                                    }),
                            )
                        })
                        .child(hint(if icon_url.is_some() { "Custom icon set." } else { "No icon." }, t)),
                ),
        );
        identity = if manage {
            identity.child(field("Name", state.name.clone(), t)).child(
                div().flex().child(
                    Button::new("guild-name-save", "Save")
                        .primary()
                        .on_click(cx.listener(|v, _, _, cx| v.save_name(cx))),
                ),
            )
        } else {
            identity.child(field("Name", div().child(g.name.clone()), t))
        };
        identity = identity.child(hint(format!("{members} {created}"), t));

        let mut page = div().flex().flex_col().gap_5().child(identity);

        if manage {
            // The guild's own address.
            let server = self.client.read(cx).guild_server(&self.guild_id).unwrap_or_default().to_owned();
            let local = state.alias.read(cx).text().trim().trim_start_matches('#').to_lowercase();
            let address = (!local.is_empty()).then(|| format!("#{local}:{server}"));
            page = page.child(
                panel(t)
                    .child(heading("vanity link", t))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(div().flex_1().font_weight(FontWeight::MEDIUM).child("Enable vanity link"))
                            .child(Switch::new("guild-alias-on", state.alias_on).on_toggle({
                                let this = cx.weak_entity();
                                move |on, _, cx| {
                                    this.update(cx, |v, cx| {
                                        v.overview.alias_on = on;
                                        cx.notify();
                                    })
                                    .ok();
                                }
                            })),
                    )
                    .child(hint("Off by default. When enabled, anyone with the address can preview and join this guild. Invite links still work too.", t))
                    .when(state.alias_on, |d| {
                        d.child(field("Address name", state.alias.clone(), t))
                            .children(address.map(|a| hint(format!("Guild address: {a}"), t)))
                    })
                    .child(div().flex().child(Button::new("guild-alias-save", "Save").primary().on_click(cx.listener(|v, _, _, cx| v.save_alias(cx))))),
            );

            // How long files stay.
            let expiry = state.expiry;
            page = page.child(
                panel(t)
                    .child(heading("file expiration", t))
                    .child(hint("The longest lifetime an attachment may keep here. Channels may set a tighter cap.", t))
                    .child(field(
                        "Files may be kept for at most",
                        Tabs::new("guild-expiry", EXPIRY.iter().map(|(_, l)| *l)).selected(expiry).on_select({
                            let this = cx.weak_entity();
                            move |i, _, cx| {
                                this.update(cx, |v, cx| {
                                    v.overview.expiry = i;
                                    cx.notify();
                                })
                                .ok();
                            }
                        }),
                        t,
                    ))
                    .child(div().flex().child(Button::new("guild-expiry-save", "Save").primary().on_click(cx.listener(|v, _, _, cx| v.save_expiry(cx))))),
            );
        }

        if owner {
            let target = state.transfer_to.read(cx).text().trim().to_owned();
            let confirm = state.confirm_transfer;
            page = page.child(
                panel(t)
                    .child(heading("transfer ownership", t))
                    .child(hint("The new owner must already be a member. After the transfer only they can transfer or delete the guild.", t))
                    .child(field("New owner account id", state.transfer_to.clone(), t))
                    .child(
                        div().flex().items_center().gap_3().child(
                            Button::new("guild-transfer", if confirm { "Really transfer?" } else { "Transfer ownership" })
                                .kind(if confirm { rhein_ui::ButtonKind::Danger } else { rhein_ui::ButtonKind::Secondary })
                                .disabled(!target.starts_with('@') || !target.contains(':'))
                                .on_click(cx.listener(|v, _, _, cx| v.transfer(cx))),
                        )
                        .when(confirm, |d| d.child(hint(format!("{target} becomes the owner."), t))),
                    ),
            );
            let typed = state.delete_name.read(cx).text().trim() == g.name.trim();
            page = page.child(
                panel(t)
                    .border_color(t.danger)
                    .child(heading("deleting this guild", t))
                    .child(hint("Channels, messages, roles and members all go at once. Nothing holds a copy to restore it from.", t))
                    .child(field("Type the name to confirm", state.delete_name.clone(), t))
                    .child(
                        div().flex().child(
                            Button::new("guild-delete", "Delete guild")
                                .kind(rhein_ui::ButtonKind::Danger)
                                .icon(Icon::Trash)
                                .disabled(!typed)
                                .on_click(cx.listener(|v, _, _, cx| v.delete_guild(cx))),
                        ),
                    ),
            );
        }
        page.into_any_element()
    }

    fn save_name(&mut self, cx: &mut Context<Self>) {
        let name = self.overview.name.read(cx).text().trim().to_owned();
        if name.is_empty() {
            self.report(false, "A guild needs a name.", cx);
            return;
        }
        self.call(
            cx,
            move |api, gid| async move { api.patch_guild(&gid, &serde_json::json!({ "name": name })).await },
            |v, r, cx| {
                v.outcome(&r, "Saved.", cx);
            },
        );
    }

    fn save_alias(&mut self, cx: &mut Context<Self>) {
        let on = self.overview.alias_on;
        let alias = self.overview.alias.read(cx).text().trim().trim_start_matches('#').to_owned();
        if on && alias.is_empty() {
            self.report(false, "Give the address a name first.", cx);
            return;
        }
        self.call(
            cx,
            move |api, gid| async move { if on { api.set_guild_alias(&gid, &alias).await } else { api.clear_guild_alias(&gid).await } },
            move |v, r, cx| {
                if v.outcome(&r, if on { "Vanity link saved." } else { "Vanity link turned off." }, cx) {
                    let gid = v.guild_id.clone();
                    v.client.update(cx, |c, cx| c.refresh_guild(&gid, cx));
                }
            },
        );
    }

    fn save_expiry(&mut self, cx: &mut Context<Self>) {
        let secs = EXPIRY[self.overview.expiry].0;
        self.call(
            cx,
            move |api, gid| async move { api.patch_guild(&gid, &serde_json::json!({ "media_expiry_limit_secs": secs })).await },
            |v, r, cx| {
                if v.outcome(&r, "Saved.", cx) {
                    let gid = v.guild_id.clone();
                    v.client.update(cx, |c, cx| c.refresh_guild(&gid, cx));
                }
            },
        );
    }

    fn transfer(&mut self, cx: &mut Context<Self>) {
        if !self.overview.confirm_transfer {
            self.overview.confirm_transfer = true;
            cx.notify();
            return;
        }
        self.overview.confirm_transfer = false;
        let to = self.overview.transfer_to.read(cx).text().trim().to_owned();
        self.call(
            cx,
            move |api, gid| async move { api.transfer_ownership(&gid, &to).await },
            |v, r, cx| {
                if v.outcome(&r, "Ownership transferred.", cx) {
                    let gid = v.guild_id.clone();
                    v.client.update(cx, |c, cx| c.refresh_guild(&gid, cx));
                }
            },
        );
    }

    fn delete_guild(&mut self, cx: &mut Context<Self>) {
        self.call(
            cx,
            |api, gid| async move { api.delete_guild(&gid).await },
            |v, r, cx| {
                // The GUILD_DELETE that follows takes it off the list, and these settings with it.
                v.outcome(&r, "Guild deleted.", cx);
            },
        );
    }

    fn pick_icon(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose an icon".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("icon").to_owned();
            let prepared = cx
                .background_spawn(async move { std::fs::read(&path).map_err(anyhow::Error::from).and_then(|bytes| crop::prepare(bytes, &name)) })
                .await;
            this.update(cx, |v, cx| match prepared {
                Ok(prepared) => v.crop_icon(prepared, cx),
                Err(e) => v.report(false, format!("Couldn't open that picture: {e}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Opens the icon crop on a file, without the file picker (`RHEIN_DEV_OPEN=guild-icon=…`).
    #[cfg(debug_assertions)]
    pub fn dev_icon(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("icon").to_owned();
        match std::fs::read(&path).map_err(anyhow::Error::from).and_then(|bytes| crop::prepare(bytes, &name)) {
            Ok(prepared) => self.crop_icon(prepared, cx),
            Err(e) => log::warn!("dev icon: {e:#}"),
        }
    }

    fn crop_icon(&mut self, prepared: crop::Prepared, cx: &mut Context<Self>) {
        let dialog = cx.new(|cx| CropDialog::new(prepared, rhein_store::crop::ICON, cx));
        let sub = cx.subscribe(&dialog, |v, _, event: &CropEvent, cx| {
            v.crop = None;
            if let CropEvent::Done(encoded) = event {
                v.upload_icon(encoded.clone(), cx);
            }
            cx.notify();
        });
        self.show_crop(dialog, sub, cx);
    }

    fn upload_icon(&mut self, encoded: rhein_store::crop::Encoded, cx: &mut Context<Self>) {
        let Some(origin) = self.client.read(cx).guild_server(&self.guild_id).map(str::to_owned) else { return };
        self.overview.icon_busy = true;
        self.report(true, "Uploading…", cx);
        self.call(
            cx,
            move |api, gid| async move { api.upload_guild_media(&gid, &origin, &encoded.filename, encoded.mime, encoded.bytes).await },
            |v, r, cx| match r {
                Ok(media) => {
                    let reference = format!("{}?cap={}", media.hash, media.capability.unwrap_or_default());
                    v.set_icon(reference, cx);
                }
                Err(e) => {
                    v.overview.icon_busy = false;
                    v.report(false, format!("Could not save icon: {e}"), cx);
                }
            },
        );
    }

    /// Points the guild at an uploaded icon, or at none (`""`).
    fn set_icon(&mut self, reference: String, cx: &mut Context<Self>) {
        self.overview.icon_busy = true;
        let removing = reference.is_empty();
        self.call(
            cx,
            move |api, gid| async move { api.patch_guild(&gid, &serde_json::json!({ "icon_ref": reference })).await },
            move |v, r, cx| {
                v.overview.icon_busy = false;
                if v.outcome(&r, if removing { "Icon removed." } else { "Icon saved." }, cx) {
                    let gid = v.guild_id.clone();
                    v.client.update(cx, |c, cx| c.refresh_guild(&gid, cx));
                }
            },
        );
    }
}
