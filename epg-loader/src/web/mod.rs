//! `epg-loader serve`: the EPGArr web pages, served by the program itself.
//! The same process also runs the scheduler (the
//! `watch` loop), so one container runs one program.
//!
//! Pages: /, /guides, /tv, /activity, /settings; data for the page scripts:
//! /api/guide, /api/logo.

pub mod activity;
pub mod auth;
pub mod common;
pub mod dash;
pub mod guides;
pub mod html;
pub mod jobs;
pub mod logfile;
pub mod logo;
pub mod presets;
pub mod settings;
pub mod tmdbapi;
pub mod tv;

use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::timeutil;

const SESSION_COOKIE: &str = "epgarr_session";
/// Sessions end after this long without a request.
const SESSION_IDLE_SECS: i64 = 14 * 86_400;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub user: Option<String>,
    /// Fingerprint of the user's password hash: a changed password or removed user ends the session.
    pub fp: String,
    pub csrf: String,
    #[serde(skip)]
    pub flashes: Vec<(String, String)>,
    /// Form values to show again after a failed save.
    #[serde(skip)]
    pub old: HashMap<String, String>,
    pub seen: i64,
}

const SESSIONS_FILE: &str = "sessions.json";

/// Logged-in sessions saved by an earlier run, so a restart or update does not log everyone out.
pub fn load_sessions(data_dir: &Path) -> HashMap<String, Session> {
    let now = timeutil::now_unix();
    std::fs::read(data_dir.join(SESSIONS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<HashMap<String, Session>>(&b).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, s)| s.user.is_some() && now - s.seen < SESSION_IDLE_SECS)
        .collect()
}

pub struct App {
    pub data_dir: PathBuf,
    /// Where the program writes guides (guides/<slug>.xml) and custom logos (logos/custom).
    pub output_dir: PathBuf,
    /// Static files (app.css, guide.js, favicon.svg).
    pub assets: PathBuf,
    pub sessions: Mutex<HashMap<String, Session>>,
    pub jobs: jobs::Jobs,
}

pub type Shared = Arc<App>;

/// What every page needs about the visitor.
pub struct Ctx {
    /// Admins can change everything; other users (viewers) only see the TV Guide.
    pub admin: bool,
    pub sid: String,
    pub new_session: bool,
    pub user: Option<String>,
    pub csrf: String,
    /// Flash messages to show on this page (taken from the session).
    pub flashes: Vec<(String, String)>,
}

impl App {
    pub fn db(&self) -> anyhow::Result<Connection> {
        crate::db::open(&self.data_dir)
    }

    /// The visitor's session (created when missing). Takes the pending flash messages.
    pub fn ctx(&self, headers: &HeaderMap) -> Ctx {
        let now = timeutil::now_unix();
        let cookie = cookie_value(headers, SESSION_COOKIE);
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, s| now - s.seen < SESSION_IDLE_SECS);
        let (sid, new_session) = match cookie {
            Some(c) if sessions.contains_key(&c) => (c, false),
            _ => {
                let id = random_token();
                sessions.insert(id.clone(), Session { user: None, fp: String::new(), csrf: random_token(), flashes: Vec::new(), old: HashMap::new(), seen: now });
                (id, true)
            }
        };
        let s = sessions.get_mut(&sid).unwrap();
        s.seen = now;
        // The user must still exist, be enabled and have the same password as at login.
        let found = s.user.as_deref().and_then(|u| auth::find_user(&self.data_dir, u)).filter(|x| !x.disabled && auth::fingerprint(&x.hash) == s.fp);
        let admin = found.as_ref().is_some_and(|x| x.role == "admin");
        let user = found.map(|x| x.username);
        if user.is_none() {
            s.user = None;
        }
        Ctx { admin, sid, new_session, user, csrf: s.csrf.clone(), flashes: std::mem::take(&mut s.flashes) }
    }

    pub fn flash(&self, ctx: &Ctx, kind: &str, msg: impl Into<String>) {
        if let Some(s) = self.sessions.lock().unwrap().get_mut(&ctx.sid) {
            s.flashes.push((kind.to_string(), msg.into()));
        }
    }

    pub fn login(&self, ctx: &Ctx, user: &auth::User) {
        let mut sessions = self.sessions.lock().unwrap();
        // A new session id on login.
        if let Some(mut s) = sessions.remove(&ctx.sid) {
            s.user = Some(user.username.clone());
            s.fp = auth::fingerprint(&user.hash);
            s.csrf = random_token();
            sessions.insert(ctx.sid.clone(), s);
        }
        drop(sessions);
        self.save_sessions();
    }

    pub fn set_old(&self, ctx: &Ctx, old: HashMap<String, String>) {
        if let Some(s) = self.sessions.lock().unwrap().get_mut(&ctx.sid) {
            s.old = old;
        }
    }

    pub fn take_old(&self, ctx: &Ctx) -> HashMap<String, String> {
        self.sessions.lock().unwrap().get_mut(&ctx.sid).map(|s| std::mem::take(&mut s.old)).unwrap_or_default()
    }

    pub fn logout(&self, ctx: &Ctx) {
        self.sessions.lock().unwrap().remove(&ctx.sid);
        self.save_sessions();
    }

    /// Save the logged-in sessions (not the anonymous ones) to the data folder.
    pub fn save_sessions(&self) {
        let json = {
            let sessions = self.sessions.lock().unwrap();
            let logged_in: HashMap<&String, &Session> = sessions.iter().filter(|(_, s)| s.user.is_some()).collect();
            serde_json::to_vec(&logged_in).unwrap_or_default()
        };
        let path = self.data_dir.join(SESSIONS_FILE);
        if crate::output::write_atomic(&path, &json).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
}

/// 32 random bytes as hex, from the kernel.
pub fn random_token() -> String {
    let mut b = [0u8; 32];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut b);
    } else {
        // Not on Linux (tests on another OS): still unpredictable enough for a dev run.
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        for (i, x) in b.iter_mut().enumerate() {
            *x = ((t >> (i % 16 * 8)) as u8) ^ (i as u8).wrapping_mul(131);
        }
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

/// Add the session cookie when the session is new.
pub fn with_cookie(ctx: &Ctx, mut r: Response) -> Response {
    if ctx.new_session {
        let c = format!("{SESSION_COOKIE}={}; Path=/; HttpOnly; SameSite=Lax", ctx.sid);
        if let Ok(v) = HeaderValue::from_str(&c) {
            r.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    r.headers_mut().insert("X-Content-Type-Options", HeaderValue::from_static("nosniff"));
    r.headers_mut().insert("X-Frame-Options", HeaderValue::from_static("SAMEORIGIN"));
    r.headers_mut().insert("Referrer-Policy", HeaderValue::from_static("same-origin"));
    r
}

pub fn html_page(ctx: &Ctx, body: String) -> Response {
    let mut r = (StatusCode::OK, body).into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    with_cookie(ctx, r)
}

pub fn redirect(ctx: &Ctx, to: &str) -> Response {
    let mut r = StatusCode::SEE_OTHER.into_response();
    if let Ok(v) = HeaderValue::from_str(to) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    with_cookie(ctx, r)
}

pub fn json(ctx: &Ctx, status: StatusCode, v: serde_json::Value) -> Response {
    let mut r = (status, v.to_string()).into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    with_cookie(ctx, r)
}

/// The site address as the visitor sees it ("http://host:8080"), for guide URLs.
pub fn base_url(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .filter(|h| h.chars().all(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(c)))
        .unwrap_or("localhost");
    let https = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|p| p.eq_ignore_ascii_case("https"));
    format!("{}://{host}", if https { "https" } else { "http" })
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "xml" => "application/xml",
        "gz" => "application/gzip",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

/// Serve one file from `dir` (no "..", no hidden files). Large guide files are streamed.
async fn send_file(dir: &Path, rel: &str, cache: &'static str) -> Response {
    if rel.is_empty() || rel.split('/').any(|p| p.is_empty() || p.starts_with('.')) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = dir.join(rel);
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let mut r = Response::new(Body::from_stream(tokio_util::io::ReaderStream::new(file)));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type(&path)));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if let Ok(v) = HeaderValue::from_str(&len.to_string()) {
        r.headers_mut().insert(header::CONTENT_LENGTH, v);
    }
    r
}

async fn assets(State(app): State<Shared>, UrlPath(rel): UrlPath<String>) -> Response {
    send_file(&app.assets, &rel, "public, max-age=3600").await
}

/// Guide files: media servers fetch these directly, no login.
async fn guide_files(State(app): State<Shared>, UrlPath(rel): UrlPath<String>) -> Response {
    send_file(&app.output_dir.join("guides"), &rel, "no-cache").await
}

async fn logo_files(State(app): State<Shared>, UrlPath(rel): UrlPath<String>) -> Response {
    send_file(&app.output_dir.join("logos"), &rel, "public, max-age=3600").await
}

/// Is the scheduler (the watch loop started by `serve`) running?
pub static SCHEDULER_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The newest version on GitHub (main), checked every 6 hours when the update check is on.
pub static LATEST_VERSION: Mutex<Option<String>> = Mutex::new(None);

const LATEST_URL: &str = "https://raw.githubusercontent.com/toorone/EPGArr/main/epg-loader/Cargo.toml";

/// "0.3.10" -> [0, 3, 10]
fn version_parts(v: &str) -> Vec<u64> {
    v.trim().trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// The version on GitHub when it is newer than this program, else None.
pub fn newer_version() -> Option<String> {
    let latest = LATEST_VERSION.lock().unwrap().clone()?;
    (version_parts(&latest) > version_parts(env!("CARGO_PKG_VERSION"))).then_some(latest)
}

/// Ask GitHub for the version of the program on main.
async fn check_latest(app: &Shared) {
    let on = common::settings(app).get("update_check").map_or(true, |v| v != "0");
    if !on {
        *LATEST_VERSION.lock().unwrap() = None;
        return;
    }
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent(concat!("EPGArr/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    let Ok(r) = client.get(LATEST_URL).send().await else { return };
    let Ok(r) = r.error_for_status() else { return };
    let Ok(text) = r.text().await else { return };
    let v = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("version").map(|r| r.trim_start_matches([' ', '=']).trim_matches('"').to_string()));
    if let Some(v) = v.filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.')) {
        *LATEST_VERSION.lock().unwrap() = Some(v);
    }
}

/// Wait for SIGTERM (docker stop) or Ctrl+C, then give running work a few seconds
/// to save what it has. Interrupted work is picked up again at the next start.
async fn shutdown(app: Shared) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => return std::future::pending().await,
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    jobs::SHUTTING_DOWN.store(true, std::sync::atomic::Ordering::Relaxed);
    println!("Stopping: saving the work in progress; it carries on at the next start");
    app.save_sessions();
    for _ in 0..16 {
        let busy = app.jobs.running().is_some() || SCHEDULER_RUNNING.load(std::sync::atomic::Ordering::Relaxed);
        if !busy {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// An uploaded file: (file name, bytes).
pub type Upload = (String, Vec<u8>);

/// Read a posted form, urlencoded or multipart. Files in a multipart form are
/// returned by field name (at most `max_file` bytes each; larger ones are dropped
/// and reported as an empty upload with the name "too-large").
pub async fn read_post(req: axum::extract::Request, max_file: usize) -> (guides::Posted, HashMap<String, Upload>) {
    use axum::extract::FromRequest;
    let multipart = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("multipart/form-data"));
    let mut fields = Vec::new();
    let mut files = HashMap::new();
    if multipart {
        if let Ok(mut mp) = axum::extract::Multipart::from_request(req, &()).await {
            while let Ok(Some(mut field)) = mp.next_field().await {
                let name = field.name().unwrap_or("").to_string();
                match field.file_name().map(String::from) {
                    Some(fname) => {
                        let mut buf = Vec::new();
                        let mut big = false;
                        while let Ok(Some(chunk)) = field.chunk().await {
                            if buf.len() + chunk.len() > max_file {
                                big = true;
                                break;
                            }
                            buf.extend_from_slice(&chunk);
                        }
                        if big {
                            files.insert(name, ("too-large".to_string(), Vec::new()));
                        } else if !buf.is_empty() || !fname.is_empty() {
                            files.insert(name, (fname, buf));
                        }
                    }
                    None => {
                        if let Ok(t) = field.text().await {
                            fields.push((name, t));
                        }
                    }
                }
            }
        }
    } else if let Ok(axum::extract::Form(f)) = axum::extract::Form::<Vec<(String, String)>>::from_request(req, &()).await {
        fields = f;
    }
    (guides::Posted(fields), files)
}

/// A viewer (not an admin) asked for an admin page: the TV Guide is theirs.
pub fn viewer_home(ctx: &Ctx) -> Response {
    redirect(ctx, "tv")
}

/// Is anything running? For the Activity menu item's light.
async fn busy(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let ctx = app.ctx(&headers);
    if ctx.user.is_none() || !ctx.admin {
        return json(&ctx, StatusCode::UNAUTHORIZED, serde_json::json!({ "busy": false }));
    }
    let mut what: Vec<String> = Vec::new();
    if let Some(j) = app.jobs.running() {
        what.push(j.label);
    } else if jobs::build_running(&app) {
        what.push("Guide build".into());
    }
    if jobs::enrich_running(&app) {
        what.push("background lookups".into());
    }
    json(&ctx, StatusCode::OK, serde_json::json!({ "busy": !what.is_empty(), "what": what.join(" and ") }))
}

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/", get(dash::page_get))
        .route("/login", post(auth::login_post))
        .route("/logout", post(auth::logout_post))
        .route("/guides", get(guides::page_get).post(guides::page_post))
        .route("/activity", get(activity::page_get).post(activity::page_post))
        .route("/settings", get(settings::page_get).post(settings::page_post).layer(axum::extract::DefaultBodyLimit::max(64 << 20)))
        .route("/api/logo", get(logo::info).post(logo::post).layer(axum::extract::DefaultBodyLimit::max(4 << 20)))
        .route("/tv", get(tv::guide_page))
        .route("/api/guide", get(tv::guide_api))
        .route("/api/busy", get(busy))
        .route("/api/presets", get(presets::get))
        .route("/api/tmdb", get(tmdbapi::get).post(tmdbapi::post))
        .route("/assets/{*rel}", get(assets))
        .route("/guides/{*rel}", get(guide_files))
        .route("/logos/{*rel}", get(logo_files))
        .with_state(app)
}

/// Run the web server on `addr` until the process ends.
pub async fn serve(app: Shared, addr: &str) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    // A task from the pages that was cut short by a shutdown starts again.
    jobs::resume(&app);
    // A cancel left over from before the shutdown must not stop anything now.
    let _ = std::fs::remove_file(app.data_dir.join(crate::CANCEL_FILE));
    // Look for a new version on GitHub now and every 6 hours.
    let a = app.clone();
    tokio::spawn(async move {
        loop {
            check_latest(&a).await;
            tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
        }
    });
    // A backup every day (and one at start-up when the last is a day old).
    let a = app.clone();
    tokio::spawn(async move {
        loop {
            settings::backup_daily(&a);
            a.save_sessions();
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    });
    println!("EPGArr pages on http://{addr}");
    let a = app.clone();
    axum::serve(listener, router(app)).with_graceful_shutdown(shutdown(a)).await?;
    Ok(())
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(version_parts("0.3.10") > version_parts("0.3.9"));
        assert!(version_parts("1.0.0") > version_parts("0.9.9"));
        assert!(version_parts("0.3.0") <= version_parts("0.3.0"));
    }
}
