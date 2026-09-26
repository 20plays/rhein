//! Bots: the applications installed in the guild, what each may do here, adding one by its
//! application id and origin (after a preview of what it asks for), and removing one.

use gpui::{AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _, div, prelude::*, px};
use rhein_store::perms;
use rhein_ui::theme::text;
use rhein_ui::{Avatar, Button, ButtonKind, Tag, TextInput, Theme};
use serde_json::{Value, json};

use super::lists::Load;
use super::{GuildSettingsView, field, heading, hint, input, panel, when};

pub(crate) struct State {
    bots: Load<Vec<Value>>,
    app_id: Entity<TextInput>,
    origin: Entity<TextInput>,
    preview: Option<Value>,
    /// The bot whose permissions are open, and the permission names chosen for it.
    editing: Option<(String, Vec<String>)>,
    confirm_remove: Option<String>,
}

impl State {
    pub(crate) fn new(cx: &mut Context<GuildSettingsView>) -> Self {
        Self {
            bots: Load::Loading,
            app_id: input("Application id", "", cx),
            origin: input("bots.example.org", "", cx),
            preview: None,
            editing: None,
            confirm_remove: None,
        }
    }
}

fn names(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

impl GuildSettingsView {
    pub(crate) fn load_bots(&mut self, cx: &mut Context<Self>) {
        self.call(cx, |api, gid| async move { api.guild_bots(&gid).await }, |v, r, _| {
            v.bots.bots = match r {
                Ok(b) => Load::Ready(b["bots"].as_array().cloned().unwrap_or_default()),
                Err(e) => Load::Failed(e),
            }
        });
    }

    fn preview_bot(&mut self, cx: &mut Context<Self>) {
        let app = self.bots.app_id.read(cx).text().trim().to_owned();
        let origin = self.bots.origin.read(cx).text().trim().to_owned();
        if app.is_empty() || origin.is_empty() {
            self.report(false, "A bot is found by its application id and the server it lives on.", cx);
            return;
        }
        self.call(
            cx,
            move |api, gid| async move { api.preview_bot(&gid, &json!({ "application_id": app, "application_origin": origin })).await },
            |v, r, cx| match r {
                Ok(p) => {
                    v.bots.preview = Some(p);
                    v.status = None;
                    cx.notify();
                }
                Err(e) => v.report(false, e, cx),
            },
        );
    }

    fn install_bot(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.bots.preview.clone() else { return };
        let body = json!({
            "application_id": p["application_id"],
            "application_origin": p["application_origin"],
            "permissions": names(&p["default_permission_names"]),
            "intents": names(&p["allowed_intent_names"]),
        });
        self.call(
            cx,
            move |api, gid| async move { api.install_bot(&gid, &body).await },
            |v, r, cx| {
                if v.outcome(&r, "Bot added.", cx) {
                    v.bots.preview = None;
                    v.bots.app_id.update(cx, |i, cx| i.clear(cx));
                    v.bots.origin.update(cx, |i, cx| i.clear(cx));
                }
                v.load_bots(cx);
            },
        );
    }

    fn save_bot(&mut self, cx: &mut Context<Self>) {
        let Some((bot, chosen)) = self.bots.editing.clone() else { return };
        self.call(
            cx,
            move |api, gid| async move { api.patch_bot(&gid, &bot, &json!({ "permissions": chosen })).await },
            |v, r, cx| {
                if v.outcome(&r, "Bot permissions saved.", cx) {
                    v.bots.editing = None;
                }
                v.load_bots(cx);
            },
        );
    }

    fn remove_bot(&mut self, bot: String, cx: &mut Context<Self>) {
        if self.bots.confirm_remove.as_deref() != Some(bot.as_str()) {
            self.bots.confirm_remove = Some(bot);
            cx.notify();
            return;
        }
        self.bots.confirm_remove = None;
        self.call(
            cx,
            move |api, gid| async move { api.uninstall_bot(&gid, &bot).await },
            |v, r, cx| {
                v.outcome(&r, "Bot removed.", cx);
                v.load_bots(cx);
            },
        );
    }

    pub(crate) fn bots_page(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut add = panel(t)
            .child(heading("add a bot", t))
            .child(hint("A bot is an application on some server. It joins with the permissions you grant it here, never more than you hold.", t))
            .child(field("Application id", self.bots.app_id.clone(), t))
            .child(field("Its server", self.bots.origin.clone(), t))
            .child(div().flex().child(Button::new("bot-preview", "Look it up").on_click(cx.listener(|v, _, _, cx| v.preview_bot(cx)))));
        if let Some(p) = self.bots.preview.clone() {
            let enabled = p["install_enabled"].as_bool().unwrap_or(false) && !p["retired"].as_bool().unwrap_or(false);
            let wants = names(&p["default_permission_names"]);
            add = add.child(
                panel(t)
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(p["name"].as_str().unwrap_or("A bot").to_owned()))
                    .children(p["description"].as_str().filter(|d| !d.is_empty()).map(|d| hint(d.to_owned(), t)))
                    .child(hint(if wants.is_empty() { "Asks for no permissions.".to_owned() } else { format!("Asks for: {}", label_list(&wants)) }, t))
                    .child(if enabled {
                        div().flex().child(Button::new("bot-install", "Add to this guild").primary().on_click(cx.listener(|v, _, _, cx| v.install_bot(cx)))).into_any_element()
                    } else {
                        hint("Its owner doesn't let it be added right now.", t).into_any_element()
                    }),
            );
        }
        let mut list = div().flex().flex_col().gap_3().child(heading("installed", t));
        match &self.bots.bots {
            Load::Loading => list = list.child(hint("Loading…", t)),
            Load::Failed(e) => list = list.child(hint(format!("Couldn't load the bots: {e}"), t)),
            Load::Ready(bots) if bots.is_empty() => list = list.child(hint("No bots here yet.", t)),
            Load::Ready(bots) => {
                for b in bots.clone() {
                    let id = b["bot_user_id"].as_str().unwrap_or_default().to_owned();
                    let name = b["display_name"].as_str().unwrap_or(&id).to_owned();
                    let granted = names(&b["permission_names"]);
                    let editing = self.bots.editing.as_ref().filter(|(e, _)| *e == id).map(|(_, c)| c.clone());
                    let (e_id, r_id) = (id.clone(), id.clone());
                    let origin = b["application_origin"].as_str().unwrap_or_default().to_owned();
                    let since = b["installed_at"].as_i64().map(|at| format!(" · since {}", when(at))).unwrap_or_default();
                    let mut card = panel(t).child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(Avatar::new(id.clone(), name.clone()).size(px(36.)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(div().flex().items_center().gap_2().child(div().font_weight(FontWeight::SEMIBOLD).child(name)).child(Tag("BOT")))
                                    .child(hint(format!("{origin}{since}"), t)),
                            )
                            .child(Button::new(SharedString::from(format!("bot-edit-{id}")), "Permissions").small().ghost().on_click(cx.listener(move |v, _, _, cx| {
                                let open = v.bots.editing.as_ref().is_some_and(|(e, _)| *e == e_id);
                                v.bots.editing = if open { None } else { Some((e_id.clone(), granted_of(v, &e_id))) };
                                cx.notify();
                            })))
                            .child(
                                Button::new(SharedString::from(format!("bot-remove-{id}")), if self.bots.confirm_remove.as_deref() == Some(id.as_str()) { "Really remove?" } else { "Remove" })
                                    .small()
                                    .kind(ButtonKind::Danger)
                                    .on_click(cx.listener(move |v, _, _, cx| v.remove_bot(r_id.clone(), cx))),
                            ),
                    );
                    card = card.child(hint(if granted.is_empty() { "May do nothing yet.".to_owned() } else { format!("May: {}", label_list(&granted)) }, t));
                    if let Some(chosen) = editing {
                        let mine = self.me_bits(cx);
                        let mut grid = div().flex().flex_wrap().gap_2();
                        for info in perms::ALL {
                            let on = chosen.iter().any(|n| n == info.name);
                            let held = mine & info.bit != 0;
                            let name = info.name;
                            grid = grid.child(
                                div()
                                    .id(SharedString::from(format!("bot-perm-{id}-{name}")))
                                    .px_2()
                                    .py_1()
                                    .rounded(rhein_ui::theme::radius::MD)
                                    .border_1()
                                    .border_color(if on { t.text_muted } else { t.border })
                                    .when(on, |d| d.bg(t.bg_selected))
                                    .text_size(text::SM)
                                    .when(!held, |d| d.opacity(0.45))
                                    .when(held, |d| {
                                        d.cursor_pointer().on_click(cx.listener(move |v, _, _, cx| {
                                            if let Some((_, chosen)) = v.bots.editing.as_mut() {
                                                if let Some(i) = chosen.iter().position(|n| n == name) {
                                                    chosen.remove(i);
                                                } else {
                                                    chosen.push(name.to_owned());
                                                }
                                            }
                                            cx.notify();
                                        }))
                                    })
                                    .child(info.label),
                            );
                        }
                        card = card.child(grid).child(div().flex().child(Button::new(SharedString::from(format!("bot-save-{id}")), "Save").primary().on_click(cx.listener(|v, _, _, cx| v.save_bot(cx)))));
                    }
                    list = list.child(card);
                }
            }
        }
        div().flex().flex_col().gap_5().child(list).child(add).into_any_element()
    }

    /// The permissions this account holds in the guild.
    fn me_bits(&self, cx: &gpui::App) -> u64 {
        let client = self.client.read(cx);
        if client.is_owner(&self.guild_id) {
            return u64::MAX;
        }
        let bits = client.guild(&self.guild_id).and_then(|g| g.guild.me.as_ref()).and_then(|m| m["permissions"].as_str()).map(perms::parse_bits).unwrap_or(0);
        if bits & perms::ADMINISTRATOR != 0 { u64::MAX } else { bits }
    }
}

/// The permissions a bot has now, from the loaded list.
fn granted_of(v: &GuildSettingsView, bot: &str) -> Vec<String> {
    match &v.bots.bots {
        Load::Ready(list) => list.iter().find(|b| b["bot_user_id"].as_str() == Some(bot)).map(|b| names(&b["permission_names"])).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Permission names as their labels.
fn label_list(names: &[String]) -> String {
    names.iter().map(|n| perms::ALL.iter().find(|i| i.name == n).map_or(n.as_str(), |i| i.label)).collect::<Vec<_>>().join(", ")
}
