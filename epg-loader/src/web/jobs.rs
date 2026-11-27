//! Background work started from the pages: guide builds, connection tests and
//! the background TMDB lookups run as child processes of this program, with
//! their output in <data dir>/logs/<name>.log.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;

use super::{App, Shared};
use crate::timeutil;

#[derive(Clone, Debug)]
pub struct Job {
    pub name: String,
    pub label: String,
    pub started: i64,
    pub pid: u32,
    /// Exit code once finished.
    pub exit: Option<i32>,
    pub cancelled: bool,
}

#[derive(Default)]
pub struct Jobs {
    pub current: Mutex<Option<Job>>,
}

impl Jobs {
    pub fn running(&self) -> Option<Job> {
        self.current.lock().unwrap().clone().filter(|j| j.exit.is_none())
    }
    pub fn last(&self) -> Option<Job> {
        self.current.lock().unwrap().clone()
    }
}

pub fn logs_dir(app: &App) -> PathBuf {
    app.data_dir.join("logs")
}

/// Is `<data dir>/<name>` locked by a running build or lookup (in this or another process)?
pub fn lock_held(dir: &Path, name: &str) -> bool {
    match crate::acquire_named_lock(dir, name) {
        Ok(Some(_free)) => false,
        Ok(None) => true,
        Err(_) => false,
    }
}

pub fn build_running(app: &App) -> bool {
    lock_held(&app.data_dir, "build.lock")
}

pub fn enrich_running(app: &App) -> bool {
    lock_held(&app.data_dir, crate::enrich::LOCK_FILE)
}

fn program() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("epg-loader"))
}

/// Start `epg-loader <args>` in the background. Output goes to logs/<name>.log,
/// or to `stdout_to` (e.g. check.json) with errors in the log. Returns "" or an error.
pub fn start(app: &Shared, name: &str, label: &str, args: &[&str], stdout_to: Option<&Path>) -> String {
    if let Some(j) = app.jobs.running() {
        return format!("{} is still running. Wait for it to finish.", j.label);
    }
    let logs = logs_dir(app);
    if let Err(e) = std::fs::create_dir_all(&logs) {
        return format!("Could not create {}: {e}", logs.display());
    }
    let log_path = logs.join(format!("{name}.log"));
    let log = match std::fs::File::create(&log_path) {
        Ok(f) => f,
        Err(e) => return format!("Could not write {}: {e}", log_path.display()),
    };
    let out: Stdio = match stdout_to {
        Some(p) => match std::fs::File::create(p) {
            Ok(f) => f.into(),
            Err(e) => return format!("Could not write {}: {e}", p.display()),
        },
        None => match log.try_clone() {
            Ok(f) => f.into(),
            Err(e) => return format!("Could not use the log file: {e}"),
        },
    };
    // A Stop or Cancel left from an earlier run must not stop this one.
    let _ = std::fs::remove_file(app.data_dir.join(crate::CANCEL_FILE));
    let mut cmd = tokio::process::Command::new(program());
    cmd.arg("--data-dir")
        .arg(&app.data_dir)
        .arg("--output-dir")
        .arg(&app.output_dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(log);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return format!("Could not start the task: {e}"),
    };
    let pid = child.id().unwrap_or(0);
    *app.jobs.current.lock().unwrap() =
        Some(Job { name: name.to_string(), label: label.to_string(), started: timeutil::now_unix(), pid, exit: None, cancelled: false });
    let app2 = app.clone();
    tokio::spawn(async move {
        let code = child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1);
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&log_path) {
            use std::io::Write;
            let _ = write!(f, "\n[finished with exit code {code}]\n");
        }
        if let Some(j) = app2.jobs.current.lock().unwrap().as_mut() {
            if j.pid == pid {
                j.exit = Some(code);
            }
        }
        // Settings were saved while this ran: test them now.
        let pending = app2.data_dir.join("check.pending");
        if pending.exists() {
            let _ = std::fs::remove_file(&pending);
            let _ = start_check(&app2);
        }
    });
    String::new()
}

/// Stop the running task (and a scheduled build): ask nicely through the cancel
/// file, then end the process after a few seconds. Returns "" or a message.
pub async fn cancel(app: &App) -> String {
    let job = app.jobs.running();
    let building = build_running(app);
    if job.is_none() && !building {
        return "Nothing is running.".into();
    }
    let _ = std::fs::write(app.data_dir.join(crate::CANCEL_FILE), timeutil::now_unix().to_string());
    for _ in 0..16 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if app.jobs.running().is_none() && !build_running(app) {
            break;
        }
    }
    if let Some(j) = app.jobs.running() {
        kill(j.pid);
        if let Some(cur) = app.jobs.current.lock().unwrap().as_mut() {
            cur.cancelled = true;
        }
    }
    let _ = std::fs::remove_file(app.data_dir.join(crate::CANCEL_FILE));
    String::new()
}

/// End a process (SIGTERM on Linux).
pub fn kill(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill").arg("-TERM").arg(pid.to_string()).status();
    }
    #[cfg(not(unix))]
    {
        let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).status();
    }
}

/// The last `lines` lines of a log file (reads at most the last 64 KiB).
pub fn tail(path: &Path, lines: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(65_536)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let all: Vec<&str> = text.trim_end().lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// Start a guide build: one guide (slug) or every enabled guide.
pub fn start_build(app: &Shared, slug: Option<&str>, name: &str) -> String {
    if build_running(app) {
        return "A build is already running.".into();
    }
    match slug {
        Some(s) => start(app, "guide-build", &format!("Guide build: {name}"), &["--guide", s, "build"], None),
        None => start(app, "guide-build", "Guide build", &["build"], None),
    }
}

/// Test internet, the TMDB key and the first guide's source (writes check.json).
pub fn start_check(app: &Shared) -> String {
    let out = app.data_dir.join("check.json");
    let _ = std::fs::remove_file(&out);
    start(app, "check", "Test internet, TMDB key and source", &["check"], Some(&out))
}

/// Settings changed: drop the old test result and test again now, or once the
/// running task has finished.
pub fn queue_check(app: &Shared) {
    let _ = std::fs::remove_file(app.data_dir.join("check.json"));
    if !start_check(app).is_empty() {
        let _ = std::fs::write(app.data_dir.join("check.pending"), b"1");
    }
}

/// Test one guide's source, or the URL handed over in check-source.txt ("@form").
pub fn start_guide_check(app: &Shared, slug: &str) -> String {
    let out = guide_check_file(app, slug);
    let _ = std::fs::remove_file(&out);
    start(app, "check-guide", "Test guide source", &["--guide", slug, "check"], Some(&out))
}

pub fn guide_check_file(app: &App, slug: &str) -> PathBuf {
    let safe: String = slug.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    app.data_dir.join(format!("check-{}.json", if safe.is_empty() { "form" } else { &safe }))
}

/// The last source test for a guide: (ok, detail, unix time).
pub fn guide_check(app: &App, slug: &str) -> Option<(bool, String, i64)> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(guide_check_file(app, slug)).ok()?).ok()?;
    let s = v.get("source")?;
    Some((
        s.get("ok").and_then(|x| x.as_bool()).unwrap_or(false),
        s.get("detail").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        v.get("checked_at").and_then(|x| x.as_i64()).unwrap_or(0),
    ))
}

/// Start the background TMDB lookups (they take enrich.lock themselves).
pub fn start_enrich(app: &App) -> String {
    if enrich_running(app) {
        return "Background TMDB lookups are already running.".into();
    }
    let _ = std::fs::remove_file(app.data_dir.join(crate::enrich::CANCEL_FILE));
    let logs = logs_dir(app);
    let _ = std::fs::create_dir_all(&logs);
    let log = match std::fs::File::create(logs.join("enrich.log")) {
        Ok(f) => f,
        Err(e) => return format!("Could not write the log: {e}"),
    };
    let err = match log.try_clone() {
        Ok(f) => f,
        Err(e) => return format!("Could not use the log: {e}"),
    };
    let r = std::process::Command::new(program())
        .arg("--data-dir")
        .arg(&app.data_dir)
        .arg("--output-dir")
        .arg(&app.output_dir)
        .arg("enrich")
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err)
        .spawn();
    match r {
        Ok(mut child) => {
            // Reap it when it ends so no zombie is left behind.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            String::new()
        }
        Err(e) => format!("Could not start the lookups: {e}"),
    }
}

/// Ask the background lookups to stop (they save what they have).
pub async fn stop_enrich(app: &App) -> String {
    if !enrich_running(app) {
        return "Background TMDB lookups are not running.".into();
    }
    let _ = std::fs::write(app.data_dir.join(crate::enrich::CANCEL_FILE), timeutil::now_unix().to_string());
    for _ in 0..16 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if !enrich_running(app) {
            return String::new();
        }
    }
    "The lookups were asked to stop; they stop within a few seconds.".into()
}
