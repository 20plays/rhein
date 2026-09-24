//! Who may add members, remove them and change these rules, as the web client defines it
//! (`normalisePolicy` / `policyAllows` in `mls.js`). It rides in GroupContext extension `0xF7A0`
//! as JSON, so every member checks the same rules and no server can edit them.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// `creator` is the account that made the group, `admins` the creator plus the admin list,
/// `anyone` every member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rule {
    Creator,
    Admins,
    Anyone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub v: u8,
    pub creator: String,
    pub add: Rule,
    pub remove: Rule,
    pub policy: Rule,
    #[serde(default)]
    pub admins: Vec<String>,
}

impl Policy {
    /// The same normalisation as the web client: version 1, a creator, at most 256 admins,
    /// deduplicated and sorted.
    pub fn normalised(mut self) -> Result<Policy> {
        if self.v != 1 || self.creator.is_empty() {
            bail!("not a version 1 policy");
        }
        if self.admins.len() > 256 || self.admins.iter().any(String::is_empty) {
            bail!("bad admin list");
        }
        self.admins.sort();
        self.admins.dedup();
        Ok(self)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Policy> {
        serde_json::from_slice::<Policy>(bytes)?.normalised()
    }

    /// JSON with the keys in the web client's order (`v, creator, add, remove, policy, admins`),
    /// so both clients write byte-identical extensions.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Whether `user_id` may do what `rule` guards.
    pub fn allows(&self, rule: Rule, user_id: &str) -> bool {
        match rule {
            Rule::Anyone => true,
            _ if user_id == self.creator => true,
            Rule::Admins => self.admins.iter().any(|a| a == user_id),
            Rule::Creator => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_web_client() {
        let raw = br#"{"v":1,"creator":"@a:x","add":"anyone","remove":"admins","policy":"creator","admins":["@c:x","@b:x","@b:x"]}"#;
        let p = Policy::from_bytes(raw).unwrap();
        assert_eq!(p.admins, ["@b:x", "@c:x"]);
        assert_eq!(
            String::from_utf8(p.to_bytes()).unwrap(),
            r#"{"v":1,"creator":"@a:x","add":"anyone","remove":"admins","policy":"creator","admins":["@b:x","@c:x"]}"#
        );
        assert!(p.allows(p.add, "@z:x"));
        assert!(p.allows(p.remove, "@b:x"));
        assert!(!p.allows(p.remove, "@z:x"));
        assert!(p.allows(p.policy, "@a:x"));
        assert!(!p.allows(p.policy, "@b:x"));
        assert!(Policy::from_bytes(br#"{"v":2,"creator":"@a:x","add":"anyone","remove":"anyone","policy":"anyone"}"#).is_err());
    }
}
