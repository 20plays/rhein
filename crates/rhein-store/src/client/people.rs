//! What a profile card shows about someone beyond their profile: the guilds and friends you
//! share, a link to their public profile, and calling them.

use gpui::Context;

use super::{Client, join};

impl Client {
    /// The guilds you share with someone: those whose loaded member list holds them.
    pub fn mutual_guilds(&self, user_id: &str) -> Vec<(String, String)> {
        self.guilds
            .iter()
            .filter(|g| self.member(&g.guild.id, user_id).is_some())
            .map(|g| (g.guild.id.clone(), g.guild.name.clone()))
            .collect()
    }

    /// The friends you share with someone, asked of your home server once per person (it asks
    /// the servers your friends live on, without naming anyone to them).
    pub fn mutual_friends(&mut self, user_id: &str, cx: &mut Context<Self>) -> Option<Vec<String>> {
        if let Some(known) = self.mutual_friends.get(user_id) {
            return known.clone();
        }
        let api = self.home.as_ref()?.api.clone();
        self.mutual_friends.insert(user_id.to_owned(), None);
        let (io, user) = (self.io.clone(), user_id.to_owned());
        let target = user.clone();
        cx.spawn(async move |this, cx| {
            let answer = join(io.spawn(async move { Ok(api.mutual_friends(&target).await?) })).await;
            this.update(cx, |this, cx| {
                let ids: Vec<String> = answer
                    .ok()
                    .and_then(|a| a["friends"].as_array().cloned())
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|f| f["user_id"].as_str().map(str::to_owned))
                    .collect();
                this.mutual_friends.insert(user, Some(ids));
                cx.notify();
            })
            .ok();
        })
        .detach();
        None
    }

    /// Someone's public profile page, on your home server's web client:
    /// `https://app.example/u/@name:server`.
    pub fn profile_link(&self, user_id: &str) -> Option<String> {
        let doc = &self.home.as_ref()?.doc;
        let base = doc.client.clone().unwrap_or_else(|| doc.api.clone());
        Some(format!("{}/u/{user_id}", base.trim_end_matches('/')))
    }

    /// Calls someone: in your conversation with them, opened (or started) first.
    pub fn call_user(&mut self, user_id: String, cx: &mut Context<Self>) {
        let me = self.user_id().unwrap_or_default().to_owned();
        let existing = self
            .conversations
            .iter()
            .find(|c| c.kind == "dm" && c.members.iter().any(|m| m.user_id == user_id) && c.members.iter().any(|m| m.user_id == me))
            .map(|c| c.id.clone());
        match existing {
            Some(id) => {
                self.open_conversation(Some(id.clone()), cx);
                self.join_dm_call(id, cx);
            }
            None => {
                self.call_when_open = Some(user_id.clone());
                self.message_user(user_id, cx);
            }
        }
    }
}
