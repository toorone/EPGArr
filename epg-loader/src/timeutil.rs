//! Small date helpers, so we need no date crate.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Parse an XMLTV time such as "20261126030000 +0100" into Unix seconds.
pub fn parse_xmltv_time(s: &str) -> Option<i64> {
    let s = s.trim();
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() < 8 {
        return None;
    }
    let num = |a: usize, b: usize| -> Option<i64> { digits.get(a..b).and_then(|x| x.parse().ok()) };
    let (y, mo, d) = (num(0, 4)?, num(4, 6)?, num(6, 8)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let h = num(8, 10).unwrap_or(0);
    let mi = num(10, 12).unwrap_or(0);
    let sec = num(12, 14).unwrap_or(0);
    let offset = parse_offset(s[digits.len()..].trim()).unwrap_or(0);
    Some(days_from_civil(y, mo as u32, d as u32) * 86_400 + h * 3600 + mi * 60 + sec - offset)
}

fn parse_offset(s: &str) -> Option<i64> {
    let mut chars = s.chars();
    let sign = match chars.next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let digits: String = chars.take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() != 4 {
        return None;
    }
    let h: i64 = digits[0..2].parse().ok()?;
    let m: i64 = digits[2..4].parse().ok()?;
    Some(sign * (h * 3600 + m * 60))
}

/// Format Unix seconds as "2026-11-26T03:20:11Z".
pub fn format_utc(ts: i64) -> String {
    let (y, m, d) = civil_from_days(ts.div_euclid(86_400));
    let s = ts.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_known_dates() {
        assert_eq!(parse_xmltv_time("19700101000000 +0000"), Some(0));
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_868_800), "2000-03-01T00:00:00Z");
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
    }

    #[test]
    fn applies_offsets() {
        let t = parse_xmltv_time("20261126030000 +0100").unwrap();
        assert_eq!(format_utc(t), "2026-11-26T02:00:00Z");
        let t = parse_xmltv_time("20261126030000 -0530").unwrap();
        assert_eq!(format_utc(t), "2026-11-26T08:30:00Z");
        let t = parse_xmltv_time("202611260300").unwrap();
        assert_eq!(format_utc(t), "2026-11-26T03:00:00Z");
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_xmltv_time(""), None);
        assert_eq!(parse_xmltv_time("2026"), None);
        assert_eq!(parse_xmltv_time("20261326000000"), None);
    }
}
