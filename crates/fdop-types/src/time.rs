//! Civil-time helpers (UTC, proleptic Gregorian) without external crates.

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date for days since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Unix seconds for a UTC civil date-time.
pub fn unix_from_utc(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: f64) -> f64 {
    days_from_civil(y, mo, d) as f64 * 86_400.0 + h as f64 * 3600.0 + mi as f64 * 60.0 + s
}

/// Format unix seconds as `YYYY-MM-DDTHH:MM:SS(.sss)Z`.
pub fn iso_utc(t: f64) -> String {
    if !t.is_finite() {
        return "NaN".into();
    }
    let days = (t / 86_400.0).floor();
    let rem = t - days * 86_400.0;
    let (y, m, d) = civil_from_days(days as i64);
    let h = (rem / 3600.0).floor();
    let mi = ((rem - h * 3600.0) / 60.0).floor();
    let s = rem - h * 3600.0 - mi * 60.0;
    if (s - s.round()).abs() < 5e-4 {
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            h as u32,
            mi as u32,
            s.round() as u32 % 60
        )
    } else {
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:06.3}Z",
            h as u32, mi as u32, s
        )
    }
}

/// `YYYY-MM-DD` of the UTC day containing `t`.
pub fn utc_date(t: f64) -> (i64, u32, u32) {
    civil_from_days((t / 86_400.0).floor() as i64)
}

/// Parse a civil date `YYYY-MM-DD` (also accepts `YYYY/MM/DD` and `YYYYMMDD`).
pub fn parse_date(s: &str) -> Result<(i64, u32, u32), String> {
    let t = s.trim();
    let parts: Vec<&str> = if t.contains('-') {
        t.split('-').collect()
    } else if t.contains('/') {
        t.split('/').collect()
    } else if t.len() == 8 && t.chars().all(|c| c.is_ascii_digit()) {
        vec![&t[0..4], &t[4..6], &t[6..8]]
    } else {
        return Err(format!("unrecognised date '{s}' (want YYYY-MM-DD)"));
    };
    if parts.len() != 3 {
        return Err(format!("unrecognised date '{s}' (want YYYY-MM-DD)"));
    }
    let y: i64 = parts[0].parse().map_err(|_| format!("bad year in '{s}'"))?;
    let m: u32 = parts[1]
        .parse()
        .map_err(|_| format!("bad month in '{s}'"))?;
    let d: u32 = parts[2].parse().map_err(|_| format!("bad day in '{s}'"))?;
    if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return Err(format!("invalid calendar date '{s}'"));
    }
    Ok((y, m, d))
}

pub fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// Parse an ISO-8601 instant. A missing zone designator means **UTC** (never local time).
/// Accepts `Z`, `+HH:MM`, `-HHMM`, fractional seconds, a space instead of `T`, a date alone,
/// and plain unix seconds.
pub fn parse_iso_utc(s: &str) -> Result<f64, String> {
    let t = s.trim();
    if let Ok(v) = t.parse::<f64>()
        && v > 1.0e8
    {
        return Ok(v);
    }
    let (date_part, rest) = match t.find(['T', 't', ' ']) {
        Some(i) => (&t[..i], &t[i + 1..]),
        None => (t, ""),
    };
    let (y, mo, d) = parse_date(date_part)?;
    if rest.is_empty() {
        return Ok(unix_from_utc(y, mo, d, 0, 0, 0.0));
    }
    // split zone
    let (clock, offset_s) = if let Some(c) = rest.strip_suffix(['Z', 'z']) {
        (c, 0.0)
    } else if let Some(i) = rest.rfind(['+', '-']) {
        let (c, z) = (&rest[..i], &rest[i..]);
        let sign = if z.starts_with('-') { -1.0 } else { 1.0 };
        let digits: String = z[1..].chars().filter(|ch| ch.is_ascii_digit()).collect();
        let (hh, mm) = match digits.len() {
            2 => (digits[..2].parse::<f64>().unwrap_or(0.0), 0.0),
            4 => (
                digits[..2].parse::<f64>().unwrap_or(0.0),
                digits[2..].parse::<f64>().unwrap_or(0.0),
            ),
            _ => return Err(format!("bad zone offset in '{s}'")),
        };
        (c, sign * (hh * 3600.0 + mm * 60.0))
    } else {
        (rest, 0.0)
    };
    let f: Vec<&str> = clock.split(':').collect();
    if f.is_empty() || f.len() > 3 {
        return Err(format!("bad time in '{s}'"));
    }
    let h: u32 = f[0].parse().map_err(|_| format!("bad hour in '{s}'"))?;
    let mi: u32 = if f.len() > 1 {
        f[1].parse().map_err(|_| format!("bad minute in '{s}'"))?
    } else {
        0
    };
    let sec: f64 = if f.len() > 2 {
        f[2].parse().map_err(|_| format!("bad second in '{s}'"))?
    } else {
        0.0
    };
    if h > 24 || mi > 59 || !(0.0..61.0).contains(&sec) {
        return Err(format!("time out of range in '{s}'"));
    }
    Ok(unix_from_utc(y, mo, d, h, mi, sec) - offset_s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_roundtrip() {
        for z in [-1000i64, 0, 1, 20_000, 20_704] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(days_from_civil(2026, 9, 8), 20_704);
    }

    #[test]
    fn iso_parse_defaults_to_utc() {
        let a = parse_iso_utc("2026-09-08T20:53:25Z").unwrap();
        let b = parse_iso_utc("2026-09-08T20:53:25").unwrap();
        let c = parse_iso_utc("2026-09-08T16:53:25-04:00").unwrap();
        let d = parse_iso_utc("2026-09-08 20:53:25.000+0000").unwrap();
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_eq!(a, d);
        assert_eq!(iso_utc(a), "2026-09-08T20:53:25Z");
        assert_eq!(
            parse_iso_utc("2026-09-08").unwrap(),
            unix_from_utc(2026, 9, 8, 0, 0, 0.0)
        );
    }

    #[test]
    fn rejects_bad_dates() {
        assert!(parse_date("2026-02-30").is_err());
        assert!(parse_date("2026-13-01").is_err());
        assert!(parse_date("2024-02-29").is_ok());
    }
}
