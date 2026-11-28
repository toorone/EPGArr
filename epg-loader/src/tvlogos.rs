//! Channel logos from the tv-logo/tv-logos repository on GitHub.
//!
//! The repository keeps logos as countries/<country>/<name>-<cc>.png, for
//! example countries/united-states/abc-us.png. The file list comes from the
//! GitHub tree API, is kept in epg.db and refreshed at most once a week.
//! Logos are linked from raw.githubusercontent.com (both URLs are settings).

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::Connection;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

use crate::{db, timeutil};

const BRANCH: &str = "main";
/// Refresh a complete list after a week.
const MAX_AGE_SECS: i64 = 7 * 86_400;
/// A partial list (GitHub rate limit hit) is completed after an hour.
const PARTIAL_RETRY_SECS: i64 = 3_600;
/// Words that say nothing about which channel it is.
const DROP_WORDS: [&str; 10] = ["hd", "fhd", "uhd", "sd", "4k", "hevc", "h265", "1080p", "720p", "channel"];
/// Fuzzy matches need names at least this long, differing by at most FUZZY_SLACK letters.
const FUZZY_MIN: usize = 5;
const FUZZY_SLACK: usize = 4;

#[derive(Debug, Deserialize)]
struct Tree {
    #[serde(default)]
    tree: Vec<TreeEntry>,
    #[serde(default)]
    truncated: bool,
}

#[derive(Debug, Deserialize)]
struct TreeEntry {
    path: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    sha: String,
}

/// True for a word that only repeats the country: "us"/"usa" for us, "uk"/"gb" for uk.
fn is_country_word(w: &str, cc: &str) -> bool {
    w == cc || (cc == "us" && w == "usa") || (cc == "uk" && w == "gb")
}

/// Lowercase words of a channel name without quality tags, "+1", "channel",
/// punctuation and a trailing country: "Discovery Channel HD (US)" -> ["discovery"].
pub fn name_words(s: &str, cc: &str) -> Vec<String> {
    let lower = s.to_lowercase().replace("+1", " ").replace('&', " and ");
    let mut words: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !DROP_WORDS.contains(w))
        .map(String::from)
        .collect();
    while words.len() > 1 && words.last().is_some_and(|w| is_country_word(w, cc)) {
        words.pop();
    }
    words
}

/// The words joined: "BBC One HD" -> "bbcone". Names and logo files are compared this way.
pub fn compact(s: &str, cc: &str) -> String {
    name_words(s, cc).concat()
}

/// "countries/united-states/abc-us.png" -> ("us", "abc")
fn parse_path(path: &str) -> Option<(String, String)> {
    let rest = path.strip_prefix("countries/")?;
    let file = rest.rsplit('/').next()?;
    let (stem, ext) = file.rsplit_once('.')?;
    if !matches!(ext.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "svg" | "webp") {
        return None;
    }
    let (_, cc) = stem.rsplit_once('-')?;
    let cc = cc.to_ascii_lowercase();
    if cc.len() != 2 || !cc.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let key = compact(stem, &cc);
    if key.is_empty() {
        return None;
    }
    Some((cc, key))
}

/// One logo found by `search`: (name, country folder, image URL).
pub type Found = (String, String, String);

/// Search the tv-logos file list for the logo dialog: every word of `query`
/// must be in the file name. Best first: the exact name, logos from
/// `prefer_cc`, then shorter names. At most `limit` results.
pub fn search(paths: &[String], raw_base: &str, query: &str, prefer_cc: &str, limit: usize) -> Vec<Found> {
    let words: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    let wanted = words.concat();
    let base = raw_base.trim_end_matches('/');
    let mut hits: Vec<(u8, usize, &String, String, String)> = paths
        .iter()
        .filter_map(|p| {
            let rest = p.strip_prefix("countries/")?;
            let (folder, file) = rest.split_once('/').map(|(f, _)| (f, rest.rsplit('/').next().unwrap_or(""))).unwrap_or(("", rest));
            let (stem, ext) = file.rsplit_once('.')?;
            if !matches!(ext.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp") {
                return None;
            }
            let lower = stem.to_lowercase();
            let flat: String = lower.chars().filter(|c| c.is_alphanumeric()).collect();
            if !words.iter().all(|w| flat.contains(w.as_str())) {
                return None;
            }
            let cc = lower.rsplit_once('-').map(|(_, c)| c.to_string()).unwrap_or_default();
            let name_only: String = lower.rsplit_once('-').map(|(n, _)| n).unwrap_or(&lower).chars().filter(|c| c.is_alphanumeric()).collect();
            let rank = match (name_only == wanted, !prefer_cc.is_empty() && cc == prefer_cc) {
                (true, true) => 0,
                (true, false) => 1,
                (false, true) => 2,
                (false, false) => 3,
            };
            let name = stem.replace('-', " ");
            Some((rank, stem.len(), p, name, folder.replace('-', " ")))
        })
        .collect();
    hits.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    hits.into_iter()
        .take(limit)
        .map(|(_, _, p, name, country)| {
            let url = format!("{base}/{}", p.split('/').map(|s| s.replace(' ', "%20")).collect::<Vec<_>>().join("/"));
            (name, country, url)
        })
        .collect()
}

/// The channel's country from an id such as "ABC.us" or "BBCOne.uk@HD", and the id without it.
fn split_id(xml_id: &str) -> (&str, Option<String>) {
    let id = xml_id.trim();
    let base = id.split('@').next().unwrap_or(id);
    match base.rsplit_once('.') {
        Some((name, cc)) if cc.len() == 2 && cc.chars().all(|c| c.is_ascii_alphabetic()) => {
            let cc = cc.to_ascii_lowercase();
            (name, Some(if cc == "gb" { "uk".to_string() } else { cc }))
        }
        _ => (base, None),
    }
}

#[derive(Debug, Default)]
pub struct TvLogos {
    raw_base: String,
    /// country code -> compact name -> file path
    by_country: HashMap<String, HashMap<String, String>>,
    /// compact name -> countries that have a logo with that name
    countries_of: HashMap<String, Vec<String>>,
}

impl TvLogos {
    /// Use the list in epg.db while it is fresh; otherwise fetch it again,
    /// falling back to the stored list when GitHub cannot be reached.
    pub async fn load(conn: &Connection, client: &reqwest::Client, api_url: &str, raw_base: &str) -> Result<TvLogos> {
        let (cached, fetched_at, partial) = db::tv_logo_list(conn)?;
        let age = timeutil::now_unix() - fetched_at;
        let max_age = if partial { PARTIAL_RETRY_SECS } else { MAX_AGE_SECS };
        let fresh = !cached.is_empty() && age < max_age;
        if fresh {
            return Ok(Self::from_paths(&cached, raw_base));
        }
        let fetched = fetch_list(client, api_url, &cached).await.and_then(|(paths, partial)| {
            if paths.is_empty() {
                Err(anyhow!("the tv-logos list came back empty"))
            } else {
                Ok((paths, partial))
            }
        });
        match fetched {
            Ok((paths, partial)) => {
                if let Err(e) = db::set_tv_logo_list(conn, &paths, timeutil::now_unix(), partial) {
                    eprintln!("warning: could not save the tv-logos list: {e:#}");
                }
                println!(
                    "tv-logos: {} logo files listed{}",
                    paths.len(),
                    if partial { " (partial: GitHub's rate limit was reached; the rest follows later)" } else { "" }
                );
                Ok(Self::from_paths(&paths, raw_base))
            }
            Err(e) if !cached.is_empty() => {
                eprintln!("warning: {e:#}; using the stored tv-logos list");
                Ok(Self::from_paths(&cached, raw_base))
            }
            Err(e) => Err(e),
        }
    }

    pub fn from_paths(paths: &[String], raw_base: &str) -> TvLogos {
        let mut by_country: HashMap<String, HashMap<String, String>> = HashMap::new();
        for p in paths {
            let Some((cc, key)) = parse_path(p) else { continue };
            let m = by_country.entry(cc).or_default();
            // "abc-us.png" beats "abc-hd-us.png": the shortest file name wins.
            let better = m.get(&key).map_or(true, |old| (p.len(), p) < (old.len(), old));
            if better {
                m.insert(key, p.clone());
            }
        }
        let mut countries_of: HashMap<String, Vec<String>> = HashMap::new();
        for (cc, m) in &by_country {
            for k in m.keys() {
                countries_of.entry(k.clone()).or_default().push(cc.clone());
            }
        }
        TvLogos { raw_base: raw_base.trim_end_matches('/').to_string(), by_country, countries_of }
    }

    pub fn len(&self) -> usize {
        self.by_country.values().map(HashMap::len).sum()
    }

    /// Find a logo for a channel: the exact name in the channel's country
    /// (from its id, else `default_cc`), then the same name in the only
    /// other country that has it, then a careful fuzzy match that is used
    /// only when exactly one logo fits.
    pub fn lookup(&self, xml_id: &str, names: &[String], default_cc: &str) -> Option<String> {
        let (id_name, hint) = split_id(xml_id);
        let cc = hint.unwrap_or_else(|| default_cc.to_ascii_lowercase());
        let mut keys: Vec<String> = Vec::new();
        for n in names.iter().map(String::as_str).chain(std::iter::once(id_name)) {
            let k = compact(n, &cc);
            if !k.is_empty() && !keys.contains(&k) {
                keys.push(k);
            }
        }
        let local = self.by_country.get(&cc);
        // 1. Exact name in the channel's country.
        if let Some(m) = local {
            if let Some(p) = keys.iter().find_map(|k| m.get(k)) {
                return Some(self.url(p));
            }
        }
        // 2. The same name in exactly one country.
        for k in &keys {
            if let Some([only]) = self.countries_of.get(k).map(Vec::as_slice) {
                if let Some(p) = self.by_country.get(only).and_then(|m| m.get(k)) {
                    return Some(self.url(p));
                }
            }
        }
        // 3. Fuzzy, in the channel's country only: one name starts with the other.
        let m = local?;
        for k in keys.iter().filter(|k| k.len() >= FUZZY_MIN) {
            let mut hits = m.iter().filter(|(slug, _)| {
                let (short, long) = if slug.len() <= k.len() { (slug.as_str(), k.as_str()) } else { (k.as_str(), slug.as_str()) };
                short.len() >= FUZZY_MIN && long.len() - short.len() <= FUZZY_SLACK && long.starts_with(short)
            });
            if let (Some((_, p)), None) = (hits.next(), hits.next()) {
                return Some(self.url(p));
            }
        }
        None
    }

    fn url(&self, path: &str) -> String {
        let mut out = String::with_capacity(self.raw_base.len() + path.len() + 1);
        out.push_str(&self.raw_base);
        out.push('/');
        for b in path.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }
}

/// GET one tree from the GitHub API; Ok(None) when the rate limit is reached.
async fn get_tree(client: &reqwest::Client, url: &str) -> Result<Option<Tree>> {
    let r = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| anyhow!("fetching the tv-logos list: {}", e.without_url()))?;
    let status = r.status();
    if status.as_u16() == 403 || status.as_u16() == 429 {
        return Ok(None);
    }
    if !status.is_success() {
        bail!("fetching the tv-logos list: HTTP {status}");
    }
    let body = r.bytes().await.map_err(|e| anyhow!("fetching the tv-logos list: {}", e.without_url()))?;
    Ok(Some(serde_json::from_slice(&body).context("parsing the tv-logos list")?))
}

/// File paths under countries/ in a tree whose paths are relative to `prefix`.
fn blobs(tree: Vec<TreeEntry>, prefix: &str) -> Vec<String> {
    tree.into_iter()
        .filter(|e| e.kind == "blob")
        .map(|e| format!("{prefix}{}", e.path))
        .filter(|p| p.starts_with("countries/"))
        .collect()
}

/// The whole file list in one request; when GitHub truncates it, one request
/// per country folder (skipping folders already known). Returns the paths and
/// whether the list is still partial.
async fn fetch_list(client: &reqwest::Client, api_url: &str, cached: &[String]) -> Result<(Vec<String>, bool)> {
    const LIMITED: &str = "GitHub's API rate limit was reached; the tv-logos list is fetched again later";
    let api = api_url.trim_end_matches('/');
    let Some(full) = get_tree(client, &format!("{api}/{BRANCH}?recursive=1")).await? else {
        bail!(LIMITED);
    };
    if !full.truncated {
        return Ok((blobs(full.tree, ""), false));
    }
    let mut paths = blobs(full.tree, "");
    paths.extend(cached.iter().cloned());
    let Some(root) = get_tree(client, &format!("{api}/{BRANCH}")).await? else {
        bail!(LIMITED);
    };
    let countries = root
        .tree
        .iter()
        .find(|e| e.path == "countries" && e.kind == "tree")
        .map(|e| e.sha.clone())
        .ok_or_else(|| anyhow!("the tv-logos repository has no countries folder"))?;
    let Some(dirs) = get_tree(client, &format!("{api}/{countries}")).await? else {
        bail!(LIMITED);
    };
    let have: HashSet<String> = paths
        .iter()
        .filter_map(|p| p.strip_prefix("countries/"))
        .filter_map(|r| r.split_once('/'))
        .map(|(d, _)| d.to_string())
        .collect();
    let mut partial = false;
    for d in dirs.tree.iter().filter(|e| e.kind == "tree" && !have.contains(&e.path)) {
        match get_tree(client, &format!("{api}/{}?recursive=1", d.sha)).await? {
            Some(t) => paths.extend(blobs(t.tree, &format!("countries/{}/", d.path))),
            None => {
                partial = true;
                break;
            }
        }
    }
    paths.sort();
    paths.dedup();
    Ok((paths, partial))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_the_file_list() {
        let paths: Vec<String> = [
            "countries/united-states/abc-us.png",
            "countries/united-states/abc-news-us.png",
            "countries/united-kingdom/abc-uk.png",
            "countries/united-states/cnn-us.png",
            "misc/readme.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let r = search(&paths, "https://raw/x/", "ABC", "uk", 10);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].2, "https://raw/x/countries/united-kingdom/abc-uk.png", "exact name in the preferred country first");
        assert_eq!(r[1].0, "abc us");
        assert_eq!(r[1].1, "united states");
        assert_eq!(search(&paths, "https://raw/x", "abc news", "us", 10).len(), 1);
        assert!(search(&paths, "https://raw/x", "  ", "us", 10).is_empty());
    }

    fn index() -> TvLogos {
        let paths: Vec<String> = [
            "countries/united-states/abc-us.png",
            "countries/united-states/abc-hd-us.png",
            "countries/united-states/discovery-channel-us.png",
            "countries/united-states/fox-sports-1-us.png",
            "countries/united-states/nickelodeon-us.png",
            "countries/united-states/nick-jr-us.png",
            "countries/united-kingdom/bbc-one-uk.png",
            "countries/united-kingdom/itv-1-uk.png",
            "countries/australia/abc-au.png",
            "countries/canada/ctv-ca.png",
            "countries/united-states/space test-us.png",
            "README.md",
            "countries/united-states/notes.txt",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        TvLogos::from_paths(&paths, "https://raw.example/tv-logos/main/")
    }

    #[test]
    fn normalises_names() {
        assert_eq!(compact("BBC One HD", "uk"), "bbcone");
        assert_eq!(compact("Discovery Channel (US)", "us"), "discovery");
        assert_eq!(compact("ITV +1", "uk"), "itv");
        assert_eq!(compact("A&E", "us"), "aande");
        assert_eq!(compact("Fox Sports 1 FHD USA", "us"), "foxsports1");
        assert_eq!(compact("BBC One GB", "uk"), "bbcone");
        // A name that is only a country word keeps it.
        assert_eq!(compact("US", "us"), "us");
        assert_eq!(compact("HD", "us"), "");
    }

    #[test]
    fn parses_logo_paths() {
        assert_eq!(parse_path("countries/united-states/abc-us.png"), Some(("us".into(), "abc".into())));
        assert_eq!(parse_path("countries/united-kingdom/bbc-one-uk.png"), Some(("uk".into(), "bbcone".into())));
        assert_eq!(parse_path("countries/united-states/notes.txt"), None);
        assert_eq!(parse_path("misc/abc-us.png"), None);
        assert_eq!(parse_path("countries/world/nosuffix.png"), None);
    }

    #[test]
    fn splits_country_from_id() {
        assert_eq!(split_id("ABC.us"), ("ABC", Some("us".to_string())));
        assert_eq!(split_id("BBCOne.gb@HD"), ("BBCOne", Some("uk".to_string())));
        assert_eq!(split_id("I123.json.schedulesdirect.org"), ("I123.json.schedulesdirect.org", None));
    }

    #[test]
    fn exact_match_uses_the_channel_country() {
        let i = index();
        assert_eq!(i.lookup("ABC.us", &["ABC".into()], "us").as_deref(), Some("https://raw.example/tv-logos/main/countries/united-states/abc-us.png"));
        assert_eq!(i.lookup("ABC.au", &["ABC".into()], "us").as_deref(), Some("https://raw.example/tv-logos/main/countries/australia/abc-au.png"));
        // No country in the id: the default country decides.
        assert!(i.lookup("12345", &["ABC HD".into()], "au").unwrap().ends_with("/abc-au.png"));
        assert!(i.lookup("Discovery.us", &["Discovery Channel HD".into()], "us").unwrap().ends_with("/discovery-channel-us.png"));
    }

    #[test]
    fn falls_back_to_the_only_country_with_that_name() {
        let i = index();
        assert!(i.lookup("x1", &["BBC One HD".into()], "us").unwrap().ends_with("/bbc-one-uk.png"));
        // "abc" exists in two countries, so it is not guessed for another country.
        assert_eq!(i.lookup("x2.ca", &["ABC".into()], "us"), None);
    }

    #[test]
    fn fuzzy_match_only_when_unambiguous() {
        let i = index();
        // "nickelodeoneast" is not a logo, but "nickelodeon" is the only one it starts with.
        assert!(i.lookup("x3", &["Nickelodeon East".into()], "us").unwrap().ends_with("/nickelodeon-us.png"));
        // Too short to guess.
        assert_eq!(i.lookup("x4", &["Nick".into()], "us"), None);
        assert_eq!(i.lookup("x5", &["Completely Different".into()], "us"), None);
    }

    #[test]
    fn encodes_urls() {
        let i = index();
        assert!(i.lookup("x6.us", &["Space Test".into()], "us").unwrap().ends_with("/space%20test-us.png"));
        assert_eq!(i.len(), 10);
    }
}
