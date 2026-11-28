//! Channel logos set from the TV Guide page (api/logo). JSON only, login
//! and CSRF required.
//!
//!   GET  ?channel=<id>                    current logo, where it comes from, limits
//!   POST action=save, channel, file=<upload> | url=<http(s) URL>
//!   POST action=reset, channel            back to the automatic logo
//!   POST action=build                     build the guides now
//!
//! Uploads and URLs pass the same checks: PNG, JPEG, WebP or GIF by their
//! magic bytes (never SVG), at most 2 MB, 32x32 to 4096x4096. The image is
//! stored as PNG, at most 512 px on its longest side, in
//! <output dir>/logos/custom/ (media servers fetch logos directly), and is
//! recorded as a channel override.

use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashMap;

use super::{base_url, jobs, json, read_post, App, Ctx, Shared};
use crate::timeutil;

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MIN_SIDE: u32 = 32;
const MAX_SIDE: u32 = 4096;
const STORE_SIDE: u32 = 512;

fn mb(b: usize) -> String {
    let s = format!("{:.1}", b as f64 / 1_048_576.0);
    format!("{} MB", s.strip_suffix(".0").unwrap_or(&s))
}

/// The image type from its first bytes, or None (SVG and anything else is refused).
fn image_type(b: &[u8]) -> Option<image::ImageFormat> {
    use image::ImageFormat::*;
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Png)
    } else if b.starts_with(b"\xFF\xD8\xFF") {
        Some(Jpeg)
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some(Gif)
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(WebP)
    } else {
        None
    }
}

/// Check an image and turn it into a PNG of at most 512 px. Errors are for the dialog.
fn convert(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.is_empty() {
        return Err("The file is empty.".into());
    }
    if bytes.len() > MAX_BYTES {
        return Err(format!("The image is too large: {} (the limit is {}).", mb(bytes.len()), mb(MAX_BYTES)));
    }
    let fmt = image_type(bytes).ok_or("That is not a PNG, JPEG, WebP or GIF image. SVG is not allowed.")?;
    let reader = || image::ImageReader::with_format(std::io::Cursor::new(bytes), fmt);
    // The size first, so a huge image is refused before it is decoded.
    let (w, h) = reader().into_dimensions().map_err(|_| "The image could not be read; it may be damaged.")?;
    if w < MIN_SIDE || h < MIN_SIDE {
        return Err(format!("The image is too small: {w}x{h} (at least {MIN_SIDE}x{MIN_SIDE})."));
    }
    if w > MAX_SIDE || h > MAX_SIDE {
        return Err(format!("The image is too big: {w}x{h} (at most {MAX_SIDE}x{MAX_SIDE})."));
    }
    let img = reader().decode().map_err(|_| "The image could not be read; it may be damaged.")?;
    let img = if w.max(h) > STORE_SIDE { img.resize(STORE_SIDE, STORE_SIDE, image::imageops::FilterType::Lanczos3) } else { img };
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).map_err(|e| format!("Could not convert the image: {e}"))?;
    Ok(out.into_inner())
}

/// Download a logo: http(s) only, 10 s, at most 3 redirects, never more than 2 MB.
async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let ok = url.len() <= 2000
        && (url.starts_with("http://") || url.starts_with("https://"))
        && !url.chars().any(char::is_whitespace)
        && url.split_once("://").is_some_and(|(_, r)| r.len() > 1 && !r.starts_with(['/', '.', '?', '#', '$']));
    if !ok {
        return Err("Enter a full http:// or https:// address.".into());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent("EPGArr logo fetch")
        .build()
        .map_err(|_| "Could not fetch that address.")?;
    let mut resp = client.get(url).header("Accept", "image/*").send().await.map_err(|_| "Could not fetch that address.")?;
    if !resp.status().is_success() {
        return Err(format!("Could not fetch that address (HTTP {}).", resp.status().as_u16()));
    }
    let mut data = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|_| "Fetching the image failed.")? {
        data.extend_from_slice(&chunk);
        if data.len() > MAX_BYTES {
            return Err(format!("The image is larger than {}.", mb(MAX_BYTES)));
        }
    }
    Ok(data)
}

/// The first 20 hex digits of the channel id's SHA-256.
fn file_base(channel: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(channel.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>()[..20].to_string()
}

fn remove_old(app: &App, base: &str) {
    if let Ok(rd) = std::fs::read_dir(app.output_dir.join("logos").join("custom")) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().split('.').next() == Some(base) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn store(app: &App, channel: &str, png: &[u8]) -> Result<String, String> {
    let dir = app.output_dir.join("logos").join("custom");
    std::fs::create_dir_all(&dir).map_err(|_| "Could not create the logos folder.")?;
    let base = file_base(channel);
    remove_old(app, &base);
    let name = format!("{base}.png");
    crate::output::write_atomic(&dir.join(&name), png).map_err(|_| "Could not save the logo.")?;
    Ok(name)
}

/// How many guides have this channel, and where its logo comes from now.
fn logo_info(conn: &Connection, channel: &str) -> Value {
    let mut guides = 0;
    let mut logo = String::new();
    if let Ok(mut st) = conn.prepare("SELECT logo FROM guide_channels WHERE channel_id = ?1") {
        if let Ok(rows) = st.query_map([channel], |r| r.get::<_, Option<String>>(0)) {
            for l in rows.flatten() {
                guides += 1;
                if logo.is_empty() {
                    logo = l.unwrap_or_default();
                }
            }
        }
    }
    let ov: String = conn
        .query_row("SELECT logo_url FROM channel_overrides WHERE channel_id = ?1", [channel], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut source = if ov.contains("/logos/custom/") {
        "custom".to_string()
    } else {
        conn.query_row("SELECT source FROM channel_logo_sources WHERE channel_id = ?1 LIMIT 1", [channel], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
            .unwrap_or_default()
    };
    if source.is_empty() {
        source = if logo.is_empty() { "none".into() } else { "feed".into() };
    }
    json!({ "guides": guides.max(1), "logo": logo, "source": source })
}

/// Search tv-logos for the dialog. The list is the one the builds downloaded.
fn search(app: &App, channel: &str, query: &str) -> Value {
    let query = query.trim();
    if query.is_empty() || query.len() > 100 {
        return json!({ "error": "Type a channel name to search for." });
    }
    let Ok(conn) = app.db() else { return json!({ "error": "The database is not available." }) };
    let paths = crate::db::tv_logo_list(&conn).map(|(p, _, _)| p).unwrap_or_default();
    if paths.is_empty() {
        return json!({ "error": "The tv-logos list is downloaded with the first guide build. Build a guide, then search again." });
    }
    let cfg = crate::db::load_config(&conn, &app.data_dir, Some(&app.output_dir)).ok();
    let raw = cfg.as_ref().map(|c| c.tv_logos_raw_url.clone()).unwrap_or_default();
    // The channel's own country (from an id like "ABC.us"), else the default logo country.
    let cc = channel
        .split('@')
        .next()
        .and_then(|b| b.rsplit_once('.'))
        .map(|(_, c)| c.to_ascii_lowercase())
        .filter(|c| c.len() == 2 && c.chars().all(|x| x.is_ascii_alphabetic()))
        .or_else(|| cfg.as_ref().map(|c| c.logo_country.clone()))
        .unwrap_or_default();
    let cc = if cc == "gb" { "uk".to_string() } else { cc };
    let found = crate::tvlogos::search(&paths, &raw, query, &cc, 60);
    json!({ "results": found.into_iter().map(|(name, country, url)| json!({ "name": name, "country": country, "url": url })).collect::<Vec<_>>() })
}

fn merge(mut a: Value, b: Value) -> Value {
    if let (Some(a), Value::Object(b)) = (a.as_object_mut(), b) {
        a.extend(b);
    }
    a
}

fn valid_channel(c: &str) -> bool {
    !c.is_empty() && c.len() <= 200 && !c.chars().any(|ch| ch.is_control())
}

pub async fn info(State(app): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let ctx = app.ctx(&headers);
    if ctx.user.is_none() {
        return json(&ctx, StatusCode::UNAUTHORIZED, json!({ "error": "Log in first." }));
    }
    let channel = q.get("channel").cloned().unwrap_or_default();
    if !valid_channel(&channel) {
        return json(&ctx, StatusCode::BAD_REQUEST, json!({ "error": "Which channel?" }));
    }
    if let Some(query) = q.get("search") {
        return json(&ctx, StatusCode::OK, search(&app, &channel, query));
    }
    match app.db() {
        Ok(conn) => json(
            &ctx,
            StatusCode::OK,
            merge(logo_info(&conn, &channel), json!({ "limit": MAX_BYTES, "limit_text": mb(MAX_BYTES), "converts": true })),
        ),
        Err(e) => json(&ctx, StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": format!("Something went wrong: {e}") })),
    }
}

pub async fn post(State(app): State<Shared>, headers: HeaderMap, req: Request) -> Response {
    let ctx = app.ctx(&headers);
    if ctx.user.is_none() {
        return json(&ctx, StatusCode::UNAUTHORIZED, json!({ "error": "Log in first." }));
    }
    let (f, files) = read_post(req, MAX_BYTES).await;
    if f.get("csrf") != ctx.csrf {
        return json(&ctx, StatusCode::FORBIDDEN, json!({ "error": "The page expired. Reload it and try again." }));
    }
    match handle(&app, &ctx, &headers, &f, &files).await {
        Ok(v) => json(&ctx, StatusCode::OK, v),
        Err((code, msg)) => json(&ctx, code, json!({ "error": msg })),
    }
}

async fn handle(
    app: &Shared,
    _ctx: &Ctx,
    headers: &HeaderMap,
    f: &super::guides::Posted,
    files: &HashMap<String, super::Upload>,
) -> Result<Value, (StatusCode, String)> {
    let ok = StatusCode::OK;
    let internal = |e: anyhow::Error| (StatusCode::INTERNAL_SERVER_ERROR, format!("Something went wrong: {e}"));
    let action = f.get("action");
    let channel = f.get("channel").trim().to_string();
    if !valid_channel(&channel) {
        return Err((StatusCode::BAD_REQUEST, "Which channel?".into()));
    }
    if action == "build" {
        let e = jobs::start_build(app, None, "");
        return Ok(if e.is_empty() { json!({ "ok": true }) } else { json!({ "error": e }) });
    }
    let conn = app.db().map_err(internal)?;
    let now = timeutil::now_unix();
    if action == "reset" {
        remove_old(app, &file_base(&channel));
        let r = (|| -> rusqlite::Result<()> {
            conn.execute("UPDATE channel_overrides SET logo_url = '', updated_at = ?1 WHERE channel_id = ?2", params![now, channel])?;
            conn.execute("DELETE FROM channel_overrides WHERE channel_id = ?1 AND logo_url = '' AND group_name = ''", [&channel])?;
            conn.execute("UPDATE guide_channels SET logo = NULL WHERE channel_id = ?1 AND logo LIKE '%/logos/custom/%'", [&channel])?;
            Ok(())
        })();
        r.map_err(|e| internal(e.into()))?;
        return Ok(merge(
            json!({ "ok": true, "logo": "", "message": "Back to the automatic logo from the next build." }),
            logo_info(&conn, &channel),
        ));
    }
    if action != "save" {
        return Err((StatusCode::BAD_REQUEST, "Unknown action.".into()));
    }
    let bytes = match files.get("file") {
        Some((name, _)) if name == "too-large" => return Err((ok, format!("The image is too large (the limit is {}).", mb(MAX_BYTES)))),
        Some((_, b)) if !b.is_empty() => b.clone(),
        _ => {
            let url = f.get("url").trim().to_string();
            if url.is_empty() {
                return Err((ok, "Choose an image or enter an address.".into()));
            }
            fetch(&url).await.map_err(|e| (ok, e))?
        }
    };
    let png = tokio::task::spawn_blocking(move || convert(&bytes)).await.map_err(|e| internal(e.into()))?.map_err(|e| (ok, e))?;
    let file = store(app, &channel, &png).map_err(|e| (ok, e))?;
    let url = format!("{}/logos/custom/{file}?v={now}", base_url(headers));
    let r = (|| -> rusqlite::Result<()> {
        conn.execute(
            "INSERT INTO channel_overrides (channel_id, logo_url, group_name, updated_at) VALUES (?1, ?2, '', ?3)
             ON CONFLICT(channel_id) DO UPDATE SET logo_url = excluded.logo_url, updated_at = excluded.updated_at",
            params![channel, url, now],
        )?;
        conn.execute("UPDATE guide_channels SET logo = ?1 WHERE channel_id = ?2", params![url, channel])?;
        Ok(())
    })();
    r.map_err(|e| internal(e.into()))?;
    Ok(merge(
        json!({ "ok": true, "logo": url, "converted": true, "message": "Saved. It is in the TV Guide now; your media server gets it with the next build." }),
        logo_info(&conn, &channel),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_and_converts() {
        assert_eq!(mb(MAX_BYTES), "2 MB");
        assert!(convert(b"<svg></svg>").is_err());
        // A 600x300 PNG comes back as 512x256.
        let img = image::DynamicImage::new_rgba8(600, 300);
        let mut b = std::io::Cursor::new(Vec::new());
        img.write_to(&mut b, image::ImageFormat::Png).unwrap();
        let out = convert(&b.into_inner()).unwrap();
        let back = image::load_from_memory(&out).unwrap();
        assert_eq!((back.width(), back.height()), (512, 256));
        // Too small.
        let tiny = image::DynamicImage::new_rgba8(10, 10);
        let mut b = std::io::Cursor::new(Vec::new());
        tiny.write_to(&mut b, image::ImageFormat::Png).unwrap();
        assert!(convert(&b.into_inner()).unwrap_err().contains("too small"));
        assert_eq!(file_base("abc").len(), 20);
    }
}
