//! Geoid undulation N (m) from the NOAA NGS geoid API (GEOID18 over CONUS):
//! `https://geodesy.noaa.gov/api/geoid/ght?lat=..&lon=..` -> `{"geoidHeight": -30.839, ...}`.
//! MSL height H = ellipsoidal height h - N.
//!
//! NGS can answer a request with the height of another made at the same moment, while echoing the
//! coordinates it was asked for, so one reply cannot show that it is wrong: four simultaneous
//! requests on 2026-11-05 returned a Pennsylvania point's height and error for a point in Georgia.
//! A height is therefore taken only once two reads agree, and kept per point in the cache.

use crate::DataError;
use fdop_http::Net;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const NGS_GEOID_URL: &str = "https://geodesy.noaa.gov/api/geoid/ght";
/// Used when the API is unreachable: a typical CONUS value (range about -8 to -36 m).
pub const DEFAULT_GEOID_M: f64 = -30.0;
/// Most reads of one point before giving up on two agreeing. A crossed reply needs another request
/// at the same moment, so a third read settles a pair that differ.
const MAX_READS: usize = 3;
/// Two heights this close (m) are one reading: NGS prints three decimals.
const SAME_M: f64 = 0.0015;

pub fn geoid_height(net: &Net, lat: f64, lon: f64) -> Result<f64, DataError> {
    let url = format!("{NGS_GEOID_URL}?lat={lat:.6}&lon={lon:.6}");
    let r = net.get(&url, &[("Accept", "application/json")])?;
    let v = r.json().map_err(|e| DataError(format!("{url}: {e}")))?;
    let n = v
        .get("geoidHeight")
        .and_then(|x| x.as_f64())
        .ok_or_else(|| DataError(format!("{url}: no geoidHeight in response")))?;
    if !plausible(n) {
        return Err(DataError(format!("{url}: implausible geoidHeight {n}")));
    }
    Ok(n)
}

fn plausible(n: f64) -> bool {
    n.is_finite() && n.abs() <= 120.0
}

/// Pause before a repeat read: 300 ms to 1 s, set by the point. A kept height outlives the run, so
/// two reads of a point must not both cross. With a fixed pause, runs started together on different
/// points would read in step, and only different points can cross.
fn read_gap(lat: f64, lon: f64) -> Duration {
    let (a, b) = ((lat * 1e6).abs() as u64, (lon * 1e6).abs() as u64);
    Duration::from_millis(300 + a.wrapping_mul(31).wrapping_add(b) % 700)
}

/// The first height two reads agree on. `read` gets the read's index, from 0.
fn agreed_height(mut read: impl FnMut(usize) -> Result<f64, DataError>) -> Result<f64, DataError> {
    let mut seen = Vec::with_capacity(MAX_READS);
    for i in 0..MAX_READS {
        let n = read(i)?;
        if seen.iter().any(|s: &f64| (s - n).abs() < SAME_M) {
            return Ok(n);
        }
        seen.push(n);
    }
    Err(DataError(format!(
        "NGS geoid reads disagree: {seen:?} m (a crossed reply needs a simultaneous request)"
    )))
}

fn cache_path(cache_dir: &Path, lat: f64, lon: f64) -> PathBuf {
    cache_dir
        .join("geoid")
        .join(format!("{lat:.6}_{lon:.6}.txt"))
}

/// A height kept for this point. An unreadable or implausible file counts as none.
fn read_cached(path: &Path) -> Option<f64> {
    let n: f64 = std::fs::read_to_string(path).ok()?.trim().parse().ok()?;
    plausible(n).then_some(n)
}

/// Keep a height, renamed into place so a simultaneous run never reads half a file. A failure only
/// costs the next run its reads again.
fn write_cached(path: &Path, n: f64) {
    let Some(dir) = path.parent() else { return };
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let _ = std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(&tmp, format!("{n}\n")))
        .and_then(|()| std::fs::rename(&tmp, path));
}

/// Geoid height and its source: ("ngs") from the cache or from two NGS reads that agree, else
/// ("default") -30 m, which the caller reports as a warning.
pub fn geoid_height_or_default(cache_dir: &Path, lat: f64, lon: f64) -> (f64, &'static str) {
    let path = cache_path(cache_dir, lat, lon);
    if let Some(n) = read_cached(&path) {
        return (n, "ngs");
    }
    let read = Net::new().map_err(DataError::from).and_then(|net| {
        let net = net.with_retries(1);
        agreed_height(|i| {
            if i > 0 {
                std::thread::sleep(read_gap(lat, lon));
            }
            geoid_height(&net, lat, lon)
        })
    });
    match read {
        Ok(n) => {
            write_cached(&path, n);
            (n, "ngs")
        }
        Err(_) => (DEFAULT_GEOID_M, "default"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reads(v: &[f64]) -> (Result<f64, DataError>, usize) {
        let mut n = 0;
        let r = agreed_height(|i| {
            n += 1;
            Ok(v[i])
        });
        (r, n)
    }

    /// Two agreeing reads settle it; a crossed read costs a third; three different heights are no
    /// height. A crossed value must never be taken, whichever read it arrives on.
    #[test]
    fn a_height_needs_two_reads_to_agree() {
        let (r, n) = reads(&[-31.364, -31.364]);
        assert_eq!((r.unwrap(), n), (-31.364, 2));
        let (r, n) = reads(&[-24.07, -31.364, -31.364]);
        assert_eq!((r.unwrap(), n), (-31.364, 3));
        let (r, n) = reads(&[-31.364, -24.07, -31.364]);
        assert_eq!((r.unwrap(), n), (-31.364, 3));
        let (r, n) = reads(&[-31.364, -24.07, -33.657]);
        assert!(r.is_err());
        assert_eq!(n, 3);
    }

    /// A failed read is a failed lookup, not a read to skip: it stops the reads there.
    #[test]
    fn a_failed_read_stops_the_reads() {
        let mut n = 0;
        let r = agreed_height(|_| {
            n += 1;
            Err(DataError("down".into()))
        });
        assert!(r.is_err());
        assert_eq!(n, 1);
    }

    /// The pause stays in its range and differs by point, so runs started together on different
    /// points do not read in step.
    #[test]
    fn the_read_gap_depends_on_the_point() {
        let (a, b) = (read_gap(40.876466, -73.524951), read_gap(40.0, -75.0));
        assert_ne!(a, b);
        for g in [a, b, read_gap(0.0, 0.0), read_gap(-14.3, 170.7)] {
            assert!(
                g >= Duration::from_millis(300) && g < Duration::from_secs(1),
                "{g:?}"
            );
        }
    }

    /// A kept height comes back exactly, and the lookup returns it from the file, with no read of
    /// the service. Rubbish in the file counts as nothing kept.
    #[test]
    fn a_kept_height_is_used_and_a_bad_file_is_not() {
        let dir = std::env::temp_dir().join(format!("fdop-geoid-test-{}", std::process::id()));
        let (lat, lon) = (40.876466, -73.524951);
        let path = cache_path(&dir, lat, lon);
        write_cached(&path, -31.364);
        assert_eq!(read_cached(&path), Some(-31.364));
        assert_eq!(geoid_height_or_default(&dir, lat, lon), (-31.364, "ngs"));
        for bad in ["", "not a number", "-999.0", "NaN"] {
            std::fs::write(&path, bad).unwrap();
            assert_eq!(read_cached(&path), None, "{bad:?}");
        }
    }
}
