//! `epg-loader enrich`: look up every TMDB title the guide still needs, with
//! no time budget, so a large guide gets fully enriched without the guide
//! builds waiting for it. Results go into the same tmdb_cache table the build
//! uses. When new results were found, a normal build runs so they show up.
//!
//! Progress is written to <data dir>/enrich_status.json for the dashboard.
//! The schedule (`build --if-due`, every 15 minutes) and watch mode call
//! `maybe_spawn`, so the lookups resume on their own after a reboot or an
//! interruption until everything is done.

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::tmdb::{LookupKey, Tmdb, TmdbCache};
use crate::tvmaze;
use crate::{
    acquire_named_lock, build_guides, cancel_requested, check_cancel, db, output, timeutil, watch_cancel, Cancelled,
    Opts, Outcome, Target, CANCEL, TERMINATED, TMDB_SAVE_BATCH,
};

/// Held while enrich runs; the dashboard checks it with flock().
pub const LOCK_FILE: &str = "enrich.lock";
/// Progress for the dashboard, in the data dir (not the guide files folder).
pub const STATUS_FILE: &str = "enrich_status.json";
/// The dashboard's Stop button creates this file in the data dir.
pub const CANCEL_FILE: &str = "enrich-cancel";

const STATUS_EVERY: Duration = Duration::from_secs(5);
const MAX_LOGGED_ERRORS: usize = 20;
/// How many of the latest lookups the Activity page shows.
const RECENT_MAX: usize = 20;
/// Rounds of retrying failed lookups (network or TMDB errors) before leaving them for the next start.
const MAX_ROUNDS: u32 = 6;
const RETRY_FIRST: Duration = Duration::from_secs(30);
const RETRY_MAX: Duration = Duration::from_secs(900);
/// While a build is running, check this often (up to an hour) before rebuilding with the new results.
const BUILD_WAIT_STEP: Duration = Duration::from_secs(30);
const BUILD_WAIT_STEPS: u32 = 120;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EnrichStatus {
    pub running: bool,
    /// running, retrying, building, done, stopped or failed
    pub state: String,
    pub message: String,
    pub pid: u32,
    pub started_at: String,
    pub started_unix: i64,
    pub updated_at: String,
    pub updated_unix: i64,
    pub finished_unix: i64,
    /// Unique titles in the guide that TMDB could add something to.
    pub titles: usize,
    /// Of those, already in the cache when this run started.
    pub cached: usize,
    /// Titles this run set out to look up.
    pub total: usize,
    /// Looked up so far (found, or confirmed not on TMDB).
    pub done: usize,
    /// Of `done`, found on TMDB.
    pub found: usize,
    /// Lookups that failed in the last round and wait for a retry.
    pub failed: usize,
    /// Titles still to look up (total - done).
    pub pending: usize,
    /// Lookups per second over this run.
    pub rate: f64,
    pub eta_secs: Option<u64>,
    pub requests: usize,
    /// Unique titles TMDB is not asked about (news, sports, filler).
    pub skipped: usize,
    /// TVmaze: series looked up and found this run.
    pub tvmaze_done: usize,
    pub tvmaze_found: usize,
    /// The latest lookups, newest first.
    pub recent: Vec<RecentTitle>,
    /// Set when Stop was pressed; the schedule then leaves it alone until Start.
    pub stopped_by_user: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RecentTitle {
    pub title: String,
    pub year: Option<i32>,
    pub found: bool,
    /// The title TMDB matched it to.
    pub tmdb_name: String,
    pub poster: Option<String>,
    pub rating: Option<String>,
}

impl EnrichStatus {
    fn progress(&mut self, started: Instant) {
        self.pending = self.total.saturating_sub(self.done);
        let secs = started.elapsed().as_secs_f64();
        self.rate = if secs > 0.5 { ((self.done as f64 / secs) * 10.0).round() / 10.0 } else { 0.0 };
        self.eta_secs = if self.rate > 0.0 { Some((self.pending as f64 / self.rate) as u64) } else { None };
    }
}

pub fn read_status(data_dir: &Path) -> EnrichStatus {
    std::fs::read(data_dir.join(STATUS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_status(data_dir: &Path, st: &mut EnrichStatus) {
    let now = timeutil::now_unix();
    st.updated_unix = now;
    st.updated_at = timeutil::format_utc(now);
    let res = serde_json::to_vec_pretty(st)
        .map_err(anyhow::Error::from)
        .and_then(|b| output::write_atomic(&data_dir.join(STATUS_FILE), &b));
    if let Err(e) = res {
        eprintln!("warning: could not write {STATUS_FILE}: {e:#}");
    }
}

/// Start `epg-loader enrich` in the background when TMDB titles are pending
/// and it is not running yet. Called by the schedule and by watch mode.
pub fn maybe_spawn(conn: &Connection, cfg: &Config, data_dir: &Path, output_dir: Option<&Path>) {
    if !cfg.tmdb_background || cfg.tmdb_api_key.is_empty() {
        return;
    }
    let st = read_status(data_dir);
    if st.stopped_by_user {
        return;
    }
    // Titles the last build of each enabled guide left for later.
    let build_left: usize = db::guides(conn)
        .unwrap_or_default()
        .iter()
        .filter(|g| g.enabled)
        .map(|g| {
            let s = crate::read_status(&Target::new(g, &cfg.output_dir).status);
            s.tmdb_deferred + s.tvmaze_deferred
        })
        .sum();
    // Pending titles left, or a run that never finished (killed, rebooted).
    let unfinished = st.pending > 0 || (st.running && st.state != "done");
    if build_left == 0 && !unfinished {
        return;
    }
    // A free lock means it is not running. Release it before the child takes it.
    match acquire_named_lock(data_dir, LOCK_FILE) {
        Ok(Some(lock)) => drop(lock),
        _ => return,
    }
    match spawn(data_dir, output_dir) {
        Ok(()) => println!("Started background TMDB lookups (see logs/enrich.log)"),
        Err(e) => eprintln!("warning: could not start background TMDB lookups: {e:#}"),
    }
}

fn spawn(data_dir: &Path, output_dir: Option<&Path>) -> Result<()> {
    let exe = std::env::current_exe().context("finding this program")?;
    let logs = data_dir.join("logs");
    std::fs::create_dir_all(&logs).with_context(|| format!("creating {}", logs.display()))?;
    let log = File::create(logs.join("enrich.log")).context("creating logs/enrich.log")?;
    let mut cmd = Command::new(exe);
    cmd.arg("--data-dir").arg(data_dir);
    if let Some(o) = output_dir {
        cmd.arg("--output-dir").arg(o);
    }
    cmd.arg("enrich").stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group, so it keeps running after the schedule's process exits.
        cmd.process_group(0);
    }
    cmd.spawn().context("starting epg-loader enrich")?;
    Ok(())
}

/// Take enrich.lock, look everything up, then rebuild the guide if anything new was found.
pub async fn run(conn: &Connection, cfg: &Config, opts: Opts) -> Result<Outcome> {
    let dir = cfg.base_dir.as_path();
    let Some(_lock) = acquire_named_lock(dir, LOCK_FILE)? else {
        return Ok(Outcome::Busy);
    };
    // A Stop left from an earlier run must not stop this one.
    let flag = dir.join(CANCEL_FILE);
    std::fs::remove_file(&flag).ok();
    CANCEL.store(false, Ordering::Relaxed);
    let now = timeutil::now_unix();
    let mut st = EnrichStatus {
        running: true,
        state: "running".into(),
        message: "Reading the guide source".into(),
        pid: std::process::id(),
        started_at: timeutil::format_utc(now),
        started_unix: now,
        ..Default::default()
    };
    write_status(dir, &mut st);
    println!("epg-loader {} background TMDB lookups started", env!("CARGO_PKG_VERSION"));

    let watcher = tokio::spawn(watch_cancel(flag.clone()));
    let result = lookup_all(conn, cfg, opts, &mut st).await;
    watcher.abort();
    std::fs::remove_file(&flag).ok();
    st.finished_unix = timeutil::now_unix();
    let found = match result {
        Ok(found) => found,
        Err(e) => {
            st.running = false;
            if e.downcast_ref::<Cancelled>().is_some() {
                st.state = "stopped".into();
                st.stopped_by_user = !TERMINATED.load(Ordering::Relaxed);
                st.message = if st.stopped_by_user {
                    "Stopped from the dashboard; press Start to continue".into()
                } else {
                    "Stopped by the system; it resumes on the next scheduled run".into()
                };
            } else {
                st.state = "failed".into();
                st.message = format!("{e:#}");
            }
            write_status(dir, &mut st);
            return Err(e);
        }
    };

    if found > 0 && !opts.dry_run {
        st.state = "building".into();
        st.message = format!("Rebuilding the guide with {found} new TMDB results");
        write_status(dir, &mut st);
        println!("{}", st.message);
        let mut note = format!("{found} new TMDB results are waiting for the next scheduled build");
        for _ in 0..BUILD_WAIT_STEPS {
            match build_guides(conn, cfg, None, false, opts).await {
                Ok(Outcome::Built) => {
                    note = format!("{found} new TMDB results were added to the guides");
                    break;
                }
                Ok(Outcome::Busy) => {
                    st.message = format!("Waiting for the running build to finish, then adding {found} new TMDB results");
                    write_status(dir, &mut st);
                    tokio::time::sleep(BUILD_WAIT_STEP).await;
                }
                Err(e) => {
                    eprintln!("error: rebuilding the guides: {e:#}");
                    note = format!("Rebuilding the guides failed ({e:#}); the next scheduled build adds the {found} new results");
                    break;
                }
            }
        }
        st.message = note;
    }
    if st.pending > 0 {
        st.message = format!("{}. {} titles kept failing and are retried later.", st.message, st.pending);
    }
    st.running = false;
    st.state = "done".into();
    st.finished_unix = timeutil::now_unix();
    write_status(dir, &mut st);
    println!("Background TMDB lookups finished: {}", st.message);
    Ok(Outcome::Built)
}

/// Look up every pending title. Returns how many were found on TMDB.
async fn lookup_all(conn: &Connection, cfg: &Config, opts: Opts, st: &mut EnrichStatus) -> Result<usize> {
    if opts.no_tmdb {
        bail!("--no-tmdb was given, so there is nothing to look up");
    }
    if cfg.tmdb_api_key.is_empty() {
        bail!("no TMDB key is set (enter it on the settings page)");
    }
    let dir = cfg.base_dir.as_path();
    let client = reqwest::Client::builder()
        .user_agent(concat!("epg-loader/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .build()
        .context("building HTTP client")?;

    // Which titles do the guides need? Each build saves its list, so the sources (which can be
    // several GB) are never downloaded or read here.
    let now = timeutil::now_unix();
    let ids: Vec<i64> = db::guides(conn)?.into_iter().filter(|g| g.enabled).map(|g| g.id).collect();
    let (wanted, first_start) = db::wanted_for(conn, &ids)?;
    if wanted.is_empty() {
        bail!("no titles are known yet: build the guides first");
    }
    // Skipped titles (news, sports, filler) are counted by the builds, not here.
    st.skipped = 0;

    let mut cache = TmdbCache::load(conn, &cfg.language, &cfg.rating_country)?;
    let now_u = now.max(0) as u64;
    let mut keys: Vec<LookupKey> = Vec::new();
    for k in wanted.values() {
        if cache.get_fresh(&k.key, now_u).is_some() {
            st.cached += 1;
        } else {
            keys.push(k.clone());
        }
    }
    // Soonest-airing first, then by key so the order is stable.
    keys.sort_by(|a, b| {
        let sa = first_start.get(&a.key).copied().unwrap_or(i64::MAX);
        let sb = first_start.get(&b.key).copied().unwrap_or(i64::MAX);
        sa.cmp(&sb).then_with(|| a.key.cmp(&b.key))
    });
    st.titles = wanted.len();
    st.total = keys.len();
    st.pending = keys.len();
    println!("TMDB: {} unique titles, {} cached, {} to look up (no time budget)", st.titles, st.cached, st.total);
    if keys.is_empty() {
        let tv = tvmaze_pass(conn, cfg, &client, &wanted, &first_start, &cache, st).await?;
        st.message = if tv > 0 {
            format!("Every TMDB title is looked up; TVmaze found {tv} series")
        } else {
            "Every title is already looked up".into()
        };
        return Ok(tv);
    }
    st.message = format!("Looking up {} titles", st.total);
    write_status(dir, st);

    let started = Instant::now();
    let mut requests_before = 0usize;
    let mut save_error: Option<String> = None;
    let mut round = 0u32;
    loop {
        round += 1;
        let api = Arc::new(Tmdb::new(
            client.clone(),
            &cfg.tmdb_api_key,
            &cfg.language,
            &cfg.rating_country,
            cfg.tmdb_concurrency,
        ));
        let mut retry: Vec<LookupKey> = Vec::new();
        let mut last_write = Instant::now();
        st.failed = 0;
        Arc::clone(&api)
            .lookup_stream(keys, None, &CANCEL, |k, r| {
                match r {
                    Ok(info) => {
                        st.done += 1;
                        if info.found {
                            st.found += 1;
                        }
                        st.recent.insert(
                            0,
                            RecentTitle {
                                title: k.query.clone(),
                                year: k.year,
                                found: info.found,
                                tmdb_name: info.name.clone(),
                                poster: info.poster_url(),
                                rating: info.certification.clone(),
                            },
                        );
                        st.recent.truncate(RECENT_MAX);
                        cache.insert(k, info);
                        if cache.pending_len() >= TMDB_SAVE_BATCH {
                            if let Err(e) = cache.save(conn) {
                                if save_error.is_none() {
                                    eprintln!("error: saving the TMDB cache: {e:#}");
                                    save_error = Some(format!("{e:#}"));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        st.failed += 1;
                        if st.failed <= MAX_LOGGED_ERRORS {
                            eprintln!("warning: TMDB lookup for \"{}\": {e:#}", k.query);
                        }
                        retry.push(k);
                    }
                }
                if last_write.elapsed() >= STATUS_EVERY {
                    last_write = Instant::now();
                    st.requests = requests_before + api.request_count();
                    st.progress(started);
                    write_status(dir, &mut *st);
                }
            })
            .await;
        st.requests = requests_before + api.request_count();
        requests_before = st.requests;
        if let Err(e) = cache.save(conn) {
            eprintln!("error: saving the TMDB cache: {e:#}");
            save_error.get_or_insert(format!("{e:#}"));
        }
        st.progress(started);
        write_status(dir, st);
        // Results so far are saved, so stopping here loses nothing.
        check_cancel()?;
        if api.key_rejected() {
            bail!("TMDB rejected the API key; check it on the settings page");
        }
        if retry.is_empty() {
            break;
        }
        st.failed = retry.len();
        if round >= MAX_ROUNDS {
            println!("{} lookups kept failing; they are retried on the next start", retry.len());
            break;
        }
        // Network trouble or TMDB errors: back off and try those again.
        let wait = RETRY_FIRST.saturating_mul(1u32 << (round - 1)).min(RETRY_MAX);
        st.state = "retrying".into();
        st.message = format!("{} lookups failed (network or TMDB errors); retrying in {}s", retry.len(), wait.as_secs());
        write_status(dir, st);
        println!("{}", st.message);
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = cancel_requested() => {}
        }
        check_cancel()?;
        st.state = "running".into();
        st.message = format!("Retrying {} titles", retry.len());
        keys = retry;
    }
    st.progress(started);
    let tv = tvmaze_pass(conn, cfg, &client, &wanted, &first_start, &cache, st).await?;
    st.message = format!("Looked up {} titles, {} found on TMDB", st.done, st.found);
    if let Some(e) = save_error {
        st.message = format!("{}; saving some results failed: {e}", st.message);
    }
    if tv > 0 {
        st.message = format!("{}; TVmaze found {tv} series", st.message);
    }
    Ok(st.found + tv)
}

/// TVmaze for the series TMDB has no match (or no poster) for, with no time limit.
/// Returns how many it found.
async fn tvmaze_pass(
    conn: &Connection,
    cfg: &Config,
    client: &reqwest::Client,
    wanted: &HashMap<String, LookupKey>,
    first_start: &HashMap<String, i64>,
    cache: &TmdbCache,
    st: &mut EnrichStatus,
) -> Result<usize> {
    if !cfg.tvmaze {
        return Ok(0);
    }
    let mut tvm = tvmaze::Cache::load(conn)?;
    let mut todo = tvmaze::candidates(wanted.values(), cache, &tvm);
    if todo.is_empty() {
        return Ok(0);
    }
    todo.sort_by_key(|k| first_start.get(&k.key).copied().unwrap_or(i64::MAX));
    st.state = "running".into();
    st.message = format!("TVmaze: looking up {} series", todo.len());
    write_status(&cfg.base_dir, st);
    println!("{}", st.message);
    let (n, f) = tvmaze::lookup_all(client, &cfg.tvmaze_url, &mut tvm, conn, todo, None, &CANCEL).await;
    st.tvmaze_done = n;
    st.tvmaze_found = f;
    write_status(&cfg.base_dir, st);
    check_cancel()?;
    Ok(f)
}
