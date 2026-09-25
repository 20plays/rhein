//! Picking the nearest SFU region before joining, as `web/src/lib/voiceRegion.js` does it: at
//! most four healthy candidates, two `GET {sfu}/health` rounds each within a 2 s budget, the
//! smallest round trip wins, and no preference at all ("auto") unless the measurement says
//! something the server's default does not.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

const LIMIT: usize = 4;
const ROUNDS: u32 = 2;
const BUDGET: Duration = Duration::from_secs(2);
const MARGIN: Duration = Duration::from_millis(25);
const TTL: Duration = Duration::from_secs(10 * 60);

/// A candidate from `GET /voice/regions`.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub name: String,
    pub sfu: String,
}

/// The healthy, addressed candidates of a `/voice/regions` answer, at most four.
pub fn candidates(published: &Value) -> Vec<Candidate> {
    published["regions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["healthy"].as_bool() != Some(false))
        .filter_map(|r| Some(Candidate { name: r["name"].as_str()?.to_owned(), sfu: r["sfu"].as_str().filter(|s| !s.is_empty())?.to_owned() }))
        .take(LIMIT)
        .collect()
}

/// The region to ask for, given what was measured (`None` for no answer), or `None` for no
/// preference.
pub fn pick(measured: &[(String, Option<Duration>)], default: Option<&str>) -> Option<String> {
    if measured.len() < 2 {
        return None;
    }
    let mut answered: Vec<(&str, Duration)> = measured.iter().filter_map(|(n, rtt)| Some((n.as_str(), (*rtt)?))).collect();
    answered.sort_by_key(|(_, rtt)| *rtt);
    let (best, best_rtt) = *answered.first()?;
    if Some(best) == default {
        return None;
    }
    // Within a hair of the default is jitter, not a reason to scatter a guild across regions.
    if let Some((_, incumbent)) = answered.iter().find(|(n, _)| Some(*n) == default)
        && *incumbent - best_rtt < MARGIN
    {
        return None;
    }
    Some(best.to_owned())
}

/// The smallest round trip to `{sfu}/health` over a few rounds, if any answered.
pub async fn probe(http: &reqwest::Client, sfu: &str, rounds: u32, timeout: Duration) -> Option<Duration> {
    let url = format!("{}/health", sfu.trim_end_matches('/'));
    let mut best: Option<Duration> = None;
    for _ in 0..rounds {
        let started = Instant::now();
        // Any HTTP answer is a round trip; only no answer is no measurement.
        if tokio::time::timeout(timeout, http.get(&url).send()).await.is_ok_and(|r| r.is_ok()) {
            let rtt = started.elapsed();
            best = Some(best.map_or(rtt, |b| b.min(rtt)));
        }
    }
    best
}

/// Candidate set → when it was measured and what that chose.
type Measured = HashMap<String, (Instant, Option<String>)>;
static CACHE: Mutex<Option<Measured>> = Mutex::new(None);

/// The region to put in a join request: the one the person chose, or for `auto` the nearest by
/// measurement (cached for ten minutes per candidate set). `None` means "auto".
pub async fn choose(http: &reqwest::Client, published: &Value, preferred: Option<&str>) -> Option<String> {
    if let Some(preferred) = preferred.filter(|p| *p != "auto") {
        return Some(preferred.to_owned());
    }
    let candidates = candidates(published);
    let key = candidates.iter().map(|c| c.sfu.as_str()).collect::<Vec<_>>().join("|");
    if let Some((at, name)) = CACHE.lock().unwrap().get_or_insert_with(HashMap::new).get(&key)
        && at.elapsed() < TTL
    {
        return name.clone();
    }
    let timeout = BUDGET / ROUNDS;
    let probes = candidates.iter().map(|c| async move { (c.name.clone(), probe(http, &c.sfu, ROUNDS, timeout).await) });
    let measured = futures::future::join_all(probes).await;
    let choice = pick(&measured, published["default"].as_str());
    CACHE.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, (Instant::now(), choice.clone()));
    choice
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Option<Duration> {
        Some(Duration::from_millis(n))
    }

    #[test]
    fn the_nearest_wins_unless_the_default_is_close() {
        let measured = vec![("eu".to_owned(), ms(80)), ("us".to_owned(), ms(20)), ("asia".to_owned(), None)];
        assert_eq!(pick(&measured, Some("eu")).as_deref(), Some("us"));
        // The winner is the default already: no preference.
        assert_eq!(pick(&measured, Some("us")), None);
        // Within 25 ms of the default: no preference.
        let close = vec![("eu".to_owned(), ms(30)), ("us".to_owned(), ms(20))];
        assert_eq!(pick(&close, Some("eu")), None);
        // One candidate is not a choice; nothing measurable is no preference.
        assert_eq!(pick(&measured[..1], None), None);
        assert_eq!(pick(&[("a".into(), None), ("b".into(), None)], None), None);
    }

    #[test]
    fn unhealthy_and_unaddressed_regions_are_not_candidates() {
        let published = serde_json::json!({ "default": "a", "regions": [
            { "name": "a", "sfu": "http://a", "healthy": true },
            { "name": "b", "sfu": "http://b", "healthy": false },
            { "name": "c", "sfu": "" },
            { "name": "d", "sfu": "http://d" },
        ]});
        let names: Vec<String> = candidates(&published).into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["a", "d"]);
    }
}
