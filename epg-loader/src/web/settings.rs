//! The Settings page (settings): artwork and info sources, channel logos,
//! schedule, users, backups and the system checks.

use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use super::common::{settings, when_long};
use super::guides::Posted;
use super::html::{csrf_field, esc, icon, page};
use super::{auth, html_page, jobs, read_post, redirect, with_cookie, App, Ctx, Shared};
use crate::timeutil;

pub const BACKUP_KEEP: usize = 30;
const BACKUP_TABLES: [&str; 4] = ["settings", "guides", "channel_overrides", "tmdb_manual"];

fn key_hint(key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("••••{tail}")
}

fn checked(on: bool) -> &'static str {
    if on { " checked" } else { "" }
}

// ---------------------------------------------------------------- backups
//
// A backup is one gzipped JSON file in <data dir>/backups/: the settings, the
// guides, channel overrides, TMDB matches made by hand, users and the custom
// channel logos. The newest
// BACKUP_KEEP are kept. Restoring makes a backup of the current state first.

pub struct Backup {
    pub name: String,
    pub time: i64,
    pub size: u64,
    pub reason: String,
}

fn backups_dir(app: &App) -> PathBuf {
    app.data_dir.join("backups")
}

fn custom_logos_dir(app: &App) -> PathBuf {
    app.output_dir.join("logos").join("custom")
}

/// "epgarr-20261127-151000.json.gz" or "epgarr-20261127-151000-daily.json.gz": the reason, or None.
fn backup_reason(name: &str) -> Option<String> {
    let rest = name.strip_prefix("epgarr-")?.strip_suffix(".json.gz")?;
    let b = rest.as_bytes();
    if b.len() < 15 || !b[..8].iter().all(u8::is_ascii_digit) || b[8] != b'-' || !b[9..15].iter().all(u8::is_ascii_digit) {
        return None;
    }
    match &rest[15..] {
        "" => Some("manual".into()),
        r => {
            let r = r.strip_prefix('-')?;
            (!r.is_empty() && r.bytes().all(|c| c.is_ascii_lowercase() || c == b'-')).then(|| r.to_string())
        }
    }
}

/// Backups, newest first.
pub fn backups(app: &App) -> Vec<Backup> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(backups_dir(app)) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(reason) = backup_reason(&name) else { continue };
            let Ok(m) = e.metadata() else { continue };
            let time = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
            out.push(Backup { name, time, size: m.len(), reason });
        }
    }
    out.sort_by(|a, b| b.time.cmp(&a.time).then_with(|| b.name.cmp(&a.name)));
    out
}

/// A backup file from the list (never a path from the browser).
fn backup_path(app: &App, name: &str) -> Option<PathBuf> {
    backup_reason(name)?;
    let p = backups_dir(app).join(name);
    p.is_file().then_some(p)
}

fn sql_to_json(v: rusqlite::types::ValueRef) -> Value {
    use rusqlite::types::ValueRef::*;
    match v {
        Null => Value::Null,
        Integer(i) => json!(i),
        Real(f) => json!(f),
        Text(t) => json!(String::from_utf8_lossy(t)),
        Blob(b) => json!(String::from_utf8_lossy(b)),
    }
}

fn json_to_sql(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match v {
        Value::Null => V::Null,
        Value::Bool(b) => V::Integer(*b as i64),
        Value::Number(n) => n.as_i64().map(V::Integer).unwrap_or_else(|| V::Real(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => V::Text(s.clone()),
        other => V::Text(other.to_string()),
    }
}

fn has_table(conn: &rusqlite::Connection, t: &str) -> bool {
    conn.query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1", [t], |_| Ok(())).is_ok()
}

fn is_logo_name(name: &str) -> bool {
    match name.split_once('.') {
        Some((base, ext)) => {
            base.len() == 20
                && base.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                && ["png", "jpg", "gif", "webp"].contains(&ext)
        }
        None => false,
    }
}

/// Make a backup now. Returns the file name or an error.
pub fn backup_create(app: &App, reason: &str) -> Result<String, String> {
    let dir = backups_dir(app);
    std::fs::create_dir_all(&dir).map_err(|_| "The backups folder could not be created in the data folder.".to_string())?;
    let conn = app.db().map_err(|e| format!("The backup failed: {e}"))?;
    let mut tables = serde_json::Map::new();
    for t in BACKUP_TABLES {
        if !has_table(&conn, t) {
            continue;
        }
        let mut st = conn.prepare(&format!("SELECT * FROM {t}")).map_err(|e| format!("The backup failed: {e}"))?;
        let cols: Vec<String> = st.column_names().iter().map(|c| c.to_string()).collect();
        let rows: Vec<Value> = st
            .query_map([], |r| {
                let mut m = serde_json::Map::new();
                for (i, c) in cols.iter().enumerate() {
                    m.insert(c.clone(), sql_to_json(r.get_ref(i)?));
                }
                Ok(Value::Object(m))
            })
            .map_err(|e| format!("The backup failed: {e}"))?
            .filter_map(|r| r.ok())
            .collect();
        tables.insert(t.to_string(), Value::Array(rows));
    }
    let mut logos = serde_json::Map::new();
    if let Ok(rd) = std::fs::read_dir(custom_logos_dir(app)) {
        use base64::Engine;
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if is_logo_name(&n) {
                if let Ok(b) = std::fs::read(e.path()) {
                    logos.insert(n, json!(base64::engine::general_purpose::STANDARD.encode(b)));
                }
            }
        }
    }
    let data = json!({
        "app": super::html::APP_NAME, "format": 1, "created": timeutil::now_unix(), "reason": reason,
        "tables": tables, "users": auth::users(&app.data_dir), "logos": logos,
    });
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    gz.write_all(data.to_string().as_bytes()).map_err(|_| "The backup could not be written.".to_string())?;
    let bytes = gz.finish().map_err(|_| "The backup could not be written.".to_string())?;
    let safe: String = reason.chars().filter(|c| c.is_ascii_lowercase() || *c == '-').collect();
    // Names are to the second: never overwrite one made in the same second.
    let mut name = String::new();
    for _ in 0..3 {
        let stamp = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::system()).strftime("%Y%m%d-%H%M%S").to_string();
        name = if reason == "manual" || safe.is_empty() { format!("epgarr-{stamp}.json.gz") } else { format!("epgarr-{stamp}-{safe}.json.gz") };
        if !dir.join(&name).exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
    crate::output::write_atomic(&dir.join(&name), &bytes).map_err(|_| "The backup could not be written.".to_string())?;
    for old in backups(app).into_iter().skip(BACKUP_KEEP) {
        let _ = std::fs::remove_file(dir.join(old.name));
    }
    Ok(name)
}

/// A backup a day: made when the newest is more than a day old.
pub fn backup_daily(app: &App) {
    if !app.data_dir.is_dir() || !app.data_dir.join("epg.db").exists() {
        return;
    }
    let list = backups(app);
    if list.first().map_or(true, |b| timeutil::now_unix() - b.time > 86_400) {
        if let Err(e) = backup_create(app, "daily") {
            eprintln!("warning: daily backup: {e}");
        }
    }
}

fn read_backup(bytes: &[u8]) -> Option<Value> {
    let mut s = String::new();
    flate2::read::GzDecoder::new(bytes).read_to_string(&mut s).ok()?;
    serde_json::from_str(&s).ok()
}

/// What a backup holds: "12 settings · 3 guides · 2 users · 4 logos".
fn backup_summary(app: &App, name: &str) -> String {
    let Some(d) = backup_path(app, name).and_then(|p| std::fs::read(p).ok()).and_then(|b| read_backup(&b)) else {
        return "unreadable".into();
    };
    let n = |t: &str| d["tables"][t].as_array().map_or(0, |a| a.len());
    let users = d["users"].as_array().map_or(0, |a| a.len());
    let logos = d["logos"].as_object().map_or(0, |a| a.len());
    format!("{} settings · {} guides · {users} users · {logos} logos", n("settings"), n("guides"))
}

/// Restore a backup (from the list or an uploaded file). Returns "" or an error.
fn backup_restore(app: &App, gz: &[u8]) -> String {
    let Some(d) = read_backup(gz).filter(|d| d["format"].as_i64() == Some(1) && d["tables"].is_object()) else {
        return "That is not an EPGArr backup.".into();
    };
    if let Err(e) = backup_create(app, "before-restore") {
        return format!("Not restored: the current state could not be backed up first ({e})");
    }
    let r = (|| -> anyhow::Result<()> {
        let mut conn = app.db()?;
        let tx = conn.transaction()?;
        for t in BACKUP_TABLES {
            let Some(rows) = d["tables"][t].as_array() else { continue };
            if !has_table(&tx, t) {
                continue;
            }
            let cols: Vec<String> = {
                let mut st = tx.prepare(&format!("PRAGMA table_info({t})"))?;
                let v = st.query_map([], |r| r.get::<_, String>(1))?.filter_map(|r| r.ok()).collect();
                v
            };
            tx.execute(&format!("DELETE FROM {t}"), [])?;
            for row in rows {
                let Some(obj) = row.as_object() else { continue };
                let pairs: Vec<(&String, &Value)> = obj.iter().filter(|(k, _)| cols.contains(k)).collect();
                if pairs.is_empty() {
                    continue;
                }
                let names: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
                let marks = vec!["?"; names.len()].join(", ");
                let vals: Vec<rusqlite::types::Value> = pairs.iter().map(|(_, v)| json_to_sql(v)).collect();
                tx.execute(&format!("INSERT INTO {t} ({}) VALUES ({marks})", names.join(", ")), rusqlite::params_from_iter(vals))?;
            }
        }
        tx.commit()?;
        Ok(())
    })();
    if let Err(e) = r {
        return format!("Restoring failed, nothing was changed: {e}");
    }
    if let Ok(users) = serde_json::from_value::<Vec<auth::User>>(d["users"].clone()) {
        if !users.is_empty() {
            let _ = auth::save_users(&app.data_dir, &users);
        }
    }
    if let Some(logos) = d["logos"].as_object() {
        use base64::Engine;
        let dir = custom_logos_dir(app);
        if std::fs::create_dir_all(&dir).is_ok() {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    if is_logo_name(&e.file_name().to_string_lossy()) {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
            for (name, b64) in logos {
                if is_logo_name(name) {
                    if let Ok(b) = base64::engine::general_purpose::STANDARD.decode(b64.as_str().unwrap_or("")) {
                        let _ = std::fs::write(dir.join(name), b);
                    }
                }
            }
        }
    }
    String::new()
}

// ---------------------------------------------------------------- system checks

fn writable(dir: &std::path::Path) -> bool {
    let t = dir.join(format!(".write-test-{}", std::process::id()));
    let ok = std::fs::write(&t, b"x").is_ok();
    let _ = std::fs::remove_file(&t);
    ok
}

/// What Settings › Advanced shows: (title, ok|warn|fail, detail).
fn system_checks(app: &App) -> Vec<(String, &'static str, String)> {
    let mut c = Vec::new();
    let schema = app.db().ok().and_then(|conn| crate::db::schema_version(&conn).ok()).unwrap_or(0);
    c.push(("Program".into(), "ok", format!("epg-loader {} · database schema {schema}", env!("CARGO_PKG_VERSION"))));
    let sched = super::SCHEDULER_RUNNING.load(std::sync::atomic::Ordering::Relaxed);
    c.push((
        "Scheduler".into(),
        if sched { "ok" } else { "fail" },
        if sched { "running: guides are rebuilt on their schedule".into() } else { "not running: restart the container".into() },
    ));
    let d = app.data_dir.display().to_string();
    let w = writable(&app.data_dir);
    c.push(("Data volume".into(), if w { "ok" } else { "fail" }, if w { d } else { format!("{d} is not writable") }));
    if let Some(j) = std::fs::read(app.data_dir.join("check.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
        for (k, label) in [("internet", "Internet"), ("tmdb", "TMDB key"), ("source", "First guide source")] {
            if let Some(x) = j.get(k).filter(|x| x.is_object()) {
                let ok = x["ok"].as_bool().unwrap_or(false);
                c.push((label.into(), if ok { "ok" } else { "fail" }, x["detail"].as_str().unwrap_or("").to_string()));
            }
        }
    }
    c
}

// ---------------------------------------------------------------- page

pub async fn page_get(State(app): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let ctx = app.ctx(&headers);
    if ctx.user.is_none() {
        return auth::login_page(&app, &ctx, "settings");
    }
    if !ctx.admin {
        return super::viewer_home(&ctx);
    }
    if let Some(name) = q.get("backup") {
        return download(&ctx, &app, name);
    }
    backup_daily(&app);
    let old = app.take_old(&ctx);
    let s = settings(&app);
    let v = |k: &str, d: &str| -> String { old.get(k).or_else(|| s.get(k)).cloned().unwrap_or_else(|| d.to_string()) };
    let on = |k: &str, d: &str, off: &str| v(k, d) != off;
    let csrf = csrf_field(&ctx);
    let saved_key = s.get("tmdb_api_key").cloned().unwrap_or_default();
    let me = ctx.user.clone().unwrap_or_default();

    let key_part = if saved_key.is_empty() {
        format!(r#"<p class="hint">Free at themoviedb.org: <a href="{}" target="_blank" rel="noopener">get your TMDB key ↗</a>. Without it, no posters or ratings are added.</p>"#, super::dash::TMDB_KEY_URL)
    } else {
        r#"<label class="check"><input type="checkbox" name="clear_key" value="1"> Remove the saved key (turns TMDB lookups off)</label>"#.to_string()
    };
    let key_ph = if saved_key.is_empty() {
        "Paste a v3 API key or v4 read access token".to_string()
    } else {
        format!("Saved key {} · leave blank to keep it", key_hint(&saved_key))
    };

    let user_rows: String = auth::users(&app.data_dir)
        .iter()
        .map(|u| {
            let is_me = u.username.eq_ignore_ascii_case(&me);
            let name = esc(&u.username);
            let actions = if is_me {
                String::new()
            } else {
                let confirm = esc(&serde_json::to_string(&format!("Delete the user {}?", u.username)).unwrap_or_default());
                format!(
                    r#"<button class="ghost" form="user-role" name="username" value="{name}" title="{rt}">{rl}</button><button class="ghost" form="user-toggle" name="username" value="{name}">{t}</button><button class="ghost danger-text" form="user-delete" name="username" value="{name}" onclick="return confirm({confirm})">Delete</button>"#,
                    t = if u.disabled { "Enable" } else { "Disable" },
                    rl = if u.role == "admin" { "Make viewer" } else { "Make admin" },
                    rt = if u.role == "admin" { "Viewers only see the TV Guide and cannot change anything" } else { "Admins can change everything" },
                )
            };
            format!(
                r#"<tr><td><strong>{name}</strong>{you}</td><td>{role}</td><td>{last}</td><td>{status}</td><td class="user-actions"><button type="button" class="ghost" data-pw="{name}">Change password</button>{actions}</td></tr>"#,
                you = if is_me { r#" <span class="muted">(you)</span>"# } else { "" },
                role = if u.role == "admin" { "Admin" } else { "Viewer" },
                last = if u.last_login > 0 { esc(&when_long(u.last_login)) } else { r#"<span class="muted">never</span>"#.into() },
                status = if u.disabled { r#"<span class="warn-text">disabled</span>"# } else { r#"<span class="ok-text">active</span>"# },
            )
        })
        .collect();

    let bks = backups(&app);
    let bk_table = if bks.is_empty() {
        r#"<p class="muted">No backups yet.</p>"#.to_string()
    } else {
        let rows: String = bks
            .iter()
            .map(|b| {
                let n = esc(&b.name);
                format!(
                    r#"<tr><td>{when}</td><td>{made}</td><td class="muted">{sum}</td><td class="num">{kb} KB</td><td class="user-actions"><a class="button ghost" href="settings?backup={url}">Download</a><button class="ghost" form="bk-restore" name="name" value="{n}">Restore</button><button class="ghost danger-text" form="bk-delete" name="name" value="{n}">Delete</button></td></tr>"#,
                    when = esc(&when_long(b.time)),
                    made = esc(if b.reason == "before-restore" { "before a restore" } else { &b.reason }),
                    sum = esc(&backup_summary(&app, &b.name)),
                    kb = super::common::num(((b.size as f64) / 1024.0).round() as i64),
                    url = super::tv::url_encode(&b.name),
                )
            })
            .collect();
        format!(r#"<table class="compact bk-table"><thead><tr><th>When</th><th>Made</th><th>Contains</th><th class="num">Size</th><th></th></tr></thead><tbody>{rows}</tbody></table>"#)
    };

    let checks = system_checks(&app);
    let bad = checks.iter().filter(|c| c.1 == "fail").count();
    let check_items: String = checks
        .iter()
        .map(|(t, st, d)| format!(r#"<li><span class="dot {st}"></span><strong>{}</strong> <span class="muted">{}</span></li>"#, esc(t), esc(d)))
        .collect();
    let schema = app.db().ok().and_then(|c| crate::db::schema_version(&c).ok()).unwrap_or(0);
    let program = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "missing".into());

    let body = format!(
        r##"<form method="post" id="user-add" autocomplete="off">{csrf}<input type="hidden" name="action" value="add_user"></form>
<form method="post" id="user-setpw" autocomplete="off">{csrf}<input type="hidden" name="action" value="set_password"></form>
<form method="post" id="user-role">{csrf}<input type="hidden" name="action" value="set_role"></form>
<form method="post" id="user-toggle">{csrf}<input type="hidden" name="action" value="toggle_user"></form>
<form method="post" id="user-delete">{csrf}<input type="hidden" name="action" value="delete_user"></form>
<form method="post" id="bk-create">{csrf}<input type="hidden" name="action" value="backup_create"></form>
<form method="post" id="bk-restore" onsubmit="return confirm('Restore this backup? Your current settings, guides, users and custom logos are backed up first, then replaced.')">{csrf}<input type="hidden" name="action" value="backup_restore"></form>
<form method="post" id="bk-delete" onsubmit="return confirm('Delete this backup?')">{csrf}<input type="hidden" name="action" value="backup_delete"></form>
<form method="post" id="bk-upload" enctype="multipart/form-data" onsubmit="return confirm('Restore from this file? Your current state is backed up first.')">{csrf}<input type="hidden" name="action" value="backup_upload"></form>
<form method="post" autocomplete="off" id="settings-form" class="set-layout">{csrf}<input type="hidden" name="action" value="save_settings">
{tabs_nav}
<div class="set-panels">
<section class="card set-panel" id="artwork"><h2>Artwork &amp; info</h2>
<label for="tmdb_api_key">TMDB API key or read access token</label>
<input type="password" id="tmdb_api_key" name="tmdb_api_key" value="" autocomplete="new-password" placeholder="{key_ph}">
{key_part}
<div class="row"><div><label for="language">Language</label><input type="text" id="language" name="language" value="{language}" pattern="[a-z]{{2}}(-[A-Z]{{2}})?" required><p class="hint">For titles and descriptions, e.g. en-US.</p></div>
<div><label for="rating_country">Rating country</label><input type="text" id="rating_country" name="rating_country" value="{rating_country}" maxlength="2" required><p class="hint">Whose age ratings to use, e.g. US.</p></div></div>
<label class="check"><input type="checkbox" name="tmdb_background" value="1"{c_bg}> Keep looking up titles in the background until all are done</label>
<p class="hint">Watch progress, Start or Pause on the <a href="activity">Activity</a> page.</p>
<label class="check"><input type="checkbox" name="imdb_ratings" value="1"{c_imdb}> Add IMDb star ratings (e.g. 7.8/10) to matched programmes</label>
<p class="hint">IMDb data is for personal, non-commercial use.</p>
<label class="check"><input type="checkbox" name="tvmaze" value="1"{c_tvmaze}> Use TVmaze for series TMDB has no match or poster for</label>
<p class="hint">Free, no key needed.</p>
<label class="check"><input type="checkbox" name="sportsdb" value="1"{c_sports}> Use TheSportsDB for sports matches without a poster</label>
<label for="sportsdb_key">TheSportsDB API key</label>
<input type="text" id="sportsdb_key" name="sportsdb_key" value="{sportsdb_key}" placeholder="123 (free test key)" class="short" autocomplete="off">
<p class="hint">Leave empty to use the free key; enter your own for higher limits.</p>
</section>
<section class="card set-panel" id="logos"><h2>Channel logos</h2>
<p class="hint" style="margin-top:0">Logos already in your source are always kept.</p>
<label class="check"><input type="checkbox" name="logo_source" value="1"{c_logo}> Fill in missing logos from tv-logos, then iptv-org</label>
<label for="logo_country">Default logo country</label>
<input type="text" id="logo_country" name="logo_country" value="{logo_country}" maxlength="2" pattern="[A-Za-z]{{2}}" required class="short">
<p class="hint">Used when a channel id has no country, e.g. us or uk.</p>
</section>
<section class="card set-panel" id="schedule"><h2>Schedule</h2>
<label for="refresh_hours">Rebuild every (hours)</label>
<input type="number" id="refresh_hours" name="refresh_hours" min="0.5" max="168" step="0.5" value="{refresh_hours}" required class="short">
<p class="hint">Default for every guide; a guide can set its own on the <a href="guides">Guides</a> page.</p>
</section>
<section class="card set-panel" id="users"><h2>Users</h2>
<table class="compact users-table"><thead><tr><th>Username</th><th>Role</th><th>Last login</th><th>Status</th><th></th></tr></thead><tbody>{user_rows}</tbody></table>
<div class="user-pw" id="user-pw" hidden><h3 class="adv-h">Change password for <span id="user-pw-name"></span></h3>
<input form="user-setpw" type="hidden" name="username" id="user-pw-user">
<div class="row"><div><label for="upw1">New password</label><input form="user-setpw" type="password" id="upw1" name="new_password" autocomplete="new-password" minlength="8"><p class="hint">At least 8 characters.</p></div>
<div><label for="upw2">Repeat it</label><input form="user-setpw" type="password" id="upw2" name="confirm_password" autocomplete="new-password"></div></div>
<div class="form-actions"><button form="user-setpw">Save password</button><button type="button" class="ghost" id="user-pw-cancel">Cancel</button></div></div>
<h3 class="adv-h">Add a user</h3>
<div class="row"><div><label for="nu-name">Username</label><input form="user-add" type="text" id="nu-name" name="username" maxlength="32" autocomplete="off"><p class="hint">Letters, digits, dots, dashes or underscores.</p></div>
<div><label for="nu-pw1">Password</label><input form="user-add" type="password" id="nu-pw1" name="new_password" autocomplete="new-password" minlength="8"><p class="hint">At least 8 characters.</p></div>
<div><label for="nu-pw2">Repeat it</label><input form="user-add" type="password" id="nu-pw2" name="confirm_password" autocomplete="new-password"></div><div><label for="nu-role">Role</label><select form="user-add" id="nu-role" name="role"><option value="viewer">Viewer: TV Guide only</option><option value="admin">Admin: can change everything</option></select></div></div>
<button form="user-add">Add user</button>
</section>
<section class="card set-panel" id="backups"><h2>Backups</h2>
<p class="hint" style="margin-top:0">Settings, guides, channel overrides, your TMDB matches, users and custom logos. One is made every day automatically and before every restore; the newest {keep} are kept in the data folder.</p>
<div class="form-actions" style="margin-top:0"><button form="bk-create">Back up now</button>
<label class="button secondary bk-upload">Restore from a file…<input form="bk-upload" type="file" name="file" accept=".gz,application/gzip" onchange="if (this.files.length) this.form.requestSubmit()"></label></div>
{bk_table}
</section>
<section class="card set-panel" id="advanced"><h2>Advanced</h2>
<h3 class="adv-h">Program</h3>
<dl><dt>Version</dt><dd>EPGArr {version} (Docker image)</dd><dt>Database schema</dt><dd>{schema}</dd></dl>
<p class="hint">To update: docker compose pull, then docker compose up -d. Your data in /data stays.</p>
<label class="check"><input type="checkbox" name="update_check" value="1"{c_update}> Check GitHub for new versions</label>
<p class="hint">Every 6 hours. When a newer version is out, the version at the bottom of the menu lights up. Only the version number is fetched; nothing is sent.</p>
<details class="adv-checks" id="adv-checks"{open}><summary>System checks · <span class="{bad_cls}">{bad_text}</span></summary><ul class="adv-check-list">{check_items}</ul></details>
<h3 class="adv-h">Folders in the container</h3>
<dl><dt>Data volume</dt><dd><code>{data_dir}</code></dd><dt>Guide files</dt><dd><code>{guide_dir}</code></dd><dt>Program</dt><dd><code>{program}</code></dd></dl>
</section>
</div>
<div class="save-bar" id="save-bar" hidden><span>You have unsaved changes.</span><button type="button" class="ghost" id="discard">Discard</button><button type="submit">Save settings</button></div>
</form>
<script src="assets/page-settings.js?v={version}"></script>"##,
        tabs_nav = tabs_nav(false),
        key_ph = esc(&key_ph),
        language = esc(&v("language", "en-US")),
        rating_country = esc(&v("rating_country", "US")),
        c_bg = checked(on("tmdb_background", "1", "0")),
        c_imdb = checked(on("imdb_ratings", "1", "0")),
        c_tvmaze = checked(on("tvmaze", "1", "0")),
        c_sports = checked(on("sportsdb", "1", "0")),
        sportsdb_key = esc(&v("sportsdb_key", "")),
        c_logo = checked(on("logo_source", "iptv-org", "off")),
        c_update = checked(on("update_check", "1", "0")),
        logo_country = esc(&v("logo_country", "us")),
        refresh_hours = esc(&v("refresh_hours", "6")),
        keep = BACKUP_KEEP,
        version = env!("CARGO_PKG_VERSION"),
        open = if bad > 0 { " open" } else { "" },
        bad_cls = if bad > 0 { "bad-text" } else { "ok-text" },
        bad_text = if bad > 0 { format!("{bad} problem{}", if bad > 1 { "s" } else { "" }) } else { "All good".into() },
        data_dir = esc(&app.data_dir.display().to_string()),
        guide_dir = esc(&format!("{}/", app.output_dir.join("guides").display())),
        program = esc(&program),
    );
    html_page(&ctx, page(&ctx, "settings", "Settings", "", &body))
}

/// The Settings section list. Guides is its own page (/guides) shown inside
/// Settings; the other sections switch in place on /settings.
pub fn tabs_nav(on_guides: bool) -> String {
    let sections = [
        ("artwork", "grid", "Artwork &amp; info"),
        ("logos", "tv", "Channel logos"),
        ("schedule", "history", "Schedule"),
        ("users", "user", "Users"),
        ("backups", "history", "Backups"),
        ("advanced", "settings", "Advanced"),
    ];
    let guides = format!(
        r#"<a href="guides"{cur}>{}Guides</a>"#,
        icon("list"),
        cur = if on_guides { r#" class="active" aria-current="page""# } else { "" }
    );
    let rest: String = sections
        .iter()
        .map(|(id, ic, label)| {
            if on_guides {
                format!(r#"<a href="settings#{id}">{}{label}</a>"#, icon(ic))
            } else {
                format!(r##"<a href="#{id}" data-tab="{id}">{}{label}</a>"##, icon(ic))
            }
        })
        .collect();
    format!(r#"<nav class="set-tabs" aria-label="Settings sections">{guides}{rest}</nav>"#)
}

fn download(ctx: &Ctx, app: &App, name: &str) -> Response {
    let Some(bytes) = backup_path(app, name).and_then(|p| std::fs::read(p).ok()) else {
        return with_cookie(ctx, (StatusCode::NOT_FOUND, "Backup not found.").into_response());
    };
    let mut r = bytes.into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/gzip"));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    with_cookie(ctx, r)
}

fn valid_key(k: &str) -> bool {
    (16..=2048).contains(&k.len()) && k.bytes().all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}

fn valid_language(l: &str) -> bool {
    let b = l.as_bytes();
    match b.len() {
        2 => b.iter().all(u8::is_ascii_lowercase),
        5 => b[..2].iter().all(u8::is_ascii_lowercase) && b[2] == b'-' && b[3..].iter().all(u8::is_ascii_uppercase),
        _ => false,
    }
}

fn save_settings(app: &Shared, ctx: &Ctx, f: &Posted) -> Response {
    let mut errors: Vec<&str> = Vec::new();
    let mut kv: Vec<(&str, String)> = Vec::new();
    let key = f.get("tmdb_api_key").trim().to_string();
    if !key.is_empty() {
        if !valid_key(&key) {
            errors.push("That does not look like a TMDB API key or read access token.");
        }
        kv.push(("tmdb_api_key", key));
    } else if f.has("clear_key") {
        kv.push(("tmdb_api_key", String::new()));
    }
    let lang = f.get("language").trim().to_string();
    if !valid_language(&lang) {
        errors.push("Language must look like en-US.");
    }
    kv.push(("language", lang));
    let country = f.get("rating_country").trim().to_ascii_uppercase();
    if !(country.len() == 2 && country.bytes().all(|c| c.is_ascii_uppercase())) {
        errors.push("Rating country must be a two-letter code such as US.");
    }
    kv.push(("rating_country", country));
    let hours = f.get("refresh_hours").trim().parse::<f64>().ok();
    if !hours.is_some_and(|h| (0.5..=168.0).contains(&h)) {
        errors.push("Refresh interval must be between 0.5 and 168 hours.");
    }
    kv.push(("refresh_hours", hours.map_or("6".into(), |h| h.to_string())));
    kv.push(("logo_source", if f.has("logo_source") { "iptv-org" } else { "off" }.into()));
    for k in ["tmdb_background", "imdb_ratings", "tvmaze", "sportsdb", "update_check"] {
        kv.push((k, if f.has(k) { "1" } else { "0" }.into()));
    }
    let sk = f.get("sportsdb_key").trim().to_string();
    if !sk.is_empty() && !(sk.len() <= 64 && sk.bytes().all(|c| c.is_ascii_alphanumeric())) {
        errors.push("The TheSportsDB key should be letters and digits only.");
    }
    kv.push(("sportsdb_key", sk));
    let lc = f.get("logo_country").trim().to_ascii_lowercase();
    if !(lc.len() == 2 && lc.bytes().all(|c| c.is_ascii_lowercase())) {
        errors.push("Logo country must be a two-letter code such as us or uk.");
    }
    kv.push(("logo_country", lc));

    if !errors.is_empty() {
        for e in errors {
            app.flash(ctx, "error", e);
        }
        app.set_old(ctx, kv.into_iter().filter(|(k, _)| *k != "tmdb_api_key").map(|(k, v)| (k.to_string(), v)).collect());
        return redirect(ctx, "settings");
    }
    kv.push(("settings_saved_at", timeutil::now_unix().to_string()));
    let r = (|| -> anyhow::Result<()> {
        let mut conn = app.db()?;
        let tx = conn.transaction()?;
        for (k, v) in &kv {
            tx.execute("INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [k, v.as_str()])?;
        }
        tx.commit()?;
        Ok(())
    })();
    match r {
        Ok(()) => {
            // Old test results no longer apply: test the new settings.
            jobs::queue_check(app);
            app.flash(ctx, "ok", "Settings saved. They apply from the next build. The TMDB key and source are being tested; see Advanced › System checks for the result.");
        }
        Err(e) => app.flash(ctx, "error", format!("Error: {e}")),
    }
    redirect(ctx, "settings")
}

/// Just the API keys (from Get started on the Dashboard). Empty fields keep what is saved.
fn save_keys(app: &Shared, ctx: &Ctx, f: &Posted) -> Response {
    let back = if f.get("back") == "index" { "./" } else { "settings#artwork" };
    let key = f.get("tmdb_api_key").trim().to_string();
    let sk = f.get("sportsdb_key").trim().to_string();
    if !key.is_empty() && !valid_key(&key) {
        app.flash(ctx, "error", "That does not look like a TMDB API key or read access token.");
        return redirect(ctx, back);
    }
    if !sk.is_empty() && !(sk.len() <= 64 && sk.bytes().all(|c| c.is_ascii_alphanumeric())) {
        app.flash(ctx, "error", "The TheSportsDB key should be letters and digits only.");
        return redirect(ctx, back);
    }
    let mut kv: Vec<(&str, String)> = Vec::new();
    if !key.is_empty() {
        kv.push(("tmdb_api_key", key));
    }
    if !sk.is_empty() {
        kv.push(("sportsdb_key", sk));
    }
    if kv.is_empty() {
        app.flash(ctx, "error", "Paste a key first.");
        return redirect(ctx, back);
    }
    let r = (|| -> anyhow::Result<()> {
        let conn = app.db()?;
        for (k, v) in &kv {
            conn.execute("INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [k, v.as_str()])?;
        }
        Ok(())
    })();
    match r {
        Ok(()) => {
            jobs::queue_check(app);
            app.flash(ctx, "ok", "Keys saved. The TMDB key is being tested; the result is in Settings › Advanced › System checks.");
        }
        Err(e) => app.flash(ctx, "error", format!("Error: {e}")),
    }
    redirect(ctx, back)
}

fn users_action(app: &Shared, ctx: &Ctx, action: &str, f: &Posted) -> Response {
    let me = ctx.user.clone().unwrap_or_default();
    let mut users = auth::users(&app.data_dir);
    let name = f.get("username").trim().to_string();
    let idx = users.iter().position(|u| u.username.eq_ignore_ascii_case(&name));
    let active_admins = users.iter().filter(|u| !u.disabled && u.role == "admin").count();
    let (pw, repeat) = (f.get("new_password"), f.get("confirm_password"));
    let mut msg = String::new();
    let mut err: String = String::new();
    if action == "add_user" {
        if let Some(e) = auth::check_username(&name) {
            err = e.into();
        } else if let Some(i) = idx {
            err = format!("There is already a user called {}.", users[i].username);
        } else if let Some(e) = auth::check_password(&pw, &repeat) {
            err = e.into();
        } else {
            match auth::hash_password(&pw) {
                Ok(hash) => {
                    users.push(auth::User { username: name.clone(), hash, role: if f.get("role") == "viewer" { "viewer".into() } else { "admin".into() }, disabled: false, created: timeutil::now_unix(), last_login: 0 });
                    msg = format!("{} {name} added.", if f.get("role") == "viewer" { "Viewer" } else { "Admin" });
                }
                Err(e) => err = format!("Could not set the password: {e}"),
            }
        }
    } else if let Some(i) = idx {
        if action == "set_password" {
            if let Some(e) = auth::check_password(&pw, &repeat) {
                err = e.into();
            } else {
                match auth::hash_password(&pw) {
                    Ok(hash) => {
                        users[i].hash = hash;
                        msg = format!("Password changed for {}.", users[i].username);
                    }
                    Err(e) => err = format!("Could not set the password: {e}"),
                }
            }
        } else if users[i].username.eq_ignore_ascii_case(&me) {
            err = format!(
                "You cannot {} yourself.",
                match action { "delete_user" => "delete", "set_role" => "change the role of", _ => "disable" }
            );
        } else if users[i].role == "admin" && !users[i].disabled && active_admins <= 1 {
            err = "This is the last active admin.".into();
        } else if action == "set_role" {
            users[i].role = if users[i].role == "admin" { "viewer".into() } else { "admin".into() };
            msg = if users[i].role == "admin" {
                format!("{} is now an admin and can change everything.", users[i].username)
            } else {
                format!("{} is now a viewer: only the TV Guide, no changes.", users[i].username)
            };
        } else if action == "toggle_user" {
            users[i].disabled = !users[i].disabled;
            msg = format!(
                "{}{}",
                users[i].username,
                if users[i].disabled { " is disabled and cannot log in." } else { " is enabled again." }
            );
        } else {
            msg = format!("User {} deleted.", users[i].username);
            users.remove(i);
        }
    } else {
        err = "That user no longer exists.".into();
    }
    if err.is_empty() && auth::save_users(&app.data_dir, &users).is_err() {
        err = "Could not save the users: the data folder is not writable.".into();
    }
    if !err.is_empty() {
        app.flash(ctx, "error", err);
    } else {
        // Changing your own password keeps you logged in; other sessions of that user end.
        if action == "set_password" && name.eq_ignore_ascii_case(&me) {
            if let Some(u) = auth::find_user(&app.data_dir, &me) {
                app.login(ctx, &u);
            }
        }
        app.flash(ctx, "ok", msg);
    }
    redirect(ctx, "settings#users")
}

pub async fn page_post(State(app): State<Shared>, headers: HeaderMap, req: Request) -> Response {
    let ctx = app.ctx(&headers);
    if ctx.user.is_none() {
        return auth::login_page(&app, &ctx, "settings");
    }
    if !ctx.admin {
        return super::viewer_home(&ctx);
    }
    let (f, files) = read_post(req, 60 << 20).await;
    if f.get("csrf") != ctx.csrf {
        app.flash(&ctx, "error", "The form expired. Please try again.");
        return redirect(&ctx, "settings");
    }
    let action = f.get("action");
    match action.as_str() {
        "save_settings" => save_settings(&app, &ctx, &f),
        "save_keys" => save_keys(&app, &ctx, &f),
        "add_user" | "set_password" | "toggle_user" | "delete_user" | "set_role" => users_action(&app, &ctx, &action, &f),
        "backup_create" => {
            match backup_create(&app, "manual") {
                Ok(n) => app.flash(&ctx, "ok", format!("Backup made: {n}")),
                Err(e) => app.flash(&ctx, "error", e),
            }
            redirect(&ctx, "settings#backups")
        }
        "backup_delete" => {
            match backup_path(&app, &f.get("name")).map(std::fs::remove_file) {
                Some(Ok(())) => app.flash(&ctx, "ok", "Backup deleted."),
                _ => app.flash(&ctx, "error", "That backup could not be deleted."),
            }
            redirect(&ctx, "settings#backups")
        }
        "backup_restore" | "backup_upload" => {
            let gz = if action == "backup_restore" {
                backup_path(&app, &f.get("name")).and_then(|p| std::fs::read(p).ok()).unwrap_or_default()
            } else {
                files.get("file").map(|(_, b)| b.clone()).unwrap_or_default()
            };
            let err = if gz.is_empty() { "Choose a backup to restore.".to_string() } else { backup_restore(&app, &gz) };
            if err.is_empty() {
                app.flash(&ctx, "ok", "Backup restored. The state before it was saved as a backup too. Rebuild the guides to use it.");
            } else {
                app.flash(&ctx, "error", err);
            }
            redirect(&ctx, "settings#backups")
        }
        _ => {
            app.flash(&ctx, "error", "Unknown action.");
            redirect(&ctx, "settings")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_names() {
        assert_eq!(backup_reason("epgarr-20261127-151000.json.gz").as_deref(), Some("manual"));
        assert_eq!(backup_reason("epgarr-20261127-151000-before-restore.json.gz").as_deref(), Some("before-restore"));
        assert_eq!(backup_reason("epgarr-2026-151000.json.gz"), None);
        assert_eq!(backup_reason("../epgarr-20261127-151000.json.gz"), None);
        assert_eq!(backup_reason("epgarr-20261127-151000-x/y.json.gz"), None);
        assert!(is_logo_name("0123456789abcdef0123.png"));
        assert!(!is_logo_name("0123456789abcdef0123.svg"));
        assert!(valid_language("en-US") && valid_language("fr") && !valid_language("EN"));
        assert_eq!(key_hint("abcdefgh1234"), "••••1234");
    }
}
