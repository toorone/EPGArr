//! The TV Guide page and its data feed (api/guide).

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashMap;

use super::html::{esc, icon, json_script, page};
use super::{auth, html_page, json, Ctx, Shared};
use crate::db;
use crate::timeutil;

/// The guide to show: ?guide=slug, else the first one that is on.
fn pick_guide<'a>(guides: &'a [db::Guide], slug: &str) -> Option<&'a db::Guide> {
    guides
        .iter()
        .find(|g| g.slug == slug)
        .or_else(|| guides.iter().find(|g| g.enabled))
        .or_else(|| guides.first())
}

pub async fn guide_page(State(app): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let ctx = app.ctx(&headers);
    if ctx.user.is_none() {
        let next = match q.get("guide") {
            Some(g) => format!("tv?guide={}", url_encode(g)),
            None => "tv".into(),
        };
        return auth::login_page(&app, &ctx, &next);
    }
    let (guides, empty) = match app.db() {
        Ok(conn) => {
            let guides = db::guides(&conn).unwrap_or_default();
            let sel = pick_guide(&guides, q.get("guide").map(String::as_str).unwrap_or(""));
            let has: i64 = sel
                .map(|g| {
                    conn.query_row("SELECT COUNT(*) FROM (SELECT 1 FROM guide_programmes WHERE guide_id = ?1 LIMIT 1)", params![g.id], |r| r.get(0))
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            let built = sel.is_some_and(|g| super::common::st_i64(&super::common::guide_status(&app, g), "built_at_unix") > 0);
            let empty = match sel {
                _ if has > 0 => String::new(),
                Some(g) if built => format!(
                    "The guide \"{}\" is built, but its source has no programmes from today on. Check the source on the Guides page.",
                    g.name
                ),
                Some(g) => format!("The guide \"{}\" has not been built yet. Build it on the Guides page.", g.name),
                None => "No guide has been added yet. Add one on the Guides page and it will show up here.".to_string(),
            };
            (guides, empty)
        }
        Err(e) => (Vec::new(), format!("{e:#}")),
    };
    let sel = pick_guide(&guides, q.get("guide").map(String::as_str).unwrap_or(""));
    page_html(&ctx, &guides, sel, &empty)
}

fn page_html(ctx: &Ctx, guides: &[db::Guide], sel: Option<&db::Guide>, empty: &str) -> Response {
    let picker = match sel {
        Some(s) if !guides.is_empty() => {
            let items: String = guides
                .iter()
                .map(|g| {
                    let on = g.slug == s.slug;
                    format!(
                        r#"<a role="option" href="tv?guide={v}" aria-selected="{on}" class="g-pick-item{cls}"><span>{n}</span>{off}{tick}</a>"#,
                        v = url_encode(&g.slug),
                        cls = if on { " on" } else { "" },
                        n = esc(&g.name),
                        off = if g.enabled { "" } else { r#"<span class="pill">off</span>"# },
                        tick = if on { icon("check") } else { String::new() },
                    )
                })
                .collect();
            format!(
                r#"<div class="g-pick" id="g-pick"><button type="button" class="g-pick-btn" id="g-pick-btn" aria-haspopup="listbox" aria-expanded="false" aria-label="Guide: {name}">{ic}<span class="g-pick-name">{name}</span><svg class="g-pick-caret" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="m6 9 6 6 6-6"/></svg></button><div class="g-pick-menu" id="g-pick-menu" role="listbox" aria-label="Choose a guide" hidden>{items}<a class="g-pick-item g-pick-add" href="guides?add=1">+ Add guide…</a></div></div>"#,
                ic = icon("list"),
                name = esc(&s.name),
            )
        }
        _ => String::new(),
    };
    let search = format!(
        r#"<div class="g-tools-top">{picker}<span class="s-mode" role="group" aria-label="Search in"><button type="button" class="on" data-mode="chans" aria-pressed="true">Channels</button><button type="button" data-mode="progs" aria-pressed="false">Programmes</button></span><div class="search search-field">{ic}<input type="search" id="g-search" class="input field" placeholder="Search channels" aria-label="Search channels" autocomplete="off"><button type="button" class="search-clear" id="g-search-clear" aria-label="Clear search" title="Clear search" hidden>{x}</button><div class="s-results" id="g-sresults" role="listbox" aria-label="Programmes found" hidden></div></div></div>"#,
        ic = icon("search"),
        x = icon("x"),
    );
    let globals = format!(
        "<script>window.EPG_GUIDE = {};window.EPG_GUIDES = {};</script>",
        json_script(&json!(sel.map(|g| g.slug.clone()).unwrap_or_default())),
        json_script(&json!(guides.iter().map(|g| g.slug.clone()).collect::<Vec<_>>())),
    );
    let v = env!("CARGO_PKG_VERSION");
    let (tools, body) = if !empty.is_empty() {
        (
            format!(r#"<div class="g-tools-top">{picker}</div>"#),
            format!(
                r#"{globals}<div class="card g-empty"><h2>{g}Nothing to show yet</h2><p class="muted">{m}</p><a class="button" href="guides">{l}Open Guides</a></div><script src="assets/tvguide.js?v={v}"></script>"#,
                g = icon("grid"),
                m = esc(empty),
                l = icon("list"),
            ),
        )
    } else {
        (
            String::new(),
            format!(
                r#"{globals}<div class="g-bar"><div class="g-days"><button type="button" class="secondary" id="g-prev" aria-label="Previous day">&#8249;</button><span id="g-day" class="g-day-label">Today</span><button type="button" class="secondary" id="g-next" aria-label="Next day">&#8250;</button><button type="button" id="g-now">{clock}Now</button></div><div class="g-chips-wrap" id="g-chips-wrap"><button type="button" class="g-scroll-btn" data-dir="-1" aria-label="Scroll groups left" hidden>&#8249;</button><div class="g-chips" id="g-chips" role="group" aria-label="Filter by group"></div><button type="button" class="g-scroll-btn" data-dir="1" aria-label="Scroll groups right" hidden>&#8250;</button></div>{search}</div><div class="g-wrap" id="g-wrap"><div class="g-scroll" id="g-scroll" tabindex="0" aria-label="Programme guide"><div class="g-inner" id="g-inner"><div class="g-head" id="g-head"><div class="g-corner" id="g-count"></div></div><div class="g-body" id="g-body"></div><div class="g-nowline" id="g-nowline" hidden></div></div></div><div class="g-status" id="g-status">Loading the guide…</div></div><aside class="g-panel" id="g-panel" hidden aria-live="polite"><button type="button" class="ghost g-close" id="g-close" aria-label="Close">&times;</button><div id="g-detail"></div></aside>{dialog}<script src="assets/tvguide.js?v={v}"></script><script src="assets/guide.js?v={v}"></script>"#,
                clock = icon("clock"),
                dialog = logo_dialog(ctx),
            ),
        )
    };
    html_page(ctx, page(ctx, "guide", "TV Guide", &tools, &body))
}

fn logo_dialog(ctx: &Ctx) -> String {
    format!(
        r#"<dialog class="g-dialog logo-dialog" id="logo-dialog" aria-labelledby="logo-title"><form class="guide-form" id="logo-form" method="post" enctype="multipart/form-data" data-csrf="{csrf}"><div class="hero-head"><h2 id="logo-title">Channel logo</h2><button type="button" class="ghost" data-close aria-label="Close">&times;</button></div><div class="logo-now"><div class="logo-preview" id="logo-preview"><span class="g-logo-none" id="logo-initials"></span></div><div><strong id="logo-channel"></strong><p class="hint" id="logo-source">Loading…</p></div></div><div class="logo-tabs" role="tablist"><button type="button" role="tab" class="on" data-tab="upload" aria-selected="true">Upload</button><button type="button" role="tab" data-tab="url" aria-selected="false">From URL</button></div><div class="logo-pane" data-pane="upload"><label class="logo-drop" id="logo-drop"><input type="file" id="logo-file" accept="image/png,image/jpeg,image/webp,image/gif"><span id="logo-drop-text">Drop an image here, or click to choose one</span></label></div><div class="logo-pane" data-pane="url" hidden><div class="url-box"><input type="text" id="logo-url" placeholder="https://example.com/logo.png"><button type="button" class="secondary" id="logo-url-preview">Preview</button></div></div><p class="hint" id="logo-rules">PNG, JPEG, WebP or GIF, 32 to 4096 pixels per side.</p><p class="hint" id="logo-msg" role="status"></p><div class="form-actions"><button type="submit" id="logo-save">Save</button><button type="button" class="ghost" id="logo-reset">Reset to automatic</button><button type="button" class="ghost" data-close>Cancel</button><button type="button" class="secondary" id="logo-build" hidden>Build now</button></div></form></dialog>"#,
        csrf = esc(&ctx.csrf)
    )
}

pub fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

// ---------------------------------------------------------------- data feed

/// api/guide: the channel list (default), ?progs=1&ch=..&from=..&to=.., ?id=.., ?search=..
pub async fn guide_api(State(app): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let ctx = app.ctx(&headers);
    if ctx.user.is_none() {
        return json(&ctx, StatusCode::UNAUTHORIZED, json!({ "error": "Log in first." }));
    }
    let conn = match app.db() {
        Ok(c) => c,
        Err(e) => return json(&ctx, StatusCode::OK, json!({ "empty": true, "reason": format!("{e:#}") })),
    };
    let guides = db::guides(&conn).unwrap_or_default();
    let gid = pick_guide(&guides, q.get("guide").map(String::as_str).unwrap_or("")).map_or(0, |g| g.id);
    match feed(&conn, gid, &q) {
        Ok((status, v)) => json(&ctx, status, v),
        Err(e) => json(&ctx, StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": format!("The guide could not be read: {e:#}") })),
    }
}

const MAX_CHANNELS: usize = 200;
const MAX_SPAN: i64 = 2 * 86_400;

fn feed(conn: &Connection, gid: i64, q: &HashMap<String, String>) -> anyhow::Result<(StatusCode, Value)> {
    if let Some(id) = q.get("id") {
        let id: i64 = id.parse().unwrap_or(0);
        let row = conn
            .query_row(
                "SELECT p.id, p.channel_pos, p.start, p.stop, p.title, p.sub_title, p.episode, p.descr, p.icon, p.rating,
                        p.categories, p.group_name, c.name, c.logo, p.star
                 FROM guide_programmes p JOIN guide_channels c ON c.pos = p.channel_pos AND c.guide_id = p.guide_id
                 WHERE p.id = ?1",
                params![id],
                |r| {
                    let cats: String = r.get(10)?;
                    Ok(json!({
                        "id": r.get::<_, i64>(0)?, "ch": r.get::<_, i64>(1)?, "start": r.get::<_, i64>(2)?, "stop": r.get::<_, i64>(3)?,
                        "title": r.get::<_, String>(4)?, "sub_title": r.get::<_, Option<String>>(5)?, "episode": r.get::<_, Option<String>>(6)?,
                        "desc": r.get::<_, Option<String>>(7)?, "icon": r.get::<_, Option<String>>(8)?, "rating": r.get::<_, Option<String>>(9)?,
                        "categories": serde_json::from_str::<Value>(&cats).unwrap_or(json!([])),
                        "group": r.get::<_, Option<String>>(11)?, "channel": r.get::<_, String>(12)?, "logo": r.get::<_, Option<String>>(13)?,
                        "star": r.get::<_, Option<String>>(14)?,
                    }))
                },
            )
            .optional()?;
        return Ok(match row {
            Some(v) => (StatusCode::OK, v),
            None => (StatusCode::NOT_FOUND, json!({ "error": "not found" })),
        });
    }

    if q.contains_key("progs") {
        let mut ch: Vec<i64> = q
            .get("ch")
            .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).filter(|x: &i64| *x >= 0).collect())
            .unwrap_or_default();
        ch.sort_unstable();
        ch.dedup();
        ch.truncate(MAX_CHANNELS);
        let from: i64 = q.get("from").and_then(|s| s.parse().ok()).unwrap_or(0);
        let to: i64 = q.get("to").and_then(|s| s.parse().ok()).unwrap_or(0);
        if ch.is_empty() || to <= from || to - from > MAX_SPAN {
            return Ok((StatusCode::BAD_REQUEST, json!({ "error": "bad request" })));
        }
        let list = ch.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, channel_pos, start, stop, title, episode, group_name FROM guide_programmes
             WHERE guide_id = ?1 AND channel_pos IN ({list}) AND start < ?2 AND stop > ?3 ORDER BY channel_pos, start"
        );
        let mut st = conn.prepare(&sql)?;
        let rows = st
            .query_map(params![gid, to, from], |r| {
                Ok(json!([
                    r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?, r.get::<_, Option<String>>(5)?, r.get::<_, Option<String>>(6)?
                ]))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        return Ok((StatusCode::OK, json!({ "ch": ch, "from": from, "to": to, "progs": rows })));
    }

    if let Some(term) = q.get("search") {
        let term = term.trim();
        if term.len() < 2 {
            return Ok((StatusCode::OK, json!({ "results": [] })));
        }
        let cut: String = term.chars().take(120).collect();
        let like = format!("%{}%", cut.replace('!', "!!").replace('%', "!%").replace('_', "!_"));
        let mut st = conn.prepare(
            "SELECT p.id, p.channel_pos, p.start, p.stop, p.title, p.episode, p.icon, p.group_name, p.star, c.name, c.logo
             FROM guide_programmes p JOIN guide_channels c ON c.pos = p.channel_pos AND c.guide_id = p.guide_id
             WHERE p.guide_id = ?1 AND p.stop > ?2 AND (p.title LIKE ?3 ESCAPE '!' OR p.sub_title LIKE ?3 ESCAPE '!')
             ORDER BY p.start, c.pos LIMIT 50",
        )?;
        let rows = st
            .query_map(params![gid, timeutil::now_unix(), like], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "ch": r.get::<_, i64>(1)?, "start": r.get::<_, i64>(2)?, "stop": r.get::<_, i64>(3)?,
                    "title": r.get::<_, String>(4)?, "episode": r.get::<_, Option<String>>(5)?, "icon": r.get::<_, Option<String>>(6)?,
                    "group": r.get::<_, Option<String>>(7)?, "star": r.get::<_, Option<String>>(8)?,
                    "channel": r.get::<_, String>(9)?, "logo": r.get::<_, Option<String>>(10)?,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        return Ok((StatusCode::OK, json!({ "now": timeutil::now_unix(), "results": rows })));
    }

    // The channel list.
    let mut st = conn.prepare("SELECT pos, channel_id, name, logo, group_name FROM guide_channels WHERE guide_id = ?1 ORDER BY pos")?;
    let channels = st
        .query_map(params![gid], |r| {
            Ok(json!([r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, Option<String>>(4)?]))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if channels.is_empty() {
        return Ok((StatusCode::OK, json!({ "empty": true, "reason": "No guide has been stored yet. Run a guide build." })));
    }
    let mut st = conn.prepare(
        "SELECT group_name, COUNT(*) FROM guide_channels WHERE guide_id = ?1 AND group_name IS NOT NULL AND group_name <> ''
         GROUP BY group_name ORDER BY COUNT(*) DESC",
    )?;
    let groups = st
        .query_map(params![gid], |r| Ok(json!([r.get::<_, String>(0)?, r.get::<_, i64>(1)?])))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (min, max): (Option<i64>, Option<i64>) =
        conn.query_row("SELECT MIN(start), MAX(stop) FROM guide_programmes WHERE guide_id = ?1", params![gid], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok((
        StatusCode::OK,
        json!({ "now": timeutil::now_unix(), "min": min.unwrap_or(0), "max": max.unwrap_or(0), "groups": groups, "channels": channels }),
    ))
}
