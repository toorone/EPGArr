//! `epg-loader serve`: the EPGArr web pages, served by the program itself
//! (no PHP, no nginx). The same process also runs the scheduler (the
//! `watch` loop), so one container runs one program.
//!
//! The URLs are the same as the old PHP pages (index.php, guides.php,
//! guide_api.php, ...) so bookmarks and the page scripts keep working.

pub mod activity;
pub mod auth;
pub mod common;
pub mod dash;
pub mod guides;
pub mod html;
pub mod jobs;
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

pub struct Session {
    pub user: Option<String>,
    /// Fingerprint of the user's password hash: a changed password or removed user ends the session.
    pub fp: String,
    pub csrf: String,
    pub flashes: Vec<(String, String)>,
    pub seen: i64,
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
                sessions.insert(id.clone(), Session { user: None, fp: String::new(), csrf: random_token(), flashes: Vec::new(), seen: now });
                (id, true)
            }
        };
        let s = sessions.get_mut(&sid).unwrap();
        s.seen = now;
        // The user must still exist, be enabled and have the same password as at login.
        let user = s.user.clone().filter(|u| {
            auth::find_user(&self.data_dir, u).is_some_and(|x| !x.disabled && auth::fingerprint(&x.hash) == s.fp)
        });
        if user.is_none() {
            s.user = None;
        }
        Ctx { sid, new_session, user, csrf: s.csrf.clone(), flashes: std::mem::take(&mut s.flashes) }
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
    }

    pub fn logout(&self, ctx: &Ctx) {
        self.sessions.lock().unwrap().remove(&ctx.sid);
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

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/", get(dash::page_get))
        .route("/index.php", get(dash::page_get))
        .route("/login", post(auth::login_post))
        .route("/logout", post(auth::logout_post))
        .route("/guides.php", get(guides::page_get).post(guides::page_post))
        .route("/activity.php", get(activity::page_get).post(activity::page_post))
        .route("/guide.php", get(tv::guide_page))
        .route("/guide_api.php", get(tv::guide_api))
        .route("/assets/{*rel}", get(assets))
        .route("/guides/{*rel}", get(guide_files))
        .route("/logos/{*rel}", get(logo_files))
        .with_state(app)
}

/// Run the web server on `addr` until the process ends.
pub async fn serve(app: Shared, addr: &str) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("EPGArr pages on http://{addr}");
    axum::serve(listener, router(app)).await?;
    Ok(())
}
