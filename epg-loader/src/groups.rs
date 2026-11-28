//! Choosing a main group (category) for each programme.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

use crate::tmdb::TmdbInfo;

pub const GROUPS: [&str; 7] = ["Movie", "Series", "News", "Sports", "Kids", "Music", "Documentary"];

/// Shipped rules, used when rules.toml is missing.
pub const DEFAULT_RULES: &str = include_str!("../rules.toml");

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Rules {
    pub category_rule: Vec<Rule>,
    pub keyword_rule: Vec<Rule>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub group: String,
    #[serde(rename = "match")]
    pub matches: Vec<String>,
}

impl Rules {
    pub fn load(path: &Path) -> Result<Rules> {
        if path.exists() {
            let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
        } else {
            Self::parse(DEFAULT_RULES).context("parsing built-in rules")
        }
    }

    pub fn parse(text: &str) -> Result<Rules> {
        let mut r: Rules = toml::from_str(text)?;
        for rule in r.category_rule.iter_mut().chain(r.keyword_rule.iter_mut()) {
            rule.group = canonical_group(&rule.group);
            rule.matches = rule.matches.iter().map(|m| normalize_words(m)).filter(|m| !m.is_empty()).collect();
        }
        Ok(r)
    }
}

/// Lowercase words separated by single spaces; "&" becomes "and".
/// "Grey's Anatomy!" -> "grey s anatomy"
pub fn normalize_words(s: &str) -> String {
    fn push_word_char(out: &mut String, gap: &mut bool, c: char) {
        if *gap && !out.is_empty() {
            out.push(' ');
        }
        *gap = false;
        out.extend(c.to_lowercase());
    }
    let mut out = String::with_capacity(s.len());
    let mut gap = false;
    for c in s.chars() {
        if c == '&' {
            gap = true;
            for a in "and".chars() {
                push_word_char(&mut out, &mut gap, a);
            }
            gap = true;
        } else if c.is_alphanumeric() {
            push_word_char(&mut out, &mut gap, c);
        } else {
            gap = true;
        }
    }
    out
}

/// Whole-word / whole-phrase match. `phrases` must already be normalised.
fn has_phrase(text: &str, phrases: &[String]) -> bool {
    let hay = format!(" {} ", normalize_words(text));
    phrases.iter().any(|p| hay.contains(&format!(" {p} ")))
}

/// "kids" -> "Kids"; unknown names are kept as typed.
pub fn canonical_group(g: &str) -> String {
    let t = g.trim();
    GROUPS
        .iter()
        .find(|x| x.eq_ignore_ascii_case(t))
        .map(|x| x.to_string())
        .unwrap_or_else(|| t.to_string())
}

pub struct GroupInput<'a> {
    pub override_group: Option<&'a str>,
    pub categories: &'a [String],
    pub tmdb: Option<&'a TmdbInfo>,
    pub title: &'a str,
    pub is_tv: bool,
}

/// Steps 1 and 2: the channel override's group, then the source's own categories.
pub fn source_group(override_group: Option<&str>, categories: &[String], rules: &Rules) -> Option<String> {
    if let Some(g) = override_group.map(str::trim).filter(|g| !g.is_empty()) {
        return Some(canonical_group(g));
    }
    for c in categories {
        if let Some(g) = GROUPS.iter().find(|x| x.eq_ignore_ascii_case(c.trim())) {
            return Some(g.to_string());
        }
    }
    for rule in &rules.category_rule {
        if categories.iter().any(|c| has_phrase(c, &rule.matches)) {
            return Some(rule.group.clone());
        }
    }
    None
}

// TMDB genre ids (the same in every language).
const G_ANIMATION: u64 = 16;
const G_DOCUMENTARY: u64 = 99;
const G_MUSIC: u64 = 10402;
const G_FAMILY: u64 = 10751;
const G_KIDS: u64 = 10762;
const G_NEWS: u64 = 10763;

/// Step 3: TMDB genres, then media type.
pub fn tmdb_group(info: &TmdbInfo) -> Option<&'static str> {
    if !info.found {
        return None;
    }
    let has = |g: u64| info.genre_ids.contains(&g);
    if has(G_KIDS) || (has(G_ANIMATION) && has(G_FAMILY)) {
        return Some("Kids");
    }
    if has(G_DOCUMENTARY) {
        return Some("Documentary");
    }
    if has(G_NEWS) {
        return Some("News");
    }
    if has(G_MUSIC) {
        return Some("Music");
    }
    match info.media_type.as_str() {
        "movie" => Some("Movie"),
        "tv" => Some("Series"),
        _ => None,
    }
}

pub fn choose_group(input: &GroupInput, rules: &Rules) -> Option<String> {
    if let Some(g) = source_group(input.override_group, input.categories, rules) {
        return Some(g);
    }
    if let Some(g) = input.tmdb.and_then(tmdb_group) {
        return Some(g.to_string());
    }
    for rule in &rules.keyword_rule {
        if has_phrase(input.title, &rule.matches) {
            return Some(rule.group.clone());
        }
    }
    if input.is_tv {
        return Some("Series".to_string());
    }
    None
}

/// True unless the programme already has this category (case-insensitive).
pub fn needs_category(group: &str, existing: &[String]) -> bool {
    !existing.iter().any(|c| c.trim().eq_ignore_ascii_case(group))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Rules {
        Rules::parse(DEFAULT_RULES).unwrap()
    }

    fn cats(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn info(media: &str, genres: &[u64]) -> TmdbInfo {
        TmdbInfo { found: true, media_type: media.into(), genre_ids: genres.to_vec(), ..Default::default() }
    }

    fn pick(over: Option<&str>, c: &[&str], t: Option<&TmdbInfo>, title: &str, tv: bool) -> Option<String> {
        let c = cats(c);
        choose_group(&GroupInput { override_group: over, categories: &c, tmdb: t, title, is_tv: tv }, &rules())
    }

    #[test]
    fn normalises_words() {
        assert_eq!(normalize_words("Grey's Anatomy!"), "grey s anatomy");
        assert_eq!(normalize_words("Law & Order"), "law and order");
        assert_eq!(normalize_words("  NEWS  at   Ten "), "news at ten");
    }

    #[test]
    fn maps_source_categories() {
        assert_eq!(pick(None, &["Film"], None, "x", false).as_deref(), Some("Movie"));
        assert_eq!(pick(None, &["series"], None, "x", false).as_deref(), Some("Series"));
        assert_eq!(pick(None, &["Sports event"], None, "x", false).as_deref(), Some("Sports"));
        assert_eq!(pick(None, &["Movie / Drama"], None, "x", false).as_deref(), Some("Movie"));
        assert_eq!(pick(None, &["Children's"], None, "x", false).as_deref(), Some("Kids"));
        // "Newsroom" is not the word "news".
        assert_eq!(pick(None, &["Newsroom"], None, "x", false), None);
    }

    #[test]
    fn channel_override_wins() {
        assert_eq!(pick(Some("kids"), &["Film"], Some(&info("movie", &[])), "x", false).as_deref(), Some("Kids"));
        assert_eq!(pick(Some("  "), &["Film"], None, "x", false).as_deref(), Some("Movie"));
    }

    #[test]
    fn uses_tmdb_genres_then_media_type() {
        assert_eq!(pick(None, &[], Some(&info("movie", &[18])), "x", false).as_deref(), Some("Movie"));
        assert_eq!(pick(None, &[], Some(&info("tv", &[18])), "x", false).as_deref(), Some("Series"));
        assert_eq!(pick(None, &[], Some(&info("movie", &[99])), "x", false).as_deref(), Some("Documentary"));
        assert_eq!(pick(None, &[], Some(&info("tv", &[10762, 16])), "x", true).as_deref(), Some("Kids"));
        assert_eq!(pick(None, &[], Some(&info("tv", &[10763])), "x", true).as_deref(), Some("News"));
        let not_found = TmdbInfo::default();
        assert_eq!(pick(None, &[], Some(&not_found), "x", false), None);
    }

    #[test]
    fn falls_back_to_title_keywords_then_series() {
        assert_eq!(pick(None, &[], None, "Premier League Football", false).as_deref(), Some("Sports"));
        assert_eq!(pick(None, &[], None, "Evening News", false).as_deref(), Some("News"));
        assert_eq!(pick(None, &[], None, "Some Drama", true).as_deref(), Some("Series"));
        assert_eq!(pick(None, &[], None, "Some Drama", false), None);
    }

    #[test]
    fn never_duplicates_a_category() {
        assert!(!needs_category("Movie", &cats(&["Film", " movie "])));
        assert!(needs_category("Movie", &cats(&["Film"])));
    }
}
