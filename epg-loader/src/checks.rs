//! `epg-loader check`: connection tests for Settings › Advanced and the Guides page. Prints JSON.
//! Never includes the TMDB key or the source URL in its output.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use crate::{db, source, timeutil, tmdb, xmltv};

fn result(ok: bool, detail: impl Into<String>) -> Value {
    json!({ "ok": ok, "detail": detail.into() })
}

/// `source`: the guide source to test (the first guide, or --guide); None falls back to the settings.
pub async fn run(conn: &Connection, data_dir: &Path, _output_dir: Option<&Path>, source: Option<String>) -> Value {
    let settings = db::settings(conn).unwrap_or_default();
    let get = |k: &str| settings.get(k).map(|v| v.trim().to_string()).unwrap_or_default();
    let key = std::env::var("TMDB_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .unwrap_or_else(|| get("tmdb_api_key"));
    let src = source.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| get("source_url"));

    let client = match reqwest::Client::builder()
        .user_agent(concat!("epg-loader/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let r = result(false, format!("cannot create an HTTP client: {e}"));
            return json!({ "checked_at": timeutil::now_unix(), "internet": r.clone(), "tmdb": r.clone(), "source": r });
        }
    };

    let internet = match tmdb::check_reachable(&client).await {
        Ok(code) => result(true, format!("api.themoviedb.org answered (HTTP {code})")),
        Err(e) => result(false, format!("{e:#}")),
    };

    let tmdb_key = if key.trim().is_empty() {
        result(false, "no TMDB key saved yet")
    } else {
        match tmdb::check_key(&client, key.trim()).await {
            Ok(()) => result(true, "the TMDB key works"),
            Err(e) => result(false, format!("{e:#}")),
        }
    };

    let source = if src.is_empty() {
        result(false, "no XMLTV source URL saved yet")
    } else {
        match source::fetch(&client, &src, data_dir).await {
            Err(e) => result(false, format!("{e:#}")),
            Ok(raw) => {
                let size = raw.len();
                let gz = source::is_gzip(&raw);
                match source::decode(raw) {
                    Err(e) => result(false, format!("downloaded {size} bytes but could not gunzip them: {e:#}")),
                    Ok(xml) => {
                        let head = String::from_utf8_lossy(&xml[..xml.len().min(4096)]).to_string();
                        if !head.contains("<tv") {
                            let preview: String = head.chars().filter(|c| !c.is_control()).take(80).collect();
                            result(false, format!("downloaded {size} bytes, but it does not look like XMLTV (starts with: {preview})"))
                        } else {
                            match xmltv::scan(&xml) {
                                Ok(s) => result(
                                    true,
                                    format!(
                                        "XMLTV{}: {} bytes, {} channels, {} programmes",
                                        if gz { " (gzipped)" } else { "" },
                                        xml.len(),
                                        s.channels.len(),
                                        s.programmes.len()
                                    ),
                                ),
                                Err(e) => result(false, format!("XMLTV, but it does not parse: {e:#}")),
                            }
                        }
                    }
                }
            }
        }
    };

    json!({
        "checked_at": timeutil::now_unix(),
        "internet": internet,
        "tmdb": tmdb_key,
        "source": source,
    })
}
