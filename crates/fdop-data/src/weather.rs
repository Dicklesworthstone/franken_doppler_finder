//! Historical atmosphere (temperature, humidity and wind profiles) for the acoustic model.
//!
//! Sources, tried in order (each with a short timeout; every response is cached under
//! `<cache>/weather/`):
//! 1. Open-Meteo Historical Forecast API: hourly surface values plus temperature and wind on
//!    pressure levels (925..300 hPa) on a <= 3x3 grid over the region; level heights from the
//!    hypsometric equation (archive of operational model runs, 2022 onwards).
//! 2. Radiosondes from the IEM RAOB archive (00Z/12Z soundings at the nearest NWS upper-air
//!    stations), with the boundary layer pulled to hourly ERA5 surface temperatures from the
//!    Open-Meteo archive API when that answers.
//! 3. ERA5 surface values with a 6.5 K/km standard lapse.
//! 4. A 15 C standard atmosphere (reported as a warning).

use crate::DataError;
use fdop_types::BBox;
use fdop_types::atmo::{
    AdjustedAtmosphere, Atmosphere, GriddedAtmosphere, Level, ScatteredAtmosphere,
    StandardAtmosphere, standard_levels, wind_en,
};
use fdop_types::time::utc_date;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub const OM_HIST_FORECAST: &str = "https://historical-forecast-api.open-meteo.com/v1/forecast";
pub const OM_ARCHIVE: &str = "https://archive-api.open-meteo.com/v1/archive";
pub const IEM_RAOB: &str = "https://mesonet.agron.iastate.edu/json/raob.py";
pub const IEM_RAOB_NETWORK: &str =
    "https://mesonet.agron.iastate.edu/geojson/network.py?network=RAOB";
const LEVELS_HPA: [u32; 5] = [925, 850, 700, 500, 300];
const RD_OVER_G: f64 = 287.05 / 9.80665;
const KT: f64 = 0.514444;
const PRESSURE_LEVEL_TIMEOUT_S: u64 = 40;

/// Allow the preferred request plus the original fallback wait after audio analysis.
/// Fallback services start only after the preferred source fails.
pub const FETCH_WAIT_TIMEOUT: Duration = Duration::from_secs(150 + PRESSURE_LEVEL_TIMEOUT_S);

/// Weather grid points over a bbox: ascending lats and lons.
pub fn grid_axes(b: &BBox, max_step_deg: f64, max_n: usize) -> (Vec<f64>, Vec<f64>) {
    let axis = |lo: f64, hi: f64| -> Vec<f64> {
        let n = (((hi - lo) / max_step_deg).ceil() as usize + 1).clamp(1, max_n);
        if n == 1 || hi - lo < 1e-6 {
            return vec![0.5 * (lo + hi)];
        }
        (0..n)
            .map(|k| lo + (hi - lo) * k as f64 / (n - 1) as f64)
            .collect()
    };
    (axis(b.south, b.north), axis(b.west, b.east))
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn date_str(t: f64) -> String {
    let (y, m, d) = utc_date(t);
    format!("{y:04}-{m:02}-{d:02}")
}

/// GET a JSON document through the cache. `max_age_s`: re-fetch cached copies older than this.
fn get_cached(
    url: &str,
    cache: &Path,
    offline: bool,
    timeout_s: u64,
    max_age_s: Option<f64>,
    retries: u32,
) -> Result<serde_json::Value, DataError> {
    get_cached_validated(url, cache, offline, (timeout_s, max_age_s, retries), Ok)
}

/// Validate the requested weather before trusting or replacing its cache. A response may
/// contain useful hours outside this request, so a failed refresh preserves the old bytes.
fn get_cached_validated<T>(
    url: &str,
    cache: &Path,
    offline: bool,
    (timeout_s, max_age_s, retries): (u64, Option<f64>, u32),
    validate: impl Fn(serde_json::Value) -> Result<T, DataError>,
) -> Result<T, DataError> {
    let dir = cache.join("weather");
    let path = dir.join(format!("{:016x}.json", fnv(url)));
    let fresh = |p: &Path| {
        max_age_s.is_none_or(|age| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|e| e.as_secs_f64() < age)
        })
    };
    let mut cached_error = None;
    if fresh(&path)
        && let Ok(b) = std::fs::read(&path)
        && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&b)
    {
        match validate(v) {
            Ok(parsed) => return Ok(parsed),
            Err(e) => cached_error = Some(e),
        }
    }
    if offline {
        return Err(cached_error.unwrap_or_else(|| DataError::new("offline and not cached")));
    }
    // a service that just failed for this exact query is not retried for 30 minutes
    let fail = path.with_extension("fail");
    if let Ok(m) = std::fs::metadata(&fail).and_then(|m| m.modified())
        && m.elapsed().is_ok_and(|e| e.as_secs() < 1800)
    {
        return Err(DataError::new(format!(
            "failed recently ({}); retried after 30 min",
            std::fs::read_to_string(&fail).unwrap_or_default().trim()
        )));
    }
    let res = (|| -> Result<(serde_json::Value, Vec<u8>), DataError> {
        let net =
            fdop_http::Net::with_timeout(Duration::from_secs(timeout_s))?.with_retries(retries);
        let r = net.get(url, &[("Accept", "application/json")])?;
        let v = r.json().map_err(|e| DataError(format!("{url}: {e}")))?;
        if v.get("error").and_then(|e| e.as_bool()) == Some(true) {
            return Err(DataError(
                v.get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("error")
                    .to_string(),
            ));
        }
        Ok((v, r.body))
    })();
    let (v, body) = match res {
        Ok(x) => x,
        Err(e) => {
            let msg: String = e.to_string().chars().take(200).collect();
            let _ = fdop_http::write_atomic(&fail, msg.as_bytes());
            return Err(e);
        }
    };
    // A response can be unusable only for these hours. Do not turn that into a
    // URL-wide failure marker that would block another window's valid weather.
    let parsed = validate(v)?;
    fdop_http::write_atomic(&path, &body)?;
    Ok(parsed)
}

fn arr(h: &serde_json::Value, key: &str, i: usize) -> f64 {
    h.get(key)
        .and_then(|a| a.get(i))
        .and_then(|x| x.as_f64())
        .unwrap_or(f64::NAN)
}

fn times_of(h: &serde_json::Value) -> Result<Vec<f64>, DataError> {
    let times: Vec<f64> = h
        .get("time")
        .and_then(|t| t.as_array())
        .ok_or_else(|| DataError::new("open-meteo: no time axis"))?
        .iter()
        .map(|t| {
            t.as_i64()
                .map(|x| x as f64)
                .ok_or_else(|| DataError::new("open-meteo: invalid hourly timestamp"))
        })
        .collect::<Result<_, _>>()?;
    if times.len() < 2 {
        return Err(DataError::new("open-meteo: fewer than two hours"));
    }
    if times.windows(2).any(|p| p[1] - p[0] != 3600.0) {
        return Err(DataError::new("open-meteo: non-hourly time axis"));
    }
    Ok(times)
}

/// Original row indices bracketing the entire requested interval. A point on an exact
/// hour needs only that row; otherwise retain both sides of every interpolation.
fn hours_for_window(
    times: &[f64],
    t_start: f64,
    t_end: f64,
) -> Result<std::ops::Range<usize>, DataError> {
    if t_start < times[0] || t_end > times[times.len() - 1] {
        return Err(DataError::new(
            "open-meteo: hourly data do not cover the requested window",
        ));
    }
    let start = times.partition_point(|&t| t <= t_start).saturating_sub(1);
    let end = times.partition_point(|&t| t < t_end) + 1;
    Ok(start..end)
}

/// Selected original rows of one Open-Meteo hourly block -> level lists. With `levels`, pressure
/// levels are placed by the hypsometric equation from the surface pressure and temperature;
/// without, the profile is the surface value with a standard lapse.
fn parse_location(
    loc: &serde_json::Value,
    levels: bool,
    hours: std::ops::Range<usize>,
) -> Result<Vec<Vec<Level>>, DataError> {
    let elev = loc.get("elevation").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let h = loc
        .get("hourly")
        .ok_or_else(|| DataError::new("open-meteo: no hourly block"))?;
    let mut out = Vec::with_capacity(hours.len());
    for i in hours {
        let t2 = arr(h, "temperature_2m", i);
        if !t2.is_finite() {
            return Err(DataError::new("open-meteo: missing surface temperature"));
        }
        let rh2 = arr(h, "relative_humidity_2m", i) / 100.0;
        let rh2 = if rh2.is_finite() { rh2 } else { 0.5 };
        let (ws, wd) = (arr(h, "wind_speed_10m", i), arr(h, "wind_direction_10m", i));
        let (we, wn) = if ws.is_finite() && wd.is_finite() {
            wind_en(ws, wd)
        } else {
            (0.0, 0.0)
        };
        if !levels {
            out.push(standard_levels(t2, elev + 2.0, rh2, we, wn));
            continue;
        }
        let ps = arr(h, "surface_pressure", i);
        if !ps.is_finite() {
            return Err(DataError::new("open-meteo: missing surface pressure"));
        }
        let mut lv = vec![Level {
            z_msl: elev + 2.0,
            t_c: t2,
            rh: rh2,
            wind_e: we,
            wind_n: wn,
        }];
        let (mut z_prev, mut p_prev, mut t_prev) = (elev, ps, t2);
        for p in LEVELS_HPA {
            let pf = p as f64;
            let t = arr(h, &format!("temperature_{p}hPa"), i);
            if pf >= p_prev || !t.is_finite() {
                continue;
            }
            let z = z_prev + RD_OVER_G * (0.5 * (t + t_prev) + 273.15) * (p_prev / pf).ln();
            let (ws, wd) = (
                arr(h, &format!("wind_speed_{p}hPa"), i),
                arr(h, &format!("wind_direction_{p}hPa"), i),
            );
            let (we, wn) = if ws.is_finite() && wd.is_finite() {
                wind_en(ws, wd)
            } else {
                (f64::NAN, f64::NAN)
            };
            lv.push(Level {
                z_msl: z,
                t_c: t,
                rh: 0.3,
                wind_e: we,
                wind_n: wn,
            });
            (z_prev, p_prev, t_prev) = (z, pf, t);
        }
        if lv.len() < 3 {
            return Err(DataError::new("open-meteo: too few pressure levels"));
        }
        // the surface wind alone would be copied to every height and pass for winds aloft
        if lv[1..].iter().all(|l| l.wind_e.is_nan()) {
            return Err(DataError::new("open-meteo: no winds aloft"));
        }
        fill_wind(&mut lv);
        out.push(lv);
    }
    Ok(out)
}

/// Replace NaN winds by linear interpolation in height (constant beyond the ends). At least one
/// level must have a wind: callers refuse a profile that has none rather than call it calm.
fn fill_wind(lv: &mut [Level]) {
    let good: Vec<(f64, f64, f64)> = lv
        .iter()
        .filter(|l| l.wind_e.is_finite() && l.wind_n.is_finite())
        .map(|l| (l.z_msl, l.wind_e, l.wind_n))
        .collect();
    for l in lv.iter_mut() {
        if l.wind_e.is_finite() && l.wind_n.is_finite() {
            continue;
        }
        let k = good.partition_point(|g| g.0 <= l.z_msl);
        let (we, wn) = if k == 0 {
            (good[0].1, good[0].2)
        } else if k >= good.len() {
            (good[good.len() - 1].1, good[good.len() - 1].2)
        } else {
            let (a, b) = (good[k - 1], good[k]);
            let w = (l.z_msl - a.0) / (b.0 - a.0);
            (a.1 + w * (b.1 - a.1), a.2 + w * (b.2 - a.2))
        };
        (l.wind_e, l.wind_n) = (we, wn);
    }
}

fn om_grid(
    url_base: &str,
    b: &BBox,
    t_start: f64,
    t_end: f64,
    cache: &Path,
    offline: bool,
    levels: bool,
) -> Result<GriddedAtmosphere, DataError> {
    if !t_start.is_finite() || !t_end.is_finite() || t_start > t_end {
        return Err(DataError::new("open-meteo: invalid requested time window"));
    }
    let (lats, lons) = grid_axes(b, 1.5, 3);
    let mut qla = Vec::new();
    let mut qlo = Vec::new();
    for &la in &lats {
        for &lo in &lons {
            qla.push(format!("{la:.3}"));
            qlo.push(format!("{lo:.3}"));
        }
    }
    let mut vars = vec![
        "temperature_2m".to_string(),
        "relative_humidity_2m".into(),
        "wind_speed_10m".into(),
        "wind_direction_10m".into(),
    ];
    if levels {
        vars.push("surface_pressure".into());
        for p in LEVELS_HPA {
            for v in ["temperature", "wind_speed", "wind_direction"] {
                vars.push(format!("{v}_{p}hPa"));
            }
        }
    }
    let url = format!(
        "{url_base}?latitude={}&longitude={}&start_date={}&end_date={}&hourly={}&wind_speed_unit=ms&timeformat=unixtime&timezone=GMT",
        qla.join(","),
        qlo.join(","),
        date_str(t_start),
        // A late-night interval needs the next day's 00Z interpolation bracket.
        date_str((t_end / 3600.0).ceil() * 3600.0),
        vars.join(",")
    );
    get_cached_validated(
        &url,
        cache,
        offline,
        (
            if levels { PRESSURE_LEVEL_TIMEOUT_S } else { 45 },
            None,
            if levels { 0 } else { 1 },
        ),
        |v| parse_om_grid(v, &lats, &lons, t_start, t_end, levels),
    )
}

fn parse_om_grid(
    v: serde_json::Value,
    lats: &[f64],
    lons: &[f64],
    t_start: f64,
    t_end: f64,
    levels: bool,
) -> Result<GriddedAtmosphere, DataError> {
    let locs: Vec<serde_json::Value> = match v {
        serde_json::Value::Array(a) => a,
        o => vec![o],
    };
    if locs.len() != lats.len() * lons.len() {
        return Err(DataError(format!(
            "open-meteo: {} locations returned, {} asked",
            locs.len(),
            lats.len() * lons.len()
        )));
    }
    let mut times: Option<Vec<f64>> = None;
    let mut per_loc = Vec::with_capacity(locs.len());
    for l in &locs {
        let t = times_of(&l["hourly"])?;
        if times.as_ref().is_some_and(|t0| *t0 != t) {
            return Err(DataError::new("open-meteo: mismatched time axes"));
        }
        let hours = hours_for_window(&t, t_start, t_end)?;
        let p = parse_location(l, levels, hours)?;
        times.get_or_insert(t);
        per_loc.push(p);
    }
    let times = times.unwrap_or_default();
    let hours = hours_for_window(&times, t_start, t_end)?;
    let mut profiles = Vec::with_capacity(hours.len() * locs.len());
    for it in 0..hours.len() {
        for p in &per_loc {
            profiles.push(p[it].clone());
        }
    }
    let src = if levels {
        "Open-Meteo historical forecast (pressure levels)"
    } else {
        "Open-Meteo ERA5 archive (surface + standard lapse)"
    };
    GriddedAtmosphere::new(
        lats.to_vec(),
        lons.to_vec(),
        times[hours.start],
        times[1] - times[0],
        &profiles,
        src.to_string(),
    )
    .map_err(DataError)
}

/// IEM gives lifetime bounds as dates; keep the whole final UTC date eligible. Current
/// online status says nothing about an older recording. Missing or malformed dates cannot rule
/// out a station; neither can an internally inconsistent lifetime.
fn raob_station_overlaps(properties: &serde_json::Value, window: (f64, f64)) -> bool {
    let date = |key: &str| {
        properties
            .get(key)
            .and_then(|v| v.as_str())
            .and_then(|s| fdop_types::time::parse_date(s).ok())
            .map(|(y, m, d)| fdop_types::time::unix_from_utc(y, m, d, 0, 0, 0.0))
    };
    let (begin, end) = (date("archive_begin"), date("archive_end"));
    if begin.zip(end).is_some_and(|(a, b)| a > b) {
        return true;
    }
    begin.is_none_or(|t| t <= window.1) && end.is_none_or(|t| window.0 < t + 86_400.0)
}

/// Stations whose archives may cover the requested soundings: (id, lat, lon, elevation).
fn raob_stations(
    cache: &Path,
    offline: bool,
    window: (f64, f64),
) -> Result<Vec<(String, f64, f64, f64)>, DataError> {
    let v = get_cached(
        IEM_RAOB_NETWORK,
        cache,
        offline,
        30,
        Some(30.0 * 86400.0),
        1,
    )
    .or_else(|e| {
        // a stale copy beats none
        get_cached(IEM_RAOB_NETWORK, cache, true, 30, None, 0).map_err(|_| e)
    })?;
    let feats = v
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| DataError::new("RAOB network: no features"))?;
    let mut out = Vec::new();
    for f in feats {
        let id = f.get("id").and_then(|x| x.as_str()).unwrap_or("");
        let pr = &f["properties"];
        // Alaska and Hawaii use non-K identifiers. The provider's country metadata
        // identifies US stations; old cached listings without it retain their K stations.
        let us_station = pr
            .get("country")
            .and_then(|v| v.as_str())
            .map_or_else(|| id.starts_with('K'), |country| country == "US");
        if !(id.len() == 4
            && id
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            && us_station)
            || !raob_station_overlaps(pr, window)
        {
            continue;
        }
        let c = &f["geometry"]["coordinates"];
        let (lo, la) = (
            c[0].as_f64().unwrap_or(f64::NAN),
            c[1].as_f64().unwrap_or(f64::NAN),
        );
        if la.is_finite() && lo.is_finite() {
            out.push((
                id.to_string(),
                la,
                lo,
                pr.get("elevation").and_then(|x| x.as_f64()).unwrap_or(0.0),
            ));
        }
    }
    Ok(out)
}

/// Parse an IEM RAOB profile into levels (heights MSL, winds from knots).
fn parse_raob(profile: &serde_json::Value) -> Vec<Level> {
    let mut lv = Vec::new();
    let mut winds: Vec<(f64, f64, f64)> = Vec::new();
    for r in profile.as_array().into_iter().flatten() {
        let g = |k: &str| r.get(k).and_then(|x| x.as_f64()).unwrap_or(f64::NAN);
        let (z, t, td, dd, ff) = (g("hght"), g("tmpc"), g("dwpc"), g("drct"), g("sknt"));
        if z.is_finite() && dd.is_finite() && ff.is_finite() {
            let (we, wn) = wind_en(ff * KT, dd);
            winds.push((z, we, wn));
        }
        if z.is_finite() && t.is_finite() && z < 16_000.0 {
            let es = |x: f64| (17.625 * x / (x + 243.04)).exp();
            let rh = if td.is_finite() {
                (es(td) / es(t)).clamp(0.0, 1.0)
            } else {
                0.3
            };
            lv.push(Level {
                z_msl: z,
                t_c: t,
                rh,
                wind_e: f64::NAN,
                wind_n: f64::NAN,
            });
        }
    }
    winds.sort_by(|a, b| a.0.total_cmp(&b.0));
    lv.sort_by(|a, b| a.z_msl.total_cmp(&b.z_msl));
    for l in lv.iter_mut() {
        if let Some(w) = winds.iter().find(|w| (w.0 - l.z_msl).abs() < 1.0) {
            (l.wind_e, l.wind_n) = (w.1, w.2);
        }
    }
    // add wind-only levels' information by interpolation
    let mut with_w: Vec<Level> = lv.clone();
    for &(z, we, wn) in &winds {
        if !with_w.iter().any(|l| (l.z_msl - z).abs() < 1.0) {
            with_w.push(Level {
                z_msl: z,
                t_c: f64::NAN,
                rh: 0.3,
                wind_e: we,
                wind_n: wn,
            });
        }
    }
    with_w.sort_by(|a, b| a.z_msl.total_cmp(&b.z_msl));
    // temperatures of wind-only levels by interpolation; drop those outside the temperature range
    let temps: Vec<(f64, f64)> = lv.iter().map(|l| (l.z_msl, l.t_c)).collect();
    let mut out: Vec<Level> = Vec::new();
    for mut l in with_w {
        if !l.t_c.is_finite() {
            let k = temps.partition_point(|t| t.0 <= l.z_msl);
            if k == 0 || k >= temps.len() {
                continue;
            }
            let (a, b) = (temps[k - 1], temps[k]);
            l.t_c = a.1 + (l.z_msl - a.0) / (b.0 - a.0) * (b.1 - a.1);
        }
        out.push(l);
    }
    // no wind reported is not calm air, and the lowest level's alone would pass for winds aloft:
    // the caller drops a sounding with too few levels
    if out.iter().skip(1).all(|l| l.wind_e.is_nan()) {
        return Vec::new();
    }
    fill_wind(&mut out);
    out
}

fn parse_raob_response(v: serde_json::Value) -> Result<Vec<Level>, DataError> {
    let profile = v
        .get("profiles")
        .and_then(|p| p.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| DataError::new("RAOB: no sounding profile"))?;
    let levels = parse_raob(&profile["profile"]);
    if levels.len() >= 5 && levels.last().is_some_and(|l| l.z_msl > 5000.0) {
        Ok(levels)
    } else {
        Err(DataError::new(
            "RAOB: sounding lacks usable levels through 5 km",
        ))
    }
}

fn raob_url(t: f64, station: &str) -> String {
    let (y, m, d) = utc_date(t);
    let hh = ((t.rem_euclid(86400.0)) / 3600.0).round() as u32;
    // IEM prefixes three-character inputs with K. Keep that spelling for existing
    // continental cache keys, but pass other US station identifiers intact.
    let station = station.strip_prefix('K').unwrap_or(station);
    format!("{IEM_RAOB}?ts={y:04}{m:02}{d:02}{hh:02}00&station={station}")
}

fn raob_atmosphere(
    b: &BBox,
    t_start: f64,
    t_end: f64,
    cache: &Path,
    offline: bool,
) -> Result<ScatteredAtmosphere, DataError> {
    // Select stations for the 00Z/12Z brackets too, not just times inside the recording.
    let t0 = (t_start / 43200.0).floor() * 43200.0;
    let t1 = (t_end / 43200.0).ceil() * 43200.0;
    let (clat, clon) = b.center();
    let mut st = raob_stations(cache, offline, (t0, t1))?;
    st.sort_by(|x, y| {
        fdop_types::geo::haversine_m(clat, clon, x.1, x.2)
            .total_cmp(&fdop_types::geo::haversine_m(clat, clon, y.1, y.2))
    });
    // the nearest station plus any other within 1.6x its distance (max 4) inside 700 km
    let d0 = st.first().map_or(f64::INFINITY, |s| {
        fdop_types::geo::haversine_m(clat, clon, s.1, s.2)
    });
    let half_diag = fdop_types::geo::haversine_m(b.south, b.west, b.north, b.east) / 2.0;
    let chosen: Vec<_> = st
        .into_iter()
        .filter(|s| {
            let d = fdop_types::geo::haversine_m(clat, clon, s.1, s.2);
            d <= 700_000.0 && d <= (1.6 * d0).max(d0 + half_diag)
        })
        .take(4)
        .collect();
    if chosen.is_empty() {
        return Err(DataError::new("no upper-air station within 700 km"));
    }
    let mut stations = Vec::new();
    let (mut n_ok, mut n_tried) = (0, 0);
    for (id, la, lo, _) in &chosen {
        let mut series = Vec::new();
        let mut t = t0;
        while t <= t1 + 1.0 {
            n_tried += 1;
            if let Ok(lv) = get_cached_validated(
                &raob_url(t, id),
                cache,
                offline,
                (20, None, 1),
                parse_raob_response,
            ) {
                series.push((t, lv));
                n_ok += 1;
            }
            t += 43200.0;
        }
        stations.push((*la, *lo, series));
    }
    if n_ok == 0 {
        return Err(DataError::new("no radiosonde soundings retrieved"));
    }
    let names: Vec<&str> = chosen.iter().map(|c| c.0.as_str()).collect();
    ScatteredAtmosphere::new(
        stations,
        format!(
            "NWS radiosondes {} (IEM RAOB archive, {n_ok} of {n_tried} soundings usable)",
            names.join(", ")
        ),
    )
    .map_err(DataError)
}

fn join_weather<T>(h: std::thread::JoinHandle<Result<T, DataError>>) -> Result<T, DataError> {
    h.join()
        .unwrap_or_else(|_| Err(DataError::new("weather thread panicked")))
}

/// The atmosphere a run uses, with what it lacks.
pub struct Weather {
    pub atmo: Arc<dyn Atmosphere>,
    /// Every fallback taken; empty only when Open-Meteo pressure levels answered.
    pub warnings: Vec<String>,
    /// Winds aloft are modelled, from pressure levels or radiosondes. Without them fitted speeds
    /// are not corrected for wind, so callers widen their speed gates.
    pub winds_aloft: bool,
}

/// Best available atmosphere over `bbox` for UTC `[t_start, t_end]`.
pub fn fetch_atmosphere(
    bbox: &BBox,
    t_start: f64,
    t_end: f64,
    cache: &Path,
    offline: bool,
) -> Weather {
    fetch_atmosphere_with_progress(bbox, t_start, t_end, cache, offline, |_| {})
}

/// Fetch the final atmosphere as in [`fetch_atmosphere`], reporting usable fallback
/// profiles while the other service is still loading. The callback runs on the source
/// threads; a caller with a deadline can retain these profiles instead of losing weather
/// that already arrived. The final return still combines surface data with radiosondes.
pub fn fetch_atmosphere_with_progress(
    bbox: &BBox,
    t_start: f64,
    t_end: f64,
    cache: &Path,
    offline: bool,
    on_fallback: impl Fn(Weather) + Send + Sync + 'static,
) -> Weather {
    let (b, c1) = (*bbox, cache.to_path_buf());
    let h_om = std::thread::spawn(move || {
        om_grid(OM_HIST_FORECAST, &b, t_start, t_end, &c1, offline, true)
    });
    let (c2, c3) = (cache.to_path_buf(), cache.to_path_buf());
    fetch_with_fallbacks(
        join_weather(h_om),
        move || om_grid(OM_ARCHIVE, &b, t_start, t_end, &c2, offline, false),
        move || raob_atmosphere(&b, t_start, t_end, &c3, offline),
        on_fallback,
    )
}

type AtmosphereResult = Result<Arc<dyn Atmosphere>, DataError>;

// The final adjustment and a provisional result share immutable profiles. Keep the
// public AdjustedAtmosphere interface unchanged while retaining them in both places.
struct SharedAtmosphere(Arc<dyn Atmosphere>);

impl Atmosphere for SharedAtmosphere {
    fn medium(&self, lat: f64, lon: f64, t: f64, z_a: f64, z_b: f64) -> fdop_types::atmo::Medium {
        self.0.medium(lat, lon, t, z_a, z_b)
    }

    fn describe(&self) -> String {
        self.0.describe()
    }
}

fn publish_fallback(
    result: Result<impl Atmosphere + 'static, DataError>,
    preferred_failure: &str,
    winds_aloft: bool,
    on_fallback: &dyn Fn(Weather),
) -> AtmosphereResult {
    let result = result.map(|a| Arc::new(a) as Arc<dyn Atmosphere>);
    if let Ok(a) = &result {
        on_fallback(Weather {
            atmo: Arc::clone(a),
            warnings: vec![
                preferred_failure.into(),
                if winds_aloft {
                    "hourly surface temperatures not applied; boundary layer from the 12-hourly soundings"
                } else {
                    "no upper-air data: standard lapse above the surface and no winds aloft"
                }
                .into(),
            ],
            winds_aloft,
        });
    }
    result
}

fn fetch_with_fallbacks(
    om: Result<GriddedAtmosphere, DataError>,
    surface: impl FnOnce() -> Result<GriddedAtmosphere, DataError> + Send + 'static,
    raob: impl FnOnce() -> Result<ScatteredAtmosphere, DataError> + Send + 'static,
    on_fallback: impl Fn(Weather) + Send + Sync + 'static,
) -> Weather {
    choose_atmosphere(om, |preferred_failure| {
        // A pressure-level profile already includes surface weather. Only fetch the
        // alternatives when it fails: radiosondes can require many serial requests.
        // Each source publishes its own usable result before either join can block it.
        let on_fallback = Arc::new(on_fallback);
        let surface_update = Arc::clone(&on_fallback);
        let surface_failure = preferred_failure.to_string();
        let raob_failure = preferred_failure.to_string();
        let h_sf = std::thread::spawn(move || {
            publish_fallback(surface(), &surface_failure, false, &*surface_update)
        });
        let h_rb = std::thread::spawn(move || {
            publish_fallback(raob(), &raob_failure, true, &*on_fallback)
        });
        (join_weather(h_sf), join_weather(h_rb))
    })
}

fn choose_atmosphere(
    om: Result<GriddedAtmosphere, DataError>,
    fallbacks: impl FnOnce(&str) -> (AtmosphereResult, AtmosphereResult),
) -> Weather {
    let weather = |atmo: Arc<dyn Atmosphere>, warnings: Vec<String>, winds_aloft: bool| Weather {
        atmo,
        warnings,
        winds_aloft,
    };
    let mut warn = Vec::new();
    match om {
        Ok(a) => return weather(Arc::new(a), warn, true),
        Err(e) => warn.push(format!("Open-Meteo pressure levels unavailable ({e})")),
    }
    let (surface, raob) = fallbacks(&warn[0]);
    match raob {
        Ok(r) => {
            // radiosondes give real temperature and wind profiles; with hourly surface
            // temperatures the boundary layer follows the diurnal cycle too
            return match surface {
                Ok(s) => weather(
                    Arc::new(AdjustedAtmosphere {
                        base: Box::new(SharedAtmosphere(r)),
                        surface: Box::new(SharedAtmosphere(s)),
                        depth: 1500.0,
                    }),
                    warn,
                    true,
                ),
                Err(e) => {
                    warn.push(format!(
                        "hourly surface temperatures unavailable ({e}); boundary layer from the 12-hourly soundings"
                    ));
                    weather(r, warn, true)
                }
            };
        }
        Err(e) => warn.push(format!("radiosondes unavailable ({e})")),
    }
    match surface {
        Ok(a) => {
            warn.push(
                "no upper-air data: standard lapse above the surface and no winds aloft".into(),
            );
            return weather(a, warn, false);
        }
        Err(e) => warn.push(format!("surface weather unavailable ({e})")),
    }
    warn.push(
        "using a 15 C standard atmosphere: Doppler curves of high aircraft will fit less tightly"
            .into(),
    );
    weather(
        Arc::new(StandardAtmosphere::new(15.0, 0.0, 0.5)),
        warn,
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lazy_fallbacks_keep_previous_wait_budget() {
        // Previously all sources started together. A fallback that could arrive
        // before the old pipeline deadline must still have that opportunity now
        // that it starts after the preferred attempt. Model the schedule without
        // network requests or wall-clock sleeps.
        for audio_s in [0, 10, 149, 600] {
            for preferred_s in [0, 1, PRESSURE_LEVEL_TIMEOUT_S] {
                for fallback_s in [0, 1, 91, 140, audio_s + 150] {
                    if preferred_s.max(fallback_s) <= audio_s + 150 {
                        assert!(
                            preferred_s + fallback_s <= audio_s + FETCH_WAIT_TIMEOUT.as_secs(),
                            "audio={audio_s}s preferred={preferred_s}s fallback={fallback_s}s"
                        );
                    }
                }
            }
        }
    }

    fn test_grid(source: &str, t_c: f64) -> GriddedAtmosphere {
        GriddedAtmosphere::new(
            vec![0.0],
            vec![0.0],
            0.0,
            3600.0,
            &[standard_levels(t_c, 0.0, 0.5, 1.0, 2.0)],
            source.into(),
        )
        .unwrap()
    }

    fn test_soundings() -> ScatteredAtmosphere {
        ScatteredAtmosphere::new(
            vec![(
                0.0,
                0.0,
                vec![(0.0, standard_levels(18.0, 0.0, 0.5, 3.0, 4.0))],
            )],
            "test soundings".into(),
        )
        .unwrap()
    }

    #[test]
    fn pressure_levels_skip_all_fallback_requests() {
        let expected = test_grid("test pressure levels", 25.0);
        let Weather {
            atmo: a,
            warnings: warn,
            winds_aloft,
        } = fetch_with_fallbacks(
            Ok(test_grid("test pressure levels", 25.0)),
            || panic!("pressure levels must not load surface fallback weather"),
            || panic!("pressure levels must not load radiosonde fallback weather"),
            |_| panic!("pressure levels must not publish fallback weather"),
        );
        assert_eq!(a.describe(), expected.describe());
        assert_eq!(
            a.medium(0.0, 0.0, 0.0, 0.0, 2000.0),
            expected.medium(0.0, 0.0, 0.0, 0.0, 2000.0)
        );
        assert!(warn.is_empty());
        assert!(winds_aloft);
    }

    #[test]
    fn fallback_progress_precedes_the_other_service() {
        for surface_first in [true, false] {
            let (surface_tx, surface_rx) = std::sync::mpsc::channel();
            let (raob_tx, raob_rx) = std::sync::mpsc::channel();
            let (update_tx, update_rx) = std::sync::mpsc::channel();
            let fetch = std::thread::spawn(move || {
                fetch_with_fallbacks(
                    Err(DataError::new("pressure failed")),
                    move || {
                        surface_rx.recv().unwrap();
                        Ok(test_grid("test surface", -10.0))
                    },
                    move || {
                        raob_rx.recv().unwrap();
                        Ok(test_soundings())
                    },
                    move |weather| assert!(update_tx.send(weather).is_ok()),
                )
            });
            if surface_first {
                surface_tx.send(()).unwrap();
            } else {
                raob_tx.send(()).unwrap();
            }
            // The other source cannot finish until released below. This timeout only
            // bounds a broken test; no wall-clock delay selects which source wins.
            let first = update_rx.recv_timeout(Duration::from_secs(5));
            let _ = surface_tx.send(());
            let _ = raob_tx.send(());
            let final_weather = fetch.join().unwrap();
            let first = first.expect("a ready source must publish before the other finishes");
            let expected: Box<dyn Atmosphere> = if surface_first {
                Box::new(test_grid("test surface", -10.0))
            } else {
                Box::new(test_soundings())
            };
            assert_eq!(first.atmo.describe(), expected.describe());
            assert_eq!(first.winds_aloft, !surface_first);
            assert_eq!(
                first.warnings[0],
                "Open-Meteo pressure levels unavailable (pressure failed)"
            );
            assert!(first.warnings[1].contains(if surface_first {
                "no winds aloft"
            } else {
                "hourly surface temperatures not applied"
            }));
            let combined = AdjustedAtmosphere {
                base: Box::new(test_soundings()),
                surface: Box::new(test_grid("test surface", -10.0)),
                depth: 1500.0,
            };
            for z in [0.0, 1000.0, 5000.0] {
                assert_eq!(
                    first.atmo.medium(0.0, 0.0, 0.0, 0.0, z),
                    expected.medium(0.0, 0.0, 0.0, 0.0, z)
                );
                assert_eq!(
                    final_weather.atmo.medium(0.0, 0.0, 0.0, 0.0, z),
                    combined.medium(0.0, 0.0, 0.0, 0.0, z)
                );
            }
            assert!(final_weather.winds_aloft);
            assert_eq!(
                final_weather.warnings,
                ["Open-Meteo pressure levels unavailable (pressure failed)"]
            );
        }
    }

    #[test]
    fn failed_fallbacks_do_not_publish_profiles() {
        let (tx, rx) = std::sync::mpsc::channel();
        let weather = fetch_with_fallbacks(
            Err(DataError::new("pressure failed")),
            || Err(DataError::new("surface failed")),
            || Err(DataError::new("soundings failed")),
            move |weather| assert!(tx.send(weather).is_ok()),
        );
        assert!(rx.try_recv().is_err());
        assert!(!weather.winds_aloft);
        let expected = StandardAtmosphere::new(15.0, 0.0, 0.5);
        assert_eq!(
            weather.atmo.medium(0.0, 0.0, 0.0, 0.0, 2000.0),
            expected.medium(0.0, 0.0, 0.0, 0.0, 2000.0)
        );
        assert_eq!(weather.warnings.len(), 4);
    }

    #[test]
    fn fallback_combines_soundings_with_hourly_surface_weather() {
        let expected = AdjustedAtmosphere {
            base: Box::new(test_soundings()),
            surface: Box::new(test_grid("test surface", 26.0)),
            depth: 1500.0,
        };
        let Weather {
            atmo: a,
            warnings: warn,
            winds_aloft,
        } = choose_atmosphere(Err(DataError::new("pressure failed")), |_| {
            (
                Ok(Arc::new(test_grid("test surface", 26.0))),
                Ok(Arc::new(test_soundings())),
            )
        });
        assert_eq!(a.describe(), expected.describe());
        for z in [0.0, 1000.0, 5000.0] {
            assert_eq!(
                a.medium(0.0, 0.0, 0.0, 0.0, z),
                expected.medium(0.0, 0.0, 0.0, 0.0, z)
            );
        }
        assert_eq!(
            warn,
            ["Open-Meteo pressure levels unavailable (pressure failed)"]
        );
        assert!(winds_aloft);
    }

    #[test]
    fn sounding_fallback_keeps_both_failure_warnings() {
        let expected = test_soundings();
        let Weather {
            atmo: a,
            warnings: warn,
            winds_aloft,
        } = choose_atmosphere(Err(DataError::new("pressure failed")), |_| {
            (
                Err(DataError::new("surface failed")),
                Ok(Arc::new(test_soundings())),
            )
        });
        assert_eq!(a.describe(), expected.describe());
        assert_eq!(
            a.medium(0.0, 0.0, 0.0, 0.0, 2000.0),
            expected.medium(0.0, 0.0, 0.0, 0.0, 2000.0)
        );
        assert_eq!(
            warn,
            [
                "Open-Meteo pressure levels unavailable (pressure failed)",
                "hourly surface temperatures unavailable (surface failed); boundary layer from the 12-hourly soundings",
            ]
        );
        assert!(winds_aloft);
    }

    #[test]
    fn surface_fallback_keeps_upper_air_failure_warnings() {
        let expected = test_grid("test surface", 26.0);
        let Weather {
            atmo: a,
            warnings: warn,
            winds_aloft,
        } = choose_atmosphere(Err(DataError::new("pressure failed")), |_| {
            (
                Ok(Arc::new(test_grid("test surface", 26.0))),
                Err(DataError::new("soundings failed")),
            )
        });
        assert_eq!(a.describe(), expected.describe());
        assert_eq!(
            a.medium(0.0, 0.0, 0.0, 0.0, 2000.0),
            expected.medium(0.0, 0.0, 0.0, 0.0, 2000.0)
        );
        assert_eq!(
            warn,
            [
                "Open-Meteo pressure levels unavailable (pressure failed)",
                "radiosondes unavailable (soundings failed)",
                "no upper-air data: standard lapse above the surface and no winds aloft",
            ]
        );
        assert!(!winds_aloft);
    }

    #[test]
    fn standard_fallback_keeps_all_failure_warnings() {
        let expected = StandardAtmosphere::new(15.0, 0.0, 0.5);
        let Weather {
            atmo: a,
            warnings: warn,
            winds_aloft,
        } = choose_atmosphere(Err(DataError::new("pressure failed")), |_| {
            (
                Err(DataError::new("surface failed")),
                Err(DataError::new("soundings failed")),
            )
        });
        assert_eq!(a.describe(), expected.describe());
        assert_eq!(
            a.medium(0.0, 0.0, 0.0, 0.0, 2000.0),
            expected.medium(0.0, 0.0, 0.0, 0.0, 2000.0)
        );
        assert_eq!(
            warn,
            [
                "Open-Meteo pressure levels unavailable (pressure failed)",
                "radiosondes unavailable (soundings failed)",
                "surface weather unavailable (surface failed)",
                "using a 15 C standard atmosphere: Doppler curves of high aircraft will fit less tightly",
            ]
        );
        assert!(!winds_aloft);
    }

    #[test]
    fn grid_axes_cover_box() {
        let (la, lo) = grid_axes(&BBox::new(40.0, -80.0, 45.0, -71.0), 1.5, 3);
        assert_eq!((la.len(), lo.len()), (3, 3));
        assert_eq!(la[0], 40.0);
        assert_eq!(*lo.last().unwrap(), -71.0);
        let (la, lo) = grid_axes(&BBox::new(40.8, -73.6, 40.9, -73.5), 1.5, 3);
        assert_eq!((la.len(), lo.len()), (2, 2));
    }

    fn open_meteo_fixture(day: i64, hours: usize) -> serde_json::Value {
        let mut hourly = serde_json::json!({
            "time": (0..hours).map(|i| day + i as i64 * 3600).collect::<Vec<_>>(),
            "temperature_2m": (0..hours).map(|i| 15.0 + i as f64 * 0.25).collect::<Vec<_>>(),
            "relative_humidity_2m": vec![50.0; hours],
            "wind_speed_10m": (0..hours).map(|i| 2.0 + i as f64 * 0.1).collect::<Vec<_>>(),
            "wind_direction_10m": vec![270.0; hours],
            "surface_pressure": vec![1010.0; hours],
        });
        for (j, p) in LEVELS_HPA.into_iter().enumerate() {
            hourly[format!("temperature_{p}hPa")] = serde_json::json!(
                (0..hours)
                    .map(|i| 10.0 - j as f64 * 12.0 + i as f64 * 0.25)
                    .collect::<Vec<_>>()
            );
            hourly[format!("wind_speed_{p}hPa")] = serde_json::json!(
                (0..hours)
                    .map(|i| 5.0 + j as f64 + i as f64 * 0.2)
                    .collect::<Vec<_>>()
            );
            hourly[format!("wind_direction_{p}hPa")] = serde_json::json!(vec![270.0; hours]);
        }
        serde_json::json!({"elevation": 30.0, "hourly": hourly})
    }

    fn cache_open_meteo_fixture(
        dir: &Path,
        levels: bool,
        end_date: &str,
        value: &serde_json::Value,
    ) {
        let base = if levels { OM_HIST_FORECAST } else { OM_ARCHIVE };
        let mut vars =
            "temperature_2m,relative_humidity_2m,wind_speed_10m,wind_direction_10m".to_string();
        if levels {
            vars.push_str(",surface_pressure");
            for p in LEVELS_HPA {
                vars.push_str(&format!(
                    ",temperature_{p}hPa,wind_speed_{p}hPa,wind_direction_{p}hPa"
                ));
            }
        }
        // Independent query spelling verifies the requested date bracket and existing cache keys.
        let url = format!(
            "{base}?latitude=40.876&longitude=-73.525&start_date=2026-09-08&end_date={end_date}&hourly={vars}&wind_speed_unit=ms&timeformat=unixtime&timezone=GMT"
        );
        let path = dir.join("weather").join(format!("{:016x}.json", fnv(&url)));
        std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    }

    fn weather_test_cache(url: &str, label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "fdop-weather-recovery-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(dir.join("weather")).unwrap();
        let path = dir.join("weather").join(format!("{:016x}.json", fnv(url)));
        (dir, path)
    }

    fn serve_weather_once(value: &serde_json::Value) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let body = serde_json::to_vec(value).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/weather", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < deadline, "no weather refresh");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(e) => panic!("weather accept: {e}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            socket.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                assert!(request.len() < 8192, "oversized weather request");
                let mut byte = [0u8];
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            assert!(request.starts_with(b"GET /weather HTTP/1.1\r\n"));
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            socket.write_all(&body).unwrap();
        });
        (url, server)
    }

    #[test]
    fn cached_hourly_weather_refreshes_missing_required_rows() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let (lat, lon) = (40.876466, -73.524951);
        for levels in [false, true] {
            let complete = open_meteo_fixture(day as i64, 24);
            let (url, server) = serve_weather_once(&complete);
            let (dir, path) = weather_test_cache(&url, "hourly");
            let mut incomplete = complete.clone();
            incomplete["hourly"]["temperature_2m"][5] = serde_json::Value::Null;
            let prior = serde_json::to_vec(&incomplete).unwrap();
            std::fs::write(&path, &prior).unwrap();
            let parse = |v| {
                parse_om_grid(
                    v,
                    &[lat],
                    &[lon],
                    day + 4.25 * 3600.0,
                    day + 5.75 * 3600.0,
                    levels,
                )
            };
            let read = |offline| {
                get_cached_validated(&url, &dir, offline, (2, None, 0), parse)
            };
            let error = read(true).err().expect("offline must refuse the missing hour");
            assert!(error.0.contains("missing surface temperature"), "{error}");
            assert_eq!(std::fs::read(&path).unwrap(), prior);
            let recovered = read(false).expect("refresh restores the required hour");
            server.join().unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), serde_json::to_vec(&complete).unwrap());
            let expected = parse(complete).unwrap();
            let sample = |a: &GriddedAtmosphere| {
                a.medium(lat, lon, day + 4.5 * 3600.0, 30.0, 5000.0)
            };
            assert_eq!(sample(&recovered), sample(&expected));
            // The one-shot server is gone: both online and offline reads reuse the good cache.
            assert_eq!(sample(&read(false).unwrap()), sample(&expected));
            assert_eq!(sample(&read(true).unwrap()), sample(&expected));
        }
    }

    #[test]
    fn invalid_weather_refresh_preserves_earlier_usable_hours() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let (lat, lon) = (40.876466, -73.524951);
        let (url, server) = serve_weather_once(&serde_json::json!({"hourly": {}}));
        let (dir, path) = weather_test_cache(&url, "preserve");
        let mut partial = open_meteo_fixture(day as i64, 24);
        partial["hourly"]["temperature_2m"][6] = serde_json::Value::Null;
        let prior = serde_json::to_vec(&partial).unwrap();
        std::fs::write(&path, &prior).unwrap();
        let parse = |v, hour| {
            parse_om_grid(
                v,
                &[lat],
                &[lon],
                day + hour * 3600.0,
                day + hour * 3600.0,
                true,
            )
        };
        let error = get_cached_validated(&url, &dir, false, (2, None, 0), |v| parse(v, 6.0))
            .err()
            .expect("an unusable replacement must fail");
        server.join().unwrap();
        assert!(error.0.contains("no time axis"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), prior);
        assert!(!path.with_extension("fail").exists());
        // An error for one hour must not block the old response's usable hours.
        let old = get_cached_validated(&url, &dir, false, (2, None, 0), |v| parse(v, 2.0))
            .unwrap();
        assert_eq!(
            old.medium(lat, lon, day + 7200.0, 30.0, 5000.0),
            parse(partial, 2.0).unwrap().medium(lat, lon, day + 7200.0, 30.0, 5000.0)
        );
    }

    #[test]
    fn cached_empty_soundings_refresh_when_a_usable_profile_arrives() {
        let profile: Vec<_> = (0..6)
            .map(|i| {
                serde_json::json!({
                    "hght": 100.0 + 1200.0 * f64::from(i),
                    "tmpc": 15.0 - 6.0 * f64::from(i),
                    "dwpc": 5.0 - 6.0 * f64::from(i),
                    "drct": 270.0,
                    "sknt": 20.0,
                })
            })
            .collect();
        let complete = serde_json::json!({"profiles": [{"profile": profile}]});
        for incomplete in [
            serde_json::json!({"profiles": []}),
            serde_json::json!({"profiles": [{"profile": []}]}),
        ] {
            let (url, server) = serve_weather_once(&complete);
            let (dir, path) = weather_test_cache(&url, "sounding");
            let prior = serde_json::to_vec(&incomplete).unwrap();
            std::fs::write(&path, &prior).unwrap();
            let read = |offline| {
                get_cached_validated(&url, &dir, offline, (2, None, 0), parse_raob_response)
            };
            assert!(read(true).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), prior);
            let levels = read(false).unwrap();
            server.join().unwrap();
            assert_eq!(levels.len(), 6);
            assert!(levels.iter().all(|l| (l.wind_e - 20.0 * KT).abs() < 1e-12));
            assert_eq!(read(false).unwrap(), levels);
            assert_eq!(read(true).unwrap(), levels);
            assert_eq!(std::fs::read(&path).unwrap(), serde_json::to_vec(&complete).unwrap());
        }
    }

    #[test]
    fn cached_weather_keeps_required_hours_when_other_rows_are_missing() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let (lat, lon) = (40.876466, -73.524951);
        let bbox = BBox::new(lat, lon, lat, lon);
        for levels in [true, false] {
            let dir = std::env::temp_dir().join(format!(
                "fdop-hour-window-{levels}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(dir.join("weather")).unwrap();
            let base = if levels { OM_HIST_FORECAST } else { OM_ARCHIVE };
            for (start, end, rows, count, end_date) in [
                (4.25, 5.75, 4..7, 24, "2026-09-08"),
                (6.0, 6.0, 6..7, 24, "2026-09-08"),
                (6.25, 6.25, 6..8, 24, "2026-09-08"),
                (23.25, 23.75, 23..25, 48, "2026-09-09"),
            ] {
                let (t0, t1) = (day + start * 3600.0, day + end * 3600.0);
                let complete = open_meteo_fixture(day as i64, count);
                cache_open_meteo_fixture(&dir, levels, end_date, &complete);
                let complete_window = om_grid(base, &bbox, t0, t1, &dir, true, levels).unwrap();
                let full_day = GriddedAtmosphere::new(
                    vec![lat],
                    vec![lon],
                    day,
                    3600.0,
                    &parse_location(&complete, levels, 0..count).unwrap(),
                    "complete day".into(),
                )
                .unwrap();
                let mut partial = complete.clone();
                for (key, column) in partial["hourly"].as_object_mut().unwrap() {
                    if key != "time" {
                        for (i, value) in column.as_array_mut().unwrap().iter_mut().enumerate() {
                            if !rows.contains(&i) {
                                *value = serde_json::Value::Null;
                            }
                        }
                    }
                }
                cache_open_meteo_fixture(&dir, levels, end_date, &partial);
                let recovered = om_grid(base, &bbox, t0, t1, &dir, true, levels).unwrap();
                assert_eq!(recovered.describe(), complete_window.describe());
                for fraction in [0.0, 0.137, 0.5, 0.863, 1.0] {
                    let t = t0 + (t1 - t0) * fraction;
                    for (za, zb) in [(30.0, 1000.0), (2500.0, 6000.0)] {
                        let got = recovered.medium(lat, lon, t, za, zb);
                        assert_eq!(got, complete_window.medium(lat, lon, t, za, zb));
                        let all_hours = full_day.medium(lat, lon, t, za, zb);
                        // Re-basing the grid's time origin may round interpolation
                        // weights differently; preserve the full-day physical result.
                        assert!(
                            (got.c - all_hours.c).abs() < 1e-12,
                            "{got:?} != {all_hours:?}"
                        );
                        assert!((got.wind_e - all_hours.wind_e).abs() < 1e-12);
                        assert!((got.wind_n - all_hours.wind_n).abs() < 1e-12);
                    }
                }
                let weather = fetch_atmosphere(&bbox, t0, t1, &dir, true);
                assert_eq!(weather.winds_aloft, levels);
                assert_eq!(weather.atmo.describe(), recovered.describe());
                assert_eq!(weather.warnings.is_empty(), levels);

                // Both interpolation endpoints remain mandatory, including a next-day bracket.
                for (key, missing) in [
                    ("temperature_2m", "missing surface temperature"),
                    ("surface_pressure", "missing surface pressure"),
                ] {
                    if !levels && key == "surface_pressure" {
                        continue;
                    }
                    for i in [rows.start, rows.end - 1] {
                        let mut broken = partial.clone();
                        broken["hourly"][key][i] = serde_json::Value::Null;
                        cache_open_meteo_fixture(&dir, levels, end_date, &broken);
                        let err = om_grid(base, &bbox, t0, t1, &dir, true, levels)
                            .err()
                            .unwrap();
                        assert!(err.0.contains(missing), "{err}");
                        let fallback = fetch_atmosphere(&bbox, t0, t1, &dir, true);
                        assert!(
                            fallback
                                .warnings
                                .iter()
                                .any(|w| w.contains("using a 15 C standard atmosphere"))
                        );
                        assert!(!fallback.winds_aloft);
                    }
                }
            }
        }
    }

    #[test]
    fn weather_hours_preserve_indices_and_require_real_brackets() {
        let times = times_of(&serde_json::json!({"time": [0, 3600, 7200, 10800]})).unwrap();
        for (start, end, expected) in [
            (0.0, 0.0, 0..1),
            (3600.0, 3600.0, 1..2),
            (10800.0, 10800.0, 3..4),
            (3600.0, 7200.0, 1..3),
            (3600.1, 7199.9, 1..3),
            (3600.1, 7200.1, 1..4),
        ] {
            assert_eq!(hours_for_window(&times, start, end).unwrap(), expected);
        }
        for (start, end) in [
            (-1.0, 0.0),
            (10800.0, 10801.0),
            (-7200.0, -3600.0),
            (14400.0, 18000.0),
        ] {
            assert!(hours_for_window(&times, start, end).is_err());
        }
        for axis in [
            serde_json::json!([0, null, 7200]),
            serde_json::json!([0, "3600", 7200]),
            serde_json::json!([0, true, 7200]),
            serde_json::json!([0, 3600, 10800]),
            serde_json::json!([0, 3600, 3600]),
            serde_json::json!([7200, 3600, 0]),
            serde_json::json!([0]),
        ] {
            assert!(times_of(&serde_json::json!({"time": axis})).is_err());
        }
    }

    #[test]
    fn cached_weather_rejects_shifted_location_time_axes() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let (lat, lon) = (40.876466, -73.524951);
        let bbox = BBox::new(lat, lon, lat, lon + 0.01);
        let dir = std::env::temp_dir().join(format!(
            "fdop-hour-grid-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("weather")).unwrap();
        let url = format!(
            "{OM_ARCHIVE}?latitude=40.876,40.876&longitude=-73.525,-73.515&start_date=2026-09-08&end_date=2026-09-08&hourly=temperature_2m,relative_humidity_2m,wind_speed_10m,wind_direction_10m&wind_speed_unit=ms&timeformat=unixtime&timezone=GMT"
        );
        let path = dir.join("weather").join(format!("{:016x}.json", fnv(&url)));
        let first = open_meteo_fixture(day as i64, 24);
        let second = open_meteo_fixture(day as i64 + 3600, 24);
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!([first, second])).unwrap(),
        )
        .unwrap();
        let err = om_grid(
            OM_ARCHIVE,
            &bbox,
            day + 4.25 * 3600.0,
            day + 5.75 * 3600.0,
            &dir,
            true,
            false,
        )
        .err()
        .unwrap();
        assert!(err.0.contains("mismatched time axes"), "{err}");
    }

    #[test]
    fn parses_pressure_level_payload_with_hypsometric_heights() {
        let mut hourly = serde_json::json!({
            "time": [1788900000, 1788903600],
            "temperature_2m": [26.0, 25.0], "relative_humidity_2m": [40, 45],
            "wind_speed_10m": [2.0, 2.0], "wind_direction_10m": [270, 270],
            "surface_pressure": [1023.0, 1023.0],
        });
        // the 2026-09-08 21Z profile over Long Island
        for (p, t, ws) in [
            (925, 17.4, 2.8),
            (850, 13.4, 4.3),
            (700, 5.6, 11.1),
            (500, -9.8, 17.1),
            (300, -36.0, 25.0),
        ] {
            hourly[format!("temperature_{p}hPa")] = serde_json::json!([t, t]);
            hourly[format!("wind_speed_{p}hPa")] = serde_json::json!([ws, ws]);
            hourly[format!("wind_direction_{p}hPa")] = serde_json::json!([315, 315]);
        }
        let loc = serde_json::json!({"elevation": 30.0, "hourly": hourly});
        let t = times_of(&loc["hourly"]).unwrap();
        let p = parse_location(&loc, true, 0..t.len()).unwrap();
        assert_eq!(t.len(), 2);
        // hypsometric 500 hPa height within 60 m of the model's geopotential height (5866 m)
        let z500 = p[0]
            .iter()
            .find(|l| (l.t_c + 9.8).abs() < 1e-9)
            .unwrap()
            .z_msl;
        assert!((z500 - 5866.0).abs() < 60.0, "{z500}");
        let g =
            GriddedAtmosphere::new(vec![40.9], vec![-73.5], t[0], 3600.0, &p, "t".into()).unwrap();
        let m = g.medium(40.9, -73.5, t[0], 32.0, 5557.0);
        assert!((m.c - 336.5).abs() < 1.5, "{}", m.c);
        assert!(m.wind_e > 5.0 && m.wind_n < -4.0, "NW wind blows SE: {m:?}");
    }

    #[test]
    fn pressure_levels_without_winds_aloft_are_refused() {
        let mut hourly = serde_json::json!({
            "time": [1788900000, 1788903600],
            "temperature_2m": [26.0, 25.0], "relative_humidity_2m": [40, 45],
            "wind_speed_10m": [2.0, 2.0], "wind_direction_10m": [270, 270],
            "surface_pressure": [1023.0, 1023.0],
        });
        for (p, t) in [(925, 17.4), (850, 13.4), (700, 5.6), (500, -9.8)] {
            hourly[format!("temperature_{p}hPa")] = serde_json::json!([t, t]);
        }
        let loc = |h: &serde_json::Value| serde_json::json!({"elevation": 30.0, "hourly": h});
        let err = parse_location(&loc(&hourly), true, 0..2).err().unwrap();
        assert!(err.0.contains("no winds aloft"), "{err}");
        // one hour without them is enough: the grid would hold a false profile there
        hourly["wind_speed_500hPa"] = serde_json::json!([20.0, null]);
        hourly["wind_direction_500hPa"] = serde_json::json!([315, 315]);
        let err = parse_location(&loc(&hourly), true, 0..2).err().unwrap();
        assert!(err.0.contains("no winds aloft"), "{err}");
        // one level with them is fine: the others take it, and the surface keeps its own
        hourly["wind_speed_500hPa"] = serde_json::json!([20.0, 20.0]);
        let p = parse_location(&loc(&hourly), true, 0..2).unwrap();
        let (surface, top) = (p[0].first().unwrap(), p[0].last().unwrap());
        assert!(surface.wind_e > 1.9 && surface.wind_e < 2.1, "{surface:?}");
        assert!(top.wind_e > 14.0 && top.wind_n < -14.0, "{top:?}");
    }

    #[test]
    fn a_sounding_without_wind_is_not_calm_air() {
        let levels = |with_wind: i32| {
            (0..6)
                .map(|i| {
                    let wind = i < with_wind;
                    serde_json::json!({
                        "hght": 100.0 + 1200.0 * f64::from(i),
                        "tmpc": 20.0 - 7.0 * f64::from(i),
                        "dwpc": 5.0 - 7.0 * f64::from(i),
                        "drct": wind.then_some(270.0),
                        "sknt": wind.then_some(20.0),
                    })
                })
                .collect::<Vec<_>>()
        };
        assert!(parse_raob(&serde_json::json!(levels(0))).is_empty());
        assert!(
            parse_raob(&serde_json::json!(levels(1))).is_empty(),
            "the surface wind alone is not winds aloft"
        );
        assert_eq!(parse_raob(&serde_json::json!(levels(2))).len(), 6);
        let lv = parse_raob(&serde_json::json!(levels(6)));
        assert_eq!(lv.len(), 6);
        assert!(
            lv.iter().all(|l| (l.wind_e - 20.0 * KT).abs() < 0.01),
            "{lv:?}"
        );
    }

    #[test]
    fn radiosonde_request_ids_preserve_us_prefixes_and_continental_cache_keys() {
        let t = fdop_types::time::unix_from_utc(2024, 9, 8, 12, 0, 0.0);
        for (id, requested) in [("KAAA", "AAA"), ("PAAA", "PAAA"), ("PHAA", "PHAA")] {
            assert_eq!(
                raob_url(t, id),
                format!("{IEM_RAOB}?ts=202409081200&station={requested}")
            );
        }
    }

    #[test]
    fn us_radiosonde_prefixes_recover_cached_winds() {
        use fdop_types::atmo::sound_speed;
        use fdop_types::time::unix_from_utc;
        let (lat, lon) = (40.876466, -73.524951);
        let day = unix_from_utc(2024, 9, 8, 0, 0, 0.0);
        let bbox = BBox::new(lat - 0.01, lon - 0.01, lat + 0.01, lon + 0.01);
        // Synthetic station identities and profiles at the permitted reference coordinate.
        // State labels exercise provider conventions, not real station observations.
        let feature = |id: &str, properties: serde_json::Value| {
            serde_json::json!({
                "id": id,
                "properties": properties,
                "geometry": {"coordinates": [lon, lat]},
            })
        };
        for (label, id, requested, properties) in [
            (
                "alaska",
                "PAAA",
                "PAAA",
                serde_json::json!({"country": "US", "state": "AK"}),
            ),
            (
                "hawaii",
                "PHAA",
                "PHAA",
                serde_json::json!({"country": "US", "state": "HI"}),
            ),
            (
                "continental",
                "KAAA",
                "AAA",
                serde_json::json!({"country": "US", "state": "NY"}),
            ),
            ("legacy", "KAAA", "AAA", serde_json::json!({})),
        ] {
            let dir = std::env::temp_dir().join(format!(
                "fdop-raob-prefix-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(dir.join("weather")).unwrap();
            let put = |url: &str, value: serde_json::Value| {
                let path = dir.join("weather").join(format!("{:016x}.json", fnv(url)));
                std::fs::write(path, value.to_string()).unwrap();
            };
            put(
                IEM_RAOB_NETWORK,
                serde_json::json!({"features": [
                    feature("CNEA", serde_json::json!({"country": "CA"})),
                    feature("PNEA", serde_json::json!({"country": "JP"})),
                    feature("KNEA", serde_json::json!({"country": "CA"})),
                    feature("_ALL", serde_json::json!({"country": "US"})),
                    feature("PNON", serde_json::json!({})),
                    feature(id, properties),
                ]}),
            );
            for (hour, wind) in [("00", 20.0), ("12", 40.0)] {
                let profile: Vec<_> = (0..6)
                    .map(|i| {
                        serde_json::json!({
                            "hght": 100.0 + 1200.0 * f64::from(i),
                            "tmpc": 10.0,
                            "dwpc": 10.0,
                            "drct": 270.0,
                            "sknt": wind,
                        })
                    })
                    .collect();
                // Spell the expected provider URL independently of raob_url: truncating a
                // non-K identifier must miss the cache, rather than create its own oracle.
                let url = format!("{IEM_RAOB}?ts=20240908{hour}00&station={requested}");
                put(
                    &url,
                    serde_json::json!({"profiles": [{"profile": profile}]}),
                );
            }
            let chosen = raob_stations(&dir, true, (day, day + 43_200.0)).unwrap();
            assert_eq!(chosen.len(), 1, "{label}: {chosen:?}");
            assert_eq!(chosen[0].0, id);
            let weather = fetch_atmosphere(&bbox, day + 3600.0, day + 39_600.0, &dir, true);
            let source = weather.atmo.describe();
            assert!(
                weather.winds_aloft,
                "{label}: {source}; {:?}",
                weather.warnings
            );
            assert!(
                source.contains(id) && source.contains("2 of 2 soundings usable"),
                "{source}"
            );
            let medium = weather.atmo.medium(lat, lon, day + 21_600.0, 100.0, 5000.0);
            assert!(
                (medium.c - sound_speed(10.0, 1.0)).abs() < 1e-8,
                "{medium:?}"
            );
            assert!((medium.wind_e - 30.0 * KT).abs() < 1e-8, "{medium:?}");
            assert!(medium.wind_n.abs() < 1e-8, "{medium:?}");
            assert!(
                weather
                    .warnings
                    .iter()
                    .any(|w| w.contains("hourly surface temperatures unavailable"))
            );
        }
    }

    #[test]
    fn radiosonde_station_lifetimes_use_inclusive_utc_dates() {
        use fdop_types::time::unix_from_utc;
        let day = unix_from_utc(2024, 9, 8, 0, 0, 0.0);
        let closed = serde_json::json!({
            "archive_begin": "2024-09-08",
            "archive_end": "2024-09-08",
            "online": false,
        });
        assert!(raob_station_overlaps(&closed, (day, day)));
        assert!(raob_station_overlaps(
            &closed,
            (day + 86_399.0, day + 86_399.0)
        ));
        assert!(!raob_station_overlaps(&closed, (day - 1.0, day - 1.0)));
        assert!(!raob_station_overlaps(
            &closed,
            (day + 86_400.0, day + 86_400.0)
        ));
        assert!(raob_station_overlaps(&closed, (day - 43_200.0, day)));

        for unknown in [
            serde_json::json!({}),
            serde_json::json!({"archive_begin": null, "archive_end": null, "online": false}),
            serde_json::json!({"archive_begin": "bad", "archive_end": "2024-02-30"}),
            serde_json::json!({"archive_begin": 2024, "archive_end": false}),
            serde_json::json!({"archive_begin": "2025-01-01", "archive_end": "2024-01-01"}),
        ] {
            assert!(raob_station_overlaps(&unknown, (day, day)), "{unknown}");
        }
        assert!(!raob_station_overlaps(
            &serde_json::json!({"archive_begin": "2024-09-09"}),
            (day, day + 86_399.0)
        ));
        assert!(!raob_station_overlaps(
            &serde_json::json!({"archive_end": "2024-09-07"}),
            (day, day + 86_400.0)
        ));
    }

    #[test]
    fn historical_radiosondes_use_closed_stations_and_sounding_brackets() {
        use fdop_types::atmo::sound_speed;
        use fdop_types::time::unix_from_utc;
        let (lat, lon) = (40.876466, -73.524951);
        let day = unix_from_utc(2024, 9, 8, 0, 0, 0.0);
        let feature = |id: &str, properties: serde_json::Value| {
            serde_json::json!({
                "id": id,
                "properties": properties,
                "geometry": {"coordinates": [lon, lat]},
            })
        };
        // Four present-day stations would consume every slot before the closed station.
        // All are synthetic and colocated so only the archive dates decide eligibility.
        let mut historical = Vec::new();
        for id in ["KNEA", "KNEB", "KNEC", "KNED"] {
            historical.push(feature(
                id,
                serde_json::json!({"archive_begin": "2025-01-01", "online": true}),
            ));
        }
        historical.push(feature(
            "KOLD",
            serde_json::json!({
                "archive_begin": "2020-01-01",
                "archive_end": "2024-09-08",
                "online": false,
            }),
        ));
        let bracket = vec![feature(
            "KBEG",
            serde_json::json!({"archive_begin": "2024-09-09", "online": true}),
        )];
        for (label, features, window, id, times, count) in [
            (
                "closed",
                historical,
                (day + 3_600.0, day + 46_800.0),
                "KOLD",
                vec![day, day + 43_200.0],
                "2 of 3 soundings usable",
            ),
            (
                "next-bracket",
                bracket,
                (day + 54_000.0, day + 57_600.0),
                "KBEG",
                vec![day + 86_400.0],
                "1 of 2 soundings usable",
            ),
        ] {
            let dir = std::env::temp_dir().join(format!(
                "fdop-raob-lifetime-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(dir.join("weather")).unwrap();
            let put = |url: &str, v: serde_json::Value| {
                let path = dir.join("weather").join(format!("{:016x}.json", fnv(url)));
                std::fs::write(path, v.to_string()).unwrap();
            };
            put(IEM_RAOB_NETWORK, serde_json::json!({"features": features}));
            let profile: Vec<_> = (0..6)
                .map(|i| {
                    serde_json::json!({
                        "hght": 100.0 + 1200.0 * f64::from(i),
                        "tmpc": 10.0,
                        "dwpc": 10.0,
                        "drct": 270.0,
                        "sknt": 20.0,
                    })
                })
                .collect();
            for t in times {
                put(
                    &raob_url(t, id),
                    serde_json::json!({"profiles": [{"profile": &profile}]}),
                );
            }
            let chosen = raob_stations(
                &dir,
                true,
                (
                    (window.0 / 43_200.0).floor() * 43_200.0,
                    (window.1 / 43_200.0).ceil() * 43_200.0,
                ),
            )
            .unwrap();
            assert_eq!(chosen.len(), 1, "{label}: {chosen:?}");
            assert_eq!(chosen[0].0, id);
            let bbox = BBox::new(lat - 0.01, lon - 0.01, lat + 0.01, lon + 0.01);
            let atmo = raob_atmosphere(&bbox, window.0, window.1, &dir, true)
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            let source = atmo.describe();
            assert!(source.contains(id) && source.contains(count), "{source}");
            let medium = atmo.medium(lat, lon, window.0, 100.0, 5000.0);
            assert!((medium.c - sound_speed(10.0, 1.0)).abs() < 1e-8);
            assert!((medium.wind_e - 20.0 * KT).abs() < 1e-8);
            assert!(medium.wind_n.abs() < 1e-8);
        }
    }

    #[test]
    fn radiosonde_source_counts_the_soundings_it_could_not_use() {
        let dir = std::env::temp_dir().join(format!("fdop-raob-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("weather")).unwrap();
        let put = |url: &str, v: serde_json::Value| {
            let path = dir.join("weather").join(format!("{:016x}.json", fnv(url)));
            std::fs::write(path, v.to_string()).unwrap();
        };
        put(
            IEM_RAOB_NETWORK,
            serde_json::json!({"features": [{
                "id": "KAAA",
                "properties": {},
                "geometry": {"coordinates": [-73.0, 40.0]},
            }]}),
        );
        let sounding = |wind: bool| {
            let profile: Vec<_> = (0..6)
                .map(|i| {
                    serde_json::json!({
                        "hght": 100.0 + 1200.0 * f64::from(i),
                        "tmpc": 20.0 - 7.0 * f64::from(i),
                        "dwpc": 5.0 - 7.0 * f64::from(i),
                        "drct": wind.then_some(270.0),
                        "sknt": wind.then_some(20.0),
                    })
                })
                .collect();
            serde_json::json!({"profiles": [{"profile": profile}]})
        };
        // the window needs 00Z, 12Z and the next 00Z: one good, one with no wind, one missing
        let day = 20_000.0 * 86_400.0;
        put(&raob_url(day, "KAAA"), sounding(true));
        put(&raob_url(day + 43_200.0, "KAAA"), sounding(false));
        let b = BBox::new(39.5, -73.5, 40.5, -72.5);
        let a = raob_atmosphere(&b, day + 3_600.0, day + 46_800.0, &dir, true).unwrap();
        let described = a.describe();
        assert!(described.contains("1 of 3 soundings usable"), "{described}");
    }

    #[test]
    fn parses_raob_profile() {
        let prof = serde_json::json!([
            {"pres": 1021.0, "hght": 20.0, "tmpc": 20.2, "dwpc": 18.0, "drct": 0.0, "sknt": 0.0},
            {"pres": 1000.0, "hght": 202.0, "tmpc": 22.8, "dwpc": 13.8, "drct": 250.0, "sknt": 6.0},
            {"pres": 925.0, "hght": 874.0, "tmpc": 16.8, "dwpc": 10.8, "drct": 295.0, "sknt": 5.0},
            {"pres": null, "hght": 1219.0, "tmpc": null, "dwpc": null, "drct": 330.0, "sknt": 8.0},
            {"pres": 850.0, "hght": 1580.0, "tmpc": 12.0, "dwpc": 5.0, "drct": null, "sknt": null},
            {"pres": 500.0, "hght": 5870.0, "tmpc": -10.0, "dwpc": -20.0, "drct": 320.0, "sknt": 34.0}
        ]);
        let lv = parse_raob(&prof);
        assert_eq!(lv.len(), 6);
        let w = lv.iter().find(|l| (l.z_msl - 1219.0).abs() < 1.0).unwrap();
        assert!(w.t_c < 16.8 && w.t_c > 12.0);
        assert!(lv.iter().all(|l| l.wind_e.is_finite()));
        let top = lv.last().unwrap();
        assert!((top.wind_e - 34.0 * KT * (40f64).to_radians().sin()).abs() < 0.2);
    }
}
