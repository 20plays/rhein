//! The list pages: invites, bans, the audit log, purging a member's messages and the guild's
//! voice regions. Each is loaded when it opens and changed by one call at a time.

use gpui::{AnyElement, ClipboardItem, Context, Entity, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _, div, prelude::*, px};
use rhein_proto::{AuditEntry, Ban, InviteInfo, VoiceRegion};
use rhein_ui::theme::{FONT_MONO, radius, text};
use rhein_ui::{Button, ButtonKind, Icon, IconButton, Tabs, Tag, TextInput, Theme};

use super::{GuildSettingsView, field, heading, hint, input, panel, when};

/// Something a page asked the server for.
pub(crate) enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Load<T> {
    fn from(result: Result<T, String>) -> Self {
        match result {
            Ok(v) => Load::Ready(v),
            Err(e) => Load::Failed(e),
        }
    }
}

const MAX_USES: [(Option<i64>, &str); 6] = [(None, "No limit"), (Some(1), "1 use"), (Some(5), "5"), (Some(10), "10"), (Some(25), "25"), (Some(100), "100")];
const EXPIRES: [(Option<i64>, &str); 6] =
    [(None, "Never"), (Some(1_800), "30 min"), (Some(3_600), "1 hour"), (Some(21_600), "6 hours"), (Some(86_400), "1 day"), (Some(604_800), "7 days")];
const PURGE: [(i64, &str); 4] = [(25, "25"), (100, "100"), (250, "250"), (500, "500")];

pub(crate) struct State {
    invites: Load<Vec<InviteInfo>>,
    max_uses: usize,
    expires: usize,
    bans: Load<Vec<Ban>>,
    ban_who: Entity<TextInput>,
    ban_reason: Entity<TextInput>,
    audit: Load<Vec<AuditEntry>>,
    pub(crate) audit_filter: Entity<TextInput>,
    purge_who: Entity<TextInput>,
    purge_channel: Option<String>,
    purge_limit: usize,
    purge_confirm: bool,
    regions: Load<Vec<VoiceRegion>>,
    region_name: Entity<TextInput>,
    region_sfu: Entity<TextInput>,
}

impl State {
    pub(crate) fn new(cx: &mut Context<GuildSettingsView>) -> Self {
        Self {
            invites: Load::Loading,
            max_uses: 0,
            expires: 0,
            bans: Load::Loading,
            ban_who: input("@someone:example.org", "", cx),
            ban_reason: input("Reason (recorded in the audit log)", "", cx),
            audit: Load::Loading,
            audit_filter: input("Filter by action or account", "", cx),
            purge_who: input("@member:example.org", "", cx),
            purge_channel: None,
            purge_limit: 1,
            purge_confirm: false,
            regions: Load::Loading,
            region_name: input("eu-west", "", cx),
            region_sfu: input("https://sfu.example.org", "", cx),
        }
    }
}

/// A row of a list page.
fn row(t: &Theme) -> gpui::Div {
    div().py_2().flex().items_center().gap_3().border_b_1().border_color(t.border)
}

fn loading<T>(load: &Load<T>, what: &str, t: &Theme) -> Option<AnyElement> {
    match load {
        Load::Loading => Some(hint("Loading…", t).into_any_element()),
        Load::Failed(e) => Some(hint(format!("Couldn't load {what}: {e}"), t).into_any_element()),
        Load::Ready(_) => None,
    }
}

impl GuildSettingsView {
    // ------------------------------------------------------------ invites

    pub(crate) fn load_invites(&mut self, cx: &mut Context<Self>) {
        self.call(cx, |api, gid| async move { api.invites(&gid).await }, |v, r, _| v.lists.invites = Load::from(r.map(|l| l.invites)));
    }

    fn create_invite(&mut self, cx: &mut Context<Self>) {
        let (uses, expires) = (MAX_USES[self.lists.max_uses].0, EXPIRES[self.lists.expires].0);
        self.call(
            cx,
            move |api, gid| async move { api.create_invite_with(&gid, None, uses, expires).await },
            |v, r, cx| {
                if let Ok(invite) = &r {
                    let text = invite.url.clone().unwrap_or_else(|| invite.code.clone());
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                    v.report(true, format!("Invite created and copied: {text}"), cx);
                } else {
                    v.outcome(&r, "", cx);
                }
                v.load_invites(cx);
            },
        );
    }

    fn revoke_invite(&mut self, code: String, cx: &mut Context<Self>) {
        self.call(
            cx,
            move |api, gid| async move { api.revoke_invite(&gid, &code).await },
            |v, r, cx| {
                v.outcome(&r, "Invite revoked.", cx);
                v.load_invites(cx);
            },
        );
    }

    pub(crate) fn invites_page(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.weak_entity();
        let (uses, expires) = (self.lists.max_uses, self.lists.expires);
        let mut page = div().flex().flex_col().gap_5().child(
            panel(t)
                .child(heading("new invite", t))
                .child(field(
                    "Max uses",
                    Tabs::new("invite-uses", MAX_USES.iter().map(|(_, l)| *l)).selected(uses).on_select({
                        let this = this.clone();
                        move |i, _, cx| {
                            this.update(cx, |v, cx| {
                                v.lists.max_uses = i;
                                cx.notify();
                            })
                            .ok();
                        }
                    }),
                    t,
                ))
                .child(field(
                    "Expires after",
                    Tabs::new("invite-expires", EXPIRES.iter().map(|(_, l)| *l)).selected(expires).on_select({
                        let this = this.clone();
                        move |i, _, cx| {
                            this.update(cx, |v, cx| {
                                v.lists.expires = i;
                                cx.notify();
                            })
                            .ok();
                        }
                    }),
                    t,
                ))
                .child(div().flex().child(Button::new("invite-create", "Create invite").primary().icon(Icon::Link).on_click(cx.listener(|v, _, _, cx| v.create_invite(cx))))),
        );
        let mut list = div().flex().flex_col().child(heading("active invites", t));
        if let Some(state) = loading(&self.lists.invites, "the invites", t) {
            return page.child(list.child(state)).into_any_element();
        }
        let Load::Ready(invites) = &self.lists.invites else { unreachable!() };
        if invites.is_empty() {
            list = list.child(hint("No invites yet.", t));
        }
        for i in invites.clone() {
            let (code, copy) = (i.code.clone(), i.url.clone().unwrap_or_else(|| i.code.clone()));
            let uses = match i.max_uses {
                Some(max) if max > 0 => format!("{} of {max} uses", i.uses),
                _ => format!("{} uses", i.uses),
            };
            let until = if i.expired { "expired".to_owned() } else { i.expires_at.map_or("never expires".to_owned(), |at| format!("until {}", when(at))) };
            let creator = i.creator.clone().unwrap_or_default();
            list = list.child(
                row(t)
                    .child(div().font_family(FONT_MONO).font_weight(FontWeight::SEMIBOLD).child(i.code.clone()))
                    .child(div().flex_1().min_w_0().truncate().text_size(text::SM).text_color(t.text_faint).child(format!("{uses} · {until} · by {creator}")))
                    .child(
                        IconButton::new(SharedString::from(format!("invite-copy-{code}")), Icon::Copy)
                            .tooltip("Copy link")
                            .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))),
                    )
                    .child(
                        IconButton::new(SharedString::from(format!("invite-revoke-{code}")), Icon::Trash)
                            .tooltip("Revoke")
                            .danger(true)
                            .on_click(cx.listener(move |v, _, _, cx| v.revoke_invite(code.clone(), cx))),
                    ),
            );
        }
        page = page.child(list);
        page.into_any_element()
    }

    // ------------------------------------------------------------ bans

    pub(crate) fn load_bans(&mut self, cx: &mut Context<Self>) {
        self.call(cx, |api, gid| async move { api.bans(&gid).await }, |v, r, _| v.lists.bans = Load::from(r.map(|l| l.bans)));
    }

    fn unban(&mut self, user: String, cx: &mut Context<Self>) {
        self.call(
            cx,
            move |api, gid| async move { api.unban(&gid, &user).await },
            |v, r, cx| {
                v.outcome(&r, "Unbanned.", cx);
                v.load_bans(cx);
            },
        );
    }

    fn ban_by_address(&mut self, cx: &mut Context<Self>) {
        let who = self.lists.ban_who.read(cx).text().trim().to_owned();
        let reason = self.lists.ban_reason.read(cx).text().trim().to_owned();
        if !who.starts_with('@') || !who.contains(':') {
            self.report(false, "Use the full address, like @name:server.", cx);
            return;
        }
        self.call(
            cx,
            move |api, gid| async move { api.ban_member(&gid, &who, (!reason.is_empty()).then_some(reason.as_str())).await },
            |v, r, cx| {
                if v.outcome(&r, "Banned.", cx) {
                    v.lists.ban_who.update(cx, |i, cx| i.clear(cx));
                    v.lists.ban_reason.update(cx, |i, cx| i.clear(cx));
                }
                v.load_bans(cx);
            },
        );
    }

    pub(crate) fn bans_page(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut page = div().flex().flex_col().gap_5().child(
            panel(t)
                .child(heading("ban an account", t))
                .child(hint("A ban holds across every homeserver: they can't rejoin, even with an invite.", t))
                .child(field("Account", self.lists.ban_who.clone(), t))
                .child(field("Reason", self.lists.ban_reason.clone(), t))
                .child(div().flex().child(Button::new("ban-add", "Ban").kind(ButtonKind::Danger).on_click(cx.listener(|v, _, _, cx| v.ban_by_address(cx))))),
        );
        let mut list = div().flex().flex_col().child(heading("banned", t));
        if let Some(state) = loading(&self.lists.bans, "the bans", t) {
            return page.child(list.child(state)).into_any_element();
        }
        let Load::Ready(bans) = &self.lists.bans else { unreachable!() };
        if bans.is_empty() {
            list = list.child(hint("Nobody is banned.", t));
        }
        for b in bans.clone() {
            let user = b.user_id.clone();
            let detail = [b.reason.clone().filter(|r| !r.is_empty()), b.created_at.map(when)].into_iter().flatten().collect::<Vec<_>>().join(" · ");
            list = list.child(
                row(t)
                    .child(div().flex_1().min_w_0().flex().flex_col().child(b.user_id.clone()).child(hint(if detail.is_empty() { "No reason given".into() } else { detail }, t)))
                    .child(Button::new(SharedString::from(format!("unban-{user}")), "Unban").small().on_click(cx.listener(move |v, _, _, cx| v.unban(user.clone(), cx)))),
            );
        }
        page = page.child(list);
        page.into_any_element()
    }

    // ------------------------------------------------------------ audit log

    pub(crate) fn load_audit(&mut self, cx: &mut Context<Self>) {
        self.call(cx, |api, gid| async move { api.audit_log(&gid).await }, |v, r, _| v.lists.audit = Load::from(r.map(|l| l.entries)));
    }

    pub(crate) fn audit_page(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut page = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(hint("The last hundred changes on this server, newest first.", t))
            .child(self.lists.audit_filter.clone());
        if let Some(state) = loading(&self.lists.audit, "the audit log", t) {
            return page.child(state).into_any_element();
        }
        let Load::Ready(entries) = &self.lists.audit else { unreachable!() };
        let needle = self.lists.audit_filter.read(cx).text().trim().to_lowercase();
        let client = self.client.read(cx);
        let mut shown = 0;
        for e in entries.iter().filter(|e| {
            needle.is_empty()
                || e.action.to_lowercase().contains(&needle)
                || e.actor.to_lowercase().contains(&needle)
                || e.target.as_deref().is_some_and(|x| x.to_lowercase().contains(&needle))
        }) {
            shown += 1;
            let actor = client.display_name(Some(&self.guild_id), &e.actor);
            page = page.child(
                row(t)
                    .items_start()
                    .child(div().w(px(130.)).flex_none().text_size(text::SM).text_color(t.text_faint).child(when(e.created_at)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().font_weight(FontWeight::MEDIUM).child(actor))
                                    .child(div().px_1p5().rounded(radius::SM).bg(t.bg_raised).font_family(FONT_MONO).text_size(text::XS).child(e.action.clone())),
                            )
                            .children(e.target.clone().map(|x| hint(target_name(client, &self.guild_id, &x), t))),
                    ),
            );
        }
        if shown == 0 {
            page = page.child(hint(if needle.is_empty() { "Nothing recorded yet." } else { "Nothing matches." }, t));
        }
        page.into_any_element()
    }

    // ------------------------------------------------------------ purge

    fn purge(&mut self, cx: &mut Context<Self>) {
        let who = self.lists.purge_who.read(cx).text().trim().to_owned();
        if !who.starts_with('@') || !who.contains(':') {
            self.report(false, "Use the member's full address, like @name:server.", cx);
            return;
        }
        if !self.lists.purge_confirm {
            self.lists.purge_confirm = true;
            cx.notify();
            return;
        }
        self.lists.purge_confirm = false;
        let (channel, limit) = (self.lists.purge_channel.clone(), PURGE[self.lists.purge_limit].0);
        self.call(
            cx,
            move |api, gid| async move { api.purge_messages(&gid, &who, channel.as_deref(), Some(limit)).await },
            |v, r, cx| match r {
                Ok(done) => {
                    let n = done.get("deleted").or_else(|| done.get("count")).and_then(|n| n.as_i64());
                    v.report(true, n.map_or("Messages removed.".to_owned(), |n| format!("{n} messages removed.")), cx);
                }
                Err(e) => v.report(false, e, cx),
            },
        );
    }

    pub(crate) fn purge_page(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.weak_entity();
        let client = self.client.read(cx);
        let channels: Vec<(String, String)> = client
            .guild(&self.guild_id)
            .map(|g| g.ordered_channels().into_iter().filter(|c| c.is_textual()).map(|c| (c.id.clone(), c.name.clone())).collect())
            .unwrap_or_default();
        let mut labels = vec!["Every channel".to_owned()];
        labels.extend(channels.iter().map(|(_, n)| format!("#{n}")));
        let chosen = self.lists.purge_channel.as_ref().and_then(|id| channels.iter().position(|(c, _)| c == id)).map_or(0, |i| i + 1);
        let confirm = self.lists.purge_confirm;
        panel(t)
            .child(hint("Removes a member's recent messages at once: a spammer's, say. They are deleted as if by hand, one audit entry for all.", t))
            .child(field("Member", self.lists.purge_who.clone(), t))
            .child(field(
                "In",
                Tabs::new("purge-channel", labels).selected(chosen).on_select({
                    let this = this.clone();
                    move |i, _, cx| {
                        let pick = if i == 0 { None } else { channels.get(i - 1).map(|(id, _)| id.clone()) };
                        this.update(cx, |v, cx| {
                            v.lists.purge_channel = pick;
                            cx.notify();
                        })
                        .ok();
                    }
                }),
                t,
            ))
            .child(field(
                "At most",
                Tabs::new("purge-limit", PURGE.iter().map(|(_, l)| *l)).selected(self.lists.purge_limit).on_select(move |i, _, cx| {
                    this.update(cx, |v, cx| {
                        v.lists.purge_limit = i;
                        cx.notify();
                    })
                    .ok();
                }),
                t,
            ))
            .child(div().flex().child(
                Button::new("purge-go", if confirm { "Really remove them?" } else { "Remove messages" })
                    .kind(ButtonKind::Danger)
                    .icon(Icon::Trash)
                    .on_click(cx.listener(|v, _, _, cx| v.purge(cx))),
            ))
            .into_any_element()
    }

    // ------------------------------------------------------------ voice regions

    pub(crate) fn load_regions(&mut self, cx: &mut Context<Self>) {
        self.call(cx, |api, gid| async move { api.guild_voice_regions(&gid).await }, |v, r, _| v.lists.regions = Load::from(r.map(|l| l.regions)));
    }

    fn add_region(&mut self, cx: &mut Context<Self>) {
        let name = self.lists.region_name.read(cx).text().trim().to_owned();
        let sfu = self.lists.region_sfu.read(cx).text().trim().to_owned();
        if name.is_empty() || !sfu.starts_with("https://") {
            self.report(false, "A region needs a name and the SFU's https address.", cx);
            return;
        }
        self.call(
            cx,
            move |api, gid| async move { api.add_guild_voice_region(&gid, &serde_json::json!({ "name": name, "sfu": sfu })).await },
            |v, r, cx| {
                if v.outcome(&r, "Region added.", cx) {
                    v.lists.region_name.update(cx, |i, cx| i.clear(cx));
                    v.lists.region_sfu.update(cx, |i, cx| i.clear(cx));
                }
                v.load_regions(cx);
            },
        );
    }

    fn region_action(&mut self, name: String, default: bool, cx: &mut Context<Self>) {
        self.call(
            cx,
            move |api, gid| async move {
                if default { api.set_guild_voice_region_default(&gid, &name).await } else { api.delete_guild_voice_region(&gid, &name).await }
            },
            move |v, r, cx| {
                v.outcome(&r, if default { "Default region set." } else { "Region removed." }, cx);
                v.load_regions(cx);
            },
        );
    }

    pub(crate) fn voice_page(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div().flex().flex_col().child(heading("regions", t));
        if let Some(state) = loading(&self.lists.regions, "the regions", t) {
            list = list.child(state);
        } else if let Load::Ready(regions) = &self.lists.regions {
            if regions.is_empty() {
                list = list.child(hint("This server offers no voice regions; calls go peer to peer.", t));
            }
            for r in regions.clone() {
                let (a, b) = (r.name.clone(), r.name.clone());
                let latency = r.latency_ms.map(|l| format!(" · {l} ms")).unwrap_or_default();
                list = list.child(
                    row(t)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().flex().items_center().gap_2().child(div().font_weight(FontWeight::MEDIUM).child(r.name.clone())).when(r.default, |d| d.child(Tag("DEFAULT"))).when(r.guild, |d| d.child(Tag("THIS GUILD"))))
                                .child(hint(format!("{}{}{latency}", r.sfu.clone().unwrap_or_default(), if r.healthy { "" } else { " · unreachable" }), t)),
                        )
                        .when(!r.default, |d| d.child(Button::new(SharedString::from(format!("region-default-{a}")), "Make default").small().ghost().on_click(cx.listener(move |v, _, _, cx| v.region_action(a.clone(), true, cx)))))
                        .when(r.guild, |d| {
                            d.child(
                                IconButton::new(SharedString::from(format!("region-del-{b}")), Icon::Trash)
                                    .tooltip("Remove")
                                    .danger(true)
                                    .on_click(cx.listener(move |v, _, _, cx| v.region_action(b.clone(), false, cx))),
                            )
                        }),
                );
            }
        }
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(list)
            .child(
                panel(t)
                    .child(heading("add a region of this guild's own", t))
                    .child(hint("An SFU you run yourself, offered to this guild's voice channels beside the server's.", t))
                    .child(field("Name", self.lists.region_name.clone(), t))
                    .child(field("SFU address", self.lists.region_sfu.clone(), t))
                    .child(div().flex().child(Button::new("region-add", "Add region").primary().on_click(cx.listener(|v, _, _, cx| v.add_region(cx))))),
            )
            .into_any_element()
    }
}

/// What an audit entry's target is called: a role, a channel or a member by name, else its id.
fn target_name(client: &rhein_store::Client, guild_id: &str, target: &str) -> String {
    if let Some(g) = client.guild(guild_id) {
        if target == guild_id {
            return g.guild.name.clone();
        }
        if let Some(r) = g.guild.roles.iter().find(|r| r.id == target) {
            return format!("@{}", r.name);
        }
        if let Some(c) = g.guild.channels.iter().find(|c| c.id == target) {
            return format!("#{}", c.name);
        }
    }
    if target.starts_with('@') {
        return format!("{} ({target})", client.display_name(Some(guild_id), target));
    }
    target.to_owned()
}
