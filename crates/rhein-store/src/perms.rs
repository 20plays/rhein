//! Channel permissions computed on the client, so the UI can hide actions the server would
//! refuse. Mirrors `channel_permissions` in Telium's `server/src/perms.rs`; the server still
//! decides every request.

use rhein_proto::Overwrite;

pub const VIEW_CHANNEL: u64 = 1 << 0;
pub const SEND_MESSAGES: u64 = 1 << 1;
pub const MANAGE_MESSAGES: u64 = 1 << 2;
pub const MANAGE_CHANNELS: u64 = 1 << 3;
pub const MANAGE_ROLES: u64 = 1 << 4;
pub const KICK_MEMBERS: u64 = 1 << 5;
pub const BAN_MEMBERS: u64 = 1 << 6;
pub const MANAGE_GUILD: u64 = 1 << 7;
pub const MANAGE_EXPRESSIONS: u64 = 1 << 8;
pub const CREATE_INVITE: u64 = 1 << 9;
pub const ATTACH_FILES: u64 = 1 << 10;
pub const EMBED_LINKS: u64 = 1 << 11;
pub const MENTION_EVERYONE: u64 = 1 << 12;
pub const MODERATE_MEMBERS: u64 = 1 << 13;
pub const USE_EXTERNAL_EMOJI: u64 = 1 << 14;
pub const CONNECT: u64 = 1 << 15;
pub const SPEAK: u64 = 1 << 16;
pub const VIDEO: u64 = 1 << 17;
pub const STREAM: u64 = 1 << 18;
pub const VIEW_AUDIT_LOG: u64 = 1 << 19;
pub const ADD_REACTIONS: u64 = 1 << 20;
pub const READ_MESSAGE_HISTORY: u64 = 1 << 21;
pub const PRIORITY_SPEAKER: u64 = 1 << 22;
pub const MUTE_MEMBERS: u64 = 1 << 23;
pub const DEAFEN_MEMBERS: u64 = 1 << 24;
pub const MOVE_MEMBERS: u64 = 1 << 25;
pub const ADMINISTRATOR: u64 = 1 << 40;
/// What a timed-out member keeps.
pub const TIMEOUT_ALLOWED: u64 = VIEW_CHANNEL | READ_MESSAGE_HISTORY;

/// The headings a role editor groups the switches under, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    General,
    Membership,
    Text,
    Voice,
    Advanced,
}

impl Group {
    pub const ALL: [Group; 5] = [Group::General, Group::Membership, Group::Text, Group::Voice, Group::Advanced];

    pub fn title(self) -> &'static str {
        match self {
            Group::General => "General Server Permissions",
            Group::Membership => "Membership Permissions",
            Group::Text => "Text Channel Permissions",
            Group::Voice => "Voice Channel Permissions",
            Group::Advanced => "Advanced Permissions",
        }
    }
}

/// One permission bit as a person reads it: the web client's names and hints
/// (`web/src/lib/i18n/catalog/en/perms.js`), which are security copy.
#[derive(Debug, Clone, Copy)]
pub struct Info {
    pub bit: u64,
    /// The server's name for it (`MANAGE_ROLES`).
    pub name: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    pub group: Group,
    /// Worth a second look before it is granted.
    pub danger: bool,
}

const fn info(bit: u64, name: &'static str, group: Group, danger: bool, label: &'static str, hint: &'static str) -> Info {
    Info { bit, name, label, hint, group, danger }
}

/// Every permission, in the order the web client lists them.
pub const ALL: &[Info] = &[
    info(VIEW_CHANNEL, "VIEW_CHANNEL", Group::General, false, "View Channels", "See channels they have not been shut out of by an overwrite."),
    info(MANAGE_CHANNELS, "MANAGE_CHANNELS", Group::General, false, "Manage Channels", "Create, rename, reorder and delete this guild’s channels."),
    info(MANAGE_ROLES, "MANAGE_ROLES", Group::General, true, "Manage Roles", "Edit roles below their own, and hand those roles to anyone. They can never grant a permission they do not already hold."),
    info(MANAGE_EXPRESSIONS, "MANAGE_EXPRESSIONS", Group::General, false, "Manage Emoji and Stickers", "Upload and remove the emoji and stickers this guild serves."),
    info(VIEW_AUDIT_LOG, "VIEW_AUDIT_LOG", Group::General, false, "View Audit Log", "Read the record of who changed what on this server."),
    info(MANAGE_GUILD, "MANAGE_GUILD", Group::General, true, "Manage Server", "Change the guild’s name, icon, join gate and voice region."),
    info(CREATE_INVITE, "CREATE_INVITE", Group::Membership, false, "Create Invite", "Mint invite codes that let accounts from any homeserver in."),
    info(KICK_MEMBERS, "KICK_MEMBERS", Group::Membership, false, "Kick Members", "Remove members ranked below them. A kick is not a ban: they can come back with a new invite."),
    info(BAN_MEMBERS, "BAN_MEMBERS", Group::Membership, true, "Ban Members", "Bar an account from rejoining, by user ID, across every homeserver."),
    info(MODERATE_MEMBERS, "MODERATE_MEMBERS", Group::Membership, false, "Timeout Members", "Silence a member for a while. A timed-out member keeps only reading."),
    info(SEND_MESSAGES, "SEND_MESSAGES", Group::Text, false, "Send Messages", "Post in text channels they can see."),
    info(EMBED_LINKS, "EMBED_LINKS", Group::Text, false, "Embed Links", "Have the links they post unfurl into previews."),
    info(ATTACH_FILES, "ATTACH_FILES", Group::Text, false, "Attach Files", "Upload images and files to this guild’s own media store."),
    info(ADD_REACTIONS, "ADD_REACTIONS", Group::Text, false, "Add Reactions", "Put a new reaction on a message. Joining an existing one needs nothing."),
    info(USE_EXTERNAL_EMOJI, "USE_EXTERNAL_EMOJI", Group::Text, false, "Use External Emoji", "Use emoji served by other guilds, on other homeservers."),
    info(MENTION_EVERYONE, "MENTION_EVERYONE", Group::Text, true, "Mention @everyone and Roles", "Ping the whole guild, or any role, whether or not that role is mentionable."),
    info(MANAGE_MESSAGES, "MANAGE_MESSAGES", Group::Text, true, "Manage Messages", "Delete and pin anyone’s messages."),
    info(READ_MESSAGE_HISTORY, "READ_MESSAGE_HISTORY", Group::Text, false, "Read Message History", "See what was said before they arrived. Without it a channel starts empty."),
    info(CONNECT, "CONNECT", Group::Voice, false, "Connect", "Join voice channels they can see."),
    info(SPEAK, "SPEAK", Group::Voice, false, "Speak", "Be heard in voice. Without it they can listen."),
    info(VIDEO, "VIDEO", Group::Voice, false, "Video", "Turn a camera on in voice."),
    info(STREAM, "STREAM", Group::Voice, false, "Go Live", "Share a screen in voice."),
    info(PRIORITY_SPEAKER, "PRIORITY_SPEAKER", Group::Voice, false, "Priority Speaker", "Be heard over everyone else while they talk."),
    info(MUTE_MEMBERS, "MUTE_MEMBERS", Group::Voice, false, "Mute Members", "Silence other people in voice channels."),
    info(DEAFEN_MEMBERS, "DEAFEN_MEMBERS", Group::Voice, false, "Deafen Members", "Stop other people hearing a voice channel."),
    info(MOVE_MEMBERS, "MOVE_MEMBERS", Group::Voice, false, "Move Members", "Drag people between voice channels, or out of them."),
    info(ADMINISTRATOR, "ADMINISTRATOR", Group::Advanced, true, "Administrator", "Every permission on this server, and every channel overwrite ignored. Only hand this to someone you would trust with the guild itself."),
];

/// Whether someone whose top role sits at `actor_top` (or who owns the guild) may touch a role
/// at `target`: only roles strictly below their own, as the server enforces.
pub fn outranks(actor_owner: bool, actor_top: i64, target: i64) -> bool {
    actor_owner || actor_top > target
}

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

    #[test]
    fn the_table_matches_the_web_client() {
        // Bits as in web/src/lib/perms.js PERMISSION_BITS; one entry per bit.
        let bits: Vec<(&str, u32)> = ALL.iter().map(|i| (i.name, i.bit.trailing_zeros())).collect();
        assert_eq!(bits.len(), 27);
        assert!(bits.contains(&("MANAGE_ROLES", 4)) && bits.contains(&("MOVE_MEMBERS", 25)) && bits.contains(&("ADMINISTRATOR", 40)));
        let mut seen = std::collections::HashSet::new();
        assert!(ALL.iter().all(|i| i.bit.count_ones() == 1 && seen.insert(i.bit)));
        assert!(Group::ALL.iter().all(|g| ALL.iter().any(|i| i.group == *g)));
        assert!(outranks(true, 0, 99) && outranks(false, 5, 4) && !outranks(false, 5, 5));
    }

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
