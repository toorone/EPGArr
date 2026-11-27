//! Small helpers shared by the pages: numbers, times, guide files and status.

use axum::http::HeaderMap;
use std::collections::HashMap;

use super::App;
use crate::db;
use crate::timeutil;

/// "12,345"
pub fn num(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// "3 min ago", "in 2.5 h", "just now", "—"
pub fn ago(t: i64) -> String {
    if t <= 0 {
        return "—".into();
    }
    let d = timeutil::now_unix() - t;
    let a = d.abs();
    let s = if a < 90 {
        return "just now".into();
    } else if a < 3600 {
        format!("{} min", (a as f64 / 60.0).round() as i64)
    } else if a < 172_800 {
        format!("{:.1} h", a as f64 / 3600.0).replace(".0 h", " h")
    } else {
        format!("{} days", (a as f64 / 86_400.0).round() as i64)
    };
    if d < 0 { format!("in {s}") } else { format!("{s} ago") }
}

/// A local date and time, e.g. "Thu 27 Nov, 15:10" (in the container's time zone, TZ).
pub fn when(t: i64) -> String {
    if t <= 0 {
        return "—".into();
    }
    match jiff::Timestamp::from_second(t) {
        Ok(ts) => ts.to_zoned(jiff::tz::TimeZone::system()).strftime("%a %-d %b, %H:%M").to_string(),
        Err(_) => "—".into(),
    }
}

/// The same with the year, e.g. "Thu 27 Nov 2026, 15:10".
pub fn when_long(t: i64) -> String {
    if t <= 0 {
        return "—".into();
    }
    match jiff::Timestamp::from_second(t) {
        Ok(ts) => ts.to_zoned(jiff::tz::TimeZone::system()).strftime("%a %-d %b %Y, %H:%M").to_string(),
        Err(_) => "—".into(),
    }
}

/// "2.5" for 2.5, "6" for 6.0
pub fn hours_text(h: f64) -> String {
    let s = format!("{h:.1}");
    s.strip_suffix(".0").unwrap_or(&s).to_string()
}

pub fn settings(app: &App) -> HashMap<String, String> {
    app.db().ok().and_then(|c| db::settings(&c).ok()).unwrap_or_default()
}

pub fn default_hours(app: &App) -> f64 {
    settings(app).get("refresh_hours").and_then(|v| v.parse().ok()).filter(|h: &f64| *h > 0.0).unwrap_or(6.0)
}

/// Where a guide is served: (xml url, gz url).
pub fn guide_urls(headers: &HeaderMap, g: &db::Guide) -> (String, String) {
    let base = super::base_url(headers);
    (format!("{base}/guides/{}.xml", g.slug), format!("{base}/guides/{}.xml.gz", g.slug))
}

/// The guide's last status file (written by its build), or {}.
pub fn guide_status(app: &App, g: &db::Guide) -> serde_json::Value {
    std::fs::read(app.output_dir.join("guides").join(format!("status-{}.json", g.id)))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

pub fn st_i64(v: &serde_json::Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64()).unwrap_or(0)
}

/// The host of a source URL for display (never the full URL: it may hold a password).
pub fn source_label(src: &str) -> String {
    if src.trim().is_empty() {
        return "no source set".into();
    }
    match src.split_once("://") {
        Some((_, rest)) => {
            let host = rest.split(['/', '?', '#']).next().unwrap_or("");
            let host = host.rsplit('@').next().unwrap_or(host);
            let host = host.split(':').next().unwrap_or(host);
            if host.is_empty() { "file on the server".into() } else { host.to_string() }
        }
        None => "file on the server".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(num(1234567), "1,234,567");
        assert_eq!(num(12), "12");
        assert_eq!(hours_text(6.0), "6");
        assert_eq!(hours_text(2.5), "2.5");
        assert_eq!(source_label("http://user:pw@epg.example.com:8080/x.xml"), "epg.example.com");
        assert_eq!(source_label("/srv/guide.xml"), "file on the server");
    }
}
