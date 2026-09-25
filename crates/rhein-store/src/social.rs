//! Friends and private conversations: pure helpers the client and its tests share.

use rhein_proto::{Conversation, Friend};

/// Applies a `FRIEND_UPDATE` (insert or replace by user id).
pub fn upsert_friend(list: &mut Vec<Friend>, friend: Friend) {
    match list.iter_mut().find(|f| f.user_id == friend.user_id) {
        Some(existing) => *existing = friend,
        None => list.push(friend),
    }
}

/// Applies a `FRIEND_REMOVE`.
pub fn remove_friend(list: &mut Vec<Friend>, user_id: &str) {
    list.retain(|f| f.user_id != user_id);
}

pub fn incoming_requests(list: &[Friend]) -> usize {
    list.iter().filter(|f| f.state == "pending_in").count()
}

/// What a `FRIEND_UPDATE` means to say out loud, from the state before it and after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FriendNews {
    /// Someone sent us a request.
    Requested,
    /// Someone took up the request we sent.
    Accepted,
}

pub fn friend_news(before: Option<&str>, after: &str) -> Option<FriendNews> {
    match (before, after) {
        (Some("pending_in"), "pending_in") => None,
        (_, "pending_in") => Some(FriendNews::Requested),
        (Some("pending_out"), "accepted") => Some(FriendNews::Accepted),
        _ => None,
    }
}

/// How a request we have looked at is remembered: whose account it came to, and from whom.
pub fn seen_key(me: &str, from: &str) -> String {
    format!("{me} {from}")
}

/// Incoming requests to `me` not yet looked at.
pub fn unseen_requests(list: &[Friend], me: &str, seen: &[String]) -> usize {
    list.iter().filter(|f| f.state == "pending_in" && !seen.contains(&seen_key(me, &f.user_id))).count()
}

/// A conversation's title from this user's point of view: the other person for a DM, the
/// members (or the group's own name) for a group. `name` resolves a user id to a display name.
pub fn conversation_title(c: &Conversation, me: &str, name: impl Fn(&str) -> String) -> String {
    if let Some(title) = c
        .metadata
        .as_ref()
        .and_then(|m| m.get("name"))
        .and_then(|n| n.as_str())
        .filter(|n| !n.trim().is_empty())
    {
        return title.to_owned();
    }
    let others: Vec<String> = c.members.iter().filter(|m| m.user_id != me).map(|m| name(&m.user_id)).collect();
    match others.len() {
        0 => "Just you".into(),
        1 => others[0].clone(),
        2 | 3 => others.join(", "),
        n => format!("{}, {} and {} others", others[0], others[1], n - 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhein_proto::ConversationMember;

    fn friend(id: &str, state: &str) -> Friend {
        Friend { user_id: id.into(), state: state.into(), ..Default::default() }
    }

    #[test]
    fn friend_events() {
        let mut list = Vec::new();
        upsert_friend(&mut list, friend("@b:x", "pending_in"));
        upsert_friend(&mut list, friend("@c:x", "pending_out"));
        assert_eq!(incoming_requests(&list), 1);
        upsert_friend(&mut list, friend("@b:x", "accepted"));
        assert_eq!(list.len(), 2);
        assert_eq!(incoming_requests(&list), 0);
        remove_friend(&mut list, "@c:x");
        assert_eq!(list.iter().map(|f| f.user_id.as_str()).collect::<Vec<_>>(), vec!["@b:x"]);
    }

    #[test]
    fn what_a_friend_update_says() {
        assert_eq!(friend_news(None, "pending_in"), Some(FriendNews::Requested));
        assert_eq!(friend_news(Some("pending_in"), "pending_in"), None);
        assert_eq!(friend_news(Some("pending_out"), "accepted"), Some(FriendNews::Accepted));
        // Accepting someone else's request is our own doing: nothing to announce.
        assert_eq!(friend_news(Some("pending_in"), "accepted"), None);
        assert_eq!(friend_news(None, "pending_out"), None);
        assert_eq!(friend_news(Some("accepted"), "blocked"), None);
    }

    #[test]
    fn requests_looked_at() {
        let list = vec![friend("@b:x", "pending_in"), friend("@c:x", "pending_in"), friend("@d:x", "accepted")];
        assert_eq!(unseen_requests(&list, "@me:x", &[]), 2);
        let seen = vec![seen_key("@me:x", "@b:x"), seen_key("@other:x", "@c:x")];
        assert_eq!(unseen_requests(&list, "@me:x", &seen), 1);
    }

    #[test]
    fn titles() {
        let member = |id: &str| ConversationMember { user_id: id.into(), ..Default::default() };
        let dm = Conversation { kind: "dm".into(), members: vec![member("@me:x"), member("@ben:x")], ..Default::default() };
        let name = |id: &str| id.trim_start_matches('@').split(':').next().unwrap().to_owned();
        assert_eq!(conversation_title(&dm, "@me:x", name), "ben");
        let gc = Conversation {
            kind: "gc".into(),
            members: ["@me:x", "@a:x", "@b:x", "@c:x", "@d:x"].iter().map(|i| member(i)).collect(),
            ..Default::default()
        };
        assert_eq!(conversation_title(&gc, "@me:x", name), "a, b and 2 others");
        let named = Conversation { metadata: Some(serde_json::json!({"name": "Crew"})), ..gc };
        assert_eq!(conversation_title(&named, "@me:x", name), "Crew");
    }
}
