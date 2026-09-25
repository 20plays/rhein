//! The keys of one guild call on an SFU, as `web/src/lib/guild-call-keys.js` keeps them: offer a
//! package with the join, found the call's group when the server says so, join it from the
//! welcome, follow its commits in epoch order, and, as the call's longest-standing member, add
//! whoever waits and remove whoever left. Every leaf must be a device its account vouched for;
//! a call with one that isn't is left with the reason. Each change yields the media key.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use rhein_mls::call_group::{self, CallGroup};
use rhein_mls::vault;
use rhein_net::Api;
use rhein_proto::GuildE2ee;
use serde_json::{Value, json};

/// Whether an account vouches for a device's signature key (`user`, `device`, key base64).
pub type Verifier = Arc<dyn Fn(String, String, String) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// A verifier that takes every device (tests; accounts without identities).
pub fn trust_all() -> Verifier {
    Arc::new(|_, _, _| Box::pin(async { true }))
}

pub struct GuildKeys {
    group: CallGroup,
    channel: String,
    me: String,
    api: Api,
    verify: Verifier,
    /// Events that came before the join answered.
    early: Vec<Value>,
    begun: bool,
}

/// A media key and its epoch.
pub type Key = (Vec<u8>, u64);

impl GuildKeys {
    pub fn new(user: &str, device: &str, signature: (&[u8], &[u8]), channel: &str, api: Api, verify: Verifier) -> Result<GuildKeys> {
        let group = CallGroup::new(user, device, signature.0, signature.1)?;
        Ok(GuildKeys { group, channel: channel.to_owned(), me: user.to_owned(), api, verify, early: Vec::new(), begun: false })
    }

    /// The key package for the join request.
    pub fn offer(&mut self) -> Result<String> {
        self.group.offer()
    }

    pub fn key(&self) -> Option<Key> {
        self.group.media_key(&self.channel)
    }

    /// The join answered: this is the call's group. Returns the key if we hold one now.
    pub async fn begin(&mut self, info: &GuildE2ee) -> Result<Option<Key>> {
        self.group.begin(&info.group_id, info.member)?;
        self.begun = true;
        let mut key = self.key();
        for event in std::mem::take(&mut self.early) {
            if let Some(k) = self.apply(&event).await? {
                key = Some(k);
            }
        }
        Ok(key)
    }

    /// A `VOICE_E2EE` event. Returns the new key when it changed.
    pub async fn hear(&mut self, event: &Value) -> Result<Option<Key>> {
        if !self.begun {
            self.early.push(event.clone());
            return Ok(None);
        }
        self.apply(event).await
    }

    async fn apply(&mut self, d: &Value) -> Result<Option<Key>> {
        let group_id = d["group_id"].as_str().unwrap_or_default();
        match d["op"].as_str() {
            Some("reset") => {
                if Some(group_id) == self.group.group_id() && self.group.is_member() {
                    return Ok(None);
                }
                let founder = d["founder"].as_str() == Some(self.me.as_str());
                self.group.begin(group_id, founder)?;
                Ok(self.key())
            }
            Some("commit") if Some(group_id) == self.group.group_id() => {
                let epoch = d["epoch"].as_u64().unwrap_or(0);
                if !self.group.is_member() {
                    let added = d["added"].as_array().is_some_and(|a| a.iter().any(|u| u.as_str() == Some(self.me.as_str())));
                    let Some(welcome) = d["welcome"].as_str().filter(|_| added) else { return Ok(None) };
                    // A welcome for a package of ours that is not the one we hold is not ours.
                    if let Err(err) = self.group.join(welcome) {
                        log::warn!("guild call: the welcome did not open: {err:#}");
                        return Ok(None);
                    }
                    self.admit().await?;
                    return Ok(self.key());
                }
                let now = self.group.epoch().unwrap_or(0);
                if epoch <= now {
                    return Ok(None);
                }
                if epoch != now + 1 {
                    bail!("This device lost track of the call's encryption and cannot hear it. Rejoin the call.");
                }
                let commit = d["commit"].as_str().ok_or_else(|| anyhow!("a commit without its message"))?;
                if !self.group.process(commit)? {
                    // Removed: silent until a later welcome adds us again.
                    return Ok(None);
                }
                self.admit().await?;
                Ok(self.key())
            }
            _ => Ok(None),
        }
    }

    /// Every other leaf must be a device its account vouched for.
    async fn admit(&mut self) -> Result<()> {
        for leaf in self.group.leaves() {
            if leaf.user_id.is_empty() || leaf.device_id.is_empty() {
                bail!("This call's encryption group holds a device that does not say whose it is, so this device left.");
            }
            if leaf.user_id == self.me && leaf.device_id == self.group.own_device() {
                continue;
            }
            if !(self.verify)(leaf.user_id.clone(), leaf.device_id.clone(), vault::b64(&leaf.signature_key)).await {
                bail!("A device in this call claims to be {}, but their account never signed it, so this device left.", leaf.user_id);
            }
        }
        Ok(())
    }

    /// When we are the call's committer: adds whoever waits and removes whoever left, in one
    /// commit. `roster` is the channel's voice states as JSON. Returns the new key if it moved.
    pub async fn tend(&mut self, roster: &[Value]) -> Result<Option<Key>> {
        if !self.group.is_member() || call_group::committer_of(roster).as_deref() != Some(self.me.as_str()) {
            return Ok(None);
        }
        let view = self.api.voice_e2ee(&self.channel).await.map_err(|e| anyhow!("{e}"))?;
        if view["group_id"].as_str() != self.group.group_id() || view["epoch"].as_u64() != self.group.epoch() {
            return Ok(None);
        }
        let participants = view["participants"].as_array().cloned().unwrap_or_default();
        let (removes, waiting) = call_group::plan_change(&self.group.leaves(), &participants, &self.me);
        // A waiting package is taken only if it names the device the server filed it under and
        // its account vouches for it.
        let mut adds = Vec::new();
        for p in waiting {
            let package = p["key_package"].as_str().unwrap_or_default();
            let Some((user, device, key)) = call_group::package_identity(package) else { continue };
            if Some(user.as_str()) != p["user_id"].as_str() || Some(device.as_str()) != p["device_id"].as_str() {
                continue;
            }
            if !(self.verify)(user.clone(), device, vault::b64(&key)).await {
                continue;
            }
            adds.push((user, package.to_owned()));
        }
        if removes.is_empty() && adds.is_empty() {
            return Ok(None);
        }
        let epoch = self.group.epoch().unwrap_or(0);
        let packages: Vec<String> = adds.iter().map(|(_, p)| p.clone()).collect();
        let (commit, welcome) = self.group.change(&removes, &packages)?;
        let body = json!({
            "group_id": self.group.group_id(),
            "epoch": epoch,
            "commit": commit,
            "welcome": welcome,
            "added": adds.iter().map(|(u, p)| json!({ "user_id": u, "key_package": p })).collect::<Vec<_>>(),
        });
        match self.api.voice_e2ee_commit(&self.channel, &body).await {
            Ok(_) => {
                self.group.merge_pending()?;
                Ok(self.key())
            }
            // Someone else took this epoch; their commit arrives as an event.
            Err(err) if err.status() == Some(409) => {
                self.group.discard_pending();
                Ok(None)
            }
            Err(err) => {
                self.group.discard_pending();
                Err(anyhow!("{err}"))
            }
        }
    }
}
