//! TheSportsDB (free tier) for sports programmes without a poster: match
//! titles such as "Premier League: Arsenal v Chelsea" or "Lakers at Celtics"
//! are looked up as events, and the event's poster or thumbnail is used.
//! Results are kept in the sportsdb_cache table. The free tier allows about
//! 30 requests a minute; we stay below that.

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::timeutil;

/// Time between requests (about 27 a minute).
const GAP: Duration = Duration::from_millis(2_200);
const BACKOFF: Duration = Duration::from_secs(30);
const FOUND_TTL: i64 = 3 * 86_400;
const NOT_FOUND_TTL: i64 = 14 * 86_400;
const SAVE_BATCH: usize = 25;

#[derive(Debug, Clone, Default)]
pub struct Event {
    pub found: bool,
    pub poster: Option<String>,
    pub fetched_at: i64,
}

pub struct Cache {
    entries: HashMap<String, Event>,
    pending: Vec<(String, Event)>,
}

impl Cache {
    pub fn load(conn: &Connection) -> Result<Cache> {
        let mut st = conn.prepare("SELECT query, found, poster, fetched_at FROM sportsdb_cache")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Event { found: r.get::<_, i64>(1)? != 0, poster: r.get(2)?, fetched_at: r.get(3)? },
            ))
        })?;
        let mut entries = HashMap::new();
        for row in rows {
            let (k, v) = row?;
            entries.insert(k, v);
        }
        Ok(Cache { entries, pending: Vec::new() })
    }

    /// The poster for this event query, if one was found.
    pub fn poster(&self, query: &str) -> Option<&str> {
        self.entries.get(query).filter(|e| e.found).and_then(|e| e.poster.as_deref())
    }

    pub fn is_fresh(&self, query: &str, now: i64) -> bool {
        self.entries
            .get(query)
            .is_some_and(|e| now - e.fetched_at < (if e.found { FOUND_TTL } else { NOT_FOUND_TTL }))
    }

    fn insert(&mut self, query: String, e: Event) {
        self.entries.insert(query.clone(), e.clone());
        self.pending.push((query, e));
    }

    pub fn save(&mut self, conn: &Connection) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let tx = conn.unchecked_transaction()?;
        {
            let mut st = tx.prepare(
                "INSERT OR REPLACE INTO sportsdb_cache (query, found, poster, fetched_at) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (k, e) in &self.pending {
                st.execute(params![k, e.found, e.poster, e.fetched_at])?;
            }
        }
        tx.commit()?;
        self.pending.clear();
        Ok(())
    }
}

/// "Premier League: Arsenal v Chelsea" -> "Arsenal_vs_Chelsea"; "Lakers at Celtics" ->
/// "Celtics_vs_Lakers" (the home team first). None when the title is not a match.
pub fn event_query(title: &str) -> Option<String> {
    // Drop a competition prefix ("Premier League: ") and a trailing note ("(Live)", " - Live").
    let t = title.rsplit_once(": ").map_or(title, |(_, rest)| rest);
    let t = t.split(" - ").next().unwrap_or(t);
    let t = t.split(" (").next().unwrap_or(t).trim();
    let lower = t.to_lowercase();
    let mut found: Option<(usize, usize, bool)> = None;
    for (sep, away_first) in [(" vs. ", false), (" vs ", false), (" v ", false), (" at ", true), (" @ ", true)] {
        if let Some(i) = lower.find(sep) {
            found = Some((i, sep.len(), away_first));
            break;
        }
    }
    let (i, len, away_first) = found?;
    let a = t[..i].trim();
    let b = t[i + len..].trim();
    let ok = |s: &str| s.len() >= 2 && s.len() <= 40 && s.chars().any(char::is_alphabetic);
    if !ok(a) || !ok(b) {
        return None;
    }
    let (home, away) = if away_first { (b, a) } else { (a, b) };
    let clean = |s: &str| s.split_whitespace().collect::<Vec<_>>().join("_");
    Some(format!("{}_vs_{}", clean(home), clean(away)))
}

/// The best image among the events found: the latest event with a poster or thumbnail.
fn pick_event(v: &Value, now: i64) -> Event {
    let events = v.get("event").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut best: Option<(String, String)> = None; // (dateEvent, url)
    for e in &events {
        let url = ["strPoster", "strThumb", "strBanner"]
            .iter()
            .filter_map(|k| e.get(*k).and_then(Value::as_str))
            .find(|u| u.starts_with("http"));
        let Some(url) = url else { continue };
        let date = e.get("dateEvent").and_then(Value::as_str).unwrap_or("").to_string();
        if best.as_ref().map_or(true, |(d, _)| date > *d) {
            best = Some((date, url.to_string()));
        }
    }
    match best {
        Some((_, url)) => Event { found: true, poster: Some(url), fetched_at: now },
        None => Event { found: false, poster: None, fetched_at: now },
    }
}

enum Answer {
    Event(Event),
    RateLimited,
}

async fn lookup(client: &reqwest::Client, url: &str, query: &str) -> Result<Answer> {
    let now = timeutil::now_unix();
    let r = client
        .get(url)
        .query(&[("e", query)])
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| anyhow!("TheSportsDB: {}", e.without_url()))?;
    match r.status().as_u16() {
        429 => Ok(Answer::RateLimited),
        s if !(200..300).contains(&s) => bail!("TheSportsDB: HTTP {s}"),
        _ => {
            let body = r.bytes().await.map_err(|e| anyhow!("TheSportsDB: {}", e.without_url()))?;
            // The free tier answers an unknown event with an empty body or {"event":null}.
            let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            Ok(Answer::Event(pick_event(&v, now)))
        }
    }
}

/// Look the events up one by one, politely, until done, `deadline` or a cancel.
/// Returns (looked up, found).
pub async fn lookup_all(
    client: &reqwest::Client,
    url: &str,
    cache: &mut Cache,
    conn: &Connection,
    queries: Vec<String>,
    deadline: Option<Instant>,
    cancel: &AtomicBool,
) -> (usize, usize) {
    let (mut done, mut found, mut errors) = (0usize, 0usize, 0usize);
    for q in queries {
        if cancel.load(Ordering::Relaxed) || deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        tokio::time::sleep(GAP).await;
        let mut answer = lookup(client, url, &q).await;
        if matches!(answer, Ok(Answer::RateLimited)) {
            tokio::time::sleep(BACKOFF).await;
            answer = lookup(client, url, &q).await;
        }
        match answer {
            Ok(Answer::Event(e)) => {
                done += 1;
                if e.found {
                    found += 1;
                }
                cache.insert(q, e);
            }
            Ok(Answer::RateLimited) => {}
            Err(e) => {
                errors += 1;
                if errors <= 5 {
                    eprintln!("warning: {e:#}");
                }
            }
        }
        if cache.pending.len() >= SAVE_BATCH {
            if let Err(e) = cache.save(conn) {
                eprintln!("warning: saving the TheSportsDB cache: {e:#}");
            }
        }
    }
    if let Err(e) = cache.save(conn) {
        eprintln!("warning: saving the TheSportsDB cache: {e:#}");
    }
    (done, found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_match_titles() {
        assert_eq!(event_query("Premier League: Arsenal v Chelsea").as_deref(), Some("Arsenal_vs_Chelsea"));
        assert_eq!(event_query("Los Angeles Lakers at Boston Celtics").as_deref(), Some("Boston_Celtics_vs_Los_Angeles_Lakers"));
        assert_eq!(event_query("NFL: Bears vs. Packers (Live)").as_deref(), Some("Bears_vs_Packers"));
        assert_eq!(event_query("Liverpool vs Everton - Live").as_deref(), Some("Liverpool_vs_Everton"));
        assert_eq!(event_query("SportsCenter"), None);
        assert_eq!(event_query("Match of the Day"), None);
    }

    #[test]
    fn picks_the_latest_event_with_an_image() {
        let v: Value = serde_json::from_str(
            r#"{"event":[
                {"dateEvent":"2023-01-01","strThumb":"https://t/old.jpg"},
                {"dateEvent":"2024-05-01","strPoster":"https://p/new.jpg","strThumb":"https://t/new.jpg"},
                {"dateEvent":"2025-01-01"}
            ]}"#,
        )
        .unwrap();
        let e = pick_event(&v, 1);
        assert!(e.found);
        assert_eq!(e.poster.as_deref(), Some("https://p/new.jpg"));
        assert!(!pick_event(&Value::Null, 1).found);
    }
}
