//! Keyboard navigation helpers: stepping through channels and the quick switcher's matching.

/// The next item after (or before) `current` that passes `keep`, wrapping around.
pub fn step(order: &[String], current: Option<&str>, direction: i32, keep: impl Fn(&str) -> bool) -> Option<String> {
    if order.is_empty() {
        return None;
    }
    let n = order.len() as i32;
    let start = current.and_then(|c| order.iter().position(|o| o == c)).map(|i| i as i32).unwrap_or(-1);
    let mut i = start;
    for _ in 0..n {
        i = (i + direction.signum()).rem_euclid(n);
        let candidate = &order[i as usize];
        if Some(candidate.as_str()) != current && keep(candidate) {
            return Some(candidate.clone());
        }
    }
    None
}

/// Scores `text` against a typed query: consecutive and word-start matches score higher;
/// `None` when not all query characters appear in order.
pub fn fuzzy(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0;
    let mut qi = 0;
    let mut last: Option<usize> = None;
    for (ti, ch) in t.iter().enumerate() {
        if qi < q.len() && *ch == q[qi] {
            score += 1;
            if last.is_some_and(|l| l + 1 == ti) {
                score += 3;
            }
            if ti == 0 || !t[ti - 1].is_alphanumeric() {
                score += 4;
            }
            last = Some(ti);
            qi += 1;
        }
    }
    if qi < q.len() {
        return None;
    }
    // Shorter names win ties.
    Some(score * 100 - t.len() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn stepping_wraps_and_filters() {
        let order = ids(&["a", "b", "c"]);
        assert_eq!(step(&order, Some("a"), 1, |_| true).as_deref(), Some("b"));
        assert_eq!(step(&order, Some("a"), -1, |_| true).as_deref(), Some("c"));
        assert_eq!(step(&order, Some("c"), 1, |_| true).as_deref(), Some("a"));
        assert_eq!(step(&order, Some("a"), 1, |c| c == "c").as_deref(), Some("c"));
        assert_eq!(step(&order, Some("a"), 1, |_| false), None);
        assert_eq!(step(&order, None, 1, |_| true).as_deref(), Some("a"));
    }

    #[test]
    fn fuzzy_ranks_word_starts_and_runs() {
        assert!(fuzzy("gen", "general").unwrap() > fuzzy("gen", "a-long-general-channel").unwrap());
        assert!(fuzzy("ann", "announcements").is_some());
        assert!(fuzzy("xyz", "general").is_none());
        assert!(fuzzy("gl", "general-lounge").unwrap() > fuzzy("gl", "angle").unwrap());
    }
}
