//! EPGArr (epg-loader): enrich XMLTV guides for media servers (Emby, Plex, Jellyfin or any XMLTV client).
//!
//!   epg-loader migrate - create or upgrade epg.db
//!   epg-loader build   - build once (the default when no command is given)
//!   epg-loader watch   - rebuild every refresh_hours; settings are reread every minute
//!   epg-loader enrich  - look up every pending TMDB title (no time budget), then rebuild
//!   epg-loader check   - test internet, the TMDB key and the source URL; prints JSON
//!   epg-loader info    - print version and expected schema version as JSON
//!
//! No paths are compiled in. The data dir comes from --data-dir or
//! EPG_LOADER_DIR, the output dir from --output-dir / EPG_OUTPUT_DIR or the
//! output_dir setting. Everything private lives in the data dir, outside the
//! web root: epg.db (settings incl. the TMDB key, channel overrides, TMDB
//! cache, run history), rules.toml, build.lock and cache/. Only the guides
//! (guides/<slug>.xml, .xml.gz and status-<id>.json) are written to the output dir.

mod checks;
mod config;
mod db;
mod enrich;
mod groups;
mod imdb;
mod logos;
mod output;
mod source;
mod sportsdb;
mod timeutil;
mod tmdb;
mod tvlogos;
mod tvmaze;
mod web;
mod xmltv;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::{ChannelsFile, Config, LogoSource};
use crate::groups::{GroupInput, Rules};
use crate::tmdb::{LookupKey, Tmdb, TmdbCache, TmdbInfo};
use crate::xmltv::ProgPlan;

/// Set when the build should stop: the cancel file appeared or SIGTERM arrived.
static CANCEL: AtomicBool = AtomicBool::new(false);
/// Set when SIGTERM arrived (watch mode then exits instead of waiting for the next build).
static TERMINATED: AtomicBool = AtomicBool::new(false);
/// The web pages create this file in the data dir to ask a running build to stop.
const CANCEL_FILE: &str = "cancel";

/// The build was cancelled. The guide files were not touched; cached TMDB results were saved.
#[derive(Debug)]
struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

fn is_cancelled() -> bool {
    CANCEL.load(Ordering::Relaxed)
}

fn check_cancel() -> Result<()> {
    if is_cancelled() {
        return Err(Cancelled.into());
    }
    Ok(())
}

/// Resolves once a cancel has been requested.
async fn cancel_requested() {
    while !is_cancelled() {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(unix)]
async fn sigterm() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut s) => {
            s.recv().await;
        }
        Err(_) => std::future::pending::<()>().await,
    }
}

#[cfg(not(unix))]
async fn sigterm() {
    std::future::pending::<()>().await
}

/// Runs during a build: sets CANCEL when the cancel file appears or on SIGTERM.
async fn watch_cancel(flag: PathBuf) {
    let file = async {
        while !flag.exists() {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    let why = tokio::select! {
        _ = file => "cancel requested from the web page",
        _ = sigterm() => {
            TERMINATED.store(true, Ordering::Relaxed);
            "SIGTERM received"
        }
    };
    CANCEL.store(true, Ordering::Relaxed);
    println!("Cancelling ({why}): stopping, saving the TMDB results so far, keeping the previous guide files");
}

const MAX_LISTED_ERRORS: usize = 50;
const MAX_TMDB_ERRORS: usize = 20;
const DESC_MAX_CHARS: usize = 300;
/// Save new TMDB results to epg.db in batches of this many.
const TMDB_SAVE_BATCH: usize = 50;
/// Placeholder titles that are never on TMDB (compared after normalize_words).
const FILLER_TITLES: &[&str] = &[
    "paid programming",
    "paid program",
    "paid programme",
    "to be announced",
    "tba",
    "tbd",
    "programming to be announced",
    "off air",
    "station off air",
    "sign off",
    "close",
    "closedown",
    "infomercial",
    "infomercials",
    "teleshopping",
    "shopping",
    "local programming",
    "network programming",
    "no information",
    "no information available",
    "no programme information",
    "programme information not available",
    "to be confirmed",
    "tbc",
];

fn is_filler(title: &str) -> bool {
    let n = groups::normalize_words(title);
    FILLER_TITLES.contains(&n.as_str()) || n.starts_with("paid programming") || n.starts_with("infomercial")
}

#[derive(Parser, Debug)]
#[command(name = "epg-loader", version, about = "EPGArr: enrich XMLTV guides with logos, posters, ratings and categories for your media server")]
struct Cli {
    /// Data dir holding epg.db, rules.toml, build.lock and cache/ (keep it outside the web root)
    #[arg(long, global = true, env = "EPG_LOADER_DIR")]
    data_dir: Option<PathBuf>,
    /// Folder for guides/ and logos/ (overrides the output_dir setting)
    #[arg(long, global = true, env = "EPG_OUTPUT_DIR")]
    output_dir: Option<PathBuf>,
    /// Run everything but write no guide files, and record no run
    #[arg(long, global = true)]
    dry_run: bool,
    /// Make no TMDB requests (results already in the cache are still used)
    #[arg(long, global = true)]
    no_tmdb: bool,
    /// build: do nothing unless refresh_hours have passed since the last run (for the schedule)
    #[arg(long, global = true)]
    if_due: bool,
    /// build / check: only this guide (its slug, e.g. main); default: every enabled guide
    #[arg(long, global = true)]
    guide: Option<String>,
    /// serve: address to listen on
    #[arg(long, global = true, env = "EPGARR_LISTEN", default_value = "0.0.0.0:80")]
    listen: String,
    /// serve: folder with the static files (app.css, guide.js, ...)
    #[arg(long, global = true, env = "EPGARR_ASSETS", default_value = "/app/assets")]
    assets: PathBuf,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand, Debug, Clone, Copy)]
enum Cmd {
    /// Create or upgrade epg.db and write the default rules.toml, then exit
    Migrate,
    /// Build the guides once (default): every enabled guide, or only --guide
    Build,
    /// Keep running and rebuild every refresh_hours
    Watch,
    /// Look up every pending TMDB title of all guides with no time budget, then rebuild them
    Enrich,
    /// Test internet access, the TMDB key and the source URL; prints JSON
    Check,
    /// Print the version and the database schema version this binary expects, as JSON
    Info,
    /// Run the EPGArr web pages and the scheduler (watch) in one process
    Serve,
}

#[derive(Debug, Clone, Copy)]
struct Opts {
    dry_run: bool,
    no_tmdb: bool,
}

/// Written to guides/status-<id>.json for the pages, and stored as the
/// run summary in epg.db. Never contains the TMDB key or the source URL.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Status {
    /// The guide this status is for (its slug).
    guide: String,
    ok: bool,
    running: bool,
    started_at: String,
    built_at: String,
    built_at_unix: i64,
    last_attempt: String,
    last_attempt_unix: i64,
    duration_secs: f64,
    channels: usize,
    programmes: usize,
    programmes_dropped: usize,
    logos_detected: usize,
    logos_inserted: usize,
    logos_missing: usize,
    /// Where the inserted logos came from (logos_detected were already in the feed).
    logos_from_overrides: usize,
    logos_from_tvlogos: usize,
    logos_from_iptv: usize,
    posters_added: usize,
    ratings_added: usize,
    descs_added: usize,
    categories_added: usize,
    tmdb_titles: usize,
    tmdb_cache_hits: usize,
    tmdb_lookups: usize,
    tmdb_requests: usize,
    /// Titles left for the next run because the TMDB time budget ran out.
    tmdb_deferred: usize,
    /// Unique titles TMDB has no match for (from the cache, after this run's lookups).
    tmdb_not_found: usize,
    /// Unique titles TMDB is not asked about (news, sports, filler).
    tmdb_skipped: usize,
    /// Categories added, per group (Movie, Series, News, Sports, Kids, Music, Documentary).
    categories_by_group: BTreeMap<String, usize>,
    /// IMDb star ratings added.
    imdb_ratings_added: usize,
    /// Posters that came from TVmaze (included in posters_added).
    posters_from_tvmaze: usize,
    tvmaze_lookups: usize,
    /// Series left for the background lookups because the TVmaze time budget ran out.
    tvmaze_deferred: usize,
    /// Posters from TheSportsDB (included in posters_added), and matches looked up there.
    posters_from_sportsdb: usize,
    sportsdb_lookups: usize,
    /// Size of the written guide files.
    xml_bytes: usize,
    gz_bytes: usize,
    errors_total: usize,
    errors: Vec<String>,
    warnings: Vec<String>,
    last_error: Option<String>,
}

#[derive(Default)]
struct Errors {
    list: Vec<String>,
    total: usize,
}

impl Errors {
    fn push(&mut self, msg: impl Into<String>) {
        let m = msg.into();
        eprintln!("error: {m}");
        self.total += 1;
        if self.list.len() < MAX_LISTED_ERRORS {
            self.list.push(m);
        }
    }
}

enum Outcome {
    Built,
    Busy,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let opts = Opts { dry_run: cli.dry_run, no_tmdb: cli.no_tmdb };
    let command = cli.command.unwrap_or(Cmd::Build);
    if let Cmd::Info = command {
        let info = serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "schema_version": db::expected_schema_version(),
        });
        println!("{info}");
        return ExitCode::SUCCESS;
    }
    let Some(data_dir) = cli.data_dir.clone() else {
        eprintln!("error: no data dir given: pass --data-dir DIR or set EPG_LOADER_DIR");
        return ExitCode::from(2);
    };
    let output_dir = cli.output_dir.clone();
    let conn = match open_data_dir(&data_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    match command {
        Cmd::Info => ExitCode::SUCCESS,
        Cmd::Check => {
            // The source to test: --guide, else the first guide.
            let guides = db::guides(&conn).unwrap_or_default();
            let src = match cli.guide.as_deref() {
                // "Test source" in the guide form: the URL typed there, handed over in a private file.
                Some("@form") => {
                    let f = data_dir.join("check-source.txt");
                    let s = std::fs::read_to_string(&f).ok();
                    std::fs::remove_file(&f).ok();
                    s
                }
                Some(slug) => guides.into_iter().find(|g| g.slug == slug).map(|g| g.source_url),
                None => guides.into_iter().next().map(|g| g.source_url),
            };
            let report = checks::run(&conn, &data_dir, output_dir.as_deref(), src).await;
            println!("{report}");
            ExitCode::SUCCESS
        }
        Cmd::Migrate => {
            match db::schema_version(&conn) {
                Ok(v) => println!("{} is at schema version {v}", data_dir.join(db::DB_FILE).display()),
                Err(e) => {
                    eprintln!("error: {e:#}");
                    return ExitCode::FAILURE;
                }
            }
            ExitCode::SUCCESS
        }
        Cmd::Build => {
            let cfg = match db::load_config(&conn, &data_dir, output_dir.as_deref()) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e:#}");
                    return ExitCode::FAILURE;
                }
            };
            if cli.if_due {
                // The schedule runs this every 15 minutes: resume background TMDB lookups if needed.
                enrich::maybe_spawn(&conn, &cfg, &data_dir, output_dir.as_deref());
            }
            match build_guides(&conn, &cfg, cli.guide.as_deref(), cli.if_due, opts).await {
                Ok(Outcome::Built) => ExitCode::SUCCESS,
                Err(e) if e.downcast_ref::<Cancelled>().is_some() => {
                    println!("Build cancelled; the previous guide was kept");
                    ExitCode::from(130)
                }
                Ok(Outcome::Busy) => {
                    eprintln!("another build is already running; not starting a second one");
                    ExitCode::from(3)
                }
                Err(e) => {
                    eprintln!("error: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        Cmd::Enrich => {
            let cfg = match db::load_config(&conn, &data_dir, output_dir.as_deref()) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e:#}");
                    return ExitCode::FAILURE;
                }
            };
            match enrich::run(&conn, &cfg, opts).await {
                Ok(Outcome::Built) => ExitCode::SUCCESS,
                Ok(Outcome::Busy) => {
                    eprintln!("background TMDB lookups are already running; not starting a second one");
                    ExitCode::from(3)
                }
                Err(e) if e.downcast_ref::<Cancelled>().is_some() => {
                    println!("Background TMDB lookups stopped; the results so far are saved");
                    ExitCode::from(130)
                }
                Err(e) => {
                    eprintln!("error: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        Cmd::Watch => watch(&conn, &data_dir, output_dir.as_deref(), opts).await,
        Cmd::Serve => {
            // Guides and custom logos go to the output dir (default: the data dir).
            let out = output_dir.clone().unwrap_or_else(|| data_dir.clone());
            // The scheduler runs on its own thread with its own database connection.
            let (d, o) = (data_dir.clone(), out.clone());
            std::thread::spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        eprintln!("error: could not start the scheduler: {e}");
                        return;
                    }
                };
                rt.block_on(async move {
                    // Give a task resumed by the pages (see web::serve) the first go.
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    match open_data_dir(&d) {
                        Ok(conn) => {
                            web::SCHEDULER_RUNNING.store(true, std::sync::atomic::Ordering::Relaxed);
                            let _ = watch(&conn, &d, Some(o.as_path()), opts).await;
                            web::SCHEDULER_RUNNING.store(false, std::sync::atomic::Ordering::Relaxed);
                        }
                        Err(e) => eprintln!("error: scheduler: {e:#}"),
                    }
                });
            });
            let app = std::sync::Arc::new(web::App {
                data_dir: data_dir.clone(),
                output_dir: out,
                assets: cli.assets.clone(),
                sessions: std::sync::Mutex::new(web::load_sessions(&data_dir)),
                jobs: web::jobs::Jobs::default(),
            });
            match web::serve(app, &cli.listen).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

/// True when refresh_hours (the guide's own, else the default) have passed since its last run
/// started, or when that run was cut short by a shutdown: it is then built again straight away.
fn is_due(conn: &Connection, guide_id: i64, cfg: &Config) -> bool {
    if db::last_run_interrupted(conn, guide_id) {
        return true;
    }
    let last = db::last_run_started(conn, guide_id).ok().flatten().unwrap_or(0);
    timeutil::now_unix() - last >= (cfg.refresh_hours * 3600.0) as i64
}

/// Open (and migrate) epg.db and make sure an
/// editable rules.toml exists.
fn open_data_dir(data_dir: &Path) -> Result<Connection> {
    let conn = db::open(data_dir)?;
    db::sync_slugs(&conn)?;
    let rules = data_dir.join("rules.toml");
    if !rules.exists() {
        if let Err(e) = std::fs::write(&rules, groups::DEFAULT_RULES) {
            eprintln!("warning: could not write {}: {e}", rules.display());
        }
    }
    Ok(conn)
}

async fn watch(conn: &Connection, data_dir: &Path, output_dir: Option<&Path>, opts: Opts) -> ExitCode {
    println!("epg-loader {} watching {}", env!("CARGO_PKG_VERSION"), data_dir.display());
    let mut waiting_reported = false;
    loop {
        match db::load_config(conn, data_dir, output_dir) {
            Err(e) => {
                if !waiting_reported {
                    eprintln!("waiting for settings: {e:#}");
                    waiting_reported = true;
                }
            }
            Ok(cfg) => {
                waiting_reported = false;
                enrich::maybe_spawn(conn, &cfg, data_dir, output_dir);
                match build_guides(conn, &cfg, None, true, opts).await {
                    Ok(Outcome::Built) => {}
                    Ok(Outcome::Busy) => println!("a build is already running; will check again shortly"),
                    Err(e) if e.downcast_ref::<Cancelled>().is_some() => {
                        println!("Build cancelled; the previous guide was kept")
                    }
                    Err(e) => eprintln!("error: build failed: {e:#}"),
                }
            }
        }
        if TERMINATED.load(Ordering::Relaxed) {
            return ExitCode::SUCCESS;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            _ = sigterm() => return ExitCode::SUCCESS,
        }
    }
}

/// One guide to build: its id and where its files go. Every guide writes
/// guides/<slug>.xml and .xml.gz, named after the guide, and
/// guides/status-<id>.json (keyed by id, so a rename keeps it).
struct Target {
    id: i64,
    slug: String,
    xml: PathBuf,
    gz: PathBuf,
    status: PathBuf,
    /// Files from before a rename, removed after the next successful build.
    old_slug: Option<String>,
    old_files: Vec<PathBuf>,
}

impl Target {
    fn new(g: &db::Guide, output_dir: &Path) -> Target {
        let dir = output_dir.join("guides");
        let old_slug = g.output_slug.clone().filter(|o| !o.is_empty() && *o != g.slug);
        let old_files = match old_slug.as_deref() {
            None => Vec::new(),
            Some(o) => vec![dir.join(format!("{o}.xml")), dir.join(format!("{o}.xml.gz"))],
        };
        Target {
            id: g.id,
            slug: g.slug.clone(),
            xml: dir.join(format!("{}.xml", g.slug)),
            gz: dir.join(format!("{}.xml.gz", g.slug)),
            status: dir.join(format!("status-{}.json", g.id)),
            old_slug,
            old_files,
        }
    }

    /// After a successful build: remove this guide's files from before a rename,
    /// unless another guide writes files with that name now, and remember the new name.
    fn retire_old_files(&self, conn: &Connection) {
        if let Some(old) = self.old_slug.as_deref() {
            let shared = db::slug_in_use(conn, old, self.id).unwrap_or(true);
            if !shared {
                for f in &self.old_files {
                    match std::fs::remove_file(f) {
                        Ok(()) => println!("Removed the old guide file {}", f.display()),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => eprintln!("warning: could not remove {}: {e}", f.display()),
                    }
                }
            }
        }
        if let Err(e) = db::set_output_slug(conn, self.id, &self.slug) {
            eprintln!("warning: could not record the guide's file name: {e:#}");
        }
    }
}

/// The shared settings with this guide's own source, days, refresh interval and logo country.
fn guide_config(cfg: &Config, g: &db::Guide) -> Config {
    let mut c = cfg.clone();
    c.source_url = g.source_url.trim().to_string();
    c.days = g.days.filter(|&d| d > 0);
    if let Some(h) = g.refresh_hours.filter(|h| h.is_finite() && *h > 0.0) {
        c.refresh_hours = h;
    }
    if let Some(lc) = g.logo_country.as_deref().map(str::trim).filter(|s| s.len() == 2) {
        c.logo_country = lc.to_ascii_lowercase();
    }
    c
}

/// Build one guide (--guide), or every enabled guide in order (with
/// `if_due`, only those whose refresh interval has passed). A guide that
/// fails does not stop the others; a cancel stops them all.
async fn build_guides(conn: &Connection, cfg: &Config, only: Option<&str>, if_due: bool, opts: Opts) -> Result<Outcome> {
    let all = db::guides(conn)?;
    let list: Vec<&db::Guide> = match only {
        Some(slug) => match all.iter().find(|g| g.slug == slug) {
            Some(g) => vec![g],
            None => bail!("there is no guide called {slug:?}"),
        },
        None => all.iter().filter(|g| g.enabled).collect(),
    };
    let mut failed: Vec<String> = Vec::new();
    for g in list {
        let gcfg = guide_config(cfg, g);
        if if_due && !is_due(conn, g.id, &gcfg) {
            continue;
        }
        if if_due && db::last_run_interrupted(conn, g.id) {
            println!("Guide \"{}\": its last build was cut short (EPGArr was stopped); building it again now", g.name);
        }
        if gcfg.source_url.is_empty() {
            eprintln!("error: guide \"{}\" has no XMLTV source URL yet (set it on the Guides page); skipped", g.name);
            failed.push(g.name.clone());
            continue;
        }
        println!("=== {} - Guide \"{}\" ({}) ===", timeutil::format_utc(timeutil::now_unix()), g.name, g.slug);
        match build_once(conn, &gcfg, &Target::new(g, &cfg.output_dir), opts).await {
            Ok(Outcome::Built) => {}
            Ok(Outcome::Busy) => return Ok(Outcome::Busy),
            Err(e) if e.downcast_ref::<Cancelled>().is_some() => return Err(e),
            Err(e) => {
                eprintln!("error: guide \"{}\" failed: {e:#}", g.name);
                failed.push(g.name.clone());
            }
        }
    }
    if !failed.is_empty() {
        bail!("{} guide(s) failed: {}", failed.len(), failed.join(", "));
    }
    Ok(Outcome::Built)
}

/// Take the lock, record the run, build one guide, and record the result.
async fn build_once(conn: &Connection, cfg: &Config, target: &Target, opts: Opts) -> Result<Outcome> {
    let Some(_lock) = acquire_lock(&cfg.base_dir)? else {
        return Ok(Outcome::Busy);
    };
    let started = Instant::now();
    let now = timeutil::now_unix();
    // A cancel file left from an earlier run must not stop this one.
    let flag = cfg.base_dir.join(CANCEL_FILE);
    std::fs::remove_file(&flag).ok();
    CANCEL.store(false, Ordering::Relaxed);
    let run_id = if opts.dry_run {
        None
    } else {
        update_status(&target.status, |s| {
            s.running = true;
            s.started_at = timeutil::format_utc(now);
        })
        .ok();
        Some(db::start_run(conn, target.id, now)?)
    };
    let watcher = tokio::spawn(watch_cancel(flag.clone()));
    let result = run(conn, cfg, target, opts, started).await;
    watcher.abort();
    std::fs::remove_file(&flag).ok();
    let Some(run_id) = run_id else {
        return result.map(|_| Outcome::Built);
    };
    let end = timeutil::now_unix();
    match result {
        Ok(status) => {
            let summary = serde_json::to_string(&status)?;
            db::finish_run(
                conn,
                run_id,
                end,
                &db::RunResult {
                    ok: true,
                    duration_secs: status.duration_secs,
                    channels: status.channels,
                    programmes: status.programmes,
                    logos_missing: status.logos_missing,
                    posters_added: status.posters_added,
                    ratings_added: status.ratings_added,
                    errors_total: status.errors_total,
                    error: None,
                    summary_json: &summary,
                },
            )?;
            Ok(Outcome::Built)
        }
        Err(e) => {
            let cancelled = e.downcast_ref::<Cancelled>().is_some();
            // Stopped by a shutdown, not by the user: the next start builds it again.
            let msg = if cancelled && TERMINATED.load(Ordering::Relaxed) { "interrupted".to_string() } else { format!("{e:#}") };
            let res = update_status(&target.status, |s| {
                s.guide = target.slug.clone();
                // A cancelled build leaves the previous guide in place, so that is still fine.
                if !cancelled {
                    s.ok = false;
                }
                s.running = false;
                s.last_error = Some(if cancelled {
                    "cancelled; the previous guide was kept".to_string()
                } else {
                    msg.clone()
                });
                s.last_attempt = timeutil::format_utc(now);
                s.last_attempt_unix = now;
            });
            if let Err(e2) = res {
                eprintln!("error: could not write the status file: {e2:#}");
            }
            let finished = db::finish_run(
                conn,
                run_id,
                end,
                &db::RunResult {
                    ok: false,
                    duration_secs: started.elapsed().as_secs_f64(),
                    channels: 0,
                    programmes: 0,
                    logos_missing: 0,
                    posters_added: 0,
                    ratings_added: 0,
                    errors_total: 1,
                    error: Some(msg.as_str()),
                    summary_json: "{}",
                },
            );
            if let Err(e2) = finished {
                eprintln!("error: could not record the run: {e2:#}");
            }
            Err(e)
        }
    }
}

/// An exclusive advisory lock on <dir>/build.lock, released automatically
/// when the process exits. The pages check the same file.
fn acquire_lock(dir: &Path) -> Result<Option<File>> {
    acquire_named_lock(dir, "build.lock")
}

/// An exclusive advisory lock on <dir>/<name> holding our PID (build.lock, enrich.lock).
fn acquire_named_lock(dir: &Path, name: &str) -> Result<Option<File>> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(name);
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    match f.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(e)) => return Err(e).with_context(|| format!("locking {}", path.display())),
    }
    f.set_len(0).ok();
    write!(f, "{}", std::process::id()).ok();
    Ok(Some(f))
}

fn read_status(path: &Path) -> Status {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn update_status(path: &Path, f: impl FnOnce(&mut Status)) -> Result<()> {
    let mut s = read_status(path);
    f(&mut s);
    write_status(path, &s)
}

fn write_status(path: &Path, s: &Status) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    output::write_atomic(path, &serde_json::to_vec_pretty(s)?)
}

fn override_group<'a>(o: &'a ChannelsFile, channel: &str) -> Option<&'a str> {
    o.channels
        .get(channel)
        .and_then(|c| c.group.as_deref())
        .map(str::trim)
        .filter(|g| !g.is_empty())
}

fn override_logo(o: &ChannelsFile, channel: &str) -> Option<String> {
    o.channels
        .get(channel)
        .and_then(|c| c.logo.as_deref())
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
}

/// The TMDB lookup key for a programme, or None when TMDB has nothing to add.
/// Shared by the build and by `enrich`, so both agree on what is pending.
fn tmdb_key_for(p: &xmltv::ProgInfo, overrides: &ChannelsFile, rules: &Rules, manual: &std::collections::HashSet<String>) -> Option<LookupKey> {
    let title = p.title.as_deref()?;
    // A title you identified by hand is always used, even when it looks like news or sports.
    if manual.contains(&groups::normalize_words(title)) {
        return tmdb::make_key(title, p.date.as_deref(), p.is_tv);
    }
    let og = override_group(overrides, &p.channel);
    // News and sports are rarely on TMDB and mostly produce false matches.
    let pre = groups::choose_group(
        &GroupInput { override_group: og, categories: &p.categories, tmdb: None, title, is_tv: p.is_tv },
        rules,
    );
    // Filler such as "Paid Programming" is never on TMDB either.
    if matches!(pre.as_deref(), Some("News") | Some("Sports")) || is_filler(title) {
        return None;
    }
    // A programme that already has a poster and a rating needs nothing from TMDB.
    if p.has_icon && p.has_rating {
        return None;
    }
    tmdb::make_key(title, p.date.as_deref(), p.is_tv)
}

/// Programmes that ended longer ago than this are left out of the guide tables.
const GUIDE_KEEP_PAST_SECS: i64 = 86_400;
/// Length used when a programme has no usable stop time and nothing follows it.
const GUIDE_DEFAULT_SECS: i64 = 1_800;

/// Store the enriched channels and programmes for the TV Guide page. Called only
/// after the guide files were written, so a cancelled or failed build keeps the old ones.
fn store_guide(
    conn: &Connection,
    guide_id: i64,
    scan: &xmltv::Scan,
    channel_logo: &HashMap<String, String>,
    overrides: &ChannelsFile,
    plans: &[ProgPlan],
    prog_groups: &[Option<String>],
    now: i64,
) -> Result<(usize, usize)> {
    let mut pos: HashMap<&str, usize> = HashMap::new();
    let mut chans: Vec<db::GuideChannel> = Vec::new();
    for ch in &scan.channels {
        if ch.id.is_empty() || pos.contains_key(ch.id.as_str()) {
            continue;
        }
        pos.insert(ch.id.as_str(), chans.len());
        // channel_logo holds logos for channels without one, and custom logos that replace the feed's.
        let logo = channel_logo.get(&ch.id).map(String::as_str).or(ch.icon.as_deref());
        let name = ch.names.first().map(String::as_str).unwrap_or(ch.id.as_str());
        chans.push(db::GuideChannel { id: &ch.id, name, logo, group: None });
    }

    let mut progs: Vec<db::GuideProgramme> = Vec::new();
    for ((p, plan), group) in scan.programmes.iter().zip(plans).zip(prog_groups) {
        if plan.drop {
            continue;
        }
        let (Some(&channel), Some(start)) = (pos.get(p.channel.as_str()), timeutil::parse_xmltv_time(&p.start)) else {
            continue;
        };
        // 0 = unknown for now; filled in below from the next programme.
        let stop = timeutil::parse_xmltv_time(&p.stop).filter(|&s| s > start).unwrap_or(0);
        progs.push(db::GuideProgramme {
            channel,
            start,
            stop,
            title: p.title.as_deref().unwrap_or(""),
            sub_title: p.sub_title.as_deref(),
            episode: p.episode.as_deref(),
            desc: p.desc.as_deref().or(plan.desc.as_deref()),
            icon: p.icon.as_deref().or(plan.icon.as_deref()),
            rating: p.rating.as_deref().or(plan.rating.as_ref().map(|(_, v)| v.as_str())),
            categories: &p.categories,
            extra_category: plan.category.as_deref(),
            group: group.as_deref(),
            star: plan.star.as_ref().map(|(_, v)| v.as_str()),
        });
    }
    progs.sort_by_key(|p| (p.channel, p.start));
    for i in 0..progs.len() {
        if progs[i].stop == 0 {
            let next = progs
                .get(i + 1)
                .filter(|n| n.channel == progs[i].channel && n.start > progs[i].start)
                .map(|n| n.start);
            progs[i].stop = next.unwrap_or(progs[i].start + GUIDE_DEFAULT_SECS);
        }
    }
    progs.retain(|p| p.stop > now - GUIDE_KEEP_PAST_SECS);

    // Channel group: the override, else the group most of its programmes have.
    let mut counts: Vec<HashMap<&str, usize>> = vec![HashMap::new(); chans.len()];
    for p in &progs {
        if let Some(g) = p.group {
            *counts[p.channel].entry(g).or_insert(0) += 1;
        }
    }
    for (c, n) in chans.iter_mut().zip(&counts) {
        c.group = override_group(overrides, c.id).map(groups::canonical_group).or_else(|| {
            n.iter()
                .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
                .map(|(g, _)| g.to_string())
        });
    }
    db::replace_guide(conn, guide_id, &chans, &progs)?;
    Ok((chans.len(), progs.len()))
}

async fn run(conn: &Connection, cfg: &Config, target: &Target, opts: Opts, started: Instant) -> Result<Status> {
    let mut errors = Errors::default();
    let mut warnings: Vec<String> = Vec::new();
    let client = reqwest::Client::builder()
        .user_agent(concat!("epg-loader/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .build()
        .context("building HTTP client")?;

    // 1. Source
    println!("Fetching the guide source...");
    let raw = tokio::select! {
        r = source::fetch(&client, &cfg.source_url, &cfg.base_dir) => r?,
        _ = cancel_requested() => return Err(Cancelled.into()),
    };
    let was_gzip = source::is_gzip(&raw);
    let xml = source::decode(raw)?;
    println!("Source: {} bytes{}", xml.len(), if was_gzip { " after gunzip" } else { "" });
    let scan = xmltv::scan(&xml)?;
    println!("Parsed {} channels and {} programmes", scan.channels.len(), scan.programmes.len());
    if scan.channels.is_empty() && scan.programmes.is_empty() {
        bail!("the source has no <channel> or <programme> elements; is it really XMLTV?");
    }

    check_cancel()?;
    let overrides = db::channel_overrides(conn)?;
    let rules = Rules::load(&cfg.rules_file)?;

    // 2. Channel logos
    let logo_index = if cfg.logo_source == LogoSource::IptvOrg {
        match logos::LogoIndex::load(&client, &cfg.iptv_logos_url, &cfg.iptv_channels_url, &cfg.cache_dir).await {
            Ok(i) => Some(i),
            Err(e) => {
                errors.push(format!("iptv-org logo lists unavailable: {e:#}"));
                None
            }
        }
    } else {
        None
    };
    let mut channel_logo: HashMap<String, String> = HashMap::new();
    let mut missing: Vec<(String, String)> = Vec::new();
    let tv_logos = if cfg.logo_source == LogoSource::IptvOrg {
        match tvlogos::TvLogos::load(conn, &client, &cfg.tv_logos_api_url, &cfg.tv_logos_raw_url).await {
            Ok(t) => {
                println!("tv-logos: {} logos indexed", t.len());
                Some(t)
            }
            Err(e) => {
                errors.push(format!("tv-logos list unavailable: {e:#}"));
                None
            }
        }
    } else {
        None
    };
    check_cancel()?;
    // Priority: the feed's own logo (never replaced), an override, tv-logos, then iptv-org.
    let (mut logos_detected, mut logos_inserted) = (0usize, 0usize);
    let (mut logos_from_overrides, mut logos_from_tvlogos, mut logos_from_iptv) = (0usize, 0usize, 0usize);
    let mut logo_sources: Vec<(String, &'static str)> = Vec::with_capacity(scan.channels.len());
    for ch in &scan.channels {
        // A logo set from the TV Guide page (stored under logos/custom/) wins even over the feed's own.
        if let Some(url) = override_logo(&overrides, &ch.id).filter(|u| u.contains("/logos/custom/")) {
            channel_logo.insert(ch.id.clone(), url);
            logo_sources.push((ch.id.clone(), "custom"));
            logos_from_overrides += 1;
            logos_inserted += 1;
            continue;
        }
        if ch.has_icon {
            logos_detected += 1;
            logo_sources.push((ch.id.clone(), "feed"));
            continue;
        }
        let found = override_logo(&overrides, &ch.id)
            .map(|u| (u, "override"))
            .or_else(|| {
                tv_logos
                    .as_ref()
                    .and_then(|t| t.lookup(&ch.id, &ch.names, &cfg.logo_country))
                    .map(|u| (u, "tv-logos"))
            })
            .or_else(|| logo_index.as_ref().and_then(|i| i.lookup(&ch.id, &ch.names)).map(|u| (u, "iptv-org")));
        match found {
            Some((url, src)) => {
                match src {
                    "override" => logos_from_overrides += 1,
                    "tv-logos" => logos_from_tvlogos += 1,
                    _ => logos_from_iptv += 1,
                }
                channel_logo.insert(ch.id.clone(), url);
                logo_sources.push((ch.id.clone(), src));
                logos_inserted += 1;
            }
            None => missing.push((ch.id.clone(), ch.names.first().cloned().unwrap_or_default())),
        }
    }
    println!(
        "Logos: {logos_detected} already present, {logos_inserted} inserted ({logos_from_overrides} overrides, \
         {logos_from_tvlogos} tv-logos, {logos_from_iptv} iptv-org), {} still missing",
        missing.len()
    );

    // 3. Which programmes need TMDB?
    let now = timeutil::now_unix();
    let horizon = cfg.days.map(|d| now + i64::from(d) * 86_400);
    let mut dropped = 0usize;
    let mut plans: Vec<ProgPlan> = Vec::with_capacity(scan.programmes.len());
    let mut prog_keys: Vec<Option<String>> = Vec::with_capacity(scan.programmes.len());
    // TheSportsDB event query for sports matches without a poster ("Arsenal_vs_Chelsea").
    let mut prog_sports: Vec<Option<String>> = Vec::with_capacity(scan.programmes.len());
    let mut wanted: HashMap<String, LookupKey> = HashMap::new();
    // Earliest start per lookup key, so the soonest-airing titles go first.
    let mut first_start: HashMap<String, i64> = HashMap::new();
    let mut skipped_titles: HashSet<String> = HashSet::new();
    let manual_titles = tmdb::manual_titles(conn);
    for p in &scan.programmes {
        let mut plan = ProgPlan::default();
        let start_at = timeutil::parse_xmltv_time(&p.start);
        if let (Some(h), Some(start)) = (horizon, start_at) {
            if start > h {
                plan.drop = true;
                dropped += 1;
            }
        }
        let mut key = None;
        if !plan.drop {
            if let Some(k) = tmdb_key_for(p, &overrides, &rules, &manual_titles) {
                key = Some(k.key.clone());
                let s = start_at.unwrap_or(i64::MAX);
                first_start.entry(k.key.clone()).and_modify(|e| *e = (*e).min(s)).or_insert(s);
                wanted.entry(k.key.clone()).or_insert(k);
            } else if let Some(t) = p.title.as_deref() {
                if !(p.has_icon && p.has_rating) {
                    skipped_titles.insert(groups::normalize_words(t));
                }
            }
        }
        let mut sq = None;
        if cfg.sportsdb && !plan.drop && key.is_none() && !p.has_icon {
            if let (Some(t), Some(q)) = (p.title.as_deref(), p.title.as_deref().and_then(sportsdb::event_query)) {
                let g = groups::choose_group(
                    &GroupInput {
                        override_group: override_group(&overrides, &p.channel),
                        categories: &p.categories,
                        tmdb: None,
                        title: t,
                        is_tv: p.is_tv,
                    },
                    &rules,
                );
                if g.as_deref() == Some("Sports") {
                    sq = Some(q);
                }
            }
        }
        prog_sports.push(sq);
        plans.push(plan);
        prog_keys.push(key);
    }

    // The background lookups work from this list instead of reading the source again.
    if !opts.dry_run {
        if let Err(e) = db::set_wanted(conn, target.id, &wanted, &first_start) {
            warnings.push(format!("saving the titles for the background lookups: {e:#}"));
        }
    }

    // 4. TMDB lookups (cached in epg.db)
    let mut cache = TmdbCache::load(conn, &cfg.language, &cfg.rating_country)?;
    let online = if opts.no_tmdb {
        println!("--no-tmdb: using cached TMDB results only");
        false
    } else if cfg.tmdb_api_key.is_empty() {
        let w = "No TMDB key set: skipping TMDB lookups (cached results are still used)".to_string();
        eprintln!("warning: {w}");
        warnings.push(w);
        false
    } else {
        true
    };
    let now_u = now.max(0) as u64;
    let mut cache_hits = 0usize;
    let mut to_fetch: Vec<LookupKey> = Vec::new();
    for k in wanted.values() {
        if cache.get_fresh(&k.key, now_u).is_some() {
            cache_hits += 1;
        } else if online {
            to_fetch.push(k.clone());
        }
    }
    // Soonest-airing first, then by key so the order is stable.
    to_fetch.sort_by(|a, b| {
        let sa = first_start.get(&a.key).copied().unwrap_or(i64::MAX);
        let sb = first_start.get(&b.key).copied().unwrap_or(i64::MAX);
        sa.cmp(&sb).then_with(|| a.key.cmp(&b.key))
    });
    let tmdb_lookups = to_fetch.len();
    let mut tmdb_requests = 0usize;
    let mut tmdb_deferred = 0usize;
    if online {
        println!("TMDB: {} unique titles, {cache_hits} cached, {tmdb_lookups} to look up", wanted.len());
        if !to_fetch.is_empty() {
            let budget = cfg.tmdb_time_budget;
            println!(
                "TMDB: {} at a time, about 35 requests/s, time budget {}",
                cfg.tmdb_concurrency,
                if budget == 0 { "none".to_string() } else { format!("{budget}s") }
            );
            let deadline = (budget > 0).then(|| Instant::now() + Duration::from_secs(budget));
            let api = Arc::new(Tmdb::new(
                client.clone(),
                &cfg.tmdb_api_key,
                &cfg.language,
                &cfg.rating_country,
                cfg.tmdb_concurrency,
            ));
            let mut failed = 0usize;
            let mut save_errors = 0usize;
            let outcome = Arc::clone(&api)
                .lookup_stream(to_fetch, deadline, &CANCEL, |k, r| match r {
                    Ok(info) => {
                        cache.insert(k, info);
                        if cache.pending_len() >= TMDB_SAVE_BATCH {
                            if let Err(e) = cache.save(conn) {
                                save_errors += 1;
                                if save_errors == 1 {
                                    errors.push(format!("saving the TMDB cache: {e:#}"));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        failed += 1;
                        if failed <= MAX_TMDB_ERRORS {
                            errors.push(format!("TMDB lookup for \"{}\": {e:#}", k.query));
                        }
                    }
                })
                .await;
            if failed > MAX_TMDB_ERRORS {
                errors.push(format!("{} more TMDB lookups failed", failed - MAX_TMDB_ERRORS));
            }
            tmdb_requests = api.request_count();
            tmdb_deferred = outcome.deferred;
            if let Err(e) = cache.save(conn) {
                errors.push(format!("saving the TMDB cache: {e:#}"));
            }
            println!(
                "TMDB: done: {} looked up, {failed} failed, {tmdb_requests} requests",
                tmdb_lookups - tmdb_deferred
            );
            if tmdb_deferred > 0 {
                let why = if deadline.is_some_and(|d| Instant::now() >= d) {
                    "TMDB time budget reached"
                } else {
                    "TMDB lookups stopped early"
                };
                let w = format!(
                    "{why}: {tmdb_deferred} titles left for the next run (writing the guide with what is cached)"
                );
                println!("{w}");
                warnings.push(w);
            }
        }
    }

    let tmdb_not_found = wanted.keys().filter(|k| cache.get(k).is_some_and(|i| !i.found)).count();

    // IMDb star ratings for the titles TMDB matched, by their IMDb id.
    let mut imdb_map: HashMap<String, (f64, i64)> = HashMap::new();
    if cfg.imdb_ratings {
        if !opts.no_tmdb {
            if let Err(e) = imdb::refresh(conn, &client, &cfg.imdb_ratings_url).await {
                warnings.push(format!("IMDb ratings not updated (the stored ones are used): {e:#}"));
            }
        }
        let ids = wanted.keys().filter_map(|k| cache.get(k)).filter(|i| i.found).filter_map(|i| i.imdb_id.as_deref());
        match imdb::ratings_for(conn, ids) {
            Ok(m) => imdb_map = m,
            Err(e) => warnings.push(format!("reading the IMDb ratings: {e:#}")),
        }
    }

    // TVmaze: posters and descriptions for series TMDB has no match (or no poster) for.
    let mut tvm = tvmaze::Cache::load(conn)?;
    let (mut tvmaze_lookups, mut tvmaze_deferred) = (0usize, 0usize);
    if cfg.tvmaze && !opts.no_tmdb {
        let mut todo = tvmaze::candidates(wanted.values(), &cache, &tvm);
        todo.sort_by_key(|k| first_start.get(&k.key).copied().unwrap_or(i64::MAX));
        if !todo.is_empty() {
            let total = todo.len();
            println!("TVmaze: {total} series to look up (up to {}s now; the background lookups do the rest)", cfg.tvmaze_time_budget);
            let deadline = Instant::now() + Duration::from_secs(cfg.tvmaze_time_budget);
            let (n, f) = tvmaze::lookup_all(&client, &cfg.tvmaze_url, &mut tvm, conn, todo, Some(deadline), &CANCEL).await;
            println!("TVmaze: {n} looked up, {f} found");
            tvmaze_lookups = n;
            tvmaze_deferred = total.saturating_sub(n);
        }
    }
    check_cancel()?;

    // TheSportsDB: posters for sports matches (TMDB skips sports).
    let mut sdb = sportsdb::Cache::load(conn)?;
    let mut sportsdb_lookups = 0usize;
    if cfg.sportsdb && !opts.no_tmdb {
        let now_i = timeutil::now_unix();
        let mut seen: HashSet<String> = HashSet::new();
        let todo: Vec<String> = prog_sports
            .iter()
            .flatten()
            .filter(|q| !sdb.is_fresh(q, now_i))
            .filter(|q| seen.insert((*q).clone()))
            .cloned()
            .collect();
        if !todo.is_empty() {
            println!("TheSportsDB: {} matches to look up (up to {}s)", todo.len(), cfg.sportsdb_time_budget);
            let url = format!("{}/{}/searchevents.php", cfg.sportsdb_url.trim_end_matches('/'), cfg.sportsdb_key.trim());
            let deadline = Instant::now() + Duration::from_secs(cfg.sportsdb_time_budget);
            let (n, f) = sportsdb::lookup_all(&client, &url, &mut sdb, conn, todo, Some(deadline), &CANCEL).await;
            println!("TheSportsDB: {n} looked up, {f} found");
            sportsdb_lookups = n;
        }
    }
    // The TMDB results are saved above, so stopping here loses nothing.
    check_cancel()?;

    // 5. Plan the additions
    let (mut posters, mut ratings, mut descs, mut cats_added) = (0usize, 0usize, 0usize, 0usize);
    let mut cats_by_group: BTreeMap<String, usize> = BTreeMap::new();
    let mut imdb_added = 0usize;
    let mut posters_tvmaze = 0usize;
    let mut posters_sportsdb = 0usize;
    // The group of each programme, for colour coding on the guide page.
    let mut prog_groups: Vec<Option<String>> = vec![None; scan.programmes.len()];
    for ((((p, plan), key), pg), sq) in scan
        .programmes
        .iter()
        .zip(plans.iter_mut())
        .zip(prog_keys.iter())
        .zip(prog_groups.iter_mut())
        .zip(prog_sports.iter())
    {
        if plan.drop {
            continue;
        }
        let info: Option<&TmdbInfo> = key.as_deref().and_then(|k| cache.get(k)).filter(|i| i.found);
        if let Some(info) = info {
            if !p.has_star {
                if let Some(&(r, votes)) = info.imdb_id.as_deref().and_then(|id| imdb_map.get(id)) {
                    if votes >= i64::from(cfg.imdb_min_votes) {
                        plan.star = Some(("IMDb".to_string(), imdb::star_value(r)));
                        imdb_added += 1;
                    }
                }
            }
            if !p.has_icon {
                if let Some(u) = info.poster_url() {
                    plan.icon = Some(u);
                    posters += 1;
                }
            }
            if !p.has_rating {
                if let Some(c) = &info.certification {
                    plan.rating = Some((info.rating_system().to_string(), c.clone()));
                    ratings += 1;
                }
            }
            if !p.has_desc {
                if let Some(o) = info.overview.as_deref() {
                    let d = tmdb::short_desc(o, DESC_MAX_CHARS);
                    if !d.is_empty() {
                        plan.desc = Some(d);
                        descs += 1;
                    }
                }
            }
        }
        // TVmaze fills in series TMDB had nothing (or no poster) for.
        let show = key
            .as_deref()
            .and_then(|k| wanted.get(k))
            .filter(|k| k.is_tv)
            .and_then(|k| tvm.get(&k.norm))
            .filter(|s| s.found);
        if let Some(s) = show {
            if !p.has_icon && plan.icon.is_none() {
                if let Some(u) = &s.poster {
                    plan.icon = Some(u.clone());
                    posters += 1;
                    posters_tvmaze += 1;
                }
            }
            if !p.has_desc && plan.desc.is_none() {
                if let Some(d) = s.summary.as_deref().map(|d| tmdb::short_desc(d, DESC_MAX_CHARS)).filter(|d| !d.is_empty()) {
                    plan.desc = Some(d);
                    descs += 1;
                }
            }
        }
        // TheSportsDB poster for a sports match.
        if let Some(u) = sq.as_deref().and_then(|q| sdb.poster(q)) {
            if !p.has_icon && plan.icon.is_none() {
                plan.icon = Some(u.to_string());
                posters += 1;
                posters_sportsdb += 1;
            }
        }
        let group = groups::choose_group(
            &GroupInput {
                override_group: override_group(&overrides, &p.channel),
                categories: &p.categories,
                tmdb: info,
                title: p.title.as_deref().unwrap_or(""),
                is_tv: p.is_tv,
            },
            &rules,
        );
        if let Some(g) = &group {
            if groups::needs_category(g, &p.categories) {
                plan.category = Some(g.clone());
                cats_added += 1;
                *cats_by_group.entry(g.clone()).or_insert(0) += 1;
            }
        }
        *pg = group;
    }

    // 6. Write
    println!(
        "Added {posters} posters, {ratings} ratings, {imdb_added} IMDb ratings, {descs} descriptions, {cats_added} categories; dropped {dropped} programmes"
    );
    let built = timeutil::format_utc(now);
    check_cancel()?;
    let xml_bytes: usize;
    let gz_bytes: usize;
    if opts.dry_run {
        xml_bytes = xmltv::rewrite_into(&xml, &channel_logo, &plans, output::Counter::default())?.bytes;
        gz_bytes = 0;
    } else {
        if let Some(dir) = target.xml.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        // Straight to disk, gzipped on the way: a guide can be several GB, so it is never held in memory.
        let w = output::GuideWriter::open(&target.xml, &target.gz)?;
        let w = xmltv::rewrite_into(&xml, &channel_logo, &plans, w)?;
        drop(xml);
        let (xb, gb) = w.finish()?;
        xml_bytes = xb;
        gz_bytes = gb;
        if let Err(e) = db::set_missing_logos(conn, target.id, &missing) {
            errors.push(format!("saving the missing logo list: {e:#}"));
        }
        if let Err(e) = db::set_logo_sources(conn, target.id, &logo_sources) {
            errors.push(format!("saving where each logo came from: {e:#}"));
        }
        match store_guide(conn, target.id, &scan, &channel_logo, &overrides, &plans, &prog_groups, now) {
            Ok((c, n)) => println!("Guide page tables: {c} channels, {n} programmes"),
            Err(e) => errors.push(format!("saving the guide page tables (the previous ones were kept): {e:#}")),
        }
    }

    let status = Status {
        guide: target.slug.clone(),
        ok: true,
        running: false,
        started_at: built.clone(),
        built_at: built.clone(),
        built_at_unix: now,
        last_attempt: built,
        last_attempt_unix: now,
        duration_secs: (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
        channels: scan.channels.len(),
        programmes: scan.programmes.len() - dropped,
        programmes_dropped: dropped,
        logos_detected,
        logos_inserted,
        logos_missing: missing.len(),
        logos_from_overrides,
        logos_from_tvlogos,
        logos_from_iptv,
        posters_added: posters,
        ratings_added: ratings,
        descs_added: descs,
        categories_added: cats_added,
        tmdb_titles: wanted.len(),
        tmdb_cache_hits: cache_hits,
        tmdb_lookups,
        tmdb_requests,
        tmdb_deferred,
        tmdb_not_found,
        tmdb_skipped: skipped_titles.len(),
        categories_by_group: cats_by_group,
        imdb_ratings_added: imdb_added,
        posters_from_tvmaze: posters_tvmaze,
        tvmaze_lookups,
        tvmaze_deferred,
        posters_from_sportsdb: posters_sportsdb,
        sportsdb_lookups,
        xml_bytes,
        gz_bytes,
        errors_total: errors.total,
        errors: errors.list,
        warnings,
        last_error: None,
    };
    if opts.dry_run {
        println!("Dry run: {xml_bytes} bytes of XML produced, nothing written");
    } else {
        write_status(&target.status, &status)?;
        target.retire_old_files(conn);
        println!(
            "Wrote {} ({} bytes) and its .gz in {:.1}s",
            target.xml.display(),
            xml_bytes,
            status.duration_secs
        );
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_never_mentions_secrets() {
        let s = Status::default();
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("tmdb_api_key") && !json.contains("source_url"));
    }

    #[test]
    fn lock_is_exclusive() {
        let dir = std::env::temp_dir().join(format!("epg-loader-lock-{}", std::process::id()));
        let first = acquire_lock(&dir).unwrap();
        assert!(first.is_some());
        assert!(acquire_lock(&dir).unwrap().is_none(), "a second build must not start");
        drop(first);
        assert!(acquire_lock(&dir).unwrap().is_some());
        std::fs::remove_dir_all(&dir).ok();
    }
}
