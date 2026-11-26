//! IMDb star ratings from IMDb's official ratings file (title.ratings.tsv.gz,
//! free for personal, non-commercial use). The file is downloaded at most once
//! a day, with If-Modified-Since, and kept in the imdb_ratings table. TMDB
//! gives the IMDb id of each matched title, so no guessing by name is needed.

use anyhow::{anyhow, bail, Context, Result};
use flate2::read::MultiGzDecoder;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};

use crate::{db, timeutil};

/// Look for a newer file at most this often.
const REFRESH_SECS: i64 = 86_400;
/// After a failed download, try again after this long.
const RETRY_SECS: i64 = 3_600;

/// Make sure the imdb_ratings table is no older than a day. Returns the number
/// of ratings loaded now (0 when the stored ones were still fresh).
pub async fn refresh(conn: &Connection, client: &reqwest::Client, url: &str) -> Result<usize> {
    let now = timeutil::now_unix();
    let checked: i64 = db::meta_get(conn, "imdb_checked_at")?.and_then(|v| v.parse().ok()).unwrap_or(0);
    let have: i64 = conn.query_row("SELECT COUNT(*) FROM (SELECT 1 FROM imdb_ratings LIMIT 1)", [], |r| r.get(0))?;
    if have > 0 && now - checked < REFRESH_SECS {
        return Ok(0);
    }
    let failed: i64 = db::meta_get(conn, "imdb_failed_at")?.and_then(|v| v.parse().ok()).unwrap_or(0);
    if have > 0 && now - failed < RETRY_SECS {
        return Ok(0);
    }
    let modified = db::meta_get(conn, "imdb_last_modified")?.filter(|_| have > 0);
    let result = download(client, url, modified.as_deref()).await;
    let (bytes, last_modified) = match result {
        Ok(Some(x)) => x,
        Ok(None) => {
            // 304 Not Modified: what we have is current.
            db::meta_set(conn, "imdb_checked_at", &now.to_string())?;
            return Ok(0);
        }
        Err(e) => {
            db::meta_set(conn, "imdb_failed_at", &now.to_string())?;
            return Err(e);
        }
    };
    let n = store(conn, &bytes)?;
    db::meta_set(conn, "imdb_checked_at", &now.to_string())?;
    if let Some(lm) = last_modified {
        db::meta_set(conn, "imdb_last_modified", &lm)?;
    }
    println!("IMDb: {n} ratings loaded");
    Ok(n)
}

/// Ok(None) when the server says the file has not changed.
async fn download(client: &reqwest::Client, url: &str, since: Option<&str>) -> Result<Option<(Vec<u8>, Option<String>)>> {
    let mut req = client.get(url);
    if let Some(s) = since {
        req = req.header("If-Modified-Since", s);
    }
    let r = req.send().await.map_err(|e| anyhow!("downloading the IMDb ratings: {}", e.without_url()))?;
    if r.status().as_u16() == 304 {
        return Ok(None);
    }
    if !r.status().is_success() {
        bail!("downloading the IMDb ratings: HTTP {}", r.status());
    }
    let lm = r
        .headers()
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let b = r.bytes().await.map_err(|e| anyhow!("downloading the IMDb ratings: {}", e.without_url()))?;
    Ok(Some((b.to_vec(), lm)))
}

/// Parse "tconst<TAB>averageRating<TAB>numVotes" lines (gzipped) into the table, in one transaction.
fn store(conn: &Connection, gz: &[u8]) -> Result<usize> {
    let reader = BufReader::new(MultiGzDecoder::new(gz));
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM imdb_ratings", [])?;
    let mut n = 0usize;
    {
        let mut st = tx.prepare("INSERT OR REPLACE INTO imdb_ratings (tconst, rating, votes) VALUES (?1, ?2, ?3)")?;
        for line in reader.lines() {
            let line = line.context("reading the IMDb ratings file")?;
            if let Some((id, rating, votes)) = parse_line(&line) {
                st.execute(params![id, rating, votes])?;
                n += 1;
            }
        }
    }
    if n == 0 {
        bail!("the IMDb ratings file had no ratings in it");
    }
    tx.commit()?;
    Ok(n)
}

/// "tt0000001\t5.7\t2103" -> ("tt0000001", 5.7, 2103). The header line and bad lines give None.
fn parse_line(line: &str) -> Option<(&str, f64, i64)> {
    let mut parts = line.split('\t');
    let id = parts.next()?;
    if !id.starts_with("tt") {
        return None;
    }
    let rating: f64 = parts.next()?.trim().parse().ok()?;
    let votes: i64 = parts.next()?.trim().parse().ok()?;
    (0.0..=10.0).contains(&rating).then_some((id, rating, votes))
}

/// Ratings for these IMDb ids: id -> (rating, votes).
pub fn ratings_for<'a>(conn: &Connection, ids: impl IntoIterator<Item = &'a str>) -> Result<HashMap<String, (f64, i64)>> {
    let mut st = conn.prepare_cached("SELECT rating, votes FROM imdb_ratings WHERE tconst = ?1")?;
    let mut out = HashMap::new();
    for id in ids {
        if out.contains_key(id) {
            continue;
        }
        let row: Option<(f64, i64)> = st.query_row(params![id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        if let Some(r) = row {
            out.insert(id.to_string(), r);
        }
    }
    Ok(out)
}

/// "7.8/10" for the XMLTV <star-rating>.
pub fn star_value(rating: f64) -> String {
    format!("{rating:.1}/10")
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    #[test]
    fn parses_lines() {
        assert_eq!(parse_line("tt0111161\t9.3\t2900000"), Some(("tt0111161", 9.3, 2_900_000)));
        assert_eq!(parse_line("tconst\taverageRating\tnumVotes"), None);
        assert_eq!(parse_line("tt1\tx\t5"), None);
        assert_eq!(parse_line("tt1\t11.0\t5"), None);
        assert_eq!(star_value(7.84), "7.8/10");
    }

    #[test]
    fn stores_and_finds_ratings() {
        let conn = db::open_memory();
        let mut e = GzEncoder::new(Vec::new(), Compression::fast());
        e.write_all(b"tconst\taverageRating\tnumVotes\ntt0000001\t5.7\t2103\ntt0000002\t8.1\t40\n").unwrap();
        let gz = e.finish().unwrap();
        assert_eq!(store(&conn, &gz).unwrap(), 2);
        let r = ratings_for(&conn, ["tt0000001", "tt0000002", "tt9"]).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r["tt0000001"], (5.7, 2103));
    }
}
