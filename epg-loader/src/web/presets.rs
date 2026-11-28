//! Public guides to pick from when adding a guide (api/presets?provider=...).
//!
//! The lists are read from the providers' own pages when asked for, so new
//! guides show up without an EPGArr update, and kept for 12 hours:
//!   - epgshare01.online: its file listing (epg_ripper_XX1.xml.gz)
//!   - iptv-epg.org/guides: the links to files/epg-xx.xml.gz
//!   - epg.guru: the market list in its page, as the 7-day IPTV files on cdn.epg.guru

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

use super::{json, Shared};

const KEEP_SECS: i64 = 12 * 3600;

pub struct Provider {
    pub id: &'static str,
    pub name: &'static str,
    pub page: &'static str,
}

pub const PROVIDERS: [Provider; 3] = [
    Provider { id: "epgshare01", name: "epgshare01", page: "https://epgshare01.online/epgshare01/" },
    Provider { id: "iptv-epg", name: "iptv-epg.org", page: "https://iptv-epg.org/guides" },
    Provider { id: "epg-guru", name: "epg.guru", page: "https://epg.guru/" },
];

/// One guide: (name, source URL).
pub type Preset = (String, String);

static CACHE: Mutex<Vec<(&'static str, i64, Vec<Preset>)>> = Mutex::new(Vec::new());

fn country(code: &str) -> Option<&'static str> {
    const C: &[(&str, &str)] = &[
        ("ad", "Andorra"), ("ae", "United Arab Emirates"), ("af", "Afghanistan"), ("al", "Albania"), ("am", "Armenia"),
        ("ao", "Angola"), ("ar", "Argentina"), ("at", "Austria"), ("au", "Australia"), ("az", "Azerbaijan"),
        ("ba", "Bosnia and Herzegovina"), ("bb", "Barbados"), ("bd", "Bangladesh"), ("be", "Belgium"), ("bg", "Bulgaria"),
        ("bh", "Bahrain"), ("bo", "Bolivia"), ("br", "Brazil"), ("bs", "Bahamas"), ("by", "Belarus"), ("bz", "Belize"),
        ("ca", "Canada"), ("ch", "Switzerland"), ("cl", "Chile"), ("cn", "China"), ("co", "Colombia"), ("cr", "Costa Rica"),
        ("cu", "Cuba"), ("cy", "Cyprus"), ("cz", "Czechia"), ("de", "Germany"), ("dk", "Denmark"), ("do", "Dominican Republic"),
        ("dz", "Algeria"), ("ec", "Ecuador"), ("ee", "Estonia"), ("eg", "Egypt"), ("es", "Spain"), ("fi", "Finland"),
        ("fr", "France"), ("gb", "United Kingdom"), ("ge", "Georgia"), ("gh", "Ghana"), ("gr", "Greece"), ("gt", "Guatemala"),
        ("hk", "Hong Kong"), ("hn", "Honduras"), ("hr", "Croatia"), ("ht", "Haiti"), ("hu", "Hungary"), ("id", "Indonesia"),
        ("ie", "Ireland"), ("il", "Israel"), ("in", "India"), ("iq", "Iraq"), ("ir", "Iran"), ("is", "Iceland"), ("it", "Italy"),
        ("jm", "Jamaica"), ("jo", "Jordan"), ("jp", "Japan"), ("ke", "Kenya"), ("kr", "South Korea"), ("kw", "Kuwait"),
        ("kz", "Kazakhstan"), ("lb", "Lebanon"), ("lk", "Sri Lanka"), ("lt", "Lithuania"), ("lu", "Luxembourg"), ("lv", "Latvia"),
        ("ma", "Morocco"), ("md", "Moldova"), ("me", "Montenegro"), ("mk", "North Macedonia"), ("mn", "Mongolia"), ("mt", "Malta"),
        ("mx", "Mexico"), ("my", "Malaysia"), ("ng", "Nigeria"), ("ni", "Nicaragua"), ("nl", "Netherlands"), ("no", "Norway"),
        ("np", "Nepal"), ("nz", "New Zealand"), ("om", "Oman"), ("pa", "Panama"), ("pe", "Peru"), ("ph", "Philippines"),
        ("pk", "Pakistan"), ("pl", "Poland"), ("pr", "Puerto Rico"), ("pt", "Portugal"), ("py", "Paraguay"), ("qa", "Qatar"),
        ("ro", "Romania"), ("rs", "Serbia"), ("ru", "Russia"), ("sa", "Saudi Arabia"), ("se", "Sweden"), ("sg", "Singapore"),
        ("si", "Slovenia"), ("sk", "Slovakia"), ("sv", "El Salvador"), ("th", "Thailand"), ("tn", "Tunisia"), ("tr", "Turkey"),
        ("tt", "Trinidad and Tobago"), ("tw", "Taiwan"), ("ua", "Ukraine"), ("uk", "United Kingdom"), ("us", "United States"),
        ("uy", "Uruguay"), ("uz", "Uzbekistan"), ("ve", "Venezuela"), ("vn", "Vietnam"), ("xk", "Kosovo"), ("za", "South Africa"),
    ];
    C.iter().find(|(c, _)| *c == code).map(|(_, n)| *n)
}

/// "ALL_SOURCES" -> "All Sources"; "UnitedStates-Locals" -> "United States - Locals"
fn words(s: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for ch in s.chars() {
        if ch == '_' {
            out.push(' ');
            prev_lower = false;
            continue;
        }
        if ch == '-' {
            out.push_str(" - ");
            prev_lower = false;
            continue;
        }
        if ch.is_ascii_uppercase() && prev_lower {
            out.push(' ');
        }
        prev_lower = ch.is_ascii_lowercase();
        out.push(ch);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let w = w.to_lowercase();
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Every `attr="..."` value in a page.
fn quoted_after<'a>(html: &'a str, start: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find(start) {
        rest = &rest[i + start.len()..];
        if let Some(end) = rest.find(['"', '\'']) {
            out.push(&rest[..end]);
        }
    }
    out
}

pub fn parse_epgshare(html: &str, base: &str) -> Vec<Preset> {
    // "epg_ripper_US2.xml.gz" -> ("US", "2"). A number is shown only when a name has several files.
    let files: Vec<(String, String, &str)> = quoted_after(html, "href=\"")
        .into_iter()
        .filter(|h| h.ends_with(".xml.gz") && !h.contains('/'))
        .map(|h| {
            let stem = h.trim_end_matches(".xml.gz").trim_start_matches("epg_ripper_");
            let name = stem.trim_end_matches(|c: char| c.is_ascii_digit());
            (name.to_string(), stem[name.len()..].to_string(), h)
        })
        .collect();
    let mut v: Vec<Preset> = files
        .iter()
        .map(|(name, n, h)| {
            let label = match country(&name.to_ascii_lowercase()) {
                Some(c) if name.len() == 2 => c.to_string(),
                _ => title_case(&words(name)),
            };
            let several = files.iter().filter(|(x, _, _)| x == name).count() > 1;
            let label = if several && !n.is_empty() { format!("{label} ({n})") } else { label };
            (label, format!("{base}{h}"))
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

pub fn parse_iptv_epg(html: &str) -> Vec<Preset> {
    let mut v: Vec<Preset> = quoted_after(html, "https://iptv-epg.org/files/")
        .into_iter()
        .filter(|f| f.ends_with(".xml.gz"))
        .map(|f| {
            let code = f.trim_end_matches(".xml.gz").trim_start_matches("epg-");
            let label = country(code).map(String::from).unwrap_or_else(|| title_case(&words(code)));
            (label, format!("https://iptv-epg.org/files/{f}"))
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

pub fn parse_epg_guru(html: &str) -> Vec<Preset> {
    // The page lists its markets as "Name_Channel_List.txt"; each has a 7-day IPTV guide on the CDN.
    let mut v: Vec<Preset> = quoted_after(html, "\"")
        .into_iter()
        .filter_map(|s| s.strip_suffix("_Channel_List.txt").or_else(|| s.strip_suffix("_channel_list.txt")))
        .filter(|b| !b.is_empty() && b.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .map(|b| (words(b), format!("https://cdn.epg.guru/7dayiptv/{b}.xml.gz")))
        .collect();
    v.sort();
    v.dedup();
    v
}

async fn fetch(p: &Provider) -> Result<Vec<Preset>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent(concat!("EPGArr/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let r = client.get(p.page).send().await.map_err(|_| format!("Could not reach {}.", p.name))?;
    if !r.status().is_success() {
        return Err(format!("{} answered HTTP {}.", p.name, r.status().as_u16()));
    }
    let html = r.text().await.map_err(|_| format!("Could not read {}.", p.name))?;
    let list = match p.id {
        "epgshare01" => parse_epgshare(&html, p.page),
        "iptv-epg" => parse_iptv_epg(&html),
        _ => parse_epg_guru(&html),
    };
    if list.is_empty() {
        return Err(format!("No guides found on {}; the site may have changed. You can still paste an address from {}.", p.name, p.page));
    }
    Ok(list)
}

pub async fn get(State(app): State<Shared>, headers: HeaderMap, axum::extract::Query(q): Query<HashMap<String, String>>) -> Response {
    let ctx = app.ctx(&headers);
    if ctx.user.is_none() {
        return json(&ctx, StatusCode::UNAUTHORIZED, json!({ "error": "Log in first." }));
    }
    let Some(p) = PROVIDERS.iter().find(|p| q.get("provider").is_some_and(|x| x == p.id)) else {
        let list: Vec<Value> = PROVIDERS.iter().map(|p| json!({ "id": p.id, "name": p.name, "page": p.page })).collect();
        return json(&ctx, StatusCode::OK, json!({ "providers": list }));
    };
    let now = crate::timeutil::now_unix();
    let cached = CACHE.lock().unwrap().iter().find(|(id, at, _)| *id == p.id && now - at < KEEP_SECS).map(|(_, _, l)| l.clone());
    let list = match cached {
        Some(l) => l,
        None => match fetch(p).await {
            Ok(l) => {
                let mut c = CACHE.lock().unwrap();
                c.retain(|(id, _, _)| *id != p.id);
                c.push((p.id, now, l.clone()));
                l
            }
            Err(e) => return json(&ctx, StatusCode::OK, json!({ "error": e })),
        },
    };
    let results: Vec<Value> = list.into_iter().map(|(name, url)| json!({ "name": name, "url": url })).collect();
    json(&ctx, StatusCode::OK, json!({ "provider": p.name, "page": p.page, "results": results }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_three_sites() {
        let es = parse_epgshare(
            r#"<a href="epg_ripper_US1.xml.gz">x</a><a href="epg_ripper_IN2.xml.gz">x</a><a href="epg_ripper_IN4.xml.gz">x</a><a href="epg_ripper_ALL_SOURCES1.xml.gz">x</a><a href="../">up</a>"#,
            "https://epgshare01.online/epgshare01/",
        );
        assert!(es.contains(&("United States".into(), "https://epgshare01.online/epgshare01/epg_ripper_US1.xml.gz".into())));
        assert!(es.iter().any(|(n, _)| n == "India (2)"));
        assert!(es.iter().any(|(n, _)| n == "All Sources"));
        assert_eq!(es.len(), 4);

        let ie = parse_iptv_epg(r#"<a href="https://iptv-epg.org/files/epg-gb.xml">XML</a><a href="https://iptv-epg.org/files/epg-gb.xml.gz">GZ</a>"#);
        assert_eq!(ie, vec![("United Kingdom".to_string(), "https://iptv-epg.org/files/epg-gb.xml.gz".to_string())]);

        let eg = parse_epg_guru(r#"const markets = { "North America": ["Canada_Channel_List.txt", "UnitedStates-Locals_Channel_List.txt"], "Caribbean": ["Aruba_channel_list.txt"] };"#);
        assert_eq!(eg.len(), 3);
        assert!(eg.contains(&("United States - Locals".into(), "https://cdn.epg.guru/7dayiptv/UnitedStates-Locals.xml.gz".into())));
    }
}
