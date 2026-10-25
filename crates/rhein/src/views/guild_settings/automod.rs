//! AutoMod: the guild's rules against unwanted messages and what they caught
//! (`docs.telium.chat/features/guilds`, "AutoMod"). Rules need MANAGE_GUILD; the hits need
//! MANAGE_MESSAGES, so a page without that right shows the rules alone.

use gpui::{AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement as _, Styled as _, div, prelude::*, px};
use rhein_ui::theme::{FONT_MONO, radius, text};
use rhein_ui::{Button, ButtonKind, Icon, Tabs, TextInput, Theme};
use serde_json::Value;

use super::lists::{Load, loading, row};
use super::{GuildSettingsView, field, heading, hint, input, panel, when};

/// What a rule looks for, and whether it takes a pattern or a count.
const KINDS: [(&str, &str, bool); 5] = [
    ("keyword", "Words", true),
    ("regex", "Pattern", true),
    ("link", "Links", true),
    ("mention", "Mentions", false),
    ("flood", "Flood", false),
];
const ACTIONS: [(&str, &str); 3] = [("block", "Block"), ("timeout", "Block and time out"), ("flag", "Let through, record")];

pub(crate) struct State {
    rules: Load<Vec<Value>>,
    hits: Load<Vec<Value>>,
    kind: usize,
    action: usize,
    pattern: Entity<TextInput>,
    threshold: Entity<TextInput>,
    exempt: Entity<TextInput>,
}

impl State {
    pub(crate) fn new(cx: &mut Context<GuildSettingsView>) -> Self {
        Self {
            rules: Load::Loading,
            hits: Load::Loading,
            kind: 0,
            action: 0,
            pattern: input("spam, scam, …", "", cx),
            threshold: input("5", "", cx),
            exempt: input("Role names, separated by commas", "", cx),
        }
    }
}

/// The list under `key`, whether the server answers with a bare array or an object holding one.
fn list(answer: Value, key: &str) -> Vec<Value> {
    match answer {
        Value::Array(items) => items,
        mut other => other.get_mut(key).map(Value::take).and_then(|v| if let Value::Array(a) = v { Some(a) } else { None }).unwrap_or_default(),
    }
}

/// How a rule reads in the list.
fn describe(rule: &Value) -> String {
    let kind = rule["kind"].as_str().unwrap_or("?");
    let what = match kind {
        "mention" => format!("{} or more mentions in a message", rule["threshold"].as_u64().unwrap_or(0)),
        "flood" => format!("{} or more messages within ten seconds", rule["threshold"].as_u64().unwrap_or(0)),
        "link" => match rule["pattern"].as_str().unwrap_or_default() {
            "" | "*" => "Any link".to_owned(),
            allowed => format!("Links outside {allowed}"),
        },
        "regex" => format!("Matching /{}/", rule["pattern"].as_str().unwrap_or_default()),
        _ => format!("Any of: {}", rule["pattern"].as_str().unwrap_or_default()),
    };
    let action = ACTIONS.iter().find(|(v, _)| Some(*v) == rule["action"].as_str()).map_or("Block", |(_, l)| l);
    format!("{what} → {action}")
}

impl GuildSettingsView {
    pub(crate) fn load_automod(&mut self, cx: &mut Context<Self>) {
        self.automod.rules = Load::Loading;
        self.automod.hits = Load::Loading;
        self.call(cx, |api, gid| async move { api.automod_rules(&gid).await }, |v, r, _| v.automod.rules = Load::from(r.map(|a| list(a, "rules"))));
        self.call(cx, |api, gid| async move { api.automod_hits(&gid).await }, |v, r, _| v.automod.hits = Load::from(r.map(|a| list(a, "hits"))));
    }

    fn create_rule(&mut self, cx: &mut Context<Self>) {
        let (kind, takes_pattern) = (KINDS[self.automod.kind].0, KINDS[self.automod.kind].2);
        let mut rule = serde_json::json!({ "kind": kind, "action": ACTIONS[self.automod.action].0 });
        if takes_pattern {
            let pattern = self.automod.pattern.read(cx).text().trim().to_owned();
            if pattern.is_empty() && kind != "link" {
                self.report(false, "Say what the rule looks for.", cx);
                return;
            }
            rule["pattern"] = Value::String(pattern);
        } else {
            match self.automod.threshold.read(cx).text().trim().parse::<u64>() {
                Ok(n) if n >= 1 => rule["threshold"] = Value::from(n),
                _ => {
                    self.report(false, "The count has to be 1 or more.", cx);
                    return;
                }
            }
        }
        let names: Vec<String> =
            self.automod.exempt.read(cx).text().split(',').map(|n| n.trim().to_lowercase()).filter(|n| !n.is_empty()).collect();
        if !names.is_empty() {
            let roles = self.client.read(cx).guild(&self.guild_id).map(|g| g.guild.roles.clone()).unwrap_or_default();
            let mut ids = Vec::new();
            for name in &names {
                match roles.iter().find(|r| r.name.to_lowercase() == *name) {
                    Some(role) => ids.push(Value::String(role.id.clone())),
                    None => {
                        self.report(false, format!("No role called {name}."), cx);
                        return;
                    }
                }
            }
            rule["exempt_roles"] = Value::Array(ids);
        }
        self.call(
            cx,
            move |api, gid| async move { api.create_automod_rule(&gid, &rule).await },
            |v, r, cx| {
                if v.outcome(&r, "Rule added.", cx) {
                    v.automod.pattern.update(cx, |i, cx| i.clear(cx));
                    v.automod.exempt.update(cx, |i, cx| i.clear(cx));
                    v.load_automod(cx);
                }
            },
        );
    }

    fn delete_rule(&mut self, id: String, cx: &mut Context<Self>) {
        self.call(
            cx,
            move |api, gid| async move { api.delete_automod_rule(&gid, &id).await },
            |v, r, cx| {
                if v.outcome(&r, "Rule removed.", cx) {
                    v.load_automod(cx);
                }
            },
        );
    }

    pub(crate) fn automod_page(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.automod;
        let (kind, action) = (state.kind, state.action);
        let takes_pattern = KINDS[kind].2;
        let pattern_label = match KINDS[kind].0 {
            "regex" => "Regular expression (case-insensitive)",
            "link" => "Allowed domains, separated by commas (empty or *: no links at all)",
            _ => "Words, separated by commas (case-insensitive)",
        };
        let mut page = div()
            .flex()
            .flex_col()
            .gap_5()
            .child(hint("Members with Manage Messages, and members holding an exempt role, skip every rule.", t));

        let form = panel(t)
            .child(heading("new rule", t))
            .child(field(
                "Looks for",
                Tabs::new("automod-kind", KINDS.iter().map(|(_, l, _)| *l)).selected(kind).on_select({
                    let this = cx.weak_entity();
                    move |i, _, cx| {
                        this.update(cx, |v, cx| {
                            v.automod.kind = i;
                            cx.notify();
                        })
                        .ok();
                    }
                }),
                t,
            ))
            .child(if takes_pattern {
                field(pattern_label, state.pattern.clone(), t)
            } else {
                field(if KINDS[kind].0 == "mention" { "Mentions in one message" } else { "Messages in a channel within ten seconds" }, state.threshold.clone(), t)
            })
            .child(field(
                "Then",
                Tabs::new("automod-action", ACTIONS.iter().map(|(_, l)| *l)).selected(action).on_select({
                    let this = cx.weak_entity();
                    move |i, _, cx| {
                        this.update(cx, |v, cx| {
                            v.automod.action = i;
                            cx.notify();
                        })
                        .ok();
                    }
                }),
                t,
            ))
            .child(hint("A timeout lasts five minutes. Blocked messages fail with the rule's name.", t))
            .child(field("Exempt roles", state.exempt.clone(), t))
            .child(div().flex().child(Button::new("automod-create", "Add rule").primary().icon(Icon::Plus).on_click(cx.listener(|v, _, _, cx| v.create_rule(cx)))));
        page = page.child(form);

        let mut rules = div().flex().flex_col().child(heading("rules", t));
        match loading(&state.rules, "the rules", t) {
            Some(state) => rules = rules.child(state),
            None => {
                let Load::Ready(list) = &state.rules else { unreachable!() };
                if list.is_empty() {
                    rules = rules.child(hint("No rules yet.", t));
                }
                for (n, rule) in list.iter().enumerate() {
                    let id = rule["id"].as_str().unwrap_or_default().to_owned();
                    let name = rule["name"].as_str().map(str::to_owned);
                    rules = rules.child(
                        row(t)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .children(name.map(|n| div().font_weight(FontWeight::MEDIUM).child(n)))
                                    .child(div().text_size(text::SM).child(describe(rule))),
                            )
                            .child(
                                Button::new(("automod-delete", n), "Remove")
                                    .small()
                                    .kind(ButtonKind::Danger)
                                    .disabled(id.is_empty())
                                    .on_click(cx.listener(move |v, _, _, cx| v.delete_rule(id.clone(), cx))),
                            ),
                    );
                }
            }
        }
        page = page.child(rules);

        let mut hits = div().flex().flex_col().child(heading("caught lately", t));
        match loading(&state.hits, "what AutoMod caught", t) {
            Some(state) => hits = hits.child(state),
            None => {
                let Load::Ready(list) = &state.hits else { unreachable!() };
                if list.is_empty() {
                    hits = hits.child(hint("Nothing caught.", t));
                }
                let client = self.client.read(cx);
                for hit in list.iter().take(100) {
                    let who = hit["user_id"].as_str().or(hit["author"].as_str()).unwrap_or_default();
                    let excerpt: String = hit["excerpt"].as_str().unwrap_or_default().chars().take(200).collect();
                    let action = hit["action"].as_str().unwrap_or_default().to_owned();
                    hits = hits.child(
                        row(t)
                            .items_start()
                            .child(div().w(px(130.)).flex_none().text_size(text::SM).text_color(t.text_faint).child(when(hit["created_at"].as_i64().unwrap_or(0))))
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
                                            .child(div().font_weight(FontWeight::MEDIUM).child(client.display_name(Some(&self.guild_id), who)))
                                            .when(!action.is_empty(), |d| {
                                                d.child(div().px_1p5().rounded(radius::SM).bg(t.bg_raised).font_family(FONT_MONO).text_size(text::XS).child(action))
                                            }),
                                    )
                                    .child(hint(excerpt, t)),
                            ),
                    );
                }
            }
        }
        page.child(hits).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lists_and_descriptions() {
        assert_eq!(list(json!([{"id": "a"}]), "rules").len(), 1);
        assert_eq!(list(json!({"rules": [{"id": "a"}, {"id": "b"}]}), "rules").len(), 2);
        assert!(list(json!({"other": 1}), "rules").is_empty());
        assert_eq!(describe(&json!({"kind": "mention", "threshold": 5, "action": "timeout"})), "5 or more mentions in a message → Block and time out");
        assert_eq!(describe(&json!({"kind": "link", "pattern": "*"})), "Any link → Block");
    }
}
