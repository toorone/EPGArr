//! "Identify on TMDB" in the TV Guide (api/tmdb). JSON only, login and CSRF required.
//!
//!   GET  ?q=<text>&type=multi|movie|tv     search TMDB
//!   GET  ?title=<programme title>          the current match for that title
//!   POST action=set, title, media_type, id  use this TMDB title for every programme called `title`
//!   POST action=clear, title               back to the automatic match
//!
//! A match made here is kept in the tmdb_manual table and wins over the
//! automatic search in every build from now on. The TV Guide shows it at once;
//! the guide files get it with the next build.

use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashMap;

use super::{json, read_post, Shared};
use crate::groups::normalize_words;
use crate::tmdb::{self, Tmdb, TmdbInfo};

fn tmdb_url(media_type: &str, id: u64) -> String {
    format!("https://www.themoviedb.org/{}/{id}", if media_type == "movie" { "movie" } else { "tv" })
}

fn info_json(i: &TmdbInfo, manual: bool) -> Value {
    json!({
        "manual": manual,
        "media_type": i.media_type,
        "id": i.id,
        "name": i.name,
        "poster": i.poster_url(),
        "url": tmdb_url(&i.media_type, i.id),
    })
}

/// The match the guides use for this title: one made by hand, else the automatic one.
pub fn current_match(conn: &Connection, title: &str) -> Value {
    if let Some(m) = tmdb::manual_for_title(conn, title) {
        return info_json(&m, true);
    }
    let auto = conn
        .query_row(
            "SELECT media_type, tmdb_id, name, poster_path FROM tmdb_cache WHERE norm_title = ?1 AND found = 1
             ORDER BY fetched_at DESC LIMIT 1",
            params![normalize_words(title)],
            |r| {
                Ok(TmdbInfo {
                    found: true,
                    media_type: r.get(0)?,
                    id: r.get::<_, i64>(1)?.max(0) as u64,
                    name: r.get(2)?,
                    poster_path: r.get(3)?,
                    ..Default::default()
                })
            },
        )
        .optional()
        .ok()
        .flatten();
    auto.map_or(Value::Null, |a| info_json(&a, false))
}

/// A TMDB client with the saved key, or a message for the dialog.
fn client(app: &Shared, conn: &Connection) -> Result<Tmdb, String> {
    let cfg = crate::db::load_config(conn, &app.data_dir, Some(&app.output_dir)).map_err(|e| format!("{e:#}"))?;
    if cfg.tmdb_api_key.is_empty() {
        return Err("Add your TMDB key in Settings › Artwork & info first.".into());
    }
    let http = reqwest::Client::builder()
        .user_agent(concat!("EPGArr/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    Ok(Tmdb::new(http, &cfg.tmdb_api_key, &cfg.language, &cfg.rating_country, 2))
}

fn bad(msg: impl Into<String>) -> (StatusCode, Value) {
    (StatusCode::OK, json!({ "error": msg.into() }))
}

pub async fn get(State(app): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let ctx = app.ctx(&headers);
    if ctx.user.is_none() {
        return json(&ctx, StatusCode::UNAUTHORIZED, json!({ "error": "Log in first." }));
    }
    if !ctx.admin {
        return json(&ctx, StatusCode::FORBIDDEN, json!({ "error": "Only admins can do that." }));
    }
    let conn = match app.db() {
        Ok(c) => c,
        Err(e) => return json(&ctx, StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": format!("{e:#}") })),
    };
    if let Some(title) = q.get("title") {
        return json(&ctx, StatusCode::OK, json!({ "match": current_match(&conn, title) }));
    }
    let query = q.get("q").map(|s| s.trim().to_string()).unwrap_or_default();
    if query.is_empty() || query.len() > 200 {
        return json(&ctx, StatusCode::OK, json!({ "error": "Type a title to search for." }));
    }
    let t = match client(&app, &conn) {
        Ok(t) => t,
        Err(e) => return json(&ctx, StatusCode::OK, json!({ "error": e })),
    };
    drop(conn);
    match t.search(&query, q.get("type").map(String::as_str).unwrap_or("multi")).await {
        Ok(results) => json(&ctx, StatusCode::OK, json!({ "results": results })),
        Err(e) => json(&ctx, StatusCode::OK, json!({ "error": format!("TMDB search failed: {e:#}") })),
    }
}

pub async fn post(State(app): State<Shared>, headers: HeaderMap, req: Request) -> Response {
    let ctx = app.ctx(&headers);
    if ctx.user.is_none() {
        return json(&ctx, StatusCode::OK, json!({ "error": "You were logged out. Reload the page and log in again." }));
    }
    if !ctx.admin {
        return json(&ctx, StatusCode::FORBIDDEN, json!({ "error": "Only admins can do that." }));
    }
    let (f, _) = read_post(req, 0).await;
    if f.get("csrf") != ctx.csrf {
        return json(&ctx, StatusCode::OK, json!({ "error": "The page expired. Reload it and try again." }));
    }
    let (code, body) = handle(&app, &f).await;
    json(&ctx, code, body)
}

async fn handle(app: &Shared, f: &super::guides::Posted) -> (StatusCode, Value) {
    let title = f.get("title").trim().to_string();
    if title.is_empty() || title.len() > 300 || normalize_words(&title).chars().count() < 2 {
        return bad("Which programme?");
    }
    let conn = match app.db() {
        Ok(c) => c,
        Err(e) => return bad(format!("{e:#}")),
    };
    match f.get("action").as_str() {
        "clear" => match tmdb::clear_manual(&conn, &title) {
            Ok(_) => (
                StatusCode::OK,
                json!({ "ok": true, "message": "Back to the automatic match from the next build.", "match": current_match(&conn, &title) }),
            ),
            Err(e) => bad(format!("{e:#}")),
        },
        "set" => {
            let media_type = f.get("media_type");
            let id: u64 = f.get("id").parse().unwrap_or(0);
            if !matches!(media_type.as_str(), "movie" | "tv") || id == 0 {
                return bad("Pick a result first.");
            }
            let t = match client(app, &conn) {
                Ok(t) => t,
                Err(e) => return bad(e),
            };
            let info = match t.by_id(&media_type, id).await {
                Ok(i) => i,
                Err(e) => return bad(format!("Could not get that title from TMDB: {e:#}")),
            };
            if let Err(e) = tmdb::save_manual(&conn, &title, &info) {
                return bad(format!("Could not save the match: {e:#}"));
            }
            // Show it in the TV Guide straight away; the guide files get it with the next build.
            let poster = info.poster_url();
            let desc = info.overview.as_deref().map(|o| tmdb::short_desc(o, 500)).filter(|d| !d.is_empty());
            let _ = conn.execute(
                "UPDATE guide_programmes SET icon = COALESCE(?2, icon), descr = COALESCE(NULLIF(descr, ''), ?3),
                     rating = COALESCE(NULLIF(rating, ''), ?4)
                 WHERE title = ?1 COLLATE NOCASE",
                params![title, poster, desc, info.certification],
            );
            (
                StatusCode::OK,
                json!({
                    "ok": true,
                    "message": format!("Saved: every \"{title}\" is now {} on TMDB. It is in the TV Guide now; your media server gets it with the next build.", info.name),
                    "match": info_json(&info, true),
                }),
            )
        }
        _ => bad("Unknown action."),
    }
}
