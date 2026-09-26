//! Rich presence: what someone is playing, listening to or watching. The shape and the limits
//! follow the web client (`web/src/lib/activity.js`): presence is network input, so everything is
//! normalised on the way in and on the way out.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The v1 carrier: older servers pass `activity` through as a plain string.
pub const ENVELOPE_PREFIX: &str = "telium-rich-v1:";
const MAX_TEXT: usize = 128;
const MAX_APP_ID: usize = 64;
const MAX_URL: usize = 1024;
const MAX_ARTWORK_URL: usize = 512;
const MAX_PARTY: u64 = 1_000_000;
const MAX_ENVELOPE_BYTES: usize = 4096;
/// One game, one music and one video activity at most.
pub const MAX_ACTIVITIES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Game,
    Music,
    Video,
}

impl Slot {
    pub const ALL: [Slot; 3] = [Slot::Game, Slot::Music, Slot::Video];

    /// The name a preference stores it under.
    pub fn key(self) -> &'static str {
        match self {
            Slot::Game => "game",
            Slot::Music => "music",
            Slot::Video => "video",
        }
    }

    pub fn from_key(key: &str) -> Option<Slot> {
        Slot::ALL.into_iter().find(|s| s.key() == key)
    }
}

/// `list` with the activity of the `primary` slot moved to the front, the rest in their order.
/// Viewers show the first activity where there is room for one (a member list, say).
pub fn primary_first(mut list: Vec<Activity>, primary: Option<Slot>) -> Vec<Activity> {
    if let Some(slot) = primary {
        list.sort_by_key(|a| a.slot() != slot);
    }
    list
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Timestamps {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Party {
    pub size: [u64; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityButton {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Assets {
    pub large_image: Option<String>,
    pub large_text: Option<String>,
    pub small_image: Option<String>,
    pub small_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    pub application_id: Option<String>,
    pub name: String,
    /// 0 playing, 1 streaming, 2 listening, 3 watching, 4 custom, 5 competing.
    #[serde(rename = "type")]
    pub kind: u8,
    pub details: Option<String>,
    pub state: Option<String>,
    pub timestamps: Option<Timestamps>,
    pub party: Option<Party>,
    pub buttons: Vec<ActivityButton>,
    pub assets: Option<Assets>,
}

impl Activity {
    pub fn slot(&self) -> Slot {
        match self.kind {
            2 => Slot::Music,
            1 | 3 => Slot::Video,
            _ => Slot::Game,
        }
    }

    /// "Playing", "Listening to", …, as a line above the activity.
    pub fn verb(&self) -> &'static str {
        match self.kind {
            1 => "Streaming",
            2 => "Listening to",
            3 => "Watching",
            5 => "Competing in",
            _ => "Playing",
        }
    }

    /// A one-line summary for lists: "Playing Factorio", "Listening to Spotify".
    pub fn summary(&self) -> String {
        // The name, whatever the kind, as the web client and Discord put it: an app chooses what
        // it goes by (Sonora the service, the artist or the title), the song is on the card.
        format!("{} {}", self.verb(), self.name)
    }

    /// Normalises untrusted input the way the web client does; `None` when nothing is left.
    pub fn normalize(value: &Value) -> Option<Activity> {
        let obj = value.as_object()?;
        let application_id = text(obj.get("application_id"), MAX_APP_ID);
        let supplied_name = text(obj.get("name"), MAX_TEXT);
        let details = text(obj.get("details"), MAX_TEXT);
        let state = text(obj.get("state"), MAX_TEXT);
        let kind = obj.get("type").and_then(Value::as_u64).filter(|t| *t <= 5).unwrap_or(0) as u8;

        let stamps = obj.get("timestamps");
        let timestamps = Timestamps {
            start: stamps.and_then(|t| t.get("start")).and_then(integer).filter(|n| *n > 0),
            end: stamps.and_then(|t| t.get("end")).and_then(integer).filter(|n| *n > 0),
        };
        let timestamps = (timestamps.start.is_some() || timestamps.end.is_some()).then_some(timestamps);

        let party = obj.get("party").and_then(|p| p.get("size")).and_then(Value::as_array).and_then(|size| {
            let current = size.first().and_then(integer)?;
            let maximum = size.get(1).and_then(integer)?;
            (current <= maximum && maximum <= MAX_PARTY).then_some(Party { size: [current, maximum] })
        });

        let buttons = obj
            .get("buttons")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .take(2)
                    .filter_map(|b| {
                        let label = text(b.get("label"), 32)?;
                        let url = safe_url(b.get("url"), MAX_URL)?;
                        Some(ActivityButton { label, url })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let name = supplied_name.or_else(|| {
            (application_id.is_some() || details.is_some() || state.is_some()).then(|| "Desktop activity".to_owned())
        })?;
        Some(Activity {
            application_id,
            name,
            kind,
            details,
            state,
            timestamps,
            party,
            buttons,
            assets: obj.get("assets").and_then(artwork),
        })
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// The v1 string envelope for `activity`, or `None` when it would be too large.
pub fn encode_envelope(activity: &Activity) -> Option<String> {
    let payload = serde_json::to_string(activity).ok()?;
    (payload.len() <= MAX_ENVELOPE_BYTES).then(|| format!("{ENVELOPE_PREFIX}{payload}"))
}

pub fn decode_envelope(value: &str) -> Option<Activity> {
    let raw = value.strip_prefix(ENVELOPE_PREFIX)?;
    if raw.is_empty() || raw.len() > MAX_ENVELOPE_BYTES {
        return None;
    }
    Activity::normalize(&serde_json::from_str(raw).ok()?)
}

/// At most one activity per slot, in order, capped at three.
pub fn normalize_all(values: &[Value]) -> Vec<Activity> {
    let mut out: Vec<Activity> = Vec::new();
    for activity in values.iter().filter_map(Activity::normalize) {
        if out.iter().any(|a| a.slot() == activity.slot()) {
            continue;
        }
        out.push(activity);
        if out.len() >= MAX_ACTIVITIES {
            break;
        }
    }
    out
}

/// Everything a presence object says someone is doing: `rich_activities`, else the single
/// `rich_activity`, else a v1 envelope in `activity`.
pub fn activities_of(presence: &Value) -> Vec<Activity> {
    if let Some(list) = presence.get("rich_activities").and_then(Value::as_array) {
        let many = normalize_all(list);
        if !many.is_empty() {
            return many;
        }
    }
    presence
        .get("rich_activity")
        .and_then(Activity::normalize)
        .or_else(|| presence.get("activity").and_then(Value::as_str).and_then(decode_envelope))
        .into_iter()
        .collect()
}

fn text(value: Option<&Value>, max: usize) -> Option<String> {
    let trimmed = value?.as_str()?.trim();
    (!trimmed.is_empty()).then(|| trimmed.chars().take(max).collect())
}

fn integer(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64().filter(|n| *n <= (1 << 53)),
        Value::String(s) => s.parse::<u64>().ok().filter(|n| *n <= (1 << 53)),
        _ => None,
    }
}

/// http(s) without credentials only.
fn safe_url(value: Option<&Value>, max: usize) -> Option<String> {
    let raw = text(value, max)?;
    let url = url::Url::parse(&raw).ok()?;
    let web = matches!(url.scheme(), "http" | "https");
    (web && url.username().is_empty() && url.password().is_none()).then(|| url.to_string())
}

fn artwork(value: &Value) -> Option<Assets> {
    value.as_object()?;
    let large_image = safe_url(value.get("large_image"), MAX_ARTWORK_URL);
    let small_image = safe_url(value.get("small_image"), MAX_ARTWORK_URL);
    if large_image.is_none() && small_image.is_none() {
        return None;
    }
    Some(Assets {
        large_text: large_image.as_ref().and_then(|_| text(value.get("large_text"), MAX_TEXT)),
        small_text: small_image.as_ref().and_then(|_| text(value.get("small_text"), MAX_TEXT)),
        large_image,
        small_image,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_primary_activity_goes_first() {
        let game = Activity::normalize(&json!({ "name": "Zed" })).unwrap();
        let music = Activity::normalize(&json!({ "name": "Title Fight", "type": 2 })).unwrap();
        let names = |l: Vec<Activity>| l.into_iter().map(|a| a.name).collect::<Vec<_>>();
        let both = vec![game.clone(), music.clone()];
        assert_eq!(names(primary_first(both.clone(), None)), ["Zed", "Title Fight"]);
        assert_eq!(names(primary_first(both.clone(), Some(Slot::Music))), ["Title Fight", "Zed"]);
        // A chosen slot with nothing in it changes nothing.
        assert_eq!(names(primary_first(both, Some(Slot::Video))), ["Zed", "Title Fight"]);
        assert_eq!(Slot::from_key(Slot::Music.key()), Some(Slot::Music));
    }

    #[test]
    fn normalizes_like_the_web_client() {
        let a = Activity::normalize(&json!({
            "application_id": "123",
            "details": "  Level 3  ",
            "type": 9,
            "timestamps": { "start": 1700000000000u64, "end": 0 },
            "party": { "size": [5, 2] },
            "buttons": [
                { "label": "Join", "url": "https://example.org/join" },
                { "label": "Bad", "url": "javascript:alert(1)" },
                { "label": "Creds", "url": "https://u:p@example.org" },
            ],
            "assets": { "large_image": "https://cdn.example/x.png", "large_text": "Map", "small_text": "orphan" },
        }))
        .unwrap();
        // A missing name falls back to a generic label; an out-of-range type is "playing".
        assert_eq!(a.name, "Desktop activity");
        assert_eq!(a.kind, 0);
        assert_eq!(a.details.as_deref(), Some("Level 3"));
        assert_eq!(a.timestamps, Some(Timestamps { start: Some(1700000000000), end: None }));
        assert_eq!(a.party, None);
        assert_eq!(a.buttons, vec![ActivityButton { label: "Join".into(), url: "https://example.org/join".into() }]);
        let assets = a.assets.unwrap();
        assert_eq!(assets.large_text.as_deref(), Some("Map"));
        assert_eq!(assets.small_text, None);
        assert!(Activity::normalize(&json!({ "type": 0 })).is_none());
    }

    #[test]
    fn one_activity_per_slot() {
        let list = [
            json!({ "name": "Game A" }),
            json!({ "name": "Game B" }),
            json!({ "name": "Spotify", "type": 2, "details": "Song" }),
            json!({ "name": "Video", "type": 3 }),
        ];
        let out = normalize_all(&list);
        let names: Vec<&str> = out.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Game A", "Spotify", "Video"]);
        // The name the app goes by, as in the web client; the song is on the card.
        assert_eq!(out[1].summary(), "Listening to Spotify");
    }

    #[test]
    fn envelope_round_trip_and_fallbacks() {
        let a = Activity::normalize(&json!({ "name": "Factorio" })).unwrap();
        let envelope = encode_envelope(&a).unwrap();
        assert!(envelope.starts_with(ENVELOPE_PREFIX));
        assert_eq!(decode_envelope(&envelope), Some(a.clone()));
        assert_eq!(activities_of(&json!({ "status": "online", "activity": envelope })), vec![a.clone()]);
        assert_eq!(activities_of(&json!({ "rich_activity": { "name": "Factorio" } })), vec![a]);
        assert!(activities_of(&json!({ "activity": "just text" })).is_empty());
    }
}
