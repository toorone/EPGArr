//! Users and logging in. Users live in <data dir>/users.json (the same file
//! and the same bcrypt hashes the PHP pages used, so accounts keep working).

use axum::extract::{Form, State};
use axum::http::HeaderMap;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use super::html::{csrf_field, esc, head, icon, APP_NAME};
use super::{html_page, redirect, Ctx, Shared};
use crate::timeutil;

/// At most this many failed logins per 15 minutes, per address and per username.
const MAX_FAILS: usize = 5;
const FAIL_WINDOW: i64 = 900;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub username: String,
    pub hash: String,
    #[serde(default = "admin_role")]
    pub role: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub last_login: i64,
}

fn admin_role() -> String {
    "admin".into()
}

pub fn users(data_dir: &Path) -> Vec<User> {
    if let Some(list) = std::fs::read(data_dir.join("users.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<User>>(&b).ok())
    {
        return list.into_iter().filter(|u| !u.username.is_empty() && !u.hash.is_empty()).collect();
    }
    // An install from before users existed had one admin password.
    let old = std::fs::read(data_dir.join("admin.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("hash").and_then(|h| h.as_str()).map(String::from));
    match old {
        Some(hash) if !hash.is_empty() => vec![User {
            username: "admin".into(),
            hash,
            role: "admin".into(),
            disabled: false,
            created: timeutil::now_unix(),
            last_login: 0,
        }],
        _ => Vec::new(),
    }
}

pub fn save_users(data_dir: &Path, users: &[User]) -> anyhow::Result<()> {
    let path = data_dir.join("users.json");
    crate::output::write_atomic(&path, &serde_json::to_vec_pretty(users)?)?;
    Ok(())
}

pub fn find_user(data_dir: &Path, name: &str) -> Option<User> {
    users(data_dir).into_iter().find(|u| u.username.eq_ignore_ascii_case(name))
}

/// A short fingerprint of a password hash, kept in the session.
pub fn fingerprint(hash: &str) -> String {
    // FNV-1a: enough to notice a changed hash; the hash itself never leaves the server.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in hash.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub fn check_username(name: &str) -> Option<&'static str> {
    let ok = (2..=32).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    (!ok).then_some("Usernames are 2 to 32 letters, digits, dots, dashes or underscores.")
}

pub fn check_password(pw: &str, repeat: &str) -> Option<&'static str> {
    if pw.len() < 8 {
        return Some("Use at least 8 characters for the password.");
    }
    if pw != repeat {
        return Some("The two passwords do not match.");
    }
    None
}

pub fn hash_password(pw: &str) -> anyhow::Result<String> {
    Ok(bcrypt::hash(pw, 10)?)
}

/// PHP wrote "$2y$" bcrypt hashes; the bcrypt crate reads those too.
pub fn verify(pw: &str, hash: &str) -> bool {
    bcrypt::verify(pw, hash).unwrap_or(false)
}

// ---------------------------------------------------------------- failed logins

fn attempts_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("login-attempts.json")
}

fn attempts(data_dir: &Path) -> HashMap<String, Vec<i64>> {
    let mut a: HashMap<String, Vec<i64>> = std::fs::read(attempts_path(data_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let cut = timeutil::now_unix() - FAIL_WINDOW;
    for v in a.values_mut() {
        v.retain(|t| *t > cut);
    }
    a.retain(|_, v| !v.is_empty());
    a
}

fn keys(ip: &str, user: &str) -> [String; 2] {
    [format!("ip:{ip}"), format!("user:{}", user.to_lowercase())]
}

fn blocked(data_dir: &Path, ip: &str, user: &str) -> bool {
    let a = attempts(data_dir);
    keys(ip, user).iter().any(|k| a.get(k).map_or(0, Vec::len) >= MAX_FAILS)
}

fn note_failure(data_dir: &Path, ip: &str, user: &str) {
    let mut a = attempts(data_dir);
    for k in keys(ip, user) {
        a.entry(k).or_default().push(timeutil::now_unix());
    }
    let _ = std::fs::write(attempts_path(data_dir), serde_json::to_vec(&a).unwrap_or_default());
}

fn note_success(data_dir: &Path, user: &str) {
    let mut a = attempts(data_dir);
    a.remove(&format!("user:{}", user.to_lowercase()));
    let _ = std::fs::write(attempts_path(data_dir), serde_json::to_vec(&a).unwrap_or_default());
    let mut list = users(data_dir);
    for u in list.iter_mut().filter(|u| u.username.eq_ignore_ascii_case(user)) {
        u.last_login = timeutil::now_unix();
    }
    let _ = save_users(data_dir, &list);
}

/// The client's address (X-Forwarded-For first, for a reverse proxy).
fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "direct".into())
}

// ---------------------------------------------------------------- pages

/// A local page to return to after login ("guide.php", "index.php#history"), or "".
pub fn safe_next(next: &str) -> String {
    let ok = !next.is_empty()
        && next.len() < 200
        && next.chars().all(|c| c.is_ascii_alphanumeric() || "._-?=&#".contains(c))
        && next.split(['?', '#']).next().is_some_and(|p| p.ends_with(".php") && !p.contains('/'));
    if ok { next.to_string() } else { String::new() }
}

/// The full-page login (or "create your account" when there are no users yet).
pub fn login_page(app: &Shared, ctx: &Ctx, next: &str) -> Response {
    let first = users(&app.data_dir).is_empty();
    let next = safe_next(next);
    let hidden = format!(
        r#"{csrf}<input type="hidden" name="next" value="{n}">"#,
        csrf = csrf_field(ctx),
        n = esc(&next)
    );
    let form = if first {
        format!(
            r#"<div class="card"><h2>Create your account</h2><p>This account protects every page. The first person to open this page creates it; more users can be added in Settings.</p><form method="post" action="login">{hidden}<input type="hidden" name="action" value="first_user"><label for="un">Username</label><input type="text" id="un" name="username" autocomplete="username" required autofocus maxlength="32"><label for="np">Password (at least 8 characters)</label><input type="password" id="np" name="new_password" minlength="8" autocomplete="new-password" required><label for="cp">Repeat it</label><input type="password" id="cp" name="confirm_password" autocomplete="new-password" required><button>Create account</button></form></div>"#
        )
    } else {
        format!(
            r#"<div class="card"><h2>Log in</h2><form method="post" action="login">{hidden}<input type="hidden" name="action" value="login"><label for="un">Username</label><input type="text" id="un" name="username" autocomplete="username" required autofocus maxlength="32"><label for="pw">Password</label><input type="password" id="pw" name="password" autocomplete="current-password" required><button>Log in</button></form></div>"#
        )
    };
    let flashes: String = ctx
        .flashes
        .iter()
        .map(|(t, m)| format!(r#"<div class="flash {}" role="alert">{}</div>"#, esc(t), esc(m)))
        .collect();
    let body = format!(
        r#"{head}<body class="auth-page"><main class="auth-wrap"><div class="auth-brand"><span class="brand-mark">{logo}</span><span>{APP_NAME}</span></div>{flashes}{form}</main></body></html>"#,
        head = head(if first { "Create your account" } else { "Log in" }),
        logo = icon("logo"),
    );
    html_page(ctx, body)
}

#[derive(Deserialize)]
pub struct LoginForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    action: String,
    #[serde(default)]
    next: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    new_password: String,
    #[serde(default)]
    confirm_password: String,
}

pub async fn login_post(State(app): State<Shared>, headers: HeaderMap, Form(f): Form<LoginForm>) -> Response {
    let ctx = app.ctx(&headers);
    let next = safe_next(&f.next);
    let back = if next.is_empty() { "index.php".to_string() } else { next.clone() };
    if f.csrf != ctx.csrf {
        app.flash(&ctx, "error", "The form expired. Please try again.");
        return redirect(&ctx, &back);
    }
    let name = f.username.trim();
    if f.action == "first_user" {
        if !users(&app.data_dir).is_empty() {
            return redirect(&ctx, &back);
        }
        if let Some(e) = check_username(name).or_else(|| check_password(&f.new_password, &f.confirm_password)) {
            app.flash(&ctx, "error", e);
            return redirect(&ctx, &back);
        }
        let now = timeutil::now_unix();
        let u = match hash_password(&f.new_password) {
            Ok(hash) => User { username: name.to_string(), hash, role: "admin".into(), disabled: false, created: now, last_login: now },
            Err(e) => {
                app.flash(&ctx, "error", format!("Could not create the account: {e}"));
                return redirect(&ctx, &back);
            }
        };
        if let Err(e) = save_users(&app.data_dir, std::slice::from_ref(&u)) {
            app.flash(&ctx, "error", format!("Could not save the account (is /data writable?): {e:#}"));
            return redirect(&ctx, &back);
        }
        app.login(&ctx, &u);
        app.flash(&ctx, "ok", format!("Welcome, {}. Your account is ready.", u.username));
        return redirect(&ctx, &back);
    }
    let ip = client_ip(&headers);
    if blocked(&app.data_dir, &ip, name) {
        app.flash(&ctx, "error", "Too many failed attempts. Wait 15 minutes and try again.");
        return redirect(&ctx, &back);
    }
    match find_user(&app.data_dir, name) {
        Some(u) if !u.disabled && verify(&f.password, &u.hash) => {
            app.login(&ctx, &u);
            note_success(&app.data_dir, &u.username);
        }
        _ => {
            note_failure(&app.data_dir, &ip, name);
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            app.flash(&ctx, "error", "Invalid username or password.");
        }
    }
    redirect(&ctx, &back)
}

#[derive(Deserialize)]
pub struct CsrfOnly {
    #[serde(default)]
    csrf: String,
}

pub async fn logout_post(State(app): State<Shared>, headers: HeaderMap, Form(f): Form<CsrfOnly>) -> Response {
    let ctx = app.ctx(&headers);
    if f.csrf == ctx.csrf {
        app.logout(&ctx);
    }
    redirect(&ctx, "index.php")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_must_be_a_local_page() {
        assert_eq!(safe_next("guide.php?guide=a"), "guide.php?guide=a");
        assert_eq!(safe_next("index.php#history"), "index.php#history");
        assert_eq!(safe_next("https://evil/x.php"), "");
        assert_eq!(safe_next("../x.php"), "");
        assert_eq!(safe_next("/etc/passwd"), "");
    }

    #[test]
    fn php_bcrypt_hashes_verify() {
        // password_hash("secret123", PASSWORD_DEFAULT) from PHP.
        let h = hash_password("secret123").unwrap().replacen("$2b$", "$2y$", 1);
        assert!(verify("secret123", &h));
        assert!(!verify("wrong", &h));
    }

    #[test]
    fn usernames_and_passwords() {
        assert!(check_username("rob").is_none());
        assert!(check_username("a").is_some());
        assert!(check_username("bad name").is_some());
        assert!(check_password("short", "short").is_some());
        assert!(check_password("longenough", "different").is_some());
        assert!(check_password("longenough", "longenough").is_none());
    }
}
