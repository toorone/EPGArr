//! Channel logos from the public iptv-org lists.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

const LIST_MAX_AGE: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Deserialize)]
struct IptvChannel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    alt_names: Option<Vec<String>>,
    #[serde(default)]
    country: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IptvLogo {
    channel: String,
    #[serde(default)]
    feed: Option<String>,
    #[serde(default)]
    width: Option<f64>,
    #[serde(default)]
    format: Option<String>,
    url: String,
}

#[derive(Debug, Default)]
pub struct LogoIndex {
    /// lowercase iptv-org channel id -> best logo URL
    by_id: HashMap<String, String>,
    /// normalised channel name -> [(lowercase id, lowercase country)]
    by_name: HashMap<String, Vec<(String, String)>>,
}

/// Lowercase, drop punctuation and quality tags: "BBC One HD" -> "bbcone".
pub fn normalize_name(s: &str) -> String {
    const DROP: [&str; 9] = ["hd", "fhd", "uhd", "sd", "4k", "hevc", "h265", "1080p", "720p"];
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !DROP.contains(w))
        .collect()
}

/// "BBCOne.uk" -> Some("uk")
fn country_hint(id: &str) -> Option<String> {
    let (_, cc) = id.rsplit_once('.')?;
    (cc.len() == 2 && cc.chars().all(|c| c.is_ascii_alphabetic())).then(|| cc.to_ascii_lowercase())
}

/// Prefer the channel's main logo (no feed), then raster over SVG, then size.
fn logo_score(l: &IptvLogo) -> i64 {
    let mut s = 0i64;
    if l.feed.as_deref().map_or(true, |f| f.trim().is_empty()) {
        s += 1_000_000;
    }
    if !l.format.as_deref().map_or(false, |f| f.eq_ignore_ascii_case("svg")) {
        s += 100_000;
    }
    s + l.width.unwrap_or(0.0).clamp(0.0, 99_999.0) as i64
}

impl LogoIndex {
    pub async fn load(client: &reqwest::Client, logos_url: &str, channels_url: &str, cache_dir: &Path) -> Result<Self> {
        let logos = fetch_cached(client, logos_url, &cache_dir.join("iptv-logos.json")).await?;
        let chans = fetch_cached(client, channels_url, &cache_dir.join("iptv-channels.json")).await?;
        let logos: Vec<IptvLogo> = serde_json::from_slice(&logos).context("parsing iptv-org logos.json")?;
        let chans: Vec<IptvChannel> = serde_json::from_slice(&chans).context("parsing iptv-org channels.json")?;
        Ok(Self::build(chans, logos))
    }

    fn build(chans: Vec<IptvChannel>, logos: Vec<IptvLogo>) -> Self {
        let mut best: HashMap<String, (i64, String)> = HashMap::new();
        for l in logos {
            if l.url.trim().is_empty() {
                continue;
            }
            let score = logo_score(&l);
            let key = l.channel.to_lowercase();
            let better = best.get(&key).map_or(true, |(s, _)| score > *s);
            if better {
                best.insert(key, (score, l.url));
            }
        }
        let by_id: HashMap<String, String> = best.into_iter().map(|(k, (_, u))| (k, u)).collect();

        let mut by_name: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for c in chans {
            let id = c.id.to_lowercase();
            let country = c.country.unwrap_or_default().to_lowercase();
            let mut names = c.alt_names.unwrap_or_default();
            if let Some(n) = c.name {
                names.insert(0, n);
            }
            for n in names {
                let k = normalize_name(&n);
                if k.is_empty() {
                    continue;
                }
                let v = by_name.entry(k).or_default();
                if !v.iter().any(|(i, _)| *i == id) {
                    v.push((id.clone(), country.clone()));
                }
            }
        }
        LogoIndex { by_id, by_name }
    }

    /// Find a logo by XMLTV channel id, then by display name.
    pub fn lookup(&self, xml_id: &str, names: &[String]) -> Option<String> {
        let id = xml_id.trim().to_lowercase();
        let base = id.split('@').next().unwrap_or(&id).to_string();
        for cand in [&id, &base] {
            if let Some(u) = self.by_id.get(cand) {
                return Some(u.clone());
            }
        }
        let hint = country_hint(&base);
        for n in names {
            let k = normalize_name(n);
            if k.is_empty() {
                continue;
            }
            let Some(cands) = self.by_name.get(&k) else { continue };
            let with_logo: Vec<&(String, String)> =
                cands.iter().filter(|(i, _)| self.by_id.contains_key(i)).collect();
            let pick = hint
                .as_deref()
                .and_then(|h| with_logo.iter().find(|(_, c)| c == h))
                .or_else(|| with_logo.first());
            if let Some((i, _)) = pick {
                return self.by_id.get(i).cloned();
            }
        }
        None
    }
}

/// Use a cached copy younger than a day; otherwise download, falling back to
/// an older cached copy if the download fails.
async fn fetch_cached(client: &reqwest::Client, url: &str, path: &Path) -> Result<Vec<u8>> {
    let fresh = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .map_or(false, |age| age < LIST_MAX_AGE);
    if fresh {
        if let Ok(b) = std::fs::read(path) {
            return Ok(b);
        }
    }
    match download(client, url).await {
        Ok(b) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).ok();
            }
            if let Err(e) = crate::output::write_atomic(path, &b) {
                eprintln!("warning: could not cache {url}: {e:#}");
            }
            Ok(b)
        }
        Err(e) => match std::fs::read(path) {
            Ok(b) => {
                eprintln!("warning: {e:#}; using the older cached copy");
                Ok(b)
            }
            Err(_) => Err(e),
        },
    }
}

async fn download(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let r = client
        .get(url)
        .send()
        .await
        .map_err(|e| anyhow!("fetching {url}: {}", e.without_url()))?;
    if !r.status().is_success() {
        bail!("fetching {url}: HTTP {}", r.status());
    }
    let b = r.bytes().await.map_err(|e| anyhow!("fetching {url}: {}", e.without_url()))?;
    Ok(b.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn index() -> LogoIndex {
        let chans: Vec<IptvChannel> = serde_json::from_value(json!([
            {"id": "BBCOne.uk", "name": "BBC One", "alt_names": ["BBC 1"], "country": "UK"},
            {"id": "Discovery.us", "name": "Discovery Channel", "alt_names": [], "country": "US"},
            {"id": "Discovery.ca", "name": "Discovery Channel", "alt_names": null, "country": "CA"},
            {"id": "NoLogo.fr", "name": "Sans Logo", "country": "FR"}
        ]))
        .unwrap();
        let logos: Vec<IptvLogo> = serde_json::from_value(json!([
            {"channel": "BBCOne.uk", "feed": "HD", "width": 1000, "height": 500, "format": "PNG", "url": "https://l/bbc-hd.png"},
            {"channel": "BBCOne.uk", "feed": null, "width": 512, "height": 512, "format": "PNG", "url": "https://l/bbc.png"},
            {"channel": "BBCOne.uk", "feed": null, "width": 2000, "height": 512, "format": "SVG", "url": "https://l/bbc.svg"},
            {"channel": "Discovery.us", "feed": null, "width": 300, "format": "PNG", "url": "https://l/disc-us.png"},
            {"channel": "Discovery.ca", "feed": null, "width": 300, "format": "PNG", "url": "https://l/disc-ca.png"}
        ]))
        .unwrap();
        LogoIndex::build(chans, logos)
    }

    #[test]
    fn normalises_names() {
        assert_eq!(normalize_name("BBC One HD"), "bbcone");
        assert_eq!(normalize_name("  Sky Sports: Main Event (FHD) "), "skysportsmainevent");
        assert_eq!(normalize_name("HD"), "");
    }

    #[test]
    fn matches_by_id_preferring_main_raster_logo() {
        let i = index();
        assert_eq!(i.lookup("BBCOne.uk", &[]).as_deref(), Some("https://l/bbc.png"));
        assert_eq!(i.lookup("bbcone.uk@SD", &[]).as_deref(), Some("https://l/bbc.png"));
    }

    #[test]
    fn matches_by_name_using_country_hint() {
        let i = index();
        let names = vec!["BBC 1 HD".to_string()];
        assert_eq!(i.lookup("custom-id-1", &names).as_deref(), Some("https://l/bbc.png"));
        let names = vec!["Discovery Channel".to_string()];
        assert_eq!(i.lookup("disc.ca", &names).as_deref(), Some("https://l/disc-ca.png"));
        assert_eq!(i.lookup("disc.us", &names).as_deref(), Some("https://l/disc-us.png"));
        assert!(i.lookup("whatever", &names).is_some());
    }

    #[test]
    fn reports_missing() {
        let i = index();
        assert_eq!(i.lookup("NoLogo.fr", &["Sans Logo".to_string()]), None);
        assert_eq!(i.lookup("Unknown.xx", &["Nothing Like It".to_string()]), None);
    }
}
