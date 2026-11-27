//! The Activity page (activity.php): background TMDB lookups and guide builds, live.

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde_json::{json, Value};
use std::collections::HashMap;

use super::common::{guide_status, num, settings, st_i64, when};
use super::guides::Posted;
use super::html::{csrf_field, esc, icon, json_script, page};
use super::{auth, html_page, jobs, json, redirect, Shared};
use crate::db;

fn data(app: &Shared) -> Value {
    let e = crate::enrich::read_status(&app.data_dir);
    let active = jobs::enrich_running(app);
    let (mut cached, mut matched, mut tvmaze, mut sports) = (0i64, 0i64, 0i64, 0i64);
    if let Ok(conn) = app.db() {
        if let Ok((n, f)) = conn.query_row("SELECT COUNT(*), COALESCE(SUM(found), 0) FROM tmdb_cache", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))) {
            cached = n;
            matched = f;
        }
        tvmaze = conn.query_row("SELECT COUNT(*) FROM tvmaze_cache WHERE found = 1", [], |r| r.get(0)).unwrap_or(0);
        sports = conn.query_row("SELECT COUNT(*) FROM sportsdb_cache WHERE found = 1", [], |r| r.get(0)).unwrap_or(0);
    }
    let titles = e.titles as i64;
    let done = titles.min((e.cached + e.done) as i64);
    let state = if active {
        "running"
    } else if e.stopped_by_user {
        "paused"
    } else if e.state == "done" && e.pending == 0 {
        "finished"
    } else if e.state == "failed" {
        "failed"
    } else {
        "idle"
    };
    let job = app.jobs.running();
    let build_log = {
        let p = jobs::logs_dir(app).join("guide-build.log");
        let t = std::fs::metadata(&p).and_then(|m| m.modified()).ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
        match t {
            Some(t) => format!("Started from these pages ({}):\n{}", when(t), jobs::tail(&p, 120)),
            None => String::new(),
        }
    };
    json!({
        "state": state,
        "phase": e.state,
        "message": e.message,
        "titles": titles,
        "done": done,
        "percent": if titles > 0 { (done as f64 * 1000.0 / titles as f64).round() / 10.0 } else { 0.0 },
        "pending": e.pending,
        "failed": e.failed,
        "skipped": e.skipped,
        "rate": e.rate,
        "eta_secs": e.eta_secs,
        "updated": e.updated_unix,
        "started": e.started_unix,
        "cached": cached,
        "matched": matched,
        "not_found": cached - matched,
        "tvmaze": tvmaze,
        "sports": sports,
        "recent": serde_json::to_value(&e.recent).unwrap_or(json!([])),
        "log": jobs::tail(&jobs::logs_dir(app).join("enrich.log"), 100),
        "build_log": build_log,
        "build_running": jobs::build_running(app),
        "job": job.map(|j| j.label).unwrap_or_default(),
        "now": crate::timeutil::now_unix(),
    })
}

pub async fn page_get(State(app): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let ctx = app.ctx(&headers);
    if q.contains_key("json") {
        if ctx.user.is_none() {
            return json(&ctx, StatusCode::UNAUTHORIZED, json!({ "error": "Log in first." }));
        }
        return json(&ctx, StatusCode::OK, data(&app));
    }
    if ctx.user.is_none() {
        return auth::login_page(&app, &ctx, "activity.php");
    }
    let csrf = csrf_field(&ctx);
    let bg_on = settings(&app).get("tmdb_background").map_or(true, |v| v != "0");
    let guides = app.db().ok().and_then(|c| db::guides(&c).ok()).unwrap_or_default();
    let rows: String = guides
        .iter()
        .map(|g| {
            let st = guide_status(&app, g);
            let built = st_i64(&st, "built_at_unix");
            let status = if !g.enabled {
                r#"<span class="muted">off</span>"#
            } else if built == 0 {
                r#"<span class="muted">not built yet</span>"#
            } else if !st.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
                r#"<span class="bad-text">last build failed</span>"#
            } else {
                r#"<span class="ok-text">up to date</span>"#
            };
            format!(
                r#"<tr><td><strong>{name}</strong></td><td>{status}</td><td>{when}</td><td>{ch}</td><td><form method="post" action="guides.php" class="inline act-gbuild">{csrf}<input type="hidden" name="action" value="build_guide"><input type="hidden" name="back" value="activity"><input type="hidden" name="id" value="{id}"><button class="ghost"{dis}>Build</button></form></td></tr>"#,
                name = esc(&g.name),
                when = esc(&when(built)),
                ch = num(st_i64(&st, "channels")),
                id = g.id,
                dis = if g.enabled { "" } else { " disabled" },
            )
        })
        .collect();
    let table = if guides.is_empty() {
        String::new()
    } else {
        format!(r#"<table class="compact act-guides"><thead><tr><th>Guide</th><th>Status</th><th>Last update</th><th>Channels</th><th></th></tr></thead><tbody>{rows}</tbody></table>"#)
    };
    let body = format!(
        r#"<div class="card act-main full" id="act"><div class="act-head"><div><span class="act-state" id="act-state">…</span><p class="muted act-msg" id="act-msg"></p></div><div class="act-controls"><form method="post" class="inline" id="f-start">{csrf}<input type="hidden" name="action" value="enrich_start"><button>Start</button></form><form method="post" class="inline" id="f-pause" hidden>{csrf}<input type="hidden" name="action" value="enrich_stop"><button class="ghost">Pause</button></form></div></div><div class="progress act-progress" role="progressbar" aria-label="TMDB titles looked up" aria-valuemin="0" aria-valuemax="100"><span id="act-bar" style="width:0%"></span></div><div class="act-figures"><div><strong id="act-done">0</strong> of <span id="act-titles">0</span> titles <span class="muted">(<span id="act-pct">0</span>%)</span></div><div><strong id="act-rate">–</strong> <span class="muted">titles/s</span></div><div><strong id="act-eta">–</strong> <span class="muted">left</span></div><div class="muted" id="act-updated"></div></div><p class="hint" style="margin:0.6rem 0 0">{resume}</p></div><section class="tiles act-counts full"><div class="tile"><div class="label">Cached</div><div class="value" id="c-cached">0</div><div class="note">titles in the TMDB cache</div></div><div class="tile"><div class="label">Matched</div><div class="value" id="c-matched">0</div><div class="note">found on TMDB</div></div><div class="tile"><div class="label">Not found</div><div class="value" id="c-notfound">0</div><div class="note">no TMDB match</div></div><div class="tile"><div class="label">TVmaze</div><div class="value" id="c-tvmaze">0</div><div class="note">series found there</div></div><div class="tile"><div class="label">TheSportsDB</div><div class="value" id="c-sports">0</div><div class="note">matches found there</div></div><div class="tile"><div class="label">Pending</div><div class="value" id="c-pending">0</div><div class="note">still to look up</div></div><div class="tile"><div class="label">Skipped</div><div class="value" id="c-skipped">0</div><div class="note">news, sports, filler</div></div></section><div class="card full"><h2>{grid}Recently looked up</h2><div class="act-recent" id="act-recent"><p class="muted">Nothing yet.</p></div></div><div class="card full"><h2>{list}Background lookups log</h2><pre class="act-log" id="act-log">…</pre></div><div class="card full" id="act-build"><h2>{tv}Guide build</h2><p id="act-build-text" class="muted">No guide build is running.</p><div id="act-build-cancel" hidden><form method="post" class="inline cancel-form" onsubmit="return confirm('Stop the running task?')">{csrf}<input type="hidden" name="action" value="cancel"><button class="danger">Cancel</button></form></div><pre class="act-log act-build-log" id="act-build-log">…</pre><form method="post" class="inline" id="act-build-start">{csrf}<input type="hidden" name="action" value="build"><button class="secondary">Build all guides now</button></form>{table}<p class="hint">Scheduled builds write to the container log (docker logs epgarr).</p></div><script>window.EPG_ACTIVITY = {d};window.EPG_SUPPORTED = true;</script><script src="assets/page-activity.js?v={v}"></script>"#,
        resume = if bg_on {
            "Resumes on its own every 15 minutes until every title is done. Pause stops it until you press Start.".to_string()
        } else {
            r#"Automatic resume is off in <a href="settings.php">Settings</a>; use Start here."#.to_string()
        },
        grid = icon("grid"),
        list = icon("list"),
        tv = icon("tv"),
        d = json_script(&data(&app)),
        v = env!("CARGO_PKG_VERSION"),
    );
    html_page(&ctx, page(&ctx, "activity", "Activity", "", &body))
}

pub async fn page_post(State(app): State<Shared>, headers: HeaderMap, Form(form): Form<Vec<(String, String)>>) -> Response {
    let ctx = app.ctx(&headers);
    if ctx.user.is_none() {
        return auth::login_page(&app, &ctx, "activity.php");
    }
    let f = Posted(form);
    if f.get("csrf") != ctx.csrf {
        app.flash(&ctx, "error", "The form expired. Please try again.");
        return redirect(&ctx, "activity.php");
    }
    match f.get("action").as_str() {
        "enrich_start" => match jobs::start_enrich(&app) {
            e if e.is_empty() => app.flash(&ctx, "ok", "Background TMDB lookups started."),
            e => app.flash(&ctx, "error", e),
        },
        "enrich_stop" => match jobs::stop_enrich(&app).await {
            e if e.is_empty() => app.flash(&ctx, "ok", "Paused. The results so far are kept; press Start to continue."),
            e => app.flash(&ctx, "error", e),
        },
        "build" => match jobs::start_build(&app, None, "") {
            e if e.is_empty() => app.flash(&ctx, "ok", "Guide build started."),
            e => app.flash(&ctx, "error", e),
        },
        "cancel" => match jobs::cancel(&app).await {
            e if e.is_empty() => app.flash(&ctx, "ok", "Cancelled."),
            e => app.flash(&ctx, "error", e),
        },
        _ => {}
    }
    redirect(&ctx, "activity.php")
}
