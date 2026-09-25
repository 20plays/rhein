//! The MLS group of one guild call on an SFU (`web/src/lib/guild-call-keys.js`,
//! `docs/guild-calls.md` "Encrypting a guild call"). A guild channel has no group of its own, so
//! each live call gets one made of the devices in it: joined with a fresh in-memory key package
//! signed with the device's own MLS key, founded by the device the server names, grown and
//! shrunk by the call's longest-standing member. Every epoch exports the media key.

use anyhow::{Context as _, Result, anyhow, bail};
use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;
use openmls_rust_crypto::OpenMlsRustCrypto;
use serde_json::Value;

use crate::{Device, identity_of, signature_key};

/// The exporter label of a guild call's media key; the context is `"{channel}|{group}"`.
pub const GUILD_MEDIA_LABEL: &str = "telium guild call media v1";

/// A leaf of the call's group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leaf {
    pub index: u32,
    pub user_id: String,
    pub device_id: String,
    pub signature_key: Vec<u8>,
}

/// This device's side of a call's group. Nothing of it is stored: it serves one call.
pub struct CallGroup {
    device: Device,
    user_id: String,
    device_id: String,
    group_id: Option<String>,
    group: Option<MlsGroup>,
    offered: Option<String>,
}

impl CallGroup {
    /// With the device's MLS signature key (the one its account cross-signed).
    pub fn new(user_id: &str, device_id: &str, private: &[u8], public: &[u8]) -> Result<CallGroup> {
        let signer = signature_key(private, public);
        let device = Device::with_signer(OpenMlsRustCrypto::default(), signer, &format!("{user_id}|{device_id}"))?;
        Ok(CallGroup { device, user_id: user_id.to_owned(), device_id: device_id.to_owned(), group_id: None, group: None, offered: None })
    }

    /// A fresh key package for the join request, base64.
    pub fn offer(&mut self) -> Result<String> {
        let package = crate::vault::b64(&self.device.key_package()?);
        self.offered = Some(package.clone());
        Ok(package)
    }

    pub fn own_device(&self) -> &str {
        &self.device_id
    }

    pub fn group_id(&self) -> Option<&str> {
        self.group_id.as_deref()
    }

    /// Whether this device holds the group (and so the key).
    pub fn is_member(&self) -> bool {
        self.group.is_some()
    }

    pub fn epoch(&self) -> Option<u64> {
        self.group.as_ref().map(|g| g.epoch().as_u64())
    }

    /// Takes the group the server names; founds it when we are its first member.
    pub fn begin(&mut self, group_id: &str, founder: bool) -> Result<()> {
        self.group_id = Some(group_id.to_owned());
        self.group = None;
        if founder {
            let (group, _) = self.device.create_group(group_id, None, &[])?;
            self.group = Some(group);
        }
        Ok(())
    }

    /// Joins from a welcome that added us.
    pub fn join(&mut self, welcome_b64: &str) -> Result<()> {
        let welcome = crate::vault::unb64(welcome_b64)?;
        let group = self.device.join(&welcome)?;
        if Some(String::from_utf8_lossy(group.group_id().as_slice()).as_ref()) != self.group_id.as_deref() {
            bail!("the welcome is for another group");
        }
        self.group = Some(group);
        Ok(())
    }

    /// Applies a commit; `false` when it removed us (we are out until the next welcome).
    pub fn process(&mut self, commit_b64: &str) -> Result<bool> {
        let frame = crate::vault::unb64(commit_b64)?;
        let group = self.group.as_mut().ok_or_else(|| anyhow!("not in the call's group"))?;
        self.device.process(group, &frame, |_, _, _| Ok(()))?;
        let still = group.members().any(|m| identity_of(&m.credential) == Some((self.user_id.clone(), self.device_id.clone())));
        if !still {
            self.group = None;
        }
        Ok(still)
    }

    /// The group's leaves.
    pub fn leaves(&self) -> Vec<Leaf> {
        let Some(group) = &self.group else { return Vec::new() };
        group
            .members()
            .map(|m| {
                let (user_id, device_id) = identity_of(&m.credential).unwrap_or_default();
                Leaf { index: m.index.u32(), user_id, device_id, signature_key: m.signature_key }
            })
            .collect()
    }

    /// One commit that removes `removes` (leaf indices) and adds `adds` (key packages, base64).
    /// Returns the commit and the welcome (base64), with the ratchet tree in the welcome.
    pub fn change(&mut self, removes: &[u32], adds: &[String]) -> Result<(String, Option<String>)> {
        let group = self.group.as_mut().ok_or_else(|| anyhow!("not in the call's group"))?;
        let packages = adds
            .iter()
            .map(|kp| {
                let bytes = crate::vault::unb64(kp)?;
                let message = MlsMessageIn::tls_deserialize_exact(&bytes).context("key package frame")?;
                let MlsMessageBodyIn::KeyPackage(kp) = message.extract() else { bail!("not a key package") };
                kp.validate(self.device.provider.crypto(), ProtocolVersion::Mls10).map_err(|e| anyhow!("invalid key package: {e:?}"))
            })
            .collect::<Result<Vec<_>>>()?;
        let bundle = group
            .commit_builder()
            .propose_removals(removes.iter().map(|i| LeafNodeIndex::new(*i)))
            .propose_adds(packages)
            .load_psks(self.device.provider.storage())
            .map_err(|e| anyhow!("commit: {e:?}"))?
            .build(self.device.provider.rand(), self.device.provider.crypto(), &self.device.signer, |_| true)
            .map_err(|e| anyhow!("commit: {e:?}"))?
            .stage_commit(&self.device.provider)
            .map_err(|e| anyhow!("commit: {e:?}"))?;
        let (commit, welcome, _) = bundle.into_messages();
        let commit = crate::vault::b64(&commit.tls_serialize_detached()?);
        let welcome = welcome.map(|w| w.tls_serialize_detached()).transpose()?.map(|w| crate::vault::b64(&w));
        Ok((commit, welcome))
    }

    /// Takes our own commit in once the server accepted it.
    pub fn merge_pending(&mut self) -> Result<()> {
        let group = self.group.as_mut().ok_or_else(|| anyhow!("not in the call's group"))?;
        group.merge_pending_commit(&self.device.provider).map_err(|e| anyhow!("merging commit: {e:?}"))
    }

    /// Drops our own commit when another won the epoch.
    pub fn discard_pending(&mut self) {
        if let Some(group) = self.group.as_mut() {
            let _ = group.clear_pending_commit(self.device.provider.storage());
        }
    }

    /// This epoch's media key for `channel`, and the epoch.
    pub fn media_key(&self, channel: &str) -> Option<(Vec<u8>, u64)> {
        let group = self.group.as_ref()?;
        let context = format!("{channel}|{}", self.group_id.as_deref()?);
        let key = self.device.export(group, GUILD_MEDIA_LABEL, context.as_bytes(), 32).ok()?;
        Some((key, group.epoch().as_u64()))
    }
}

/// Who commits: the longest-standing member of the call (`joined_at`, then `user_id`, over the
/// voice states with `e2ee_member`), as every client works it out from the same roster.
pub fn committer_of(roster: &[Value]) -> Option<String> {
    let mut members: Vec<&Value> = roster.iter().filter(|v| v["e2ee_member"].as_bool() == Some(true)).collect();
    members.sort_by(|a, b| {
        a["joined_at"].as_i64().unwrap_or(0).cmp(&b["joined_at"].as_i64().unwrap_or(0)).then_with(|| a["user_id"].as_str().cmp(&b["user_id"].as_str()))
    });
    members.first().and_then(|v| v["user_id"].as_str()).map(str::to_owned)
}

/// Whom a commit removes (leaves whose owner left, switched device or is not a member) and whom
/// it adds (participants waiting with a package), as `planChange` does it.
pub fn plan_change<'a>(leaves: &[Leaf], participants: &'a [Value], me: &str) -> (Vec<u32>, Vec<&'a Value>) {
    let mut removes = Vec::new();
    for leaf in leaves {
        if leaf.user_id == me {
            continue;
        }
        let here = participants.iter().find(|p| p["user_id"].as_str() == Some(leaf.user_id.as_str()));
        let keeps = here.is_some_and(|p| p["device_id"].as_str() == Some(leaf.device_id.as_str()) && p["member"].as_bool() == Some(true));
        if !keeps {
            removes.push(leaf.index);
        }
    }
    let adds = participants
        .iter()
        .filter(|p| p["member"].as_bool() != Some(true) && p["key_package"].as_str().is_some_and(|k| !k.is_empty()) && p["user_id"].as_str() != Some(me))
        .collect();
    (removes, adds)
}

/// The identity (`user_id`, `device_id`) and signature key a waiting package names.
pub fn package_identity(package_b64: &str) -> Option<(String, String, Vec<u8>)> {
    let bytes = crate::vault::unb64(package_b64).ok()?;
    let (identity, key) = crate::key_package_identity(&bytes)?;
    let (user, device) = identity.split_once('|')?;
    Some((user.to_owned(), device.to_owned(), key))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn device(user: &str, dev: &str) -> CallGroup {
        let (private, public) = crate::new_signature_key().unwrap();
        CallGroup::new(user, dev, &private, &public).unwrap()
    }

    #[test]
    fn three_devices_share_one_key_per_epoch_and_a_leaver_loses_it() {
        let (mut a, mut b, mut c) = (device("@a:x", "a1"), device("@b:x", "b1"), device("@c:x", "c1"));
        let (kb, kc) = (b.offer().unwrap(), c.offer().unwrap());
        a.offer().unwrap();
        a.begin("g1", true).unwrap();
        b.begin("g1", false).unwrap();
        c.begin("g1", false).unwrap();

        let (_commit, welcome) = a.change(&[], &[kb, kc]).unwrap();
        a.merge_pending().unwrap();
        let welcome = welcome.expect("a welcome for the added");
        b.join(&welcome).unwrap();
        c.join(&welcome).unwrap();
        let key = a.media_key("chan").unwrap();
        assert_eq!(b.media_key("chan").unwrap(), key);
        assert_eq!(c.media_key("chan").unwrap(), key);
        assert_ne!(a.media_key("other").unwrap().0, key.0, "bound to the channel");

        // c leaves: a removes its leaf, b follows, c is out and the key moves on.
        let c_leaf = a.leaves().iter().find(|l| l.user_id == "@c:x").unwrap().index;
        let (commit, welcome) = a.change(&[c_leaf], &[]).unwrap();
        assert!(welcome.is_none());
        a.merge_pending().unwrap();
        assert!(b.process(&commit).unwrap());
        assert!(!c.process(&commit).unwrap(), "c was removed");
        let next = a.media_key("chan").unwrap();
        assert_eq!(b.media_key("chan").unwrap(), next);
        assert_ne!(next.0, key.0);
        assert_eq!(next.1, key.1 + 1);
    }

    #[test]
    fn the_committer_is_the_longest_standing_member() {
        let roster = vec![
            json!({ "user_id": "@c:x", "e2ee_member": true, "joined_at": 5 }),
            json!({ "user_id": "@b:x", "e2ee_member": true, "joined_at": 3 }),
            json!({ "user_id": "@a:x", "e2ee_member": false, "joined_at": 1 }),
            json!({ "user_id": "@d:x", "e2ee_member": true, "joined_at": 3 }),
        ];
        assert_eq!(committer_of(&roster).as_deref(), Some("@b:x"));
    }

    #[test]
    fn a_change_removes_the_gone_and_adds_the_waiting() {
        let leaf = |i, u: &str, d: &str| Leaf { index: i, user_id: u.into(), device_id: d.into(), signature_key: vec![] };
        let leaves = vec![leaf(0, "@me:x", "m"), leaf(1, "@stays:x", "s"), leaf(2, "@left:x", "l"), leaf(3, "@moved:x", "old")];
        let participants = vec![
            json!({ "user_id": "@me:x", "device_id": "m", "member": true }),
            json!({ "user_id": "@stays:x", "device_id": "s", "member": true }),
            json!({ "user_id": "@moved:x", "device_id": "new", "member": false, "key_package": "kp1" }),
            json!({ "user_id": "@new:x", "device_id": "n", "member": false, "key_package": "kp2" }),
        ];
        let (removes, adds) = plan_change(&leaves, &participants, "@me:x");
        assert_eq!(removes, [2, 3]);
        let added: Vec<&str> = adds.iter().map(|p| p["user_id"].as_str().unwrap()).collect();
        assert_eq!(added, ["@moved:x", "@new:x"]);
    }
}
