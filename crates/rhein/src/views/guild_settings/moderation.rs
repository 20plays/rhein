//! The join gate (who may come in, and what their first steps are) and the moderation queue
//! (applicants waiting on screening, first messages held for approval), as in the web client.
//! Rules are edited as lines of text, the way the web client edits them.

use gpui::{AnyElement, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _, div};
use rhein_ui::theme::{FONT_MONO, text};
use rhein_ui::{Button, ButtonKind, Tabs, TextInput, Theme};
use serde_json::{Value, json};

use super::lists::Load;
use super::{GuildSettingsView, field, heading, hint, input, panel, when};

const POLICIES: [&str; 3] = ["open", "allowlist", "blocklist"];

pub(crate) struct State {
    gates: Load<Value>,
    curators: Vec<Value>,
    min_age: Entity<TextInput>,
    domain_policy: usize,
    domains: Entity<TextInput>,
    screening: Entity<TextInput>,
    hold: Entity<TextInput>,
    origin_policy: usize,
    origins: Entity<TextInput>,
    applicants: Load<Vec<Value>>,
    held: Load<Vec<Value>>,
}

impl State {
    pub(crate) fn new(cx: &mut Context<GuildSettingsView>) -> Self {
        let lines = |placeholder: &'static str, cx: &mut Context<GuildSettingsView>| cx.new(|cx| TextInput::new(placeholder, cx).multiline(8));
        Self {
            gates: Load::Loading,
            curators: Vec::new(),
            min_age: input("0", "", cx),
            domain_policy: 0,
            domains: lines("allow example.org", cx),
            screening: lines("What brings you here?", cx),
            hold: input("0", "", cx),
            origin_policy: 0,
            origins: lines("allow files.example", cx),
            applicants: Load::Loading,
            held: Load::Loading,
        }
    }
}

/// `[{domain, action}]` as lines: `allow example.org`.
fn rule_lines(rules: &Value) -> String {
    rules
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|r| {
                    let action = r["action"].as_str().unwrap_or("allow");
                    if let Some(feed) = r["feed"].as_str() {
                        let key = r["key"].as_str().unwrap_or_default();
                        return Some(format!("{action} feed:{feed} key:{key}"));
                    }
                    r["domain"].as_str().map(|d| format!("{action} {d}"))
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Lines back into rules; anything that is not `action target` is left out.
fn parse_rules(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let action = parts.next()?.to_lowercase();
            let target = parts.next()?;
            if let Some(feed) = target.strip_prefix("feed:") {
                let key = parts.find_map(|p| p.strip_prefix("key:")).unwrap_or_default();
                return Some(json!({ "action": action, "feed": feed, "key": key }));
            }
            Some(json!({ "action": action, "domain": target.to_lowercase() }))
        })
        .collect()
}

impl GuildSettingsView {
    pub(crate) fn load_gates(&mut self, cx: &mut Context<Self>) {
        self.call(
            cx,
            |api, gid| async move { api.gates(&gid).await },
            |v, r, cx| {
                if let Ok(g) = &r {
                    let number = |key: &str| g[key].as_i64().map(|n| n.to_string()).unwrap_or_default();
                    let (age, hold) = (number("min_account_age_days"), number("hold_first_messages"));
                    let screening = g["screening"].as_array().map(|q| q.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
                    let (domains, origins) = (rule_lines(&g["domains"]), rule_lines(&g["origins"]));
                    let policy = |key: &str| POLICIES.iter().position(|p| g[key].as_str() == Some(p)).unwrap_or(0);
                    v.moderation.domain_policy = policy("domain_policy");
                    v.moderation.origin_policy = policy("origin_policy");
                    v.moderation.min_age.update(cx, |i, cx| i.set_text(age, cx));
                    v.moderation.hold.update(cx, |i, cx| i.set_text(hold, cx));
                    v.moderation.screening.update(cx, |i, cx| i.set_text(screening, cx));
                    v.moderation.domains.update(cx, |i, cx| i.set_text(domains, cx));
                    v.moderation.origins.update(cx, |i, cx| i.set_text(origins, cx));
                }
                v.moderation.gates = match r {
                    Ok(g) => Load::Ready(g),
                    Err(e) => Load::Failed(e),
                };
            },
        );
        self.call(cx, |api, gid| async move { api.gate_curators(&gid).await }, |v, r, _| {
            v.moderation.curators = r.ok().and_then(|c| c["curators"].as_array().cloned()).unwrap_or_default();
        });
    }

    fn save_gates(&mut self, cx: &mut Context<Self>) {
        let m = &self.moderation;
        let number = |i: &Entity<TextInput>, cx: &Context<Self>| i.read(cx).text().trim().parse::<i64>().unwrap_or(0).max(0);
        let body = json!({
            "min_account_age_days": number(&m.min_age, cx),
            "domain_policy": POLICIES[m.domain_policy],
            "domains": parse_rules(m.domains.read(cx).text()),
            "screening": m.screening.read(cx).text().lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>(),
            "hold_first_messages": number(&m.hold, cx),
            "origin_policy": POLICIES[m.origin_policy],
            "origins": parse_rules(m.origins.read(cx).text()),
        });
        self.call(
            cx,
            move |api, gid| async move { api.set_gates(&gid, &body).await },
            |v, r, cx| {
                if v.outcome(&r, "Gates saved.", cx) {
                    v.load_gates(cx);
                }
            },
        );
    }

    fn refresh_feeds(&mut self, cx: &mut Context<Self>) {
        self.call(
            cx,
            |api, gid| async move { api.refresh_gate_feeds(&gid).await },
            |v, r, cx| {
                v.outcome(&r, "Feeds refreshed.", cx);
                v.load_gates(cx);
            },
        );
    }

    /// Adds a curator's feed to the origin rules, to take effect on save.
    fn pin_curator(&mut self, feed: String, key: String, cx: &mut Context<Self>) {
        let line = format!("allow feed:{feed} key:{key}");
        self.moderation.origins.update(cx, |i, cx| {
            let text = i.text().trim_end().to_owned();
            let next = if text.is_empty() { line } else { format!("{text}\n{line}") };
            i.set_text(next, cx);
        });
        self.report(true, "Added to the rules above. Save gates to subscribe.", cx);
    }

    pub(crate) fn gates_page(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        match &self.moderation.gates {
            Load::Loading => return hint("Loading…", t).into_any_element(),
            Load::Failed(e) => return hint(format!("Couldn't load the gates: {e}"), t).into_any_element(),
            Load::Ready(_) => {}
        }
        let this = cx.weak_entity();
        let policy_tabs = |id: &'static str, chosen: usize, set: fn(&mut GuildSettingsView, usize)| {
            let this = this.clone();
            Tabs::new(id, ["Open", "Allowlist", "Blocklist"]).selected(chosen).on_select(move |i, _, cx| {
                this.update(cx, |v, cx| {
                    set(v, i);
                    cx.notify();
                })
                .ok();
            })
        };
        let m = &self.moderation;
        let feeds: Vec<Value> = match &m.gates {
            Load::Ready(g) => g["origins"].as_array().map(|o| o.iter().filter(|r| r["feed"].is_string()).cloned().collect()).unwrap_or_default(),
            _ => Vec::new(),
        };
        let mut page = div()
            .flex()
            .flex_col()
            .gap_5()
            .child(hint("Gates run when someone joins. This guild can be stricter than its host, never looser.", t))
            .child(
                panel(t)
                    .child(heading("joining", t))
                    .child(field("Minimum account age (days)", m.min_age.clone(), t))
                    .child(hint("Measured from the account’s creation date on its home server.", t))
                    .child(field("Home domain policy", policy_tabs("gate-domain-policy", m.domain_policy, |v, i| v.moderation.domain_policy = i), t))
                    .child(field("Home domain rules", m.domains.clone(), t))
                    .child(hint("One per line, as \"allow example.org\" or \"deny spam.example\". Under the Open policy these are ignored.", t))
                    .child(field("Screening questions", m.screening.clone(), t))
                    .child(hint("One per line. Leave empty to admit without screening; otherwise a joiner lands in the applicants queue.", t))
                    .child(field("Hold first N messages", m.hold.clone(), t))
                    .child(hint("A new member’s first N messages wait for approval instead of appearing. Zero disables the hold.", t)),
            )
            .child(
                panel(t)
                    .child(heading("media origins", t))
                    .child(hint("Which origins this guild accepts attachments from. Checked when a message is posted.", t))
                    .child(field("Media origin policy", policy_tabs("gate-origin-policy", m.origin_policy, |v, i| v.moderation.origin_policy = i), t))
                    .child(field("Media origin rules", m.origins.clone(), t))
                    .child(hint("One per line, as \"allow files.example\" or \"block sketchy.example\". A curator feed is \"allow feed:https://… key:<curator key>\". A named host wins over a feed.", t)),
            )
            .child(div().flex().child(Button::new("gates-save", "Save gates").primary().on_click(cx.listener(|v, _, _, cx| v.save_gates(cx)))));

        let mut curators = panel(t).child(heading("curators", t)).child(hint("Curators this server knows of. A feed decides nothing until you pin its key here.", t));
        if m.curators.is_empty() {
            curators = curators.child(hint("None known.", t));
        }
        for c in m.curators.clone() {
            let (feed, key) = (c["feed"].as_str().unwrap_or_default().to_owned(), c["key"].as_str().unwrap_or_default().to_owned());
            let subscribed = feeds.iter().any(|f| f["feed"].as_str() == Some(feed.as_str()));
            curators = curators.child(
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
                            .child(div().font_weight(FontWeight::MEDIUM).child(c["name"].as_str().unwrap_or("Curator").to_owned()))
                            .child(hint(feed.clone(), t)),
                    )
                    .child(if subscribed {
                        hint("Subscribed", t).into_any_element()
                    } else {
                        Button::new(SharedString::from(format!("curator-pin-{feed}")), "Pin key")
                            .small()
                            .on_click(cx.listener(move |v, _, _, cx| v.pin_curator(feed.clone(), key.clone(), cx)))
                            .into_any_element()
                    }),
            );
        }
        let mut feed_list = panel(t)
            .child(heading("subscribed feeds", t))
            .child(hint("Feeds are fetched when first saved and then hourly. An expired feed stops admitting its hosts but keeps refusing the ones it blocks.", t));
        for f in &feeds {
            let status = &f["status"];
            let hosts = status["hosts"].as_i64().unwrap_or(0);
            let fetched = status["fetched_at"].as_i64().map(when).unwrap_or_else(|| "never".into());
            feed_list = feed_list.child(
                div()
                    .flex()
                    .flex_col()
                    .child(div().font_family(FONT_MONO).text_size(text::SM).child(f["feed"].as_str().unwrap_or_default().to_owned()))
                    .child(hint(format!("{} · {hosts} hosts · fetched {fetched}", status["state"].as_str().unwrap_or("pending")), t)),
            );
        }
        feed_list = feed_list.child(div().flex().child(Button::new("feeds-refresh", "Refresh feeds").small().on_click(cx.listener(|v, _, _, cx| v.refresh_feeds(cx)))));
        page = page.child(curators).child(feed_list);
        page.into_any_element()
    }

    // ------------------------------------------------------------ queue

    pub(crate) fn load_queue(&mut self, cx: &mut Context<Self>) {
        self.call(cx, |api, gid| async move { api.applicants(&gid).await }, |v, r, _| {
            v.moderation.applicants = match r {
                Ok(a) => Load::Ready(a["applicants"].as_array().cloned().unwrap_or_default().into_iter().filter(|x| x["state"] != "approved" && x["state"] != "rejected").collect()),
                Err(e) => Load::Failed(e),
            }
        });
        // Held messages need Manage Messages; without it that half stays empty.
        self.call(cx, |api, gid| async move { api.held_messages(&gid).await }, |v, r, _| {
            v.moderation.held = match r {
                Ok(h) => Load::Ready(h["held"].as_array().cloned().unwrap_or_default()),
                Err(e) => Load::Failed(e),
            }
        });
    }

    fn decide(&mut self, applicant: Option<String>, held: Option<String>, approve: bool, cx: &mut Context<Self>) {
        let decision = if approve { "approve" } else { "reject" };
        self.call(
            cx,
            move |api, gid| async move {
                match (applicant, held) {
                    (Some(user), _) => api.decide_applicant(&gid, &user, decision).await,
                    (_, Some(id)) => api.decide_held(&gid, &id, decision).await,
                    _ => Ok(Value::Null),
                }
            },
            move |v, r, cx| {
                v.outcome(&r, if approve { "Let through." } else { "Turned away." }, cx);
                v.load_queue(cx);
            },
        );
    }

    pub(crate) fn queue_page(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let client_ = self.client.clone();
        let gid = self.guild_id.clone();
        let name = |user: &str, cx: &Context<Self>| client_.read(cx).display_name(Some(&gid), user);
        let channel_name = |id: &str, cx: &Context<Self>| client_.read(cx).channel(id).map(|c| format!("#{}", c.name)).unwrap_or_else(|| "a channel".into());
        let buttons = |applicant: Option<String>, held: Option<String>, key: String, cx: &mut Context<Self>| {
            let (a2, h2) = (applicant.clone(), held.clone());
            div()
                .flex()
                .gap_2()
                .child(Button::new(SharedString::from(format!("q-yes-{key}")), "Approve").small().primary().on_click(cx.listener(move |v, _, _, cx| v.decide(applicant.clone(), held.clone(), true, cx))))
                .child(Button::new(SharedString::from(format!("q-no-{key}")), "Reject").small().kind(ButtonKind::Danger).on_click(cx.listener(move |v, _, _, cx| v.decide(a2.clone(), h2.clone(), false, cx))))
        };
        let mut applicants = panel(t).child(heading("applicants", t)).child(hint("People who answered the screening questions and wait to be let in.", t));
        match &self.moderation.applicants {
            Load::Loading => applicants = applicants.child(hint("Loading…", t)),
            Load::Failed(e) => applicants = applicants.child(hint(format!("Couldn't load them: {e}"), t)),
            Load::Ready(list) if list.is_empty() => applicants = applicants.child(hint("Nobody is waiting.", t)),
            Load::Ready(list) => {
                for a in list.clone() {
                    let user = a["user_id"].as_str().unwrap_or_default().to_owned();
                    let who = name(&user, cx);
                    let answers: Vec<String> = a["answers"].as_array().map(|x| x.iter().map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string())).collect()).unwrap_or_default();
                    applicants = applicants.child(
                        div()
                            .flex()
                            .items_start()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .child(div().font_weight(FontWeight::MEDIUM).child(format!("{who} ({user})")))
                                    .children(answers.into_iter().map(|x| hint(format!("“{x}”"), t))),
                            )
                            .child(buttons(Some(user.clone()), None, user, cx)),
                    );
                }
            }
        }
        let mut held = panel(t).child(heading("held messages", t)).child(hint("New members' first messages, waiting for approval before anyone sees them.", t));
        match &self.moderation.held {
            Load::Loading => held = held.child(hint("Loading…", t)),
            Load::Failed(_) => held = held.child(hint("Seeing held messages needs Manage Messages.", t)),
            Load::Ready(list) if list.is_empty() => held = held.child(hint("Nothing is held.", t)),
            Load::Ready(list) => {
                for h in list.clone() {
                    let id = h["id"].as_str().unwrap_or_default().to_owned();
                    let author = h["author"].as_str().unwrap_or_default().to_owned();
                    let content = h["payload"]["content"].as_str().unwrap_or("(no text)").to_owned();
                    let place = channel_name(h["channel_id"].as_str().unwrap_or_default(), cx);
                    let who = name(&author, cx);
                    let at = h["created_at"].as_i64().map(when).unwrap_or_default();
                    held = held.child(
                        div()
                            .flex()
                            .items_start()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .child(div().font_weight(FontWeight::MEDIUM).child(format!("{who} in {place}")))
                                    .child(div().text_size(text::SM).child(content))
                                    .child(hint(format!("{at} · {}", h["reason"].as_str().unwrap_or_default()), t)),
                            )
                            .child(buttons(None, Some(id.clone()), id, cx)),
                    );
                }
            }
        }
        div().flex().flex_col().gap_5().child(applicants).child(held).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_as_lines_and_back() {
        let rules = json!([{ "domain": "example.org", "action": "allow" }, { "action": "allow", "feed": "https://c.example/feed", "key": "k1" }]);
        let lines = rule_lines(&rules);
        assert_eq!(lines, "allow example.org\nallow feed:https://c.example/feed key:k1");
        assert_eq!(Value::Array(parse_rules(&lines)), rules);
        assert!(parse_rules("nonsense\n\n").is_empty());
    }
}
