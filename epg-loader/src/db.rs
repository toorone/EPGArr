//! epg.db: settings, channel overrides, TMDB cache, run history.
//!
//! The Rust binary owns the schema: it creates and migrates tables on
//! startup. settings.php only reads and writes rows.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use crate::config::{ChannelOverride, ChannelsFile, Config};

pub const DB_FILE: &str = "epg.db";

/// Each entry upgrades the schema by one version. Never edit a shipped
/// migration; append a new one.
const MIGRATIONS: &[&str] = &[
    // 1: initial schema
    r#"
    CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL DEFAULT ''
    );
    CREATE TABLE channel_overrides (
        channel_id TEXT PRIMARY KEY,
        logo_url   TEXT NOT NULL DEFAULT '',
        group_name TEXT NOT NULL DEFAULT '',
        updated_at INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE tmdb_cache (
        cache_key      TEXT PRIMARY KEY,
        norm_title     TEXT NOT NULL,
        year           INTEGER,
        is_tv          INTEGER NOT NULL DEFAULT 0,
        found          INTEGER NOT NULL,
        media_type     TEXT NOT NULL DEFAULT '',
        tmdb_id        INTEGER NOT NULL DEFAULT 0,
        name           TEXT NOT NULL DEFAULT '',
        poster_path    TEXT,
        rating         TEXT,
        genres         TEXT NOT NULL DEFAULT '',
        overview       TEXT,
        language       TEXT NOT NULL,
        rating_country TEXT NOT NULL,
        fetched_at     INTEGER NOT NULL,
        expires_at     INTEGER NOT NULL
    );
    CREATE INDEX tmdb_cache_title ON tmdb_cache(norm_title);
    CREATE TABLE runs (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        started_at    INTEGER NOT NULL,
        finished_at   INTEGER,
        ok            INTEGER,
        duration_secs REAL,
        channels      INTEGER,
        programmes    INTEGER,
        logos_missing INTEGER,
        posters_added INTEGER,
        ratings_added INTEGER,
        errors_total  INTEGER,
        error         TEXT,
        summary       TEXT
    );
    CREATE TABLE missing_logos (
        channel_id   TEXT PRIMARY KEY,
        display_name TEXT NOT NULL DEFAULT ''
    );
    "#,
    // 2: enriched guide for guide.php, replaced in one transaction per successful build
    r#"
    CREATE TABLE guide_channels (
        pos        INTEGER PRIMARY KEY,
        channel_id TEXT NOT NULL UNIQUE,
        name       TEXT NOT NULL DEFAULT '',
        logo       TEXT,
        group_name TEXT
    );
    CREATE INDEX guide_channels_group ON guide_channels(group_name);
    CREATE TABLE guide_programmes (
        id          INTEGER PRIMARY KEY,
        channel_pos INTEGER NOT NULL,
        start       INTEGER NOT NULL,
        stop        INTEGER NOT NULL,
        title       TEXT NOT NULL DEFAULT '',
        sub_title   TEXT,
        episode     TEXT,
        descr       TEXT,
        icon        TEXT,
        rating      TEXT,
        categories  TEXT NOT NULL DEFAULT '[]',
        group_name  TEXT
    );
    CREATE INDEX guide_programmes_channel ON guide_programmes(channel_pos, start);
    CREATE INDEX guide_programmes_start ON guide_programmes(start);
    CREATE INDEX guide_programmes_stop ON guide_programmes(stop);
    "#,
    // 3: tv-logos file list, a small key/value store, and which source gave each channel its logo
    r#"
    CREATE TABLE tv_logos (path TEXT PRIMARY KEY);
    CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL DEFAULT '');
    CREATE TABLE channel_logo_sources (channel_id TEXT PRIMARY KEY, source TEXT NOT NULL);
    "#,
    // 4: several guides, each with its own source and output. The existing source becomes guide 1, "main".
    r#"
    CREATE TABLE guides (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        name          TEXT NOT NULL,
        slug          TEXT NOT NULL UNIQUE,
        source_url    TEXT NOT NULL DEFAULT '',
        days          INTEGER,
        refresh_hours REAL,
        logo_country  TEXT,
        enabled       INTEGER NOT NULL DEFAULT 1,
        created_at    INTEGER NOT NULL DEFAULT 0,
        sort_order    INTEGER NOT NULL DEFAULT 0
    );
    INSERT INTO guides (id, name, slug, source_url, days, enabled, created_at, sort_order)
    SELECT 1, COALESCE(NULLIF(TRIM((SELECT value FROM settings WHERE key = 'main_guide_name')), ''), 'Main'), 'main',
           COALESCE((SELECT value FROM settings WHERE key = 'source_url'), ''),
           (SELECT CAST(value AS INTEGER) FROM settings WHERE key = 'days' AND value NOT IN ('', '0')),
           1, CAST(strftime('%s', 'now') AS INTEGER), 0;

    ALTER TABLE runs ADD COLUMN guide_id INTEGER;
    UPDATE runs SET guide_id = 1;
    CREATE INDEX runs_guide ON runs(guide_id, started_at);

    DROP INDEX guide_channels_group;
    DROP INDEX guide_programmes_channel;
    DROP INDEX guide_programmes_start;
    DROP INDEX guide_programmes_stop;
    ALTER TABLE guide_channels RENAME TO guide_channels_v3;
    ALTER TABLE guide_programmes RENAME TO guide_programmes_v3;
    CREATE TABLE guide_channels (
        guide_id   INTEGER NOT NULL,
        pos        INTEGER NOT NULL,
        channel_id TEXT NOT NULL,
        name       TEXT NOT NULL DEFAULT '',
        logo       TEXT,
        group_name TEXT,
        PRIMARY KEY (guide_id, pos),
        UNIQUE (guide_id, channel_id)
    );
    INSERT INTO guide_channels (guide_id, pos, channel_id, name, logo, group_name)
        SELECT 1, pos, channel_id, name, logo, group_name FROM guide_channels_v3;
    CREATE TABLE guide_programmes (
        id          INTEGER PRIMARY KEY,
        guide_id    INTEGER NOT NULL,
        channel_pos INTEGER NOT NULL,
        start       INTEGER NOT NULL,
        stop        INTEGER NOT NULL,
        title       TEXT NOT NULL DEFAULT '',
        sub_title   TEXT,
        episode     TEXT,
        descr       TEXT,
        icon        TEXT,
        rating      TEXT,
        categories  TEXT NOT NULL DEFAULT '[]',
        group_name  TEXT
    );
    INSERT INTO guide_programmes (id, guide_id, channel_pos, start, stop, title, sub_title, episode, descr, icon, rating, categories, group_name)
        SELECT id, 1, channel_pos, start, stop, title, sub_title, episode, descr, icon, rating, categories, group_name FROM guide_programmes_v3;
    DROP TABLE guide_programmes_v3;
    DROP TABLE guide_channels_v3;
    CREATE INDEX guide_channels_group ON guide_channels(guide_id, group_name);
    CREATE INDEX guide_programmes_channel ON guide_programmes(guide_id, channel_pos, start);
    CREATE INDEX guide_programmes_start ON guide_programmes(guide_id, start);
    CREATE INDEX guide_programmes_stop ON guide_programmes(guide_id, stop);

    DROP TABLE missing_logos;
    CREATE TABLE missing_logos (
        guide_id     INTEGER NOT NULL,
        channel_id   TEXT NOT NULL,
        display_name TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (guide_id, channel_id)
    );
    DROP TABLE channel_logo_sources;
    CREATE TABLE channel_logo_sources (
        guide_id   INTEGER NOT NULL,
        channel_id TEXT NOT NULL,
        source     TEXT NOT NULL,
        PRIMARY KEY (guide_id, channel_id)
    );
    "#,
    // 5: each guide's files are named after it; remember which files it wrote last so a rename
    // can remove the old ones. Guide 1 wrote epg.xml before guides existed.
    r#"
    ALTER TABLE guides ADD COLUMN output_slug TEXT;
    UPDATE guides SET output_slug = '__epg.xml__' WHERE id = 1;
    "#,
    // 6: IMDb star ratings: the IMDb id TMDB knows for each title, IMDb's ratings file, and the
    // star rating shown in the TV Guide.
    r#"
    ALTER TABLE tmdb_cache ADD COLUMN imdb_id TEXT;
    ALTER TABLE tmdb_cache ADD COLUMN imdb_checked INTEGER NOT NULL DEFAULT 0;
    CREATE TABLE imdb_ratings (
        tconst TEXT PRIMARY KEY,
        rating REAL NOT NULL,
        votes  INTEGER NOT NULL
    ) WITHOUT ROWID;
    ALTER TABLE guide_programmes ADD COLUMN star TEXT;
    "#,
    // 7: TVmaze results for series TMDB has no match (or no poster) for.
    r#"
    CREATE TABLE tvmaze_cache (
        norm_title TEXT PRIMARY KEY,
        found      INTEGER NOT NULL,
        poster     TEXT,
        summary    TEXT,
        genres     TEXT NOT NULL DEFAULT '',
        fetched_at INTEGER NOT NULL
    ) WITHOUT ROWID;
    "#,
    // 8: TheSportsDB events for sports programmes without a poster.
    r#"
    CREATE TABLE sportsdb_cache (
        query      TEXT PRIMARY KEY,
        found      INTEGER NOT NULL,
        poster     TEXT,
        fetched_at INTEGER NOT NULL
    ) WITHOUT ROWID;
    "#,
];

pub fn open(data_dir: &Path) -> Result<Connection> {
    std::fs::create_dir_all(data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    let path = data_dir.join(DB_FILE);
    let conn = Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
    prepare(&conn).with_context(|| format!("preparing {}", path.display()))?;
    Ok(conn)
}

#[cfg(test)]
pub fn open_memory() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    prepare(&conn).unwrap();
    conn
}

fn prepare(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(15))?;
    let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
    migrate(conn)
}

pub fn schema_version(conn: &Connection) -> Result<i64> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);")?;
    Ok(conn.query_row("SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |r| r.get(0))?)
}

fn migrate(conn: &Connection) -> Result<()> {
    let current = schema_version(conn)?;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let version = i as i64 + 1;
        if version <= current {
            continue;
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql).with_context(|| format!("schema migration {version}"))?;
        tx.execute("INSERT INTO schema_version (version) VALUES (?1)", params![version])?;
        tx.commit()?;
        println!("epg.db: migrated to schema version {version}");
    }
    Ok(())
}

// ---------------------------------------------------------------- settings

pub fn settings(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (k, v) = row?;
        out.insert(k, v);
    }
    Ok(out)
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Settings from the database. `output_dir`, when given (from --output-dir
/// or EPG_OUTPUT_DIR), wins over the stored output_dir.
pub fn load_config(conn: &Connection, data_dir: &Path, output_dir: Option<&Path>) -> Result<Config> {
    let mut s = settings(conn)?;
    if let Some(o) = output_dir {
        s.insert("output_dir".into(), o.to_string_lossy().into_owned());
    }
    Config::from_settings(&s, data_dir)
}

pub fn expected_schema_version() -> i64 {
    MIGRATIONS.len() as i64
}

// ---------------------------------------------------------------- overrides

pub fn channel_overrides(conn: &Connection) -> Result<ChannelsFile> {
    let mut stmt = conn.prepare("SELECT channel_id, logo_url, group_name FROM channel_overrides")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
    })?;
    let mut out = ChannelsFile::default();
    for row in rows {
        let (id, logo, group) = row?;
        let nz = |s: String| (!s.trim().is_empty()).then(|| s.trim().to_string());
        out.channels.insert(id, ChannelOverride { logo: nz(logo), group: nz(group) });
    }
    Ok(out)
}

pub fn set_missing_logos(conn: &Connection, guide_id: i64, missing: &[(String, String)]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM missing_logos WHERE guide_id = ?1", params![guide_id])?;
    {
        let mut stmt =
            tx.prepare("INSERT OR REPLACE INTO missing_logos (guide_id, channel_id, display_name) VALUES (?1, ?2, ?3)")?;
        for (id, name) in missing {
            stmt.execute(params![guide_id, id, name])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Which source gave each channel its logo (feed, override, tv-logos, iptv-org).
pub fn set_logo_sources(conn: &Connection, guide_id: i64, rows: &[(String, &str)]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM channel_logo_sources WHERE guide_id = ?1", params![guide_id])?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO channel_logo_sources (guide_id, channel_id, source) VALUES (?1, ?2, ?3)",
        )?;
        for (id, src) in rows {
            stmt.execute(params![guide_id, id, *src])?;
        }
    }
    tx.commit()?;
    Ok(())
}

// ---------------------------------------------------------------- tv-logos list

const META_UPSERT: &str = "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value";

pub fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0)).optional()?)
}

pub fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(META_UPSERT, params![key, value])?;
    Ok(())
}

/// The cached tv-logos file list: (paths, fetched_at unix time, partial list?).
pub fn tv_logo_list(conn: &Connection) -> Result<(Vec<String>, i64, bool)> {
    let mut stmt = conn.prepare("SELECT path FROM tv_logos ORDER BY path")?;
    let paths = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
    let fetched = meta_get(conn, "tv_logos_fetched_at")?.and_then(|v| v.parse().ok()).unwrap_or(0);
    let partial = meta_get(conn, "tv_logos_partial")?.as_deref() == Some("1");
    Ok((paths, fetched, partial))
}

pub fn set_tv_logo_list(conn: &Connection, paths: &[String], fetched_at: i64, partial: bool) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM tv_logos", [])?;
    {
        let mut stmt = tx.prepare("INSERT OR IGNORE INTO tv_logos (path) VALUES (?1)")?;
        for p in paths {
            stmt.execute(params![p])?;
        }
    }
    tx.execute(META_UPSERT, params!["tv_logos_fetched_at", fetched_at.to_string()])?;
    tx.execute(META_UPSERT, params!["tv_logos_partial", if partial { "1" } else { "0" }])?;
    tx.commit()?;
    Ok(())
}

// ---------------------------------------------------------------- guide

pub struct GuideChannel<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub logo: Option<&'a str>,
    pub group: Option<String>,
}

pub struct GuideProgramme<'a> {
    /// Index into the channel list passed alongside.
    pub channel: usize,
    pub start: i64,
    pub stop: i64,
    pub title: &'a str,
    pub sub_title: Option<&'a str>,
    pub episode: Option<&'a str>,
    pub desc: Option<&'a str>,
    pub icon: Option<&'a str>,
    pub rating: Option<&'a str>,
    pub categories: &'a [String],
    /// A category the build adds on top of `categories`.
    pub extra_category: Option<&'a str>,
    pub group: Option<&'a str>,
    /// Star rating shown in the TV Guide, e.g. "IMDb 7.8/10".
    pub star: Option<&'a str>,
}

/// Replace the guide tables in one transaction. Readers keep seeing the
/// previous guide until the commit; on any error nothing changes.
pub fn replace_guide(conn: &Connection, guide_id: i64, channels: &[GuideChannel], progs: &[GuideProgramme]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM guide_programmes WHERE guide_id = ?1", params![guide_id])?;
    tx.execute("DELETE FROM guide_channels WHERE guide_id = ?1", params![guide_id])?;
    {
        let mut st = tx.prepare(
            "INSERT INTO guide_channels (guide_id, pos, channel_id, name, logo, group_name) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (i, c) in channels.iter().enumerate() {
            st.execute(params![guide_id, i as i64, c.id, c.name, c.logo, c.group])?;
        }
        let mut st = tx.prepare(
            "INSERT INTO guide_programmes (guide_id, channel_pos, start, stop, title, sub_title, episode, descr, icon, rating, categories, group_name, star)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        for p in progs {
            let mut cats: Vec<&str> = p.categories.iter().map(String::as_str).collect();
            if let Some(extra) = p.extra_category {
                if !cats.iter().any(|c| c.eq_ignore_ascii_case(extra)) {
                    cats.push(extra);
                }
            }
            let cats = serde_json::to_string(&cats)?;
            st.execute(params![
                guide_id,
                p.channel as i64,
                p.start,
                p.stop,
                p.title,
                p.sub_title,
                p.episode,
                p.desc,
                p.icon,
                p.rating,
                cats,
                p.group,
                p.star
            ])?;
        }
    }
    tx.commit()?;
    // The delete + insert grows the WAL a lot; give the space back.
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    Ok(())
}

// ---------------------------------------------------------------- guides

/// One guide: its own XMLTV source and output. Guide "main" writes epg.xml.
#[derive(Debug, Clone, PartialEq)]
pub struct Guide {
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub source_url: String,
    pub days: Option<u32>,
    pub refresh_hours: Option<f64>,
    pub logo_country: Option<String>,
    pub enabled: bool,
    /// The slug of the files this guide wrote last (LEGACY_OUTPUT = epg.xml), so a rename can remove them.
    pub output_slug: Option<String>,
}

/// `output_slug` of guide 1 before guides existed: it wrote epg.xml, epg.xml.gz and status.json.
pub const LEGACY_OUTPUT: &str = "__epg.xml__";

pub fn guides(conn: &Connection) -> Result<Vec<Guide>> {
    let mut st = conn.prepare(
        "SELECT id, name, slug, source_url, days, refresh_hours, logo_country, enabled, output_slug
         FROM guides ORDER BY sort_order, id",
    )?;
    let rows = st.query_map([], |r| {
        Ok(Guide {
            id: r.get(0)?,
            name: r.get(1)?,
            slug: r.get(2)?,
            source_url: r.get(3)?,
            days: r.get::<_, Option<i64>>(4)?.and_then(|d| u32::try_from(d).ok()),
            refresh_hours: r.get(5)?,
            logo_country: r.get(6)?,
            enabled: r.get::<_, i64>(7)? != 0,
            output_slug: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<Guide>>>()?)
}

/// "US Locals!" -> "us-locals": the guide's file name. Same rules as epg_slugify() in epg_lib.php.
pub fn slugify(name: &str) -> String {
    let mut s = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            s.push(c);
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    let s: String = s.trim_matches('-').chars().take(40).collect();
    if s.is_empty() {
        "guide".to_string()
    } else {
        s
    }
}

/// Give every guide the slug its name asks for (unique: "news", "news-2", ...).
/// Guides whose slug already fits keep it; the pages normally do this on rename.
pub fn sync_slugs(conn: &Connection) -> Result<()> {
    let all = guides(conn)?;
    let mut taken: std::collections::HashSet<String> =
        all.iter().filter(|g| g.slug == slugify(&g.name)).map(|g| g.slug.clone()).collect();
    for g in all.iter().filter(|g| g.slug != slugify(&g.name)) {
        let base = slugify(&g.name);
        let mut slug = base.clone();
        let mut i = 2;
        while taken.contains(&slug) {
            slug = format!("{base}-{i}");
            i += 1;
        }
        // A slug that only differs by its -N suffix from what is free stays as it is.
        if slug != g.slug {
            conn.execute("UPDATE guides SET slug = ?1 WHERE id = ?2", params![slug, g.id])?;
            println!("Guide \"{}\" is now written as guides/{slug}.xml", g.name);
        }
        taken.insert(slug);
    }
    Ok(())
}

/// Remember which files a guide wrote, after a successful build.
pub fn set_output_slug(conn: &Connection, guide_id: i64, slug: &str) -> Result<()> {
    conn.execute("UPDATE guides SET output_slug = ?1 WHERE id = ?2", params![slug, guide_id])?;
    Ok(())
}

/// True when another guide now writes files with this slug.
pub fn slug_in_use(conn: &Connection, slug: &str, except_id: i64) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM guides WHERE (slug = ?1 OR output_slug = ?1) AND id <> ?2",
        params![slug, except_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

// ---------------------------------------------------------------- runs

pub fn start_run(conn: &Connection, guide_id: i64, now: i64) -> Result<i64> {
    // We hold the build lock, so any unfinished run was interrupted.
    conn.execute(
        "UPDATE runs SET finished_at = ?1, ok = 0, error = 'interrupted' WHERE finished_at IS NULL",
        params![now],
    )?;
    conn.execute("INSERT INTO runs (started_at, guide_id) VALUES (?1, ?2)", params![now, guide_id])?;
    Ok(conn.last_insert_rowid())
}

pub struct RunResult<'a> {
    pub ok: bool,
    pub duration_secs: f64,
    pub channels: usize,
    pub programmes: usize,
    pub logos_missing: usize,
    pub posters_added: usize,
    pub ratings_added: usize,
    pub errors_total: usize,
    pub error: Option<&'a str>,
    pub summary_json: &'a str,
}

pub fn finish_run(conn: &Connection, id: i64, now: i64, r: &RunResult) -> Result<()> {
    conn.execute(
        "UPDATE runs SET finished_at = ?2, ok = ?3, duration_secs = ?4, channels = ?5, programmes = ?6,
             logos_missing = ?7, posters_added = ?8, ratings_added = ?9, errors_total = ?10,
             error = ?11, summary = ?12
         WHERE id = ?1",
        params![
            id,
            now,
            r.ok,
            r.duration_secs,
            r.channels as i64,
            r.programmes as i64,
            r.logos_missing as i64,
            r.posters_added as i64,
            r.ratings_added as i64,
            r.errors_total as i64,
            r.error,
            r.summary_json
        ],
    )?;
    // Keep the history short.
    conn.execute("DELETE FROM runs WHERE id <= (SELECT MAX(id) FROM runs) - 200", [])?;
    Ok(())
}

pub fn last_run_started(conn: &Connection, guide_id: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row("SELECT MAX(started_at) FROM runs WHERE guide_id = ?1", params![guide_id], |r| r.get::<_, Option<i64>>(0))
        .optional()?
        .flatten())
}

// ---------------------------------------------------------------- import

/// Import <data_dir>/config.toml and channels.toml once, if they exist and
/// the database has nothing yet. The files are left in place.
pub fn import_legacy(conn: &Connection, data_dir: &Path) -> Result<()> {
    let cfg_path = data_dir.join("config.toml");
    let has_settings: i64 = conn.query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0))?;
    if has_settings == 0 && cfg_path.exists() {
        let text = std::fs::read_to_string(&cfg_path)?;
        let cfg = Config::parse_toml(&text, data_dir).with_context(|| format!("importing {}", cfg_path.display()))?;
        for (k, v) in cfg.to_settings() {
            set_setting(conn, k, &v)?;
        }
        // The guides table was created before this import; give guide 1 the imported source.
        conn.execute(
            "UPDATE guides SET source_url = ?1 WHERE id = 1 AND source_url = ''",
            params![cfg.source_url],
        )?;
        println!("Imported settings from {}", cfg_path.display());
        import_channels(conn, &cfg.channels_file)?;
    } else {
        import_channels(conn, &data_dir.join("channels.toml"))?;
    }
    Ok(())
}

fn import_channels(conn: &Connection, path: &Path) -> Result<()> {
    let has: i64 = conn.query_row("SELECT COUNT(*) FROM channel_overrides", [], |r| r.get(0))?;
    let imported: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = 'imported_channels_toml'", [], |r| r.get(0))
        .optional()?;
    if has > 0 || imported.is_some() || !path.exists() {
        return Ok(());
    }
    let file = crate::config::load_channels_toml(path)?;
    let now = crate::timeutil::now_unix();
    for (id, o) in &file.channels {
        conn.execute(
            "INSERT OR REPLACE INTO channel_overrides (channel_id, logo_url, group_name, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, o.logo.clone().unwrap_or_default(), o.group.clone().unwrap_or_default(), now],
        )?;
    }
    set_setting(conn, "imported_channels_toml", "1")?;
    println!("Imported {} channel overrides from {}", file.channels.len(), path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_idempotent() {
        let conn = open_memory();
        assert_eq!(schema_version(&conn).unwrap(), MIGRATIONS.len() as i64);
        migrate(&conn).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), MIGRATIONS.len() as i64);
    }

    #[test]
    fn settings_round_trip_into_config() {
        let conn = open_memory();
        set_setting(&conn, "source_url", "http://x/guide.xml").unwrap();
        set_setting(&conn, "output_dir", "/srv/epg").unwrap();
        set_setting(&conn, "days", "3").unwrap();
        set_setting(&conn, "days", "5").unwrap();
        let cfg = load_config(&conn, Path::new("/data"), None).unwrap();
        assert_eq!(cfg.days, Some(5));
        assert_eq!(cfg.source_url, "http://x/guide.xml");
        let cfg = load_config(&conn, Path::new("/data"), Some(Path::new("/elsewhere"))).unwrap();
        assert_eq!(cfg.output_dir, Path::new("/elsewhere"));
    }

    #[test]
    fn overrides_and_runs() {
        let conn = open_memory();
        conn.execute(
            "INSERT INTO channel_overrides (channel_id, logo_url, group_name) VALUES ('A.uk', 'http://l/a.png', ''), ('B.uk', '', 'News')",
            [],
        )
        .unwrap();
        let o = channel_overrides(&conn).unwrap();
        assert_eq!(o.channels["A.uk"].logo.as_deref(), Some("http://l/a.png"));
        assert_eq!(o.channels["A.uk"].group, None);
        assert_eq!(o.channels["B.uk"].group.as_deref(), Some("News"));

        let id = start_run(&conn, 1, 100).unwrap();
        let id2 = start_run(&conn, 1, 200).unwrap();
        let err: Option<String> = conn.query_row("SELECT error FROM runs WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(err.as_deref(), Some("interrupted"));
        finish_run(
            &conn,
            id2,
            260,
            &RunResult {
                ok: true,
                duration_secs: 60.0,
                channels: 3,
                programmes: 10,
                logos_missing: 1,
                posters_added: 2,
                ratings_added: 2,
                errors_total: 0,
                error: None,
                summary_json: "{}",
            },
        )
        .unwrap();
        assert_eq!(last_run_started(&conn, 1).unwrap(), Some(200));
        assert_eq!(last_run_started(&conn, 2).unwrap(), None);

        set_missing_logos(&conn, 1, &[("C.ca".into(), "C".into())]).unwrap();
        set_missing_logos(&conn, 1, &[("D.ca".into(), "D".into())]).unwrap();
        set_missing_logos(&conn, 2, &[("E.ca".into(), "E".into())]).unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM missing_logos WHERE guide_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM missing_logos", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn guide_is_replaced_whole() {
        let conn = open_memory();
        let cats = vec!["Film".to_string()];
        let chans = [GuideChannel { id: "A.uk", name: "A", logo: None, group: Some("Movie".into()) }];
        let prog = |t: &'static str| GuideProgramme {
            channel: 0,
            start: 100,
            stop: 200,
            title: t,
            sub_title: None,
            episode: None,
            desc: None,
            icon: None,
            rating: None,
            categories: &cats,
            extra_category: Some("Movie"),
            group: Some("Movie"),
            star: None,
        };
        replace_guide(&conn, 1, &chans, &[prog("One"), prog("Two")]).unwrap();
        replace_guide(&conn, 2, &chans, &[prog("Other")]).unwrap();
        replace_guide(&conn, 1, &chans, &[prog("Three")]).unwrap();
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM guide_programmes", [], |r| r.get(0)).unwrap();
        assert_eq!(total, 2, "each guide keeps its own programmes");
        let (n, c): (i64, String) = conn
            .query_row("SELECT COUNT(*), MAX(categories) FROM guide_programmes WHERE guide_id = 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((n, c.as_str()), (1, r#"["Film","Movie"]"#));
    }

    #[test]
    fn single_source_becomes_guide_main() {
        // A database from before guides existed: schema 3, one source in the settings.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE schema_version (version INTEGER NOT NULL);").unwrap();
        for (i, sql) in MIGRATIONS.iter().take(3).enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute("INSERT INTO schema_version (version) VALUES (?1)", params![i as i64 + 1]).unwrap();
        }
        set_setting(&conn, "source_url", "http://x/guide.xml").unwrap();
        set_setting(&conn, "days", "4").unwrap();
        conn.execute("INSERT INTO guide_channels (pos, channel_id, name) VALUES (0, 'A.uk', 'A')", []).unwrap();
        conn.execute("INSERT INTO runs (started_at) VALUES (50)", []).unwrap();
        migrate(&conn).unwrap();

        let g = guides(&conn).unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!(
            (g[0].id, g[0].slug.as_str(), g[0].name.as_str(), g[0].source_url.as_str(), g[0].days),
            (1, "main", "Main", "http://x/guide.xml", Some(4))
        );
        assert!(g[0].enabled);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM guide_channels WHERE guide_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "the stored guide is kept for guide 1");
        assert_eq!(last_run_started(&conn, 1).unwrap(), Some(50));
        assert_eq!(g[0].output_slug.as_deref(), Some(LEGACY_OUTPUT), "guide 1 owned epg.xml");
    }

    #[test]
    fn saved_name_carries_over_and_sets_the_file_name() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE schema_version (version INTEGER NOT NULL);").unwrap();
        for (i, sql) in MIGRATIONS.iter().take(3).enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute("INSERT INTO schema_version (version) VALUES (?1)", params![i as i64 + 1]).unwrap();
        }
        set_setting(&conn, "main_guide_name", "US Locals").unwrap();
        migrate(&conn).unwrap();
        sync_slugs(&conn).unwrap();
        let g = guides(&conn).unwrap();
        assert_eq!((g[0].name.as_str(), g[0].slug.as_str()), ("US Locals", "us-locals"));
    }

    #[test]
    fn fresh_database_has_an_empty_main_guide() {
        let conn = open_memory();
        let g = guides(&conn).unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!((g[0].slug.as_str(), g[0].source_url.as_str(), g[0].days), ("main", "", None));
    }

    #[test]
    fn slugs_follow_names() {
        assert_eq!(slugify("US Locals!"), "us-locals");
        assert_eq!(slugify("  BBC -- One  "), "bbc-one");
        assert_eq!(slugify("Café TV"), "caf-tv");
        assert_eq!(slugify("!!!"), "guide");
        assert_eq!(slugify(&"x".repeat(50)).len(), 40);
    }

    #[test]
    fn sync_slugs_keeps_them_unique() {
        let conn = open_memory();
        conn.execute("UPDATE guides SET name = 'News' WHERE id = 1", []).unwrap();
        conn.execute("INSERT INTO guides (name, slug, sort_order) VALUES ('News', 'other', 1), ('Sport', 'sport', 2)", [])
            .unwrap();
        sync_slugs(&conn).unwrap();
        sync_slugs(&conn).unwrap();
        let slugs: Vec<String> = guides(&conn).unwrap().into_iter().map(|g| g.slug).collect();
        assert_eq!(slugs, vec!["news", "news-2", "sport"]);
        assert!(slug_in_use(&conn, "sport", 1).unwrap());
        assert!(!slug_in_use(&conn, "sport", 3).unwrap());
    }
}
