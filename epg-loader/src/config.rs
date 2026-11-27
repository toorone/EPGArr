//! Settings. They live in the `settings` table of epg.db (edited from
//! the Settings page).

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoSource {
    IptvOrg,
    Off,
}

impl LogoSource {
    fn parse(s: &str) -> Result<LogoSource> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "iptv-org" | "on" | "1" | "true" => Ok(LogoSource::IptvOrg),
            "off" | "none" | "0" | "false" => Ok(LogoSource::Off),
            other => bail!("logo_source must be iptv-org or off, not {other:?}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// XMLTV source of the guide being built: http(s) URL or local file path. Plain or gzipped.
    pub source_url: String,
    /// TMDB v3 API key or v4 read access token. Empty disables TMDB.
    pub tmdb_api_key: String,
    /// Folder that receives guides/ and logos/ (/data in the container).
    pub output_dir: PathBuf,
    /// <data dir>/cache.
    pub cache_dir: PathBuf,
    /// Drop programmes starting more than this many days from now.
    pub days: Option<u32>,
    pub language: String,
    pub rating_country: String,
    pub logo_source: LogoSource,
    pub iptv_logos_url: String,
    pub iptv_channels_url: String,
    /// Country folder (two letters, e.g. us) for tv-logos when the channel id has no .xx suffix.
    pub logo_country: String,
    /// GitHub tree API for the tv-logo/tv-logos file list, and the base URL logos are linked from.
    pub tv_logos_api_url: String,
    pub tv_logos_raw_url: String,
    /// Add IMDb star ratings (IMDb data: personal, non-commercial use).
    pub imdb_ratings: bool,
    /// Only ratings with at least this many votes are added.
    pub imdb_min_votes: u32,
    pub imdb_ratings_url: String,
    /// Ask TVmaze about series TMDB has no match or poster for.
    pub tvmaze: bool,
    pub tvmaze_url: String,
    /// Seconds of TVmaze lookups per build; the background lookups do the rest.
    pub tvmaze_time_budget: u64,
    /// Posters for sports matches from TheSportsDB (free test key "123" unless you have your own).
    pub sportsdb: bool,
    pub sportsdb_key: String,
    pub sportsdb_url: String,
    pub sportsdb_time_budget: u64,
    /// <data dir>/rules.toml.
    pub rules_file: PathBuf,
    pub tmdb_concurrency: usize,
    /// Seconds of TMDB lookups per run (0 = no limit). The rest is looked up next run.
    pub tmdb_time_budget: u64,
    /// Keep looking up TMDB titles in the background (epg-loader enrich) until all are done.
    pub tmdb_background: bool,
    /// Hours between builds in watch mode.
    pub refresh_hours: f64,
    /// The data dir: epg.db, rules.toml, build.lock, cache/.
    pub base_dir: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            source_url: String::new(),
            tmdb_api_key: String::new(),
            output_dir: PathBuf::new(),
            cache_dir: PathBuf::from("cache"),
            days: None,
            language: "en-US".into(),
            rating_country: "US".into(),
            logo_source: LogoSource::IptvOrg,
            iptv_logos_url: "https://iptv-org.github.io/api/logos.json".into(),
            iptv_channels_url: "https://iptv-org.github.io/api/channels.json".into(),
            logo_country: "us".into(),
            tv_logos_api_url: "https://api.github.com/repos/tv-logo/tv-logos/git/trees".into(),
            tv_logos_raw_url: "https://raw.githubusercontent.com/tv-logo/tv-logos/main".into(),
            imdb_ratings: true,
            imdb_min_votes: 50,
            imdb_ratings_url: "https://datasets.imdbws.com/title.ratings.tsv.gz".into(),
            tvmaze: true,
            tvmaze_url: "https://api.tvmaze.com/singlesearch/shows".into(),
            tvmaze_time_budget: 60,
            sportsdb: true,
            sportsdb_key: "123".into(),
            sportsdb_url: "https://www.thesportsdb.com/api/v1/json".into(),
            sportsdb_time_budget: 60,
            rules_file: PathBuf::from("rules.toml"),
            tmdb_concurrency: 16,
            tmdb_time_budget: 120,
            tmdb_background: true,
            refresh_hours: 6.0,
            base_dir: PathBuf::from("."),
        }
    }
}

impl Config {
    /// Build the config from the settings table. Missing keys take defaults.
    pub fn from_settings(s: &HashMap<String, String>, data_dir: &Path) -> Result<Config> {
        let d = Config::default();
        let get = |k: &str| s.get(k).map(|v| v.trim()).filter(|v| !v.is_empty());
        let mut cfg = Config {
            tmdb_api_key: get("tmdb_api_key").unwrap_or("").to_string(),
            output_dir: get("output_dir").map(PathBuf::from).unwrap_or_else(|| data_dir.to_path_buf()),
            language: get("language").unwrap_or(&d.language).to_string(),
            rating_country: get("rating_country").unwrap_or(&d.rating_country).to_ascii_uppercase(),
            logo_source: LogoSource::parse(get("logo_source").unwrap_or(""))?,
            iptv_logos_url: get("iptv_logos_url").unwrap_or(&d.iptv_logos_url).to_string(),
            iptv_channels_url: get("iptv_channels_url").unwrap_or(&d.iptv_channels_url).to_string(),
            logo_country: get("logo_country").unwrap_or(&d.logo_country).to_ascii_lowercase(),
            tv_logos_api_url: get("tv_logos_api_url").unwrap_or(&d.tv_logos_api_url).to_string(),
            tv_logos_raw_url: get("tv_logos_raw_url").unwrap_or(&d.tv_logos_raw_url).to_string(),
            imdb_ratings: match get("imdb_ratings") {
                Some(v) => !matches!(v.to_ascii_lowercase().as_str(), "0" | "off" | "false" | "no"),
                None => d.imdb_ratings,
            },
            imdb_min_votes: match get("imdb_min_votes") {
                Some(v) => v.parse().with_context(|| format!("imdb_min_votes must be a whole number, not {v:?}"))?,
                None => d.imdb_min_votes,
            },
            imdb_ratings_url: get("imdb_ratings_url").unwrap_or(&d.imdb_ratings_url).to_string(),
            tvmaze: match get("tvmaze") {
                Some(v) => !matches!(v.to_ascii_lowercase().as_str(), "0" | "off" | "false" | "no"),
                None => d.tvmaze,
            },
            tvmaze_url: get("tvmaze_url").unwrap_or(&d.tvmaze_url).to_string(),
            tvmaze_time_budget: match get("tvmaze_time_budget") {
                Some(v) => v.parse().with_context(|| format!("tvmaze_time_budget must be whole seconds, not {v:?}"))?,
                None => d.tvmaze_time_budget,
            },
            sportsdb: match get("sportsdb") {
                Some(v) => !matches!(v.to_ascii_lowercase().as_str(), "0" | "off" | "false" | "no"),
                None => d.sportsdb,
            },
            // The old free key "3" was switched off by TheSportsDB; "123" replaced it.
            sportsdb_key: get("sportsdb_key").filter(|k| *k != "3").unwrap_or(&d.sportsdb_key).to_string(),
            sportsdb_url: get("sportsdb_url").unwrap_or(&d.sportsdb_url).to_string(),
            sportsdb_time_budget: match get("sportsdb_time_budget") {
                Some(v) => v.parse().with_context(|| format!("sportsdb_time_budget must be whole seconds, not {v:?}"))?,
                None => d.sportsdb_time_budget,
            },
            tmdb_concurrency: match get("tmdb_concurrency") {
                Some(v) => v.parse().with_context(|| format!("tmdb_concurrency must be a number, not {v:?}"))?,
                None => d.tmdb_concurrency,
            },
            tmdb_time_budget: match get("tmdb_time_budget") {
                Some(v) => v.parse().with_context(|| format!("tmdb_time_budget must be whole seconds, not {v:?}"))?,
                None => d.tmdb_time_budget,
            },
            tmdb_background: match get("tmdb_background") {
                Some(v) => !matches!(v.to_ascii_lowercase().as_str(), "0" | "off" | "false" | "no"),
                None => d.tmdb_background,
            },
            refresh_hours: match get("refresh_hours") {
                Some(v) => v.parse().with_context(|| format!("refresh_hours must be a number, not {v:?}"))?,
                None => d.refresh_hours,
            },
            ..d
        };
        cfg.base_dir = data_dir.to_path_buf();
        cfg.cache_dir = data_dir.join("cache");
        cfg.rules_file = data_dir.join("rules.toml");
        if let Ok(k) = std::env::var("TMDB_API_KEY") {
            if !k.trim().is_empty() {
                cfg.tmdb_api_key = k.trim().to_string();
            }
        }
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&mut self) -> Result<()> {
        // "/srv/epg" counts as absolute on every OS (the tests run on Windows too).
        if !(self.output_dir.is_absolute() || self.output_dir.to_string_lossy().starts_with('/')) {
            bail!("output_dir must be an absolute path, not {}", self.output_dir.display());
        }
        if self.tmdb_concurrency == 0 {
            self.tmdb_concurrency = 1;
        }
        if self.logo_country == "gb" {
            self.logo_country = "uk".into();
        }
        if !(self.logo_country.len() == 2 && self.logo_country.chars().all(|c| c.is_ascii_lowercase())) {
            self.logo_country = "us".into();
        }
        if !(self.refresh_hours.is_finite() && self.refresh_hours > 0.0) {
            self.refresh_hours = 6.0;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct ChannelsFile {
    pub channels: HashMap<String, ChannelOverride>,
}

#[derive(Debug, Default, Clone)]
pub struct ChannelOverride {
    pub logo: Option<String>,
    pub group: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn settings_with_defaults() {
        let dir = Path::new("/data");
        let c = Config::from_settings(&map(&[("rating_country", "gb")]), dir).unwrap();
        assert_eq!(c.language, "en-US");
        assert_eq!(c.rating_country, "GB");
        assert_eq!(c.logo_source, LogoSource::IptvOrg);
        assert_eq!(c.refresh_hours, 6.0);
        assert_eq!(c.output_dir, dir, "guide files go to the data folder unless told otherwise");
        assert_eq!(c.rules_file, dir.join("rules.toml"));
    }

    #[test]
    fn rejects_bad_settings() {
        let dir = Path::new("/data");
        assert!(Config::from_settings(&map(&[]), dir).is_ok());
        assert!(Config::from_settings(&map(&[("output_dir", "relative")]), dir).is_err());
        assert!(Config::from_settings(&map(&[("refresh_hours", "abc")]), dir).is_err());
        let off = Config::from_settings(&map(&[("logo_source", "off")]), dir).unwrap();
        assert_eq!(off.logo_source, LogoSource::Off);
    }
}
