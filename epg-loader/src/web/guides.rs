//! The Guides page (guides.php): each guide's card, add/edit dialog, order,
//! build, source test, turn off, delete.

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use rusqlite::params;
use serde_json::json;
use std::collections::HashMap;

use super::common::{ago, default_hours, guide_status, guide_urls, hours_text, num, source_label, st_i64};
use super::html::{csrf_field, esc, icon, json_script, page};
use super::{auth, html_page, jobs, json, redirect, Ctx, Shared};
use crate::db;
use crate::timeutil;

/// A posted form: the first value of each field, plus repeated fields (ids[]).
pub struct Posted(pub Vec<(String, String)>);

impl Posted {
    pub fn get(&self, k: &str) -> String {
        self.0.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default()
    }
    pub fn all(&self, k: &str) -> Vec<String> {
        self.0.iter().filter(|(n, _)| n == k).map(|(_, v)| v.clone()).collect()
    }
    pub fn has(&self, k: &str) -> bool {
        self.0.iter().any(|(n, v)| n == k && !v.is_empty())
    }
}

fn form_data(headers: &HeaderMap, g: &db::Guide) -> serde_json::Value {
    let (url, _) = guide_urls(headers, g);
    json!({
        "id": g.id, "name": g.name, "slug": g.slug, "source_url": g.source_url,
        "days": g.days.map(|d| d.to_string()).unwrap_or_default(),
        "refresh_hours": g.refresh_hours.map(hours_text).unwrap_or_default(),
        "logo_country": g.logo_country.clone().unwrap_or_default(),
        "enabled": g.enabled, "legacy": false, "url": url,
    })
}

fn unique_slug(guides: &[db::Guide], name: &str, except: i64) -> String {
    let base = db::slugify(name);
    let taken: Vec<&str> = guides.iter().filter(|g| g.id != except).map(|g| g.slug.as_str()).collect();
    let mut slug = base.clone();
    let mut i = 2;
    while taken.contains(&slug.as_str()) {
        slug = format!("{base}-{i}");
        i += 1;
    }
    slug
}

pub async fn page_get(State(app): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let ctx = app.ctx(&headers);
    if ctx.user.is_none() {
        return auth::login_page(&app, &ctx, "guides.php");
    }
    // The source test in the add/edit dialog polls this.
    if q.contains_key("check") {
        let running = app.jobs.running().is_some_and(|j| j.name == "check-guide");
        let result = if running { None } else { jobs::guide_check(&app, "@form") };
        return json(
            &ctx,
            StatusCode::OK,
            json!({ "running": running, "result": result.map(|(ok, detail, at)| json!({ "ok": ok, "detail": detail, "at": at })) }),
        );
    }
    let guides = app.db().ok().and_then(|c| db::guides(&c).ok()).unwrap_or_default();
    let open = if q.contains_key("add") {
        Some(json!({}))
    } else {
        q.get("edit").and_then(|id| id.parse::<i64>().ok()).and_then(|id| guides.iter().find(|g| g.id == id)).map(|g| form_data(&headers, g))
    };
    let tools = r#"<button type="button" class="button" data-add>+ Add guide</button>"#;
    let body = render(&app, &ctx, &headers, &guides, open);
    html_page(&ctx, page(&ctx, "guides", "Guides", tools, &body))
}

fn render(app: &Shared, ctx: &Ctx, headers: &HeaderMap, guides: &[db::Guide], open: Option<serde_json::Value>) -> String {
    let csrf = csrf_field(ctx);
    let hours = default_hours(app);
    let building = jobs::build_running(app);
    let job_label = app.jobs.running().map(|j| j.label).unwrap_or_default();
    let mut cards = String::new();
    for g in guides {
        let (url, gz_url) = guide_urls(headers, g);
        let st = guide_status(app, g);
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
        let next = if built > 0 && g.enabled {
            let due = (timeutil::now_unix()).max(built + (h * 3600.0).round() as i64);
            (due + 899) / 900 * 900
        } else {
            0
        };
        let check = jobs::guide_check(app, &g.slug)
            .map(|(ok, detail, at)| {
                format!(r#"<p class="guide-test {}">Source test {}: {}</p>"#, if ok { "ok-text" } else { "bad-text" }, esc(&ago(at)), esc(&detail))
            })
            .unwrap_or_default();
        let err = if pill == "failed" {
            st.get("last_error")
                .and_then(|x| x.as_str())
                .map(|e| format!(r#"<p class="bad-text guide-err">{}</p>"#, esc(e)))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let days = g.days.filter(|d| *d > 0).map(|d| format!(" · keeps {d} days")).unwrap_or_default();
        let handle = if guides.len() > 1 {
            format!(r#"<button type="button" class="drag-handle" title="Drag to reorder (or use the arrow keys)" aria-label="Move {n}"><span aria-hidden="true">&#8942;&#8942;</span></button>"#, n = esc(&g.name))
        } else {
            String::new()
        };
        let delete = if guides.len() > 1 {
            let msg = serde_json::to_string(&format!("Delete the guide \"{}\"? Its guide files are removed too.", g.name)).unwrap_or_default();
            format!(r#"<form method="post" onsubmit="return confirm({m})">{csrf}<input type="hidden" name="action" value="delete_guide"><input type="hidden" name="id" value="{id}"><button class="danger-text">Delete</button></form>"#, m = esc(&msg), id = g.id)
        } else {
            String::new()
        };
        let has_file = app.output_dir.join("guides").join(format!("{}.xml", g.slug)).is_file();
        cards.push_str(&format!(
            r#"<article class="card guide-card{off}" data-id="{id}"><div class="hero-head">{handle}<h2>{tv}{name}</h2><span class="pill pill-{pill}">{label}</span></div><p class="muted guide-src">{src}{days} · every {every} h</p>{check}{err}<dl class="guide-facts"><dt>Channels</dt><dd>{ch}</dd><dt>Programmes</dt><dd>{pr}</dd><dt>Last update</dt><dd>{last}</dd><dt>Next update</dt><dd>{nxt}</dd></dl><div class="url-box"><span class="url-fmt" role="group" aria-label="File format"><button type="button" class="on" data-for="url-{slug}" data-url="{url}">XML</button><button type="button" data-for="url-{slug}" data-url="{gz}">GZ</button></span><input type="text" id="url-{slug}" value="{url}" readonly aria-label="{name} guide address"><button type="button" class="copy" data-for="url-{slug}"{nf}>{copy}<span>Copy</span></button></div><div class="guide-actions"><form method="post" class="inline">{csrf}<input type="hidden" name="action" value="build_guide"><input type="hidden" name="id" value="{id}"><button class="ga-primary"{dis}>{play}Build now</button></form><span class="ga-right"><a class="ga-icon" href="guide.php?guide={slug}" title="View in TV Guide" aria-label="View {name} in TV Guide">{eye}</a><button type="button" class="ga-icon" title="Edit" aria-label="Edit {name}" data-edit="{data}">{edit}</button><details class="ga-more"><summary aria-label="More actions" title="More actions"><span aria-hidden="true">&#8943;</span></summary><div class="ga-menu"><form method="post">{csrf}<input type="hidden" name="action" value="test_guide"><input type="hidden" name="id" value="{id}"><button>Test source</button></form><form method="post">{csrf}<input type="hidden" name="action" value="toggle_guide"><input type="hidden" name="id" value="{id}"><button>{toggle}</button></form>{delete}</div></details></span></div></article>"#,
            off = if g.enabled { "" } else { " is-off" },
            id = g.id,
            tv = icon("tv"),
            name = esc(&g.name),
            src = esc(&source_label(&g.source_url)),
            every = hours_text(h),
            ch = num(st_i64(&st, "channels")),
            pr = num(st_i64(&st, "programmes")),
            last = esc(&ago(built)),
            nxt = if !g.enabled { "off".to_string() } else if next > 0 { esc(&ago(next)) } else { "after the first build".to_string() },
            slug = esc(&g.slug),
            url = esc(&url),
            gz = esc(&gz_url),
            nf = if has_file { "" } else { r#" title="Not built yet""# },
            copy = icon("copy"),
            dis = if building || !g.enabled { " disabled" } else { "" },
            play = icon("play"),
            eye = icon("eye"),
            edit = icon("edit"),
            data = esc(&form_data(headers, g).to_string()),
            toggle = if g.enabled { "Turn off" } else { "Turn on" },
        ));
    }
    let base = format!("{}/guides/", super::base_url(headers));
    let globals = format!(
        "<script>window.EPG_GUIDE_BASE = {};window.EPG_OPEN_GUIDE = {};</script>",
        json_script(&json!(base)),
        json_script(&open.unwrap_or(serde_json::Value::Null)),
    );
    format!(
        r#"<dialog class="g-dialog" id="g-dialog" aria-labelledby="g-title"><form method="post" class="guide-form" id="g-form" autocomplete="off"><div class="hero-head"><h2 id="g-title">Add a guide</h2><button type="button" class="ghost" data-close aria-label="Close">&times;</button></div>{csrf}<input type="hidden" name="action" value="save_guide"><input type="hidden" name="id" id="g-id" value="0"><label for="g-name">Name</label><input type="text" id="g-name" name="name" maxlength="60" required><p class="hint" id="g-addr">Its address is made from the name, e.g. guides/us-locals.xml.</p><label for="g-src">XMLTV source URL</label><div class="url-box"><input type="text" id="g-src" name="source_url" placeholder="https://example.com/guide.xml.gz" required><button type="button" class="secondary" id="g-test">Test source</button></div><p class="hint" id="g-test-out">Plain or gzipped XMLTV, or an absolute file path on the server.</p><div class="row"><div><label for="g-days">Days to keep</label><input type="number" id="g-days" name="days" min="0" max="60" placeholder="all"><p class="hint">Empty or 0 keeps everything.</p></div><div><label for="g-hours">Rebuild every (hours)</label><input type="number" id="g-hours" name="refresh_hours" min="0.5" max="168" step="0.5" placeholder="{dh} (default)"><p class="hint">Empty uses the default.</p></div><div class="g-only-new-schema"><label for="g-lc">Logo country</label><input type="text" id="g-lc" name="logo_country" maxlength="2" placeholder="default" class="short"><p class="hint">For channels without a country.</p></div></div><label class="check g-only-new-schema"><input type="checkbox" name="enabled" id="g-enabled" value="1" checked> Build this guide on the schedule</label><div class="form-actions"><button>Save guide</button><button type="button" class="ghost" data-close>Cancel</button></div></form></dialog><div class="guide-list">{cards}</div>{globals}<script src="assets/page-guides.js?v={v}"></script>"#,
        dh = hours_text(hours),
        v = env!("CARGO_PKG_VERSION"),
    )
}

/// Check the guide form. Returns the fields or the problems.
struct GuideInput {
    name: String,
    source_url: String,
    days: Option<i64>,
    hours: Option<f64>,
    lc: Option<String>,
    enabled: bool,
}

fn guide_input(f: &Posted) -> Result<GuideInput, Vec<String>> {
    let mut e = Vec::new();
    let name = f.get("name").trim().to_string();
    if name.is_empty() || name.chars().count() > 60 {
        e.push("Give the guide a name (up to 60 characters).".to_string());
    }
    let src = f.get("source_url").trim().to_string();
    if src.is_empty() {
        e.push("The XMLTV source URL is required.".into());
    } else if src.len() > 2000 || src.chars().any(char::is_control) {
        e.push("The XMLTV source URL is not valid.".into());
    } else if !(src.to_ascii_lowercase().starts_with("http://") || src.to_ascii_lowercase().starts_with("https://") || src.starts_with('/')) {
        e.push("The source must be an http(s) URL or an absolute file path on the server.".into());
    }
    let days_s = f.get("days");
    let days = match days_s.trim() {
        "" => None,
        d => match d.parse::<i64>() {
            Ok(n) if (0..=60).contains(&n) => Some(n),
            _ => {
                e.push("Days must be empty (keep everything) or a whole number up to 60.".into());
                None
            }
        },
    };
    let hours_s = f.get("refresh_hours");
    let hours = match hours_s.trim() {
        "" => None,
        h => match h.parse::<f64>() {
            Ok(n) if (0.5..=168.0).contains(&n) => Some(n),
            _ => {
                e.push("Rebuild interval must be empty (use the default) or between 0.5 and 168 hours.".into());
                None
            }
        },
    };
    let lc_s = f.get("logo_country").trim().to_lowercase();
    let lc = if lc_s.is_empty() {
        None
    } else if lc_s.len() == 2 && lc_s.chars().all(|c| c.is_ascii_lowercase()) {
        Some(lc_s)
    } else {
        e.push("Logo country must be empty (use the default) or a two-letter code such as us.".into());
        None
    };
    if e.is_empty() {
        Ok(GuideInput { name, source_url: src, days, hours, lc, enabled: f.has("enabled") })
    } else {
        Err(e)
    }
}

pub async fn page_post(State(app): State<Shared>, headers: HeaderMap, Form(form): Form<Vec<(String, String)>>) -> Response {
    let ctx = app.ctx(&headers);
    if ctx.user.is_none() {
        return auth::login_page(&app, &ctx, "guides.php");
    }
    let f = Posted(form);
    let action = f.get("action");
    if f.get("csrf") != ctx.csrf {
        if action == "reorder" || action == "test_source" {
            return json(&ctx, StatusCode::OK, json!({ "error": "The page expired. Reload it and try again." }));
        }
        app.flash(&ctx, "error", "The form expired. Please try again.");
        return redirect(&ctx, "guides.php");
    }
    let conn = match app.db() {
        Ok(c) => c,
        Err(e) => {
            app.flash(&ctx, "error", format!("{e:#}"));
            return redirect(&ctx, "guides.php");
        }
    };
    let guides = db::guides(&conn).unwrap_or_default();
    let id: i64 = f.get("id").parse().unwrap_or(0);
    let g = guides.iter().find(|g| g.id == id).cloned();
    let back = match f.get("back").as_str() {
        "activity" => "activity.php",
        "index" => "index.php",
        _ => "guides.php",
    };
    match action.as_str() {
        "reorder" => {
            let mut ids: Vec<i64> = f.all("ids[]").iter().filter_map(|s| s.parse().ok()).collect();
            let mut known: Vec<i64> = guides.iter().map(|g| g.id).collect();
            let order = ids.clone();
            ids.sort_unstable();
            known.sort_unstable();
            if ids != known {
                return json(&ctx, StatusCode::OK, json!({ "error": "The list changed; reload the page." }));
            }
            for (i, id) in order.iter().enumerate() {
                let _ = conn.execute("UPDATE guides SET sort_order = ?1 WHERE id = ?2", params![i as i64, id]);
            }
            return json(&ctx, StatusCode::OK, json!({ "ok": true }));
        }
        "test_source" => {
            let src = f.get("source_url").trim().to_string();
            let lower = src.to_ascii_lowercase();
            let res = if src.is_empty() || src.len() > 2000 || src.chars().any(char::is_control) || !(lower.starts_with("http://") || lower.starts_with("https://") || src.starts_with('/')) {
                json!({ "error": "Enter an http(s) URL or an absolute file path first." })
            } else if std::fs::write(app.data_dir.join("check-source.txt"), &src).is_err() {
                json!({ "error": "The data folder is not writable." })
            } else {
                match jobs::start_guide_check(&app, "@form") {
                    e if e.is_empty() => json!({ "ok": true }),
                    e => json!({ "error": e }),
                }
            };
            return json(&ctx, StatusCode::OK, res);
        }
        "save_guide" => {
            let input = guide_input(&f);
            let is_new = id == 0;
            match (input, &g) {
                (Err(errs), _) => {
                    for e in errs {
                        app.flash(&ctx, "error", e);
                    }
                    return redirect(&ctx, &if is_new { "guides.php?add=1".to_string() } else { format!("guides.php?edit={id}") });
                }
                (Ok(_), None) if !is_new => app.flash(&ctx, "error", "That guide no longer exists."),
                (Ok(i), cur) => {
                    if guides.iter().any(|o| o.name.eq_ignore_ascii_case(&i.name) && (is_new || o.id != id)) {
                        app.flash(&ctx, "error", format!("There is already a guide called \"{}\". Pick another name.", i.name));
                        return redirect(&ctx, &if is_new { "guides.php?add=1".to_string() } else { format!("guides.php?edit={id}") });
                    }
                    if is_new {
                        let slug = unique_slug(&guides, &i.name, 0);
                        let order: i64 = conn.query_row("SELECT COALESCE(MAX(sort_order), 0) + 1 FROM guides", [], |r| r.get(0)).unwrap_or(1);
                        let r = conn.execute(
                            "INSERT INTO guides (name, slug, source_url, days, refresh_hours, logo_country, enabled, created_at, sort_order)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                            params![i.name, slug, i.source_url, i.days, i.hours, i.lc, i.enabled, timeutil::now_unix(), order],
                        );
                        match r {
                            Ok(_) => app.flash(&ctx, "ok", format!("Guide \"{}\" added. Press Build now to create it; its address is {}/guides/{slug}.xml", i.name, super::base_url(&headers))),
                            Err(e) => app.flash(&ctx, "error", format!("Could not save the guide: {e}")),
                        }
                    } else if let Some(cur) = cur {
                        let slug = if db::slugify(&i.name) == db::slugify(&cur.name) { cur.slug.clone() } else { unique_slug(&guides, &i.name, cur.id) };
                        let r = conn.execute(
                            "UPDATE guides SET name = ?1, slug = ?2, source_url = ?3, days = ?4, refresh_hours = ?5, logo_country = ?6, enabled = ?7 WHERE id = ?8",
                            params![i.name, slug, i.source_url, i.days, i.hours, i.lc, i.enabled, cur.id],
                        );
                        match r {
                            Ok(_) => {
                                if slug != cur.slug {
                                    app.flash(&ctx, "warn", format!("The address of \"{}\" changes to {}/guides/{slug}.xml with its next build. Update it in your media server; the old file is removed after that build.", i.name, super::base_url(&headers)));
                                }
                                app.flash(&ctx, "ok", format!("Guide \"{}\" saved. It applies from the next build.", i.name));
                            }
                            Err(e) => app.flash(&ctx, "error", format!("Could not save the guide: {e}")),
                        }
                    }
                }
            }
        }
        "toggle_guide" => {
            if let Some(g) = &g {
                let _ = conn.execute("UPDATE guides SET enabled = ?1 WHERE id = ?2", params![!g.enabled, g.id]);
                app.flash(&ctx, "ok", format!("Guide \"{}\" {}", g.name, if g.enabled { "turned off. Its file stays as it is." } else { "turned on." }));
            }
        }
        "delete_guide" => {
            if let Some(g) = &g {
                if guides.len() <= 1 {
                    app.flash(&ctx, "error", "This is the only guide, so it cannot be deleted. Turn it off instead.");
                } else {
                    for t in ["guide_programmes", "guide_channels", "runs", "tmdb_wanted", "missing_logos", "channel_logo_sources"] {
                        let _ = conn.execute(&format!("DELETE FROM {t} WHERE guide_id = ?1"), params![g.id]);
                    }
                    let _ = conn.execute("DELETE FROM guides WHERE id = ?1", params![g.id]);
                    let dir = app.output_dir.join("guides");
                    for f in [format!("{}.xml", g.slug), format!("{}.xml.gz", g.slug), format!("status-{}.json", g.id)] {
                        let _ = std::fs::remove_file(dir.join(f));
                    }
                    app.flash(&ctx, "ok", format!("Guide \"{}\" deleted, with its guide files.", g.name));
                }
            }
        }
        "build_guide" => {
            if let Some(g) = &g {
                match jobs::start_build(&app, Some(&g.slug), &g.name) {
                    e if e.is_empty() => app.flash(&ctx, "ok", format!("Building \"{}\". Follow it on the Activity page.", g.name)),
                    e => app.flash(&ctx, "error", e),
                }
            }
        }
        "test_guide" => {
            if let Some(g) = &g {
                match jobs::start_guide_check(&app, &g.slug) {
                    e if e.is_empty() => app.flash(&ctx, "ok", format!("Testing the source of \"{}\". Reload this page in a few seconds for the result.", g.name)),
                    e => app.flash(&ctx, "error", e),
                }
            }
        }
        _ => {}
    }
    redirect(&ctx, back)
}
