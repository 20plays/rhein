//! Roles: the list by rank, and an editor for one role (display, permissions, members), as in
//! the web client. A role at or above your own top role, or one a bot holds, is read-only; a
//! permission you do not hold yourself cannot be handed out.

use gpui::{AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _, div, prelude::*, px, rgb};
use rhein_proto::{Member, Role};
use rhein_store::perms;
use rhein_ui::theme::{radius, text};
use rhein_ui::{Button, ButtonKind, Icon, IconButton, InputEvent, Switch, Tabs, TextInput, Theme};

use super::{GuildSettingsView, field, heading, hint, input, panel};
use crate::views::profile::ACCENTS;

#[derive(Clone, PartialEq)]
struct Draft {
    colour: Option<u32>,
    hoist: bool,
    mentionable: bool,
    perms: u64,
}

pub(crate) struct State {
    roles: Option<Result<Vec<Role>, String>>,
    selected: Option<String>,
    tab: usize,
    draft: Option<Draft>,
    name: Entity<TextInput>,
    hex: Entity<TextInput>,
    search: Entity<TextInput>,
    holders: Option<Vec<Member>>,
    found: Vec<Member>,
    confirm_delete: bool,
    saving: bool,
}

impl State {
    pub(crate) fn new(cx: &mut Context<GuildSettingsView>) -> Self {
        Self {
            roles: None,
            selected: None,
            tab: 0,
            draft: None,
            name: input("Role name", "", cx),
            hex: input("#rrggbb", "", cx),
            search: input("Find a member to add", "", cx),
            holders: None,
            found: Vec::new(),
            confirm_delete: false,
            saving: false,
        }
    }
}

/// Who I am in this guild: owner, the position of my top role, my permissions.
struct Me {
    owner: bool,
    top: i64,
    bits: u64,
}

impl GuildSettingsView {
    fn me(&self, cx: &gpui::App) -> Me {
        let client = self.client.read(cx);
        let owner = client.is_owner(&self.guild_id);
        let me = client.guild(&self.guild_id).and_then(|g| g.guild.me.clone()).unwrap_or_default();
        let top = me.get("top_role_position").and_then(|v| v.as_i64()).unwrap_or(0);
        let bits = me.get("permissions").and_then(|v| v.as_str()).map(perms::parse_bits).unwrap_or(0);
        Me { owner, top, bits: if owner { u64::MAX } else { bits } }
    }

    /// Watches the member search box.
    pub(crate) fn roles_subscriptions(&mut self, cx: &mut Context<Self>) {
        let search = self.roles.search.clone();
        let sub = cx.subscribe(&search, |v, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Changed) {
                let q = input.read(cx).text().trim().to_owned();
                v.search_members(q, cx);
            }
        });
        self.subs.push(sub);
    }

    pub(crate) fn load_roles(&mut self, cx: &mut Context<Self>) {
        self.call(
            cx,
            |api, gid| async move { api.roles(&gid).await },
            |v, r, cx| {
                let roles = r.map(|l| l.roles);
                v.roles.roles = Some(roles);
                // Keep the open role open, with what the server has now.
                if let Some(id) = v.roles.selected.clone() {
                    v.select_role(id, cx);
                }
            },
        );
    }

    fn ranked(&self) -> Vec<Role> {
        let mut list: Vec<Role> = match &self.roles.roles {
            Some(Ok(roles)) => roles.iter().filter(|r| r.id != self.guild_id).cloned().collect(),
            _ => Vec::new(),
        };
        list.sort_by(|a, b| b.position.cmp(&a.position).then(a.id.cmp(&b.id)));
        list
    }

    fn role(&self, id: &str) -> Option<Role> {
        match &self.roles.roles {
            Some(Ok(roles)) => roles.iter().find(|r| r.id == id).cloned(),
            _ => None,
        }
    }

    fn select_role(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(role) = self.role(&id) else { return };
        let changed = self.roles.selected.as_deref() != Some(id.as_str());
        self.roles.draft = Some(Draft { colour: role.rgb(), hoist: role.hoist, mentionable: role.mentionable, perms: role.bits() });
        self.roles.name.update(cx, |i, cx| i.set_text(role.name.clone(), cx));
        let hex = role.rgb().map(|c| format!("#{c:06x}")).unwrap_or_default();
        self.roles.hex.update(cx, |i, cx| i.set_text(hex, cx));
        self.roles.selected = Some(id.clone());
        self.roles.confirm_delete = false;
        if changed {
            self.roles.tab = if id == self.guild_id { 1 } else { 0 };
            self.roles.holders = None;
            self.roles.found.clear();
            self.roles.search.update(cx, |i, cx| i.clear(cx));
        }
        if id != self.guild_id {
            self.call(
                cx,
                move |api, gid| async move { api.role_members(&gid, &id, 100).await },
                |v, r, _| v.roles.holders = r.ok().map(|l| l.members),
            );
        }
        cx.notify();
    }

    fn search_members(&mut self, q: String, cx: &mut Context<Self>) {
        if q.is_empty() {
            self.roles.found.clear();
            cx.notify();
            return;
        }
        self.call(
            cx,
            move |api, gid| async move { api.search_members(&gid, &q, 10).await },
            |v, r, _| v.roles.found = r.map(|l| l.members).unwrap_or_default(),
        );
    }

    fn create_role(&mut self, cx: &mut Context<Self>) {
        self.call(
            cx,
            |api, gid| async move { api.create_role(&gid, &serde_json::json!({ "name": "new role" })).await },
            |v, r, cx| {
                if v.outcome(&r, "Role created.", cx) {
                    if let Ok(created) = &r
                        && let Some(id) = created.get("id").and_then(|i| i.as_str())
                    {
                        v.roles.selected = Some(id.to_owned());
                    }
                    v.load_roles(cx);
                }
            },
        );
    }

    fn save_role(&mut self, cx: &mut Context<Self>) {
        let (Some(id), Some(draft)) = (self.roles.selected.clone(), self.roles.draft.clone()) else { return };
        let everyone = id == self.guild_id;
        let names: Vec<&str> = perms::ALL.iter().filter(|i| draft.perms & i.bit != 0).map(|i| i.name).collect();
        let mut body = serde_json::json!({ "permissions": names });
        if !everyone {
            let name = self.roles.name.read(cx).text().trim().to_owned();
            if name.is_empty() {
                self.report(false, "A role needs a name.", cx);
                return;
            }
            let hex = self.roles.hex.read(cx).text().trim().trim_start_matches('#').to_owned();
            let colour = u32::from_str_radix(&hex, 16).ok().filter(|_| hex.len() == 6).or(draft.colour);
            body["name"] = name.into();
            body["color"] = colour.unwrap_or(0).into();
            body["hoist"] = draft.hoist.into();
            body["mentionable"] = draft.mentionable.into();
        }
        self.roles.saving = true;
        self.call(
            cx,
            move |api, gid| async move { api.patch_role(&gid, &id, &body).await },
            |v, r, cx| {
                v.roles.saving = false;
                if v.outcome(&r, "Role saved.", cx) {
                    v.load_roles(cx);
                }
            },
        );
    }

    fn delete_role(&mut self, cx: &mut Context<Self>) {
        if !self.roles.confirm_delete {
            self.roles.confirm_delete = true;
            cx.notify();
            return;
        }
        let Some(id) = self.roles.selected.clone() else { return };
        self.call(
            cx,
            move |api, gid| async move { api.delete_role(&gid, &id).await },
            |v, r, cx| {
                if v.outcome(&r, "Role deleted.", cx) {
                    v.roles.selected = None;
                    v.roles.draft = None;
                    v.load_roles(cx);
                }
            },
        );
    }

    /// Moves a role one place up (`-1`) or down (`1`) in the ranking.
    fn move_role(&mut self, id: String, by: i64, cx: &mut Context<Self>) {
        let mut list = self.ranked();
        let Some(at) = list.iter().position(|r| r.id == id) else { return };
        let to = at as i64 + by;
        if to < 0 || to >= list.len() as i64 {
            return;
        }
        let role = list.remove(at);
        list.insert(to as usize, role);
        let n = list.len() as i64;
        let moves: Vec<(String, i64)> =
            list.iter().enumerate().filter(|(i, r)| r.position != n - *i as i64).map(|(i, r)| (r.id.clone(), n - i as i64)).collect();
        self.call(
            cx,
            move |api, gid| async move { api.reorder_roles(&gid, &moves).await },
            |v, r, cx| {
                if v.outcome(&r, "Order saved.", cx) {
                    v.load_roles(cx);
                }
            },
        );
    }

    fn assign(&mut self, user_id: String, add: bool, cx: &mut Context<Self>) {
        let Some(id) = self.roles.selected.clone() else { return };
        let body = if add { serde_json::json!({ "add_roles": [id] }) } else { serde_json::json!({ "remove_roles": [id] }) };
        self.call(
            cx,
            move |api, gid| async move { api.patch_member(&gid, &user_id, &body).await },
            move |v, r, cx| {
                if v.outcome(&r, if add { "Role given." } else { "Role taken away." }, cx) {
                    v.roles.search.update(cx, |i, cx| i.clear(cx));
                    v.roles.found.clear();
                    // The counts beside the names, and this role's holders, as they are now.
                    v.load_roles(cx);
                }
            },
        );
    }

    pub(crate) fn roles_page(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let roles = match &self.roles.roles {
            None => return hint("Loading…", t).into_any_element(),
            Some(Err(e)) => return hint(format!("Couldn't load the roles: {e}"), t).into_any_element(),
            Some(Ok(roles)) => roles.clone(),
        };
        let me = self.me(cx);
        let ranked = self.ranked();
        let everyone = roles.iter().find(|r| r.id == self.guild_id).cloned();
        let selected = self.roles.selected.clone();

        let row = |role: &Role, i: usize, count: usize, cx: &mut Context<Self>| {
            let id = role.id.clone();
            let active = selected.as_deref() == Some(role.id.as_str());
            let movable = me.owner || me.top > role.position;
            let colour: gpui::Hsla = role.rgb().map(|c| rgb(c).into()).unwrap_or(t.text_faint.into());
            let (up, down) = (id.clone(), id.clone());
            div()
                .id(SharedString::from(format!("role-{id}")))
                .group("role-row")
                .px_2()
                .h(px(36.))
                .flex()
                .items_center()
                .gap_2()
                .rounded(radius::MD)
                .cursor_pointer()
                .when(active, |d| d.bg(t.bg_selected))
                .hover(|s| s.bg(t.bg_hover))
                .on_click(cx.listener(move |v, _, _, cx| v.select_role(id.clone(), cx)))
                .child(div().size(px(10.)).flex_none().rounded_full().bg(colour))
                .child(div().flex_1().min_w_0().truncate().child(role.name.clone()))
                .when(role.managed, |d| d.child(rhein_ui::Tag("BOT")))
                .child(div().text_size(text::XS).text_color(t.text_faint).child(role.member_count.unwrap_or(0).to_string()))
                .when(movable, |d| {
                    d.child(
                        IconButton::new(SharedString::from(format!("role-up-{up}")), Icon::ChevronUp)
                            .tooltip("Move up")
                            .on_click(cx.listener(move |v, _, _, cx| if i > 0 { v.move_role(up.clone(), -1, cx) })),
                    )
                    .child(
                        IconButton::new(SharedString::from(format!("role-down-{down}")), Icon::ChevronDown)
                            .tooltip("Move down")
                            .on_click(cx.listener(move |v, _, _, cx| if i + 1 < count { v.move_role(down.clone(), 1, cx) })),
                    )
                })
                .into_any_element()
        };

        let mut list = div().w(px(280.)).flex_none().flex().flex_col().gap_1();
        if let Some(e) = &everyone {
            let id = e.id.clone();
            let active = selected.as_deref() == Some(e.id.as_str());
            list = list.child(heading("default", t)).child(
                div()
                    .id("role-everyone")
                    .p_3()
                    .rounded(radius::MD)
                    .border_1()
                    .border_color(t.border)
                    .cursor_pointer()
                    .when(active, |d| d.bg(t.bg_selected))
                    .hover(|s| s.bg(t.bg_hover))
                    .on_click(cx.listener(move |v, _, _, cx| v.select_role(id.clone(), cx)))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("@everyone"))
                    .child(hint(format!("{} members · what everyone may do", e.member_count.unwrap_or(0)), t)),
            );
        }
        list = list.child(
            div()
                .pt_3()
                .flex()
                .items_center()
                .child(div().flex_1().child(heading("roles", t)))
                .child(Button::new("role-create", "Create role").small().icon(Icon::Plus).on_click(cx.listener(|v, _, _, cx| v.create_role(cx)))),
        );
        let count = ranked.len();
        for (i, role) in ranked.iter().enumerate() {
            list = list.child(row(role, i, count, cx));
        }
        if ranked.is_empty() {
            list = list.child(hint("No roles yet.", t));
        }

        let editor = match selected.as_deref().and_then(|id| self.role(id)) {
            Some(role) => self.role_editor(&role, &me, t, cx),
            None => hint("Pick a role to change it.", t).into_any_element(),
        };
        div().flex().items_start().gap_6().child(list).child(div().flex_1().min_w_0().child(editor)).into_any_element()
    }

    fn role_editor(&mut self, role: &Role, me: &Me, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(draft) = self.roles.draft.clone() else { return div().into_any_element() };
        let everyone = role.id == self.guild_id;
        let editable = !role.managed && (everyone && me.bits & perms::MANAGE_ROLES != 0 || me.owner || me.top > role.position);
        let tab = self.roles.tab;
        let this = cx.weak_entity();

        let mut panel_ = panel(t).child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().text_size(text::LG).font_weight(FontWeight::SEMIBOLD).child(if everyone { "@everyone".to_owned() } else { role.name.clone() })),
        );
        if role.managed {
            panel_ = panel_.child(hint("A bot holds this role for itself; it changes with the bot's permissions.", t));
        } else if !editable {
            panel_ = panel_.child(hint("This role is at or above your own, so you can only look.", t));
        }
        if everyone {
            panel_ = panel_.child(hint("Every member has these permissions. Roles add to them; channel overwrites can take them away.", t));
        } else {
            panel_ = panel_.child(Tabs::new("role-tabs", ["Display", "Permissions", "Members"]).selected(tab).on_select({
                let this = this.clone();
                move |i, _, cx| {
                    this.update(cx, |v, cx| {
                        v.roles.tab = i;
                        cx.notify();
                    })
                    .ok();
                }
            }));
        }

        match (everyone, tab) {
            (false, 0) => {
                let mut swatches = div().flex().flex_wrap().gap_2().child(swatch(None, draft.colour.is_none(), editable, &this, t));
                for (_, c) in ACCENTS {
                    swatches = swatches.child(swatch(Some(c), draft.colour == Some(c), editable, &this, t));
                }
                let (hoist, mentionable) = (draft.hoist, draft.mentionable);
                panel_ = panel_
                    .child(field("Name", self.roles.name.clone(), t))
                    .child(field("Colour", swatches, t))
                    .child(field("Or a colour of your own", div().w(px(140.)).child(self.roles.hex.clone()), t))
                    .child(switch_row("role-hoist", "Show members separately", "They get a group of their own in the member list.", hoist, editable, &this, |d, on| d.hoist = on, t))
                    .child(switch_row(
                        "role-mentionable",
                        "Allow anyone to @mention this role",
                        "Members without Mention @everyone and Roles can ping it too.",
                        mentionable,
                        editable,
                        &this,
                        |d, on| d.mentionable = on,
                        t,
                    ));
            }
            (_, 1) | (true, _) => {
                let admin = draft.perms & perms::ADMINISTRATOR != 0;
                for group in perms::Group::ALL {
                    panel_ = panel_.child(heading(group.title(), t));
                    for info in perms::ALL.iter().filter(|i| i.group == group) {
                        let held = me.owner || me.bits & perms::ADMINISTRATOR != 0 || me.bits & info.bit != 0;
                        let on = draft.perms & info.bit != 0 || (admin && info.bit != perms::ADMINISTRATOR);
                        let enabled = editable && held && (!admin || info.bit == perms::ADMINISTRATOR);
                        let bit = info.bit;
                        panel_ = panel_.child(switch_row(
                            info.name,
                            info.label,
                            info.hint,
                            on,
                            enabled,
                            &this,
                            move |d, on| if on { d.perms |= bit } else { d.perms &= !bit },
                            t,
                        ).when(info.danger, |d| d.border_color(gpui::Hsla::from(t.danger).opacity(0.4))));
                    }
                }
            }
            _ => {
                panel_ = panel_.child(self.members_tab(editable, t, cx));
            }
        }

        if editable && (everyone || tab != 2) {
            let changed = self.role_changed(role, &draft, cx);
            panel_ = panel_.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("role-save", if self.roles.saving { "Saving…" } else { "Save" })
                            .primary()
                            .disabled(!changed || self.roles.saving)
                            .on_click(cx.listener(|v, _, _, cx| v.save_role(cx))),
                    )
                    .child(
                        Button::new("role-reset", "Reset").ghost().disabled(!changed).on_click({
                            let id = role.id.clone();
                            cx.listener(move |v, _, _, cx| v.select_role(id.clone(), cx))
                        }),
                    )
                    .child(div().flex_1())
                    .when(!everyone, |d| {
                        d.child(
                            Button::new("role-delete", if self.roles.confirm_delete { "Really delete?" } else { "Delete role" })
                                .kind(ButtonKind::Danger)
                                .icon(Icon::Trash)
                                .on_click(cx.listener(|v, _, _, cx| v.delete_role(cx))),
                        )
                    }),
            );
        }
        panel_.into_any_element()
    }

    /// Whether the editor differs from the role as the server has it.
    fn role_changed(&self, role: &Role, draft: &Draft, cx: &gpui::App) -> bool {
        let hex = self.roles.hex.read(cx).text().trim().trim_start_matches('#').to_owned();
        let typed = u32::from_str_radix(&hex, 16).ok().filter(|_| hex.len() == 6);
        let colour = typed.or(draft.colour);
        self.roles.name.read(cx).text().trim() != role.name
            || colour != role.rgb()
            || draft.hoist != role.hoist
            || draft.mentionable != role.mentionable
            || draft.perms != role.bits()
    }

    fn members_tab(&mut self, editable: bool, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut col = div().flex().flex_col().gap_2();
        if editable {
            col = col.child(self.roles.search.clone());
            for m in self.roles.found.clone() {
                let id = m.user_id.clone();
                let has = self.roles.holders.as_ref().is_some_and(|h| h.iter().any(|x| x.user_id == m.user_id));
                col = col.child(member_line(&m, t).child(
                    Button::new(SharedString::from(format!("role-add-{id}")), if has { "Has it" } else { "Add" })
                        .small()
                        .disabled(has)
                        .on_click(cx.listener(move |v, _, _, cx| v.assign(id.clone(), true, cx))),
                ));
            }
        }
        col = col.child(heading("members with this role", t));
        match &self.roles.holders {
            None => col = col.child(hint("Loading…", t)),
            Some(list) if list.is_empty() => col = col.child(hint("Nobody has this role yet.", t)),
            Some(list) => {
                for m in list.clone() {
                    let id = m.user_id.clone();
                    col = col.child(member_line(&m, t).when(editable, |d| {
                        d.child(
                            Button::new(SharedString::from(format!("role-remove-{id}")), "Remove")
                                .small()
                                .ghost()
                                .on_click(cx.listener(move |v, _, _, cx| v.assign(id.clone(), false, cx))),
                        )
                    }));
                }
            }
        }
        col.into_any_element()
    }
}

fn member_line(m: &Member, t: &Theme) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(rhein_ui::Avatar::new(m.user_id.clone(), m.shown_name().to_owned()).size(px(24.)))
        .child(div().flex_1().min_w_0().truncate().child(m.shown_name().to_owned()))
        .child(div().text_size(text::XS).text_color(t.text_faint).child(m.user_id.clone()))
}

/// A colour to pick; `None` is "no colour".
fn swatch(colour: Option<u32>, chosen: bool, enabled: bool, this: &gpui::WeakEntity<GuildSettingsView>, t: &Theme) -> AnyElement {
    let this = this.clone();
    div()
        .id(SharedString::from(format!("swatch-{}", colour.map_or("none".into(), |c| format!("{c:06x}")))))
        .size(px(26.))
        .rounded_full()
        .border_2()
        .border_color(if chosen { t.text } else { t.border })
        .map(|d| match colour {
            Some(c) => d.bg(rgb(c)),
            None => d.flex().items_center().justify_center().child(rhein_ui::icon_sized(Icon::X, px(12.)).text_color(t.text_faint)),
        })
        .when(enabled, |d| {
            d.cursor_pointer().on_click(move |_, _, cx| {
                this.update(cx, |v, cx| {
                    if let Some(draft) = v.roles.draft.as_mut() {
                        draft.colour = colour;
                    }
                    let hex = colour.map(|c| format!("#{c:06x}")).unwrap_or_default();
                    v.roles.hex.update(cx, |i, cx| i.set_text(hex, cx));
                    cx.notify();
                })
                .ok();
            })
        })
        .into_any_element()
}

/// A switch with its title and explanation, changing the draft.
#[allow(clippy::too_many_arguments)]
fn switch_row(
    id: &'static str,
    title: &'static str,
    detail: &'static str,
    on: bool,
    enabled: bool,
    this: &gpui::WeakEntity<GuildSettingsView>,
    change: impl Fn(&mut Draft, bool) + 'static,
    t: &Theme,
) -> gpui::Div {
    let this = this.clone();
    div()
        .p_3()
        .rounded(radius::MD)
        .border_1()
        .border_color(t.border)
        .flex()
        .items_center()
        .gap_3()
        .when(!enabled, |d| d.opacity(0.55))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().font_weight(FontWeight::MEDIUM).child(title))
                .child(div().text_size(text::SM).text_color(t.text_faint).child(detail)),
        )
        .child(if enabled {
            Switch::new(id, on).on_toggle(move |on, _, cx| {
                this.update(cx, |v, cx| {
                    if let Some(draft) = v.roles.draft.as_mut() {
                        change(draft, on);
                    }
                    cx.notify();
                })
                .ok();
            })
        } else {
            Switch::new(id, on)
        })
}
