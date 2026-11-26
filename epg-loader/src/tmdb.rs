//! TMDB lookups with an on-disk JSON cache and a polite rate limit.
//!
//! The API key is only ever sent to api.themoviedb.org. Error messages are
//! built without request URLs, so a v3 key in the query string cannot leak
//! into logs or status.json.

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinSet;

use crate::groups::normalize_words;

const API_BASE: &str = "https://api.themoviedb.org/3";
const IMAGE_BASE: &str = "https://image.tmdb.org/t/p/w500";
const FOUND_TTL: u64 = 90 * 86_400;
const NOT_FOUND_TTL: u64 = 14 * 86_400;
/// Token bucket: about 35 requests per second, small bursts.
const RATE_PER_SEC: f64 = 35.0;
const RATE_BURST: f64 = 10.0;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const PROGRESS_EVERY: Duration = Duration::from_secs(5);
/// After the time budget, lookups already in flight get this long to finish.
const BUDGET_GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbInfo {
    pub found: bool,
    /// "movie" or "tv"
    pub media_type: String,
    pub id: u64,
    pub name: String,
    pub poster_path: Option<String>,
    pub overview: Option<String>,
    pub genre_ids: Vec<u64>,
    pub certification: Option<String>,
    pub fetched_at: u64,
    /// The IMDb id ("tt0111161"), for the IMDb star rating.
    pub imdb_id: Option<String>,
    /// Looked up by a version that asks TMDB for the IMDb id; older found results are refreshed.
    pub imdb_checked: bool,
}

impl TmdbInfo {
    pub fn poster_url(&self) -> Option<String> {
        self.poster_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| format!("{IMAGE_BASE}{p}"))
    }

    pub fn rating_system(&self) -> &'static str {
        if self.media_type == "movie" {
            "MPAA"
        } else {
            "VCHIP"
        }
    }
}

/// One unique query. Programmes with the same key share a lookup.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LookupKey {
    pub key: String,
    pub query: String,
    pub norm: String,
    pub year: Option<i32>,
    pub is_tv: bool,
}

pub fn parse_year(date: &str) -> Option<i32> {
    let y: i32 = date.trim().get(0..4)?.parse().ok()?;
    (1900..=2100).contains(&y).then_some(y)
}

/// Cache key: normalised title, plus "tv" for episodes, plus the year for
/// dated non-episodes (an episode's date is its air date, not the show's).
pub fn make_key(title: &str, date: Option<&str>, is_tv: bool) -> Option<LookupKey> {
    let norm = normalize_words(title);
    if norm.chars().count() < 2 {
        return None;
    }
    let year = if is_tv { None } else { date.and_then(parse_year) };
    let key = format!(
        "{norm}|{}|{}",
        if is_tv { "tv" } else { "any" },
        year.map(|y| y.to_string()).unwrap_or_default()
    );
    Some(LookupKey { key, query: title.trim().to_string(), norm, year, is_tv })
}

/// Shorten an overview for <desc>, cutting at a word boundary.
pub fn short_desc(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    let cut = match cut.rfind(' ') {
        Some(i) if i > max / 2 => &cut[..i],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end_matches(|c: char| c == ',' || c == ';' || c == ':' || c.is_whitespace()))
}

// ---------------------------------------------------------------- cache

/// The tmdb_cache table, read into memory for the build. Rows for another
/// language or rating country are ignored. New results are written back
/// with `save`.
pub struct TmdbCache {
    language: String,
    country: String,
    entries: BTreeMap<String, TmdbInfo>,
    pending: Vec<(LookupKey, TmdbInfo)>,
}

fn join_genres(g: &[u64]) -> String {
    g.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",")
}

fn split_genres(s: &str) -> Vec<u64> {
    s.split(',').filter_map(|x| x.trim().parse().ok()).collect()
}

impl TmdbCache {
    pub fn load(conn: &Connection, language: &str, country: &str) -> Result<TmdbCache> {
        let mut stmt = conn.prepare(
            "SELECT cache_key, found, media_type, tmdb_id, name, poster_path, rating, genres, overview, fetched_at,
                    imdb_id, imdb_checked
             FROM tmdb_cache WHERE language = ?1 AND rating_country = ?2",
        )?;
        let rows = stmt.query_map(params![language, country], |r| {
            Ok((
                r.get::<_, String>(0)?,
                TmdbInfo {
                    found: r.get::<_, bool>(1)?,
                    media_type: r.get(2)?,
                    id: r.get::<_, i64>(3)?.max(0) as u64,
                    name: r.get(4)?,
                    poster_path: r.get(5)?,
                    certification: r.get(6)?,
                    genre_ids: split_genres(&r.get::<_, String>(7)?),
                    overview: r.get(8)?,
                    fetched_at: r.get::<_, i64>(9)?.max(0) as u64,
                    imdb_id: r.get(10)?,
                    imdb_checked: r.get::<_, i64>(11)? != 0,
                },
            ))
        })?;
        let mut entries = BTreeMap::new();
        for row in rows {
            let (k, v) = row?;
            entries.insert(k, v);
        }
        Ok(TmdbCache { language: language.to_string(), country: country.to_string(), entries, pending: Vec::new() })
    }

    pub fn get(&self, key: &str) -> Option<&TmdbInfo> {
        self.entries.get(key)
    }

    pub fn get_fresh(&self, key: &str, now: u64) -> Option<&TmdbInfo> {
        let e = self.entries.get(key)?;
        // A match from before IMDb ids were fetched counts as stale, so it is looked up again.
        (now < expires_at(e) && (!e.found || e.imdb_checked)).then_some(e)
    }

    pub fn insert(&mut self, key: LookupKey, info: TmdbInfo) {
        self.entries.insert(key.key.clone(), info.clone());
        self.pending.push((key, info));
    }

    /// Results inserted but not yet saved.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Write the results added since `load`.
    pub fn save(&mut self, conn: &Connection) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO tmdb_cache (cache_key, norm_title, year, is_tv, found, media_type, tmdb_id,
                     name, poster_path, rating, genres, overview, language, rating_country, fetched_at, expires_at,
                     imdb_id, imdb_checked)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            )?;
            for (k, i) in &self.pending {
                stmt.execute(params![
                    k.key,
                    k.norm,
                    k.year,
                    k.is_tv,
                    i.found,
                    i.media_type,
                    i.id as i64,
                    i.name,
                    i.poster_path,
                    i.certification,
                    join_genres(&i.genre_ids),
                    i.overview,
                    self.language,
                    self.country,
                    i.fetched_at as i64,
                    expires_at(i) as i64,
                    i.imdb_id,
                    i.imdb_checked
                ])?;
            }
        }
        tx.commit().context("saving TMDB cache")?;
        self.pending.clear();
        Ok(())
    }
}

/// Found titles are kept 90 days, "not on TMDB" results 14 days.
fn expires_at(i: &TmdbInfo) -> u64 {
    i.fetched_at + if i.found { FOUND_TTL } else { NOT_FOUND_TTL }
}

// ---------------------------------------------------------------- client

enum Auth {
    ApiKey(String),
    Bearer(String),
}

/// A v4 read access token is a JWT ("eyJ..." with dots); anything else is a v3 key.
fn auth_from_key(key: &str) -> Auth {
    let k = key.trim().to_string();
    if k.starts_with("eyJ") && k.contains('.') {
        Auth::Bearer(k)
    } else {
        Auth::ApiKey(k)
    }
}

/// Can we reach the TMDB API at all? Any HTTP answer counts.
pub async fn check_reachable(client: &reqwest::Client) -> Result<u16> {
    let r = client
        .get(format!("{API_BASE}/configuration"))
        .send()
        .await
        .map_err(|e| anyhow!("cannot reach api.themoviedb.org: {}", e.without_url()))?;
    Ok(r.status().as_u16())
}

/// Is the key accepted? Calls /configuration with it.
pub async fn check_key(client: &reqwest::Client, key: &str) -> Result<()> {
    let req = client.get(format!("{API_BASE}/configuration"));
    let req = match auth_from_key(key) {
        Auth::ApiKey(k) => req.query(&[("api_key", k.as_str())]),
        Auth::Bearer(t) => req.bearer_auth(t),
    };
    let r = req.send().await.map_err(|e| anyhow!("TMDB request failed: {}", e.without_url()))?;
    match r.status().as_u16() {
        200 => Ok(()),
        401 => bail!("TMDB rejected the key (HTTP 401)"),
        s => bail!("TMDB answered HTTP {s}"),
    }
}

/// Shared token bucket: (tokens, last refill).
struct RateLimiter {
    state: Mutex<(f64, Instant)>,
}

impl RateLimiter {
    fn new() -> RateLimiter {
        RateLimiter { state: Mutex::new((RATE_BURST, Instant::now())) }
    }

    /// None = go ahead now; Some(d) = wait d, then try again.
    fn try_take(&self) -> Option<Duration> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let refill = now.duration_since(s.1).as_secs_f64() * RATE_PER_SEC;
        s.0 = (s.0 + refill).min(RATE_BURST);
        s.1 = now;
        if s.0 >= 1.0 {
            s.0 -= 1.0;
            None
        } else {
            Some(Duration::from_secs_f64((1.0 - s.0) / RATE_PER_SEC) + Duration::from_millis(1))
        }
    }

    /// After a 429, hold every task back for `d` by draining the bucket below zero.
    fn pause(&self, d: Duration) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let debt = -(d.as_secs_f64() * RATE_PER_SEC);
        if s.0 > debt {
            s.0 = debt;
        }
    }

    async fn acquire(&self) {
        while let Some(wait) = self.try_take() {
            tokio::time::sleep(wait).await;
        }
    }
}

/// What `lookup_stream` got through before it stopped.
pub struct StreamOutcome {
    /// Titles not looked up: the time budget ran out, the key was rejected or the build was cancelled.
    pub deferred: usize,
}

pub struct Tmdb {
    client: reqwest::Client,
    auth: Auth,
    language: String,
    country: String,
    concurrency: usize,
    limiter: RateLimiter,
    requests: AtomicUsize,
    auth_failed: AtomicBool,
}

impl Tmdb {
    pub fn new(client: reqwest::Client, key: &str, language: &str, country: &str, concurrency: usize) -> Tmdb {
        Tmdb {
            client,
            auth: auth_from_key(key),
            language: language.to_string(),
            country: country.to_string(),
            concurrency: concurrency.max(1),
            limiter: RateLimiter::new(),
            requests: AtomicUsize::new(0),
            auth_failed: AtomicBool::new(false),
        }
    }

    pub fn request_count(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    /// True once TMDB rejected the key (401); no further lookups are made.
    pub fn key_rejected(&self) -> bool {
        self.auth_failed.load(Ordering::Relaxed)
    }

    async fn get_json(&self, path: &str, params: &[(&str, &str)]) -> Result<Value> {
        let url = format!("{API_BASE}{path}");
        let mut attempt = 0u64;
        loop {
            attempt += 1;
            if self.auth_failed.load(Ordering::Relaxed) {
                bail!("skipped: TMDB rejected the API key");
            }
            self.limiter.acquire().await;
            self.requests.fetch_add(1, Ordering::Relaxed);
            let mut req = self
                .client
                .get(&url)
                .timeout(REQUEST_TIMEOUT)
                .query(params)
                .query(&[("language", self.language.as_str())]);
            req = match &self.auth {
                Auth::ApiKey(k) => req.query(&[("api_key", k.as_str())]),
                Auth::Bearer(t) => req.bearer_auth(t),
            };
            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    if attempt < 4 {
                        tokio::time::sleep(Duration::from_secs(attempt)).await;
                        continue;
                    }
                    bail!("TMDB {path}: {}", e.without_url());
                }
            };
            let status = resp.status();
            if status.as_u16() == 429 || status.is_server_error() {
                if attempt < 5 {
                    let wait = resp
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.trim().parse::<u64>().ok())
                        .unwrap_or(attempt * 2);
                    let wait = Duration::from_secs(wait.clamp(1, 60));
                    if status.as_u16() == 429 {
                        // Slow everyone down, not just this task.
                        self.limiter.pause(wait);
                    }
                    tokio::time::sleep(wait).await;
                    continue;
                }
                bail!("TMDB {path}: HTTP {status}");
            }
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.auth_failed.store(true, Ordering::Relaxed);
                bail!("TMDB rejected the API key (HTTP 401): check the TMDB key in the settings");
            }
            if status == reqwest::StatusCode::NOT_FOUND {
                bail!("TMDB {path}: not found");
            }
            if !status.is_success() {
                bail!("TMDB {path}: HTTP {status}");
            }
            let body = resp
                .text()
                .await
                .map_err(|e| anyhow!("TMDB {path}: reading response: {}", e.without_url()))?;
            return serde_json::from_str(&body).with_context(|| format!("TMDB {path}: invalid JSON"));
        }
    }

    /// Search, pick the best exact-title match, then fetch its age rating.
    pub async fn lookup(&self, k: &LookupKey) -> Result<TmdbInfo> {
        let now = crate::timeutil::now_unix().max(0) as u64;
        let v = self
            .get_json("/search/multi", &[("query", k.query.as_str()), ("include_adult", "false")])
            .await?;
        let empty = Vec::new();
        let results = v.get("results").and_then(Value::as_array).unwrap_or(&empty);
        let Some(r) = pick_result(results, &k.norm, k.year, k.is_tv) else {
            return Ok(TmdbInfo { found: false, fetched_at: now, ..Default::default() });
        };
        let mut info = info_from_result(r, now);
        info.imdb_checked = true;
        if info.id != 0 {
            let movie = info.media_type == "movie";
            let (path, append) = if movie {
                (format!("/movie/{}", info.id), "release_dates,external_ids")
            } else {
                (format!("/tv/{}", info.id), "content_ratings,external_ids")
            };
            let d = self.get_json(&path, &[("append_to_response", append)]).await?;
            info.certification = if movie {
                movie_certification(&d, &self.country)
            } else {
                tv_rating(&d, &self.country)
            };
            // Movies have imdb_id at the top; shows only in external_ids.
            info.imdb_id = non_empty_str(&d, "imdb_id")
                .or_else(|| d.get("external_ids").and_then(|x| non_empty_str(x, "imdb_id")))
                .filter(|s| s.starts_with("tt"));
            info.imdb_checked = true;
            if info.poster_path.is_none() {
                info.poster_path = non_empty_str(&d, "poster_path");
            }
            if info.overview.is_none() {
                info.overview = non_empty_str(&d, "overview");
            }
            if info.genre_ids.is_empty() {
                if let Some(g) = d.get("genres").and_then(Value::as_array) {
                    info.genre_ids = g.iter().filter_map(|x| x.get("id").and_then(Value::as_u64)).collect();
                }
            }
        }
        Ok(info)
    }

    /// Look up titles in the given order (soonest first) with `concurrency`
    /// in flight, calling `on_result` as each one finishes. No new lookups
    /// start after `deadline` (None = no limit) or once the key is rejected;
    /// lookups still running then get BUDGET_GRACE to finish before they
    /// are dropped. Prints a progress line every few seconds.
    pub async fn lookup_stream<F>(
        self: Arc<Self>,
        keys: Vec<LookupKey>,
        deadline: Option<Instant>,
        cancel: &AtomicBool,
        mut on_result: F,
    ) -> StreamOutcome
    where
        F: FnMut(LookupKey, Result<TmdbInfo>),
    {
        let total = keys.len();
        let started = Instant::now();
        let mut last_log = started;
        let mut queue = keys.into_iter();
        let mut set: JoinSet<(LookupKey, Result<TmdbInfo>)> = JoinSet::new();
        let mut done = 0usize;
        let mut stopped = false;
        loop {
            if cancel.load(Ordering::Relaxed) {
                set.abort_all();
                break;
            }
            if !stopped
                && (deadline.is_some_and(|d| Instant::now() >= d) || self.auth_failed.load(Ordering::Relaxed))
            {
                stopped = true;
            }
            while !stopped && set.len() < self.concurrency {
                let Some(k) = queue.next() else { break };
                let me = Arc::clone(&self);
                set.spawn(async move {
                    let r = me.lookup(&k).await;
                    (k, r)
                });
            }
            if set.is_empty() {
                break;
            }
            // Wake at least once a second to notice a cancel; join_next is cancel-safe.
            let tick = tokio::time::Instant::now() + Duration::from_secs(1);
            let limit = match deadline {
                Some(d) => tick.min(tokio::time::Instant::from_std(d + BUDGET_GRACE)),
                None => tick,
            };
            let r = tokio::time::timeout_at(limit, set.join_next()).await;
            let joined = match r {
                Ok(j) => j,
                Err(_) => {
                    if deadline.is_some_and(|d| Instant::now() >= d + BUDGET_GRACE) {
                        set.abort_all();
                        break;
                    }
                    continue;
                }
            };
            let Some(joined) = joined else { break };
            done += 1;
            match joined {
                Ok((k, r)) => on_result(k, r),
                Err(e) => eprintln!("warning: TMDB task failed: {e}"),
            }
            if last_log.elapsed() >= PROGRESS_EVERY {
                last_log = Instant::now();
                let secs = started.elapsed().as_secs_f64().max(0.001);
                let rate = done as f64 / secs;
                let left = total - done;
                let eta = if rate > 0.0 { format!("{:.0}s", left as f64 / rate) } else { "?".to_string() };
                println!(
                    "TMDB: {done}/{total} titles, {rate:.1} titles/s, {} requests, ETA {eta}",
                    self.request_count()
                );
            }
        }
        StreamOutcome { deferred: total - done }
    }
}

fn non_empty_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Best search result whose title (or original title) equals the programme
/// title after normalisation. Episodes only match TV results; a year from
/// <date> favours the result released that year.
pub fn pick_result<'a>(results: &'a [Value], norm: &str, year: Option<i32>, is_tv: bool) -> Option<&'a Value> {
    let mut best: Option<(&Value, i32)> = None;
    for r in results {
        let mt = r.get("media_type").and_then(Value::as_str).unwrap_or("");
        let (t1, t2, date_key) = match mt {
            "movie" => ("title", "original_title", "release_date"),
            "tv" => ("name", "original_name", "first_air_date"),
            _ => continue,
        };
        if is_tv && mt != "tv" {
            continue;
        }
        let title_match = [t1, t2]
            .iter()
            .any(|k| r.get(*k).and_then(Value::as_str).map_or(false, |s| normalize_words(s) == norm));
        if !title_match {
            continue;
        }
        let mut score = 0;
        if let (Some(y), false) = (year, is_tv) {
            let ry = r
                .get(date_key)
                .and_then(Value::as_str)
                .and_then(|d| d.get(0..4))
                .and_then(|d| d.parse::<i32>().ok());
            match ry {
                Some(ry) if ry == y => score += 3,
                Some(ry) if (ry - y).abs() == 1 => score += 1,
                _ => {}
            }
            if mt == "movie" {
                score += 1;
            }
        }
        // Results come sorted by popularity, so keep the first on ties.
        if best.map_or(true, |(_, s)| score > s) {
            best = Some((r, score));
        }
    }
    best.map(|(r, _)| r)
}

fn info_from_result(r: &Value, now: u64) -> TmdbInfo {
    let media_type = r.get("media_type").and_then(Value::as_str).unwrap_or("").to_string();
    let name_key = if media_type == "movie" { "title" } else { "name" };
    TmdbInfo {
        found: true,
        id: r.get("id").and_then(Value::as_u64).unwrap_or(0),
        name: r.get(name_key).and_then(Value::as_str).unwrap_or("").to_string(),
        poster_path: non_empty_str(r, "poster_path"),
        overview: non_empty_str(r, "overview"),
        genre_ids: r
            .get("genre_ids")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default(),
        certification: None,
        fetched_at: now,
        media_type,
        imdb_id: None,
        imdb_checked: false,
    }
}

fn cert_of(d: &Value) -> Option<String> {
    non_empty_str(d, "certification")
}

/// Movie certification for a country, preferring the theatrical release (type 3).
pub fn movie_certification(details: &Value, country: &str) -> Option<String> {
    let results = details.get("release_dates")?.get("results")?.as_array()?;
    let entry = results
        .iter()
        .find(|r| r.get("iso_3166_1").and_then(Value::as_str) == Some(country))?;
    let dates = entry.get("release_dates")?.as_array()?;
    dates
        .iter()
        .filter(|d| d.get("type").and_then(Value::as_u64) == Some(3))
        .find_map(cert_of)
        .or_else(|| dates.iter().find_map(cert_of))
}

/// TV content rating (e.g. TV-14) for a country.
pub fn tv_rating(details: &Value, country: &str) -> Option<String> {
    let results = details.get("content_ratings")?.get("results")?.as_array()?;
    let entry = results
        .iter()
        .find(|r| r.get("iso_3166_1").and_then(Value::as_str) == Some(country))?;
    non_empty_str(entry, "rating")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_key_type() {
        assert!(matches!(auth_from_key("0123456789abcdef0123456789abcdef"), Auth::ApiKey(_)));
        assert!(matches!(auth_from_key(" eyJhbGciOiJIUzI1NiJ9.eyJhdWQiOiJ4In0.sig "), Auth::Bearer(_)));
    }

    #[test]
    fn builds_cache_keys() {
        let k = make_key("The Sample Movie", Some("1999"), false).unwrap();
        assert_eq!(k.key, "the sample movie|any|1999");
        let k = make_key("Sample Show", Some("20240101"), true).unwrap();
        assert_eq!(k.key, "sample show|tv|");
        assert!(make_key("!", None, false).is_none());
        assert_eq!(parse_year("20190512"), Some(2019));
        assert_eq!(parse_year("abc"), None);
    }

    fn results() -> Vec<Value> {
        json!([
            {"media_type": "movie", "id": 1, "title": "Sample Movie", "original_title": "Sample Movie", "release_date": "2005-01-01"},
            {"media_type": "movie", "id": 2, "title": "The Sample Movie", "original_title": "The Sample Movie", "release_date": "2012-06-01"},
            {"media_type": "movie", "id": 3, "title": "The Sample Movie", "original_title": "The Sample Movie", "release_date": "1999-03-01", "poster_path": "/p3.jpg", "genre_ids": [35]},
            {"media_type": "tv", "id": 4, "name": "The Sample Movie", "original_name": "The Sample Movie", "first_air_date": "2010-01-01"},
            {"media_type": "person", "id": 5, "name": "The Sample Movie"}
        ])
        .as_array()
        .unwrap()
        .clone()
    }

    #[test]
    fn picks_exact_title_and_year() {
        let r = results();
        let best = pick_result(&r, "the sample movie", Some(1999), false).unwrap();
        assert_eq!(best["id"], 3);
        let info = info_from_result(best, 7);
        assert_eq!(info.poster_url().as_deref(), Some("https://image.tmdb.org/t/p/w500/p3.jpg"));
        assert_eq!(info.genre_ids, vec![35]);
        assert_eq!(info.rating_system(), "MPAA");
        // No year: the first (most popular) exact match.
        assert_eq!(pick_result(&r, "the sample movie", None, false).unwrap()["id"], 2);
        // Episodes only match TV.
        assert_eq!(pick_result(&r, "the sample movie", None, true).unwrap()["id"], 4);
        assert!(pick_result(&r, "something else", None, false).is_none());
    }

    #[test]
    fn extracts_ratings() {
        let movie = json!({"release_dates": {"results": [
            {"iso_3166_1": "GB", "release_dates": [{"certification": "15", "type": 3}]},
            {"iso_3166_1": "US", "release_dates": [
                {"certification": "", "type": 1},
                {"certification": "NR", "type": 4},
                {"certification": "PG-13", "type": 3}
            ]}
        ]}});
        assert_eq!(movie_certification(&movie, "US").as_deref(), Some("PG-13"));
        assert_eq!(movie_certification(&movie, "GB").as_deref(), Some("15"));
        assert_eq!(movie_certification(&movie, "DE"), None);

        let tv = json!({"content_ratings": {"results": [{"iso_3166_1": "US", "rating": "TV-14"}]}});
        assert_eq!(tv_rating(&tv, "US").as_deref(), Some("TV-14"));
        assert_eq!(tv_rating(&tv, "GB"), None);
    }

    #[test]
    fn shortens_descriptions() {
        assert_eq!(short_desc("  short  ", 300), "short");
        let long = "word ".repeat(100);
        let d = short_desc(&long, 50);
        assert!(d.ends_with('…'));
        assert!(d.chars().count() <= 51);
    }

    #[test]
    fn cache_round_trips_and_respects_ttl() {
        let conn = crate::db::open_memory();
        let mut c = TmdbCache::load(&conn, "en-US", "US").unwrap();
        let ka = make_key("The Sample Movie", Some("1999"), false).unwrap();
        let kb = make_key("Nothing Here", None, false).unwrap();
        c.insert(
            ka.clone(),
            TmdbInfo {
                found: true,
                media_type: "movie".into(),
                id: 3,
                poster_path: Some("/p.jpg".into()),
                certification: Some("PG".into()),
                genre_ids: vec![35, 18],
                fetched_at: 1_000,
                imdb_id: Some("tt0000003".into()),
                imdb_checked: true,
                ..Default::default()
            },
        );
        c.insert(kb.clone(), TmdbInfo { found: false, fetched_at: 1_000, ..Default::default() });
        c.save(&conn).unwrap();

        let c = TmdbCache::load(&conn, "en-US", "US").unwrap();
        assert_eq!(c.len(), 2);
        let a = c.get(&ka.key).unwrap();
        assert_eq!(a.genre_ids, vec![35, 18]);
        assert_eq!(a.certification.as_deref(), Some("PG"));
        let later = 1_000 + 30 * 86_400;
        assert!(c.get_fresh(&ka.key, later).is_some());
        assert!(c.get_fresh(&kb.key, later).is_none(), "negative results expire sooner");
        assert!(c.get(&kb.key).is_some());
        assert_eq!(TmdbCache::load(&conn, "de-DE", "DE").unwrap().len(), 0);
        assert_eq!(a.imdb_id.as_deref(), Some("tt0000003"));
        // A match saved before IMDb ids were fetched is looked up again.
        let kc = make_key("Old Match", None, false).unwrap();
        let mut c = TmdbCache::load(&conn, "en-US", "US").unwrap();
        c.insert(kc.clone(), TmdbInfo { found: true, fetched_at: 1_000, ..Default::default() });
        assert!(c.get_fresh(&kc.key, later).is_none());
    }
}
