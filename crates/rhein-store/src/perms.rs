//! Channel permissions computed on the client, so the UI can hide actions the server would
//! refuse. Mirrors `channel_permissions` in Telium's `server/src/perms.rs`; the server still
//! decides every request.

use rhein_proto::Overwrite;

pub const VIEW_CHANNEL: u64 = 1 << 0;
pub const SEND_MESSAGES: u64 = 1 << 1;
pub const MANAGE_MESSAGES: u64 = 1 << 2;
pub const MANAGE_CHANNELS: u64 = 1 << 3;
pub const MANAGE_GUILD: u64 = 1 << 7;
pub const CREATE_INVITE: u64 = 1 << 9;
pub const ATTACH_FILES: u64 = 1 << 10;
pub const MENTION_EVERYONE: u64 = 1 << 12;
pub const ADD_REACTIONS: u64 = 1 << 20;
pub const READ_MESSAGE_HISTORY: u64 = 1 << 21;
pub const ADMINISTRATOR: u64 = 1 << 40;
/// What a timed-out member keeps.
pub const TIMEOUT_ALLOWED: u64 = VIEW_CHANNEL | READ_MESSAGE_HISTORY;

pub struct Member<'a> {
    pub user_id: &'a str,
    pub guild_id: &'a str,
    /// Guild-level permissions as READY's `me.permissions` reports them.
    pub guild_permissions: u64,
    pub role_ids: &'a [String],
    pub timed_out: bool,
}

pub fn parse_bits(value: &str) -> u64 {
    value.parse().unwrap_or(0)
}

/// @everyone overwrite, then the union of role overwrites (deny before allow), then the member's.
pub fn channel_permissions(member: &Member, overwrites: &[Overwrite]) -> u64 {
    let mut perms = member.guild_permissions;
    if perms == u64::MAX || perms & ADMINISTRATOR != 0 {
        return if member.timed_out { TIMEOUT_ALLOWED } else { u64::MAX };
    }
    if let Some(everyone) = overwrites.iter().find(|o| o.target_type == "role" && o.target_id == member.guild_id) {
        perms &= !parse_bits(&everyone.deny);
        perms |= parse_bits(&everyone.allow);
    }
    let (mut allow, mut deny) = (0u64, 0u64);
    for o in overwrites.iter().filter(|o| o.target_type == "role" && o.target_id != member.guild_id) {
        if member.role_ids.contains(&o.target_id) {
            allow |= parse_bits(&o.allow);
            deny |= parse_bits(&o.deny);
        }
    }
    perms &= !deny;
    perms |= allow;
    if let Some(own) = overwrites.iter().find(|o| o.target_type == "member" && o.target_id == member.user_id) {
        perms &= !parse_bits(&own.deny);
        perms |= parse_bits(&own.allow);
    }
    if member.timed_out { perms & TIMEOUT_ALLOWED } else { perms }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ow(kind: &str, id: &str, allow: u64, deny: u64) -> Overwrite {
        Overwrite { target_type: kind.into(), target_id: id.into(), allow: allow.to_string(), deny: deny.to_string() }
    }

    fn member<'a>(perms: u64, roles: &'a [String], timed_out: bool) -> Member<'a> {
        Member { user_id: "@a:x", guild_id: "G", guild_permissions: perms, role_ids: roles, timed_out }
    }

    #[test]
    fn overwrite_order() {
        let roles = vec!["R".to_string()];
        let base = VIEW_CHANNEL | SEND_MESSAGES;
        // @everyone denies sending, the role allows it back, the member overwrite denies it again.
        let o = [ow("role", "G", 0, SEND_MESSAGES), ow("role", "R", SEND_MESSAGES, 0)];
        assert_ne!(channel_permissions(&member(base, &roles, false), &o) & SEND_MESSAGES, 0);
        let o2 = [o[0].clone(), o[1].clone(), ow("member", "@a:x", 0, SEND_MESSAGES)];
        assert_eq!(channel_permissions(&member(base, &roles, false), &o2) & SEND_MESSAGES, 0);
    }

    #[test]
    fn admin_and_timeout() {
        let o = [ow("role", "G", 0, u64::MAX)];
        assert_eq!(channel_permissions(&member(ADMINISTRATOR, &[], false), &o), u64::MAX);
        assert_eq!(channel_permissions(&member(u64::MAX, &[], true), &o), TIMEOUT_ALLOWED);
        assert_eq!(channel_permissions(&member(VIEW_CHANNEL | SEND_MESSAGES, &[], true), &[]) & SEND_MESSAGES, 0);
    }
}
