//! HTML helpers: escaping, icons and the page layout (sidebar, flashes).

use super::Ctx;

pub const APP_NAME: &str = "EPGArr";

/// Escape text for HTML (also safe inside attribute values).
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            _ => o.push(c),
        }
    }
    o
}

/// JSON for use inside a <script> block (no "</script>" can end it early).
pub fn json_script(v: &serde_json::Value) -> String {
    v.to_string().replace('<', "\\u003c").replace('>', "\\u003e").replace('&', "\\u0026")
}

pub fn icon(name: &str) -> String {
    let d = match name {
        "logo" => r#"<circle cx="12" cy="12" r="2"/><path d="M16.24 7.76a6 6 0 0 1 0 8.48M7.76 16.24a6 6 0 0 1 0-8.48M19.07 4.93a10 10 0 0 1 0 14.14M4.93 19.07a10 10 0 0 1 0-14.14"/>"#,
        "dashboard" => r#"<rect x="3" y="3" width="7" height="9" rx="1.5"/><rect x="14" y="3" width="7" height="5" rx="1.5"/><rect x="14" y="12" width="7" height="9" rx="1.5"/><rect x="3" y="16" width="7" height="5" rx="1.5"/>"#,
        "settings" => r#"<path d="M4 6h9M17 6h3M4 12h3M11 12h9M4 18h11M19 18h1"/><circle cx="15" cy="6" r="2"/><circle cx="9" cy="12" r="2"/><circle cx="17" cy="18" r="2"/>"#,
        "file" => r#"<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5M12 11v6M9 14l3 3 3-3"/>"#,
        "history" => r#"<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5M12 7v5l3 2"/>"#,
        "copy" => r#"<rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1"/>"#,
        "play" => r#"<path d="M7 4v16l13-8z"/>"#,
        "eye" => r#"<path d="M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12z"/><circle cx="12" cy="12" r="3"/>"#,
        "edit" => r#"<path d="M4 20h4L19 9l-4-4L4 16v4z"/><path d="m13.5 6.5 4 4"/>"#,
        "tv" => r#"<rect x="3" y="5" width="18" height="12" rx="2"/><path d="M8 21h8M12 17v4"/>"#,
        "clock" => r#"<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>"#,
        "list" => r#"<path d="M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01"/>"#,
        "pulse" => r#"<path d="M3 12h4l3-8 4 16 3-8h4"/>"#,
        "grid" => r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M8 9v11M14 9v11M8 14.5h13"/>"#,
        "search" => r#"<circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/>"#,
        "x" => r#"<path d="M18 6 6 18M6 6l12 12"/>"#,
        "user" => r#"<circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/>"#,
        _ => "",
    };
    format!(
        r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">{d}</svg>"#
    )
}

/// The app icon as a data: URL for the browser tab.
pub fn favicon_data_url() -> &'static str {
    "data:image/svg+xml,%3Csvg%20xmlns%3D%22http%3A%2F%2Fwww.w3.org%2F2000%2Fsvg%22%20viewBox%3D%220%200%2032%2032%22%3E%3Crect%20width%3D%2232%22%20height%3D%2232%22%20rx%3D%228%22%20fill%3D%22%233b7bd6%22%2F%3E%3C%2Fsvg%3E"
}

pub fn head(title: &str) -> String {
    format!(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{APP_NAME} · {t}</title><link rel="icon" type="image/svg+xml" href="assets/favicon.svg"><link rel="icon" href="{fav}"><link rel="apple-touch-icon" href="assets/favicon.svg"><link rel="stylesheet" href="assets/app.css?v={v}"></head>"#,
        t = esc(title),
        fav = favicon_data_url(),
        v = env!("CARGO_PKG_VERSION"),
    )
}

/// The hidden CSRF field for a form.
pub fn csrf_field(ctx: &Ctx) -> String {
    format!(r#"<input type="hidden" name="csrf" value="{}">"#, esc(&ctx.csrf))
}

pub fn flashes(list: &[(String, String)]) -> String {
    list.iter()
        .map(|(t, m)| format!(r#"<div class="flash {}">{}</div>"#, esc(t), esc(m)))
        .collect()
}

/// A full page: head, sidebar, optional toolbar (page actions), flashes and the body.
pub fn page(ctx: &Ctx, active: &str, title: &str, tools: &str, body: &str) -> String {
    let item = |key: &str, href: &str, label: &str, ic: &str| {
        let on = key == active;
        format!(
            r#"<a class="item{cls}" href="{href}"{cur} title="{label}">{icon}<span class="label">{label}</span></a>"#,
            cls = if on { " active" } else { "" },
            cur = if on { r#" aria-current="page""# } else { "" },
            icon = icon(ic),
        )
    };
    let user = esc(ctx.user.as_deref().unwrap_or(""));
    let nav_auth = if ctx.user.is_some() {
        format!(
            r#"<form method="post" action="logout" class="nav-auth">{csrf}<button type="submit" class="item" title="Log out {user}" aria-label="Log out {user}">{icon}<span class="label"><span class="nav-user">{user}</span><small>Log out</small></span></button></form>"#,
            csrf = csrf_field(ctx),
            icon = icon("user"),
        )
    } else {
        String::new()
    };
    let top = if tools.is_empty() {
        r#"<div class="page-top"></div>"#.to_string()
    } else {
        format!(r#"<div class="page-head page-tools">{tools}</div>"#)
    };
    format!(
        r#"{head}<body><div class="layout"><nav class="nav" aria-label="Main"><div class="brand"><span class="brand-mark">{logo}</span><span>{APP_NAME}</span></div>{a}{b}{c}{d}{e}<div class="nav-spacer"></div>{nav_auth}</nav><div class="content">{top}<main class="page-{active}{wide}">{fl}{body}</main></div></div>{busy}</body></html>"#,
        head = head(title),
        logo = icon("logo"),
        a = item("dashboard", "./", "Dashboard", "dashboard"),
        b = item("guide", "tv", "TV Guide", "grid"),
        c = item("activity", "activity", "Activity", "pulse").replacen("<a class=\"item", "<a id=\"nav-activity\" class=\"item", 1),
        d = item("settings", "settings", "Settings", "settings"),
        e = item("guides", "guides", "Guides", "list"),
        wide = if active == "guide" { " wide" } else { "" },
        fl = flashes(&ctx.flashes),
        busy = if ctx.user.is_some() { BUSY_SCRIPT } else { "" },
    )
}

/// Lights up the Activity menu item while a build, test or background lookup runs.
const BUSY_SCRIPT: &str = r#"<script>(function(){var a=document.getElementById('nav-activity');if(!a)return;var t;function check(){fetch('api/busy',{credentials:'same-origin',cache:'no-store'}).then(function(r){return r.json();}).then(function(j){a.classList.toggle('running',!!j.busy);a.title=j.busy?('Activity: '+j.what+' running'):'Activity';}).catch(function(){}).then(function(){clearTimeout(t);t=setTimeout(check,document.hidden?30000:4000);});}check();document.addEventListener('visibilitychange',function(){if(!document.hidden)check();});})();</script>"#;
