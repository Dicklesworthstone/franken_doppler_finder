//! Geoid undulation N (m) from the NOAA NGS geoid API (GEOID18 over CONUS):
//! `https://geodesy.noaa.gov/api/geoid/ght?lat=..&lon=..` -> `{"geoidHeight": -30.839, ...}`.
//! MSL height H = ellipsoidal height h - N.

use crate::DataError;
use fdop_http::Net;

pub const NGS_GEOID_URL: &str = "https://geodesy.noaa.gov/api/geoid/ght";
/// Used when the API is unreachable: a typical CONUS value (range about -8 to -36 m).
pub const DEFAULT_GEOID_M: f64 = -30.0;

pub fn geoid_height(net: &Net, lat: f64, lon: f64) -> Result<f64, DataError> {
    let url = format!("{NGS_GEOID_URL}?lat={lat:.6}&lon={lon:.6}");
    let r = net.get(&url, &[("Accept", "application/json")])?;
    let v = r.json().map_err(|e| DataError(format!("{url}: {e}")))?;
    let n = v
        .get("geoidHeight")
        .and_then(|x| x.as_f64())
        .ok_or_else(|| DataError(format!("{url}: no geoidHeight in response")))?;
    if !n.is_finite() || n.abs() > 120.0 {
        return Err(DataError(format!("{url}: implausible geoidHeight {n}")));
    }
    Ok(n)
}

/// Geoid height and its source: ("ngs") from the NGS API, else ("default") -30 m.
pub fn geoid_height_or_default(lat: f64, lon: f64) -> (f64, &'static str) {
    match Net::new()
        .map_err(DataError::from)
        .and_then(|net| geoid_height(&net.with_retries(1), lat, lon))
    {
        Ok(n) => (n, "ngs"),
        Err(_) => (DEFAULT_GEOID_M, "default"),
    }
}
