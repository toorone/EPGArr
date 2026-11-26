//! TVmaze (free, no key) as a fallback for series: when TMDB has no match for
//! a TV title, or a match without a poster, TVmaze's best match gives the
//! poster and a short description. Results are kept in the tvmaze_cache
//! table. TVmaze allows about 20 requests per 10 seconds; we stay below that.

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::timeutil;
use crate::tmdb::{LookupKey, TmdbCache};

/// Time between requests (about 1.8 per second).
const GAP: Duration = Duration::from_millis(550);
/// After HTTP 429, wait this long before going on.
const BACKOFF: Duration = Duration::from_secs(10);
const FOUND_TTL: i64 = 30 * 86_400;
const NOT_FOUND_TTL: i64 = 14 * 86_400;
const SAVE_BATCH: usize = 50;

#[derive(Debug, Clone, Default)]
pub struct Show {
    pub found: bool,
    pub poster: Option<String>,
    pub summary: Option<String>,
    pub genres: Vec<String>,
    pub fetched_at: i64,
}

pub struct Cache {
    entries: HashMap<String, Show>,
    pending: Vec<(String, Show)>,
}

impl Cache {
    pub fn load(conn: &Connection) -> Result<Cache> {
        let mut st = conn.prepare("SELECT norm_title, found, poster, summary, genres, fetched_at FROM tvmaze_cache")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Show {
                    found: r.get::<_, i64>(1)? != 0,
                    poster: r.get(2)?,
                    summary: r.get(3)?,
                    genres: r
                        .get::<_, String>(4)?
                        .split('|')
                        .filter(|g| !g.is_empty())
                        .map(String::from)
                        .collect(),
                    fetched_at: r.get(5)?,
                },
            ))
        })?;
        let mut entries = HashMap::new();
        for row in rows {
            let (k, v) = row?;
            entries.insert(k, v);
        }
        Ok(Cache { entries, pending: Vec::new() })
    }

    pub fn get(&self, norm: &str) -> Option<&Show> {
        self.entries.get(norm)
    }

    fn is_fresh(&self, norm: &str, now: i64) -> bool {
        self.entries
            .get(norm)
            .is_some_and(|s| now - s.fetched_at < (if s.found { FOUND_TTL } else { NOT_FOUND_TTL }))
    }

    fn insert(&mut self, norm: String, show: Show) {
        self.entries.insert(norm.clone(), show.clone());
        self.pending.push((norm, show));
    }

    pub fn save(&mut self, conn: &Connection) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let tx = conn.unchecked_transaction()?;
        {
            let mut st = tx.prepare(
                "INSERT OR REPLACE INTO tvmaze_cache (norm_title, found, poster, summary, genres, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for (k, s) in &self.pending {
                st.execute(params![k, s.found, s.poster, s.summary, s.genres.join("|"), s.fetched_at])?;
            }
        }
        tx.commit()?;
        self.pending.clear();
        Ok(())
    }
}

/// TV titles worth asking TVmaze about: TMDB has no match, or a match without a
/// poster, and TVmaze was not asked recently. Soonest first is up to the caller.
pub fn candidates<'a>(keys: impl IntoIterator<Item = &'a LookupKey>, tmdb: &TmdbCache, tvm: &Cache) -> Vec<LookupKey> {
    let now = timeutil::now_unix();
    let mut seen = std::collections::HashSet::new();
    keys.into_iter()
        .filter(|k| k.is_tv)
        .filter(|k| tmdb.get(&k.key).map_or(true, |i| !i.found || i.poster_url().is_none()))
        .filter(|k| !tvm.is_fresh(&k.norm, now))
        .filter(|k| seen.insert(k.norm.clone()))
        .cloned()
        .collect()
}

/// "<p>A <b>great</b> show &amp; more</p>" -> "A great show & more"
pub fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn show_from(v: &Value, now: i64) -> Show {
    let img = v.get("image");
    let poster = img
        .and_then(|i| i.get("original").or_else(|| i.get("medium")))
        .and_then(Value::as_str)
        .filter(|u| u.starts_with("http"))
        .map(String::from);
    let summary = v
        .get("summary")
        .and_then(Value::as_str)
        .map(strip_html)
        .filter(|s| !s.is_empty());
    let genres = v
        .get("genres")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
        .unwrap_or_default();
    Show { found: true, poster, summary, genres, fetched_at: now }
}

enum Answer {
    Show(Show),
    RateLimited,
}

async fn lookup(client: &reqwest::Client, url: &str, query: &str) -> Result<Answer> {
    let now = timeutil::now_unix();
    let r = client
        .get(url)
        .query(&[("q", query)])
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| anyhow!("TVmaze: {}", e.without_url()))?;
    match r.status().as_u16() {
        404 => Ok(Answer::Show(Show { found: false, fetched_at: now, ..Default::default() })),
        429 => Ok(Answer::RateLimited),
        s if !(200..300).contains(&s) => bail!("TVmaze: HTTP {s}"),
        _ => {
            let body = r.bytes().await.map_err(|e| anyhow!("TVmaze: {}", e.without_url()))?;
            let v: Value = serde_json::from_slice(&body).map_err(|e| anyhow!("TVmaze: {e}"))?;
            Ok(Answer::Show(show_from(&v, now)))
        }
    }
}

/// Look the titles up one by one, politely, until done, `deadline` or a
/// cancel. Saves as it goes. Returns (looked up, found).
pub async fn lookup_all(
    client: &reqwest::Client,
    url: &str,
    cache: &mut Cache,
    conn: &Connection,
    titles: Vec<LookupKey>,
    deadline: Option<Instant>,
    cancel: &AtomicBool,
) -> (usize, usize) {
    let (mut done, mut found, mut errors) = (0usize, 0usize, 0usize);
    let total = titles.len();
    let mut last_log = Instant::now();
    for k in titles {
        if cancel.load(Ordering::Relaxed) || deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        tokio::time::sleep(GAP).await;
        let mut answer = lookup(client, url, &k.query).await;
        if matches!(answer, Ok(Answer::RateLimited)) {
            tokio::time::sleep(BACKOFF).await;
            answer = lookup(client, url, &k.query).await;
        }
        match answer {
            Ok(Answer::Show(s)) => {
                done += 1;
                if s.found {
                    found += 1;
                }
                cache.insert(k.norm.clone(), s);
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
                eprintln!("warning: saving the TVmaze cache: {e:#}");
            }
        }
        if last_log.elapsed() >= Duration::from_secs(10) {
            last_log = Instant::now();
            println!("TVmaze: {done}/{total} titles, {found} found");
        }
    }
    if let Err(e) = cache.save(conn) {
        eprintln!("warning: saving the TVmaze cache: {e:#}");
    }
    (done, found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_html() {
        assert_eq!(strip_html("<p>A <b>great</b> show &amp; more</p>"), "A great show & more");
        assert_eq!(strip_html("No tags"), "No tags");
    }

    #[test]
    fn reads_a_show() {
        let v: Value = serde_json::from_str(
            r#"{"name":"X","genres":["Drama"],"image":{"medium":"https://m/x.jpg","original":"https://o/x.jpg"},"summary":"<p>Hi</p>"}"#,
        )
        .unwrap();
        let s = show_from(&v, 5);
        assert!(s.found);
        assert_eq!(s.poster.as_deref(), Some("https://o/x.jpg"));
        assert_eq!(s.summary.as_deref(), Some("Hi"));
        assert_eq!(s.genres, vec!["Drama"]);
    }

    #[test]
    fn cache_round_trip() {
        let conn = crate::db::open_memory();
        let mut c = Cache::load(&conn).unwrap();
        c.insert("show a".into(), Show { found: true, poster: Some("https://p".into()), fetched_at: timeutil::now_unix(), ..Default::default() });
        c.save(&conn).unwrap();
        let c = Cache::load(&conn).unwrap();
        assert_eq!(c.get("show a").unwrap().poster.as_deref(), Some("https://p"));
        assert!(c.is_fresh("show a", timeutil::now_unix()));
    }
}
