//! The Dashboard (index.php): overall tiles, one row per guide, recent builds.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use rusqlite::{params, Connection};
use serde_json::Value;
use std::collections::HashMap;

use super::common::{ago, default_hours, guide_status, guide_urls, num, st_i64, when};
use super::html::{csrf_field, esc, icon, page};
use super::{auth, html_page, jobs, Shared};
use crate::db;
use crate::timeutil;

struct Coverage {
    progs: i64,
    posters: i64,
    chans: i64,
    logos: i64,
}

/// Counting posters reads every programme, so the result is cached until the next build.
fn coverage(app: &Shared, conn: &Connection) -> Coverage {
    let stamp: String = conn
        .query_row("SELECT COALESCE(MAX(finished_at), 0) || '-' || COUNT(*) FROM runs", [], |r| r.get(0))
        .unwrap_or_default();
    let cache_file = app.data_dir.join("dashboard-cache.json");
    if let Some(v) = std::fs::read(&cache_file).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
        if v.get("stamp").and_then(|s| s.as_str()) == Some(stamp.as_str()) {
            let c = &v["cov"];
            return Coverage { progs: st_i64(c, "progs"), posters: st_i64(c, "posters"), chans: st_i64(c, "chans"), logos: st_i64(c, "logos") };
        }
    }
    let (progs, posters): (i64, i64) = conn
        .query_row("SELECT COUNT(*), COUNT(NULLIF(icon, '')) FROM guide_programmes", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap_or((0, 0));
    let (chans, logos): (i64, i64) = conn
        .query_row("SELECT COUNT(*), COUNT(NULLIF(logo, '')) FROM guide_channels", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap_or((0, 0));
    let _ = std::fs::write(
        &cache_file,
        serde_json::json!({ "stamp": stamp, "cov": { "progs": progs, "posters": posters, "chans": chans, "logos": logos } }).to_string(),
    );
    Coverage { progs, posters, chans, logos }
}

fn pct(a: i64, b: i64) -> String {
    if b > 0 { format!("{}%", (a as f64 * 100.0 / b as f64).round() as i64) } else { "—".into() }
}

pub async fn page_get(State(app): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let ctx = app.ctx(&headers);
    if ctx.user.is_none() {
        return auth::login_page(&app, &ctx, "index.php");
    }
    let conn = match app.db() {
        Ok(c) => c,
        Err(e) => {
            let body = format!(r#"<div class="card"><h2>Not ready yet</h2><p class="bad-text">{}</p></div>"#, esc(&format!("{e:#}")));
            return html_page(&ctx, page(&ctx, "dashboard", "Dashboard", "", &body));
        }
    };
    let guides = db::guides(&conn).unwrap_or_default();
    let cov = coverage(&app, &conn);
    let building = jobs::build_running(&app);
    let job_label = app.jobs.running().map(|j| j.label).unwrap_or_default();
    let hours = default_hours(&app);
    let show_all = q.contains_key("history");
    let csrf = csrf_field(&ctx);

    // Logo sources for the Logos tile's tooltip.
    let mut src: HashMap<String, i64> = HashMap::new();
    if let Ok(mut st) = conn.prepare("SELECT source, COUNT(*) FROM channel_logo_sources GROUP BY source") {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) {
            for (k, n) in rows.flatten() {
                src.insert(k, n);
            }
        }
    }
    let tip: Vec<String> = [("feed", "from the source"), ("custom", "set by you"), ("override", "overrides"), ("tv-logos", "tv-logos"), ("iptv-org", "iptv-org")]
        .iter()
        .filter_map(|(k, l)| src.get(*k).filter(|n| **n > 0).map(|n| format!("{} {l}", num(*n))))
        .collect();
    let logo_tip = if tip.is_empty() { format!("{} of {} channels", num(cov.logos), num(cov.chans)) } else { tip.join(" · ") };

    let on = guides.iter().filter(|g| g.enabled).count() as i64;
    let tiles = format!(
        r#"<section class="tiles tiles-5"><div class="tile"><div class="label">Guides</div><div class="value">{g}</div><div class="note">{on} on</div></div><div class="tile"><div class="label">Channels</div><div class="value">{c}</div><div class="note">across all guides</div></div><div class="tile"><div class="label">Programmes</div><div class="value">{p}</div><div class="note">across all guides</div></div><div class="tile" title="{at}"><div class="label">Artwork</div><div class="value">{ap}</div><div class="note">programmes with a poster</div></div><div class="tile" title="{lt}"><div class="label">Logos</div><div class="value">{lp}</div><div class="note">channels with a logo</div></div></section>"#,
        g = num(guides.len() as i64),
        on = num(on),
        c = num(cov.chans),
        p = num(cov.progs),
        at = esc(&format!("{} of {} programmes have a poster", num(cov.posters), num(cov.progs))),
        ap = pct(cov.posters, cov.progs),
        lt = esc(&logo_tip),
        lp = pct(cov.logos, cov.chans),
    );

    let mut rows = String::new();
    for g in &guides {
        let (url, gz) = guide_urls(&headers, g);
        let st = guide_status(&app, g);
        let built = st_i64(&st, "built_at_unix");
        let mine = building && (job_label.is_empty() || job_label == "Guide build" || job_label == format!("Guide build: {}", g.name));
        let (pill, label) = if !g.enabled {
            ("off", "Off")
        } else if mine {
            ("building", "Building")
        } else if built == 0 {
            ("none", "Not built yet")
        } else if !st.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
            ("failed", "Last build failed")
        } else {
            ("ok", "Up to date")
        };
        let h = g.refresh_hours.filter(|h| *h > 0.0).unwrap_or(hours);
        let next = if built > 0 && g.enabled { (timeutil::now_unix().max(built + (h * 3600.0).round() as i64) + 899) / 900 * 900 } else { 0 };
        let uid = format!("url-{}", g.slug);
        rows.push_str(&format!(
            r#"<div class="guide-row{off}"><div class="gr-name"><strong>{name}</strong><span class="pill pill-{pill}">{label}</span></div><div class="gr-facts muted"><span>Updated {upd}</span><span>Next {nxt}</span><span>{ch} channels</span></div><div class="url-box gr-url"><span class="url-fmt" role="group" aria-label="File format"><button type="button" class="on" data-for="{uid}" data-url="{url}">XML</button><button type="button" data-for="{uid}" data-url="{gz}">GZ</button></span><input type="text" id="{uid}" value="{url}" readonly aria-label="{name} guide address"><button type="button" class="copy" data-for="{uid}">{copy}<span>Copy</span></button></div><form method="post" action="guides.php" class="inline gr-build">{csrf}<input type="hidden" name="action" value="build_guide"><input type="hidden" name="back" value="index"><input type="hidden" name="id" value="{id}"><button class="secondary"{dis}>Build now</button></form></div>"#,
            off = if g.enabled { "" } else { " is-off" },
            name = esc(&g.name),
            upd = esc(&ago(built)),
            nxt = if next > 0 { esc(&ago(next)) } else if g.enabled { "after the first build".into() } else { "off".into() },
            ch = num(st_i64(&st, "channels")),
            uid = esc(&uid),
            url = esc(&url),
            gz = esc(&gz),
            copy = icon("copy"),
            id = g.id,
            dis = if building || !g.enabled { " disabled" } else { "" },
        ));
    }

    let history = history_card(&conn, &guides, show_all);
    let body = format!(
        r#"{tiles}<div class="card"><div class="hero-head"><h2>{list}Your guides</h2><a href="guides.php">Manage guides</a></div><p class="hint" style="margin:0 0 0.8rem">Add a guide's address as an XMLTV guide source in your media server (Emby, Plex, Jellyfin or any XMLTV client).</p><div class="guide-rows">{rows}</div></div>{history}<script>window.EPG_BUSY = {busy};</script><script src="assets/page-dashboard.js?v={v}"></script>"#,
        list = icon("list"),
        busy = building || app.jobs.running().is_some(),
        v = env!("CARGO_PKG_VERSION"),
    );
    html_page(&ctx, page(&ctx, "dashboard", "Dashboard", "", &body))
}

fn history_card(conn: &Connection, guides: &[db::Guide], show_all: bool) -> String {
    let names: HashMap<i64, &str> = guides.iter().map(|g| (g.id, g.name.as_str())).collect();
    let several = guides.len() > 1;
    let limit = if show_all { 50 } else { 5 };
    let mut rows = String::new();
    let mut count = 0;
    if let Ok(mut st) = conn.prepare(
        "SELECT id, guide_id, started_at, finished_at, ok, duration_secs, channels, programmes, posters_added, ratings_added,
                errors_total, error, summary FROM runs ORDER BY id DESC LIMIT ?1",
    ) {
        let it = st.query_map(params![limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<bool>>(4)?,
                r.get::<_, Option<f64>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, Option<i64>>(8)?,
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, Option<i64>>(10)?,
                r.get::<_, Option<String>>(11)?,
                r.get::<_, Option<String>>(12)?,
            ))
        });
        if let Ok(it) = it {
            for (id, gid, started, finished, ok, dur, ch, pr, posters, ratings, errs, error, summary) in it.flatten() {
                count += 1;
                let res = if finished.is_none() {
                    "running"
                } else if ok == Some(true) {
                    "ok"
                } else if error.as_deref() == Some("cancelled") {
                    "cancelled"
                } else {
                    "failed"
                };
                let cls = match res {
                    "ok" => "ok",
                    "failed" => "fail",
                    _ => "info",
                };
                let rs: Value = summary.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
                let v = |k: &str| match rs.get(k).and_then(|x| x.as_i64()) {
                    Some(n) => num(n),
                    None => r#"<span class="muted">n/a</span>"#.to_string(),
                };
                let opt = |n: Option<i64>| n.map(num).unwrap_or_default();
                let bytes = |k: &str| match rs.get(k).and_then(|x| x.as_i64()) {
                    Some(b) if b >= 1_048_576 => format!("{:.1} MB", b as f64 / 1_048_576.0),
                    Some(b) => format!("{} KB", (b as f64 / 1024.0).round() as i64),
                    None => r#"<span class="muted">n/a</span>"#.to_string(),
                };
                let warns = rs.get("warnings").and_then(|w| w.as_array()).cloned().unwrap_or_default();
                let errors = rs.get("errors").and_then(|w| w.as_array()).cloned().unwrap_or_default();
                let notes = errs.unwrap_or(0) + warns.len() as i64;
                let groups: String = rs
                    .get("categories_by_group")
                    .and_then(|g| g.as_object())
                    .map(|m| m.iter().map(|(k, n)| format!(r#"<dt class="sub">{}</dt><dd>{}</dd>"#, esc(k), num(n.as_i64().unwrap_or(0)))).collect())
                    .unwrap_or_default();
                let notes_html: String = warns
                    .iter()
                    .filter_map(|w| w.as_str())
                    .map(|w| format!(r#"<p class="warn-text run-note">{}</p>"#, esc(w)))
                    .chain(errors.iter().take(12).filter_map(|e| e.as_str()).map(|e| format!(r#"<p class="bad-text run-note">{}</p>"#, esc(e))))
                    .collect();
                let err_line = error.as_deref().filter(|e| !e.is_empty()).map(|e| format!(r#"<p class="bad-text" style="margin:0 0 0.6rem">{}</p>"#, esc(e))).unwrap_or_default();
                let gcell = if several { format!("<td>{}</td>", esc(gid.and_then(|g| names.get(&g).copied()).unwrap_or("—"))) } else { String::new() };
                rows.push_str(&format!(
                    r#"<tr class="run-row" tabindex="0" aria-expanded="false" aria-controls="run-{id}"><td class="chev-col"><span class="chev" aria-hidden="true">&#8250;</span></td>{gcell}<td><span class="dot {cls}"></span>{res}{nt}</td><td>{when}</td><td class="num">{dur}</td><td class="num">{ch}</td><td class="num">{pr}</td><td class="num">{logos}</td><td class="num">{posters}</td><td class="num">{groupsadded}</td></tr><tr class="run-detail" id="run-{id}" hidden><td colspan="{cols}">{err_line}<div class="run-grid"><section><h4>Logos</h4><dl><dt>Kept from the source</dt><dd>{l1}</dd><dt>From overrides</dt><dd>{l2}</dd><dt>From tv-logos</dt><dd>{l3}</dd><dt>From iptv-org</dt><dd>{l4}</dd><dt>Still missing</dt><dd>{l5}</dd></dl></section><section><h4>Added to programmes</h4><dl><dt>Posters</dt><dd>{posters}</dd><dt>Ratings</dt><dd>{ratings}</dd><dt>IMDb ratings</dt><dd>{imdb}</dd><dt class="sub">Posters from TVmaze</dt><dd>{tvm}</dd><dt class="sub">Posters from TheSportsDB</dt><dd>{sdb}</dd><dt>Descriptions</dt><dd>{descs}</dd><dt>Groups</dt><dd>{groupsadded}</dd>{groups}</dl></section><section><h4>TMDB titles</h4><dl><dt>Titles in the guide</dt><dd>{t1}</dd><dt>From the cache</dt><dd>{t2}</dd><dt>Looked up</dt><dd>{t3}</dd><dt>Not on TMDB</dt><dd>{t4}</dd><dt>Left for later</dt><dd>{t5}</dd><dt>Skipped (news, sports, filler)</dt><dd>{t6}</dd><dt>TVmaze looked up</dt><dd>{t7}</dd><dt>TheSportsDB looked up</dt><dd>{t8}</dd></dl></section><section><h4>Output</h4><dl><dt>.xml</dt><dd>{xb}</dd><dt>.xml.gz</dt><dd>{gb}</dd><dt>Dropped (days limit)</dt><dd>{dropped}</dd></dl></section></div>{notes_html}</td></tr>"#,
                    nt = if notes > 0 { format!(r#" <span class="muted">· {notes} note{}</span>"#, if notes > 1 { "s" } else { "" }) } else { String::new() },
                    when = esc(&when(started)),
                    dur = dur.map(|d| format!("{} s", d.round() as i64)).unwrap_or_default(),
                    ch = opt(ch),
                    pr = opt(pr),
                    logos = v("logos_inserted"),
                    posters = posters.map(num).unwrap_or_else(|| v("posters_added")),
                    ratings = ratings.map(num).unwrap_or_else(|| v("ratings_added")),
                    groupsadded = v("categories_added"),
                    cols = if several { 10 } else { 9 },
                    l1 = v("logos_detected"),
                    l2 = v("logos_from_overrides"),
                    l3 = v("logos_from_tvlogos"),
                    l4 = v("logos_from_iptv"),
                    l5 = v("logos_missing"),
                    imdb = v("imdb_ratings_added"),
                    tvm = v("posters_from_tvmaze"),
                    sdb = v("posters_from_sportsdb"),
                    descs = v("descs_added"),
                    t1 = v("tmdb_titles"),
                    t2 = v("tmdb_cache_hits"),
                    t3 = v("tmdb_lookups"),
                    t4 = v("tmdb_not_found"),
                    t5 = v("tmdb_deferred"),
                    t6 = v("tmdb_skipped"),
                    t7 = v("tvmaze_lookups"),
                    t8 = v("sportsdb_lookups"),
                    xb = bytes("xml_bytes"),
                    gb = bytes("gz_bytes"),
                    dropped = v("programmes_dropped"),
                ));
            }
        }
    }
    let link = if count == 0 {
        String::new()
    } else if show_all {
        r#"<a href="index.php#history">Show fewer</a>"#.to_string()
    } else {
        r#"<a href="index.php?history=all#history">View all</a>"#.to_string()
    };
    let table = if count == 0 {
        r#"<p class="muted">No builds yet.</p>"#.to_string()
    } else {
        format!(
            r#"<div class="table-scroll"><table class="compact runs"><thead><tr><th class="chev-col"></th>{gh}<th>Result</th><th>When</th><th class="num">Duration</th><th class="num">Channels</th><th class="num">Programmes</th><th class="num" title="Channel logos added (not already in the source)">Logos added</th><th class="num">Posters added</th><th class="num" title="Categories (groups) added, e.g. Movie, News, Sports">Groups added</th></tr></thead><tbody>{rows}</tbody></table></div>"#,
            gh = if several { "<th>Guide</th>" } else { "" },
        )
    };
    format!(
        r#"<div class="card" id="history"><div class="hero-head"><h2>{ic}{title}</h2>{link}</div>{table}</div>"#,
        ic = icon("history"),
        title = if show_all { "Build history" } else { "Recent builds" },
    )
}
