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
    AdjustedAtmosphere, Atmosphere, GriddedAtmosphere, Level, Medium, MediumBounds,
    ScatteredAtmosphere, StandardAtmosphere, standard_levels, wind_en,
};
use fdop_types::time::utc_date;
use std::collections::BTreeMap;
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

/// Validate the requested weather before trusting or replacing its cache. A response may
/// contain useful hours outside this request, so a failed refresh preserves the old bytes.
fn get_cached_validated<T>(
    url: &str,
    cache: &Path,
    offline: bool,
    (timeout_s, max_age_s, retries): (u64, Option<f64>, u32),
    validate: impl Fn(serde_json::Value) -> Result<T, DataError>,
    warnings: &mut Vec<String>,
) -> Result<T, DataError> {
    get_cached_recoverable(
        url,
        cache,
        offline,
        (timeout_s, max_age_s, retries),
        validate,
        |_| None,
        warnings,
    )
}

/// A partial response can still be useful while an online refresh attempts to repair it.
/// `quality` is `None` for complete data, otherwise the number of usable grid cells.
fn get_cached_recoverable<T>(
    url: &str,
    cache: &Path,
    offline: bool,
    (timeout_s, max_age_s, retries): (u64, Option<f64>, u32),
    validate: impl Fn(serde_json::Value) -> Result<T, DataError>,
    quality: impl Fn(&T) -> Option<usize>,
    warnings: &mut Vec<String>,
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
    let mut recovery = None;
    if fresh(&path)
        && let Ok(b) = std::fs::read(&path)
        && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&b)
    {
        match validate(v) {
            Ok(parsed) if offline || quality(&parsed).is_none() => return Ok(parsed),
            Ok(parsed) => recovery = Some(parsed),
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
        return recovery.ok_or_else(|| {
            DataError::new(format!(
                "failed recently ({}); retried after 30 min",
                std::fs::read_to_string(&fail).unwrap_or_default().trim()
            ))
        });
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
            return recovery.ok_or(e);
        }
    };
    // A response can be unusable only for these hours. Do not turn that into a
    // URL-wide failure marker that would block another window's valid weather.
    let parsed = match validate(v) {
        Ok(parsed) => parsed,
        Err(e) => return recovery.ok_or(e),
    };
    if let Some(previous) = recovery
        && let Some(usable) = quality(&parsed)
        && quality(&previous).is_some_and(|kept| kept >= usable)
    {
        return Ok(previous);
    }
    // The requested profile is already validated. Persistence only enables later
    // reuse; losing that optimization must not replace valid weather by a fallback.
    if let Err(error) = fdop_http::write_atomic(&path, &body) {
        warnings.push(format!(
            "weather response could not be cached at {} ({error}); using the validated response for this run",
            path.display()
        ));
    }
    Ok(parsed)
}

/// A fetched atmosphere with source-specific recovery warnings. Partial grids retain
/// observed profiles; their quality also requests cache repair. Zero complete cells
/// denotes recovery from complementary hourly slices, below any complete-column source.
struct WeatherSource {
    atmo: Arc<dyn Atmosphere>,
    warnings: Vec<String>,
    partial_cells: Option<usize>,
}

impl WeatherSource {
    fn complete(atmo: impl Atmosphere + 'static) -> Self {
        Self {
            atmo: Arc::new(atmo),
            warnings: Vec::new(),
            partial_cells: None,
        }
    }
}

impl Atmosphere for WeatherSource {
    fn medium(&self, lat: f64, lon: f64, t: f64, z_a: f64, z_b: f64) -> fdop_types::atmo::Medium {
        self.atmo.medium(lat, lon, t, z_a, z_b)
    }

    fn describe(&self) -> String {
        self.atmo.describe()
    }
}

fn arr(h: &serde_json::Value, key: &str, i: usize) -> f64 {
    h.get(key)
        .and_then(|a| a.get(i))
        .and_then(|x| x.as_f64())
        .unwrap_or(f64::NAN)
}

/// Profile winds must be observed, including genuine calm. Direction is
/// undefined at zero speed; an unavailable positive-speed vector is not calm air.
fn pressure_wind(speed: f64, direction: f64) -> Option<(f64, f64)> {
    if !speed.is_finite() || speed < 0.0 {
        None
    } else if speed == 0.0 {
        Some((0.0, 0.0))
    } else if (0.0..=360.0).contains(&direction) {
        Some(wind_en(speed, direction))
    } else {
        None
    }
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
struct LocationProfiles {
    profiles: Vec<Vec<Level>>,
    missing_surface_winds: usize,
    missing_aloft_winds: usize,
    wind_only_levels: usize,
}

fn parse_location(
    loc: &serde_json::Value,
    levels: bool,
    hours: std::ops::Range<usize>,
) -> Result<LocationProfiles, DataError> {
    let elev = loc.get("elevation").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let h = loc
        .get("hourly")
        .ok_or_else(|| DataError::new("open-meteo: no hourly block"))?;
    let mut out = LocationProfiles {
        profiles: Vec::with_capacity(hours.len()),
        missing_surface_winds: 0,
        missing_aloft_winds: 0,
        wind_only_levels: 0,
    };
    for i in hours {
        let t2 = arr(h, "temperature_2m", i);
        if !t2.is_finite() {
            return Err(DataError::new("open-meteo: missing surface temperature"));
        }
        let rh2 = arr(h, "relative_humidity_2m", i) / 100.0;
        let rh2 = if rh2.is_finite() { rh2 } else { 0.5 };
        let (ws, wd) = (arr(h, "wind_speed_10m", i), arr(h, "wind_direction_10m", i));
        let (we, wn) = if levels {
            pressure_wind(ws, wd).unwrap_or((f64::NAN, f64::NAN))
        } else if ws.is_finite() && wd.is_finite() {
            wind_en(ws, wd)
        } else {
            (0.0, 0.0)
        };
        if !levels {
            out.profiles
                .push(standard_levels(t2, elev + 2.0, rh2, we, wn));
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
        let mut thermal_pressures = vec![ps];
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
            let (we, wn) = pressure_wind(ws, wd).unwrap_or((f64::NAN, f64::NAN));
            lv.push(Level {
                z_msl: z,
                t_c: t,
                rh: 0.3,
                wind_e: we,
                wind_n: wn,
            });
            thermal_pressures.push(pf);
            (z_prev, p_prev, t_prev) = (z, pf, t);
        }
        if lv.len() < 3 {
            return Err(DataError::new("open-meteo: too few pressure levels"));
        }
        // A missing temperature does not erase an observed wind. Recover only levels
        // inside the original thermodynamic support; the minimum above still requires
        // two observed aloft temperatures. Log pressure locates the level within its
        // hypsometric layer. Interpolating temperature and humidity at that height
        // leaves the original piecewise-linear thermal profile unchanged.
        let thermal_count = lv.len();
        for p in LEVELS_HPA {
            if arr(h, &format!("temperature_{p}hPa"), i).is_finite() {
                continue;
            }
            let Some((we, wn)) = pressure_wind(
                arr(h, &format!("wind_speed_{p}hPa"), i),
                arr(h, &format!("wind_direction_{p}hPa"), i),
            ) else {
                continue;
            };
            let pf = p as f64;
            let k = thermal_pressures.partition_point(|&known| known > pf);
            if k == 0 || k >= thermal_count {
                continue;
            }
            let (a, b) = (lv[k - 1], lv[k]);
            let w = (thermal_pressures[k - 1] / pf).ln()
                / (thermal_pressures[k - 1] / thermal_pressures[k]).ln();
            if !(a.z_msl < b.z_msl && (0.0..1.0).contains(&w)) {
                continue;
            }
            lv.push(Level {
                z_msl: a.z_msl + w * (b.z_msl - a.z_msl),
                t_c: a.t_c + w * (b.t_c - a.t_c),
                rh: a.rh + w * (b.rh - a.rh),
                wind_e: we,
                wind_n: wn,
            });
            out.wind_only_levels += 1;
        }
        if lv.len() > thermal_count {
            lv.sort_by(|a, b| a.z_msl.total_cmp(&b.z_msl));
        }
        // the surface wind alone would be copied to every height and pass for winds aloft
        if lv[1..].iter().all(|l| l.wind_e.is_nan()) {
            return Err(DataError::new("open-meteo: no winds aloft"));
        }
        out.missing_surface_winds += usize::from(!lv[0].wind_e.is_finite());
        out.missing_aloft_winds += lv[1..].iter().filter(|l| !l.wind_e.is_finite()).count();
        // Missing surface winds take the nearest observed aloft vector, just as
        // missing interior levels interpolate. Keep real calm as its own anchor.
        fill_wind(&mut lv);
        out.profiles.push(lv);
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
) -> Result<WeatherSource, DataError> {
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
    let mut cache_warnings = Vec::new();
    let mut source = get_cached_recoverable(
        &url,
        cache,
        offline,
        (
            if levels { PRESSURE_LEVEL_TIMEOUT_S } else { 45 },
            None,
            if levels { 0 } else { 1 },
        ),
        |v| parse_om_grid(v, &lats, &lons, t_start, t_end, levels),
        |a| a.partial_cells,
        &mut cache_warnings,
    )?;
    source.warnings.extend(cache_warnings);
    Ok(source)
}

/// Spatially supported hourly slices, used only when no whole cell covers the window.
/// Interpolate slowness and wind after spatial interpolation, never extend a cell's
/// last valid sounding through its missing hours. The default unknown bounds keep
/// search pruning conservative for this provider.
struct HourlyPressureGrid {
    slices: Vec<(f64, ScatteredAtmosphere)>,
    source: String,
}

impl Atmosphere for HourlyPressureGrid {
    fn medium(&self, lat: f64, lon: f64, t: f64, z_a: f64, z_b: f64) -> Medium {
        let k = self.slices.partition_point(|s| s.0 <= t);
        if !t.is_finite() || k == 0 || t > self.slices.last().unwrap().0 {
            return Medium {
                c: f64::NAN,
                wind_e: f64::NAN,
                wind_n: f64::NAN,
            };
        }
        let (ta, a) = &self.slices[k - 1];
        let ma = a.medium(lat, lon, *ta, z_a, z_b);
        if t == *ta || k == self.slices.len() {
            return ma;
        }
        let (tb, b) = &self.slices[k];
        let mb = b.medium(lat, lon, *tb, z_a, z_b);
        let w = (t - ta) / (tb - ta);
        Medium {
            c: 1.0 / ((1.0 - w) / ma.c + w / mb.c),
            wind_e: (1.0 - w) * ma.wind_e + w * mb.wind_e,
            wind_n: (1.0 - w) * ma.wind_n + w * mb.wind_n,
        }
    }

    fn describe(&self) -> String {
        self.source.clone()
    }
}

fn pressure_grid_by_hour(
    locs: &[serde_json::Value],
    lats: &[f64],
    lons: &[f64],
    t_start: f64,
    t_end: f64,
    missing: &[String],
) -> Option<WeatherSource> {
    if !t_start.is_finite() || !t_end.is_finite() || t_start > t_end {
        return None;
    }
    let mut hours: BTreeMap<i64, Vec<(usize, LocationProfiles)>> = BTreeMap::new();
    for (cell, loc) in locs.iter().enumerate() {
        let Ok(times) = times_of(&loc["hourly"]) else {
            continue;
        };
        for (i, &t) in times.iter().enumerate() {
            // At most the adjacent hourly brackets can contribute to this request.
            if t < t_start - 3600.0 || t > t_end + 3600.0 {
                continue;
            }
            if let Ok(profiles) = parse_location(loc, true, i..i + 1) {
                hours.entry(t as i64).or_default().push((cell, profiles));
            }
        }
    }
    let times: Vec<_> = hours.keys().map(|&t| t as f64).collect();
    if times.is_empty() {
        return None;
    }
    let needed = hours_for_window(&times, t_start, t_end).ok()?;
    // Missing interior hours and mismatched phases cannot establish hourly support.
    if times[needed.clone()]
        .windows(2)
        .any(|pair| pair[1] - pair[0] != 3600.0)
    {
        return None;
    }
    let expected = needed.len() * locs.len();
    let mut retained = 0;
    let mut recoveries = [0; 3];
    let mut slices = Vec::with_capacity(needed.len());
    for (t, cells) in hours.into_iter().skip(needed.start).take(needed.len()) {
        retained += cells.len();
        let stations = cells
            .into_iter()
            .map(|(cell, mut p)| {
                recoveries[0] += p.missing_surface_winds;
                recoveries[1] += p.missing_aloft_winds;
                recoveries[2] += p.wind_only_levels;
                (
                    lats[cell / lons.len()],
                    lons[cell % lons.len()],
                    vec![(t as f64, p.profiles.pop().unwrap())],
                )
            })
            .collect();
        slices.push((
            t as f64,
            ScatteredAtmosphere::new(stations, String::new()).ok()?,
        ));
    }
    let source = "Open-Meteo historical forecast (pressure levels), partial hourly grid";
    let mut warnings = vec![format!(
        "{source}: no complete spatial cell covers the requested window; using {retained} of {expected} observed cell-hours with inverse-distance interpolation at each hour and slowness/wind interpolation between adjacent supported hours ({})",
        missing.join("; ")
    )];
    warnings.extend(wind_recovery_warnings(source, recoveries));
    Some(WeatherSource {
        atmo: Arc::new(HourlyPressureGrid {
            slices,
            source: source.into(),
        }),
        warnings,
        partial_cells: Some(0),
    })
}

fn wind_recovery_warnings(src: &str, [surface, aloft, wind_only]: [usize; 3]) -> Vec<String> {
    let mut warnings = Vec::new();
    if surface > 0 {
        warnings.push(format!(
            "{src}: surface wind unavailable in {surface} retained cell-hours; extrapolating the nearest observed pressure-level wind to the surface"
        ));
    }
    if aloft > 0 {
        warnings.push(format!(
            "{src}: {aloft} retained pressure-level winds unavailable; interpolating in height and extrapolating the nearest observed wind beyond the available levels"
        ));
    }
    if wind_only > 0 {
        warnings.push(format!(
            "{src}: retained {wind_only} observed pressure-level winds without temperatures; heights interpolated in log pressure within observed thermodynamic layers, preserving their temperature and humidity profiles"
        ));
    }
    warnings
}

fn parse_om_grid(
    v: serde_json::Value,
    lats: &[f64],
    lons: &[f64],
    t_start: f64,
    t_end: f64,
    levels: bool,
) -> Result<WeatherSource, DataError> {
    let locs: Vec<serde_json::Value> = match v {
        serde_json::Value::Array(a) => a,
        o => vec![o],
    };
    if locs.is_empty() || locs.len() != lats.len() * lons.len() {
        return Err(DataError(format!(
            "open-meteo: {} locations returned, {} asked",
            locs.len(),
            lats.len() * lons.len()
        )));
    }
    let mut per_loc = Vec::with_capacity(locs.len());
    let mut missing = Vec::new();
    let mut surface_wind_recoveries = 0usize;
    let mut aloft_wind_recoveries = 0usize;
    let mut wind_only_recoveries = 0usize;
    for (index, l) in locs.iter().enumerate() {
        let t = match times_of(&l["hourly"]) {
            Ok(t) => t,
            Err(e) => {
                missing.push(format!("cell {}: {e}", index + 1));
                continue;
            }
        };
        let hours = match hours_for_window(&t, t_start, t_end) {
            Ok(hours) => hours,
            Err(e) => {
                missing.push(format!("cell {}: {e}", index + 1));
                continue;
            }
        };
        // Each cell may have different leading/trailing hours or a different
        // hourly phase. Retain its actual UTC brackets alongside its profiles;
        // neither its row indices nor its time axis can stand in for another cell.
        let selected = t[hours.clone()].to_vec();
        match parse_location(l, levels, hours) {
            Ok(p) => {
                surface_wind_recoveries += p.missing_surface_winds;
                aloft_wind_recoveries += p.missing_aloft_winds;
                wind_only_recoveries += p.wind_only_levels;
                per_loc.push((index, selected, p.profiles));
            }
            Err(e) => missing.push(format!("cell {}: {e}", index + 1)),
        }
    }
    if per_loc.is_empty() {
        if levels
            && let Some(source) = pressure_grid_by_hour(&locs, lats, lons, t_start, t_end, &missing)
        {
            return Ok(source);
        }
        return Err(DataError::new(format!(
            "open-meteo: no usable grid cells ({})",
            missing.join("; ")
        )));
    }
    let src = if levels {
        "Open-Meteo historical forecast (pressure levels)"
    } else {
        "Open-Meteo ERA5 archive (surface + standard lapse)"
    };
    let wind_warnings = wind_recovery_warnings(
        src,
        [
            surface_wind_recoveries,
            aloft_wind_recoveries,
            wind_only_recoveries,
        ],
    );
    let partial = !missing.is_empty();
    let aligned = per_loc.windows(2).all(|p| p[0].1 == p[1].1);
    if partial || !aligned {
        let usable = per_loc.len();
        let stations = per_loc
            .into_iter()
            .map(|(index, times, profiles)| {
                (
                    lats[index / lons.len()],
                    lons[index % lons.len()],
                    times.into_iter().zip(profiles).collect(),
                )
            })
            .collect();
        let (source, warning) = if partial {
            (
                format!("{src}, partial grid: {usable} of {} cells", locs.len()),
                format!(
                    "{src}: only {usable} of {} grid cells cover the requested window; using complete cells with inverse-distance interpolation and each cell's UTC timestamps ({})",
                    locs.len(),
                    missing.join("; ")
                ),
            )
        } else {
            (
                format!("{src}, independently timed grid"),
                format!(
                    "{src}: hourly timestamps differ between cells; using all {usable} complete grid cells with inverse-distance interpolation and each cell's UTC timestamps"
                ),
            )
        };
        let atmo = ScatteredAtmosphere::new(stations, source).map_err(DataError)?;
        let mut warnings = vec![warning];
        warnings.extend(wind_warnings);
        return Ok(WeatherSource {
            atmo: Arc::new(atmo),
            warnings,
            partial_cells: partial.then_some(usable),
        });
    }
    let times = &per_loc[0].1;
    let mut profiles = Vec::with_capacity(times.len() * locs.len());
    for it in 0..times.len() {
        for (_, _, p) in &per_loc {
            profiles.push(p[it].clone());
        }
    }
    GriddedAtmosphere::new(
        lats.to_vec(),
        lons.to_vec(),
        times[0],
        3600.0,
        &profiles,
        src.to_string(),
    )
    .map(|atmo| {
        let mut source = WeatherSource::complete(atmo);
        source.warnings = wind_warnings;
        source
    })
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
    warnings: &mut Vec<String>,
) -> Result<Vec<(String, f64, f64, f64)>, DataError> {
    raob_stations_from(IEM_RAOB_NETWORK, cache, offline, window, warnings)
}

fn raob_stations_from(
    url: &str,
    cache: &Path,
    offline: bool,
    window: (f64, f64),
    warnings: &mut Vec<String>,
) -> Result<Vec<(String, f64, f64, f64)>, DataError> {
    let parse = |v| parse_raob_stations(v, window);
    get_cached_validated(
        url,
        cache,
        offline,
        (30, Some(30.0 * 86400.0), 1),
        parse,
        warnings,
    )
    .or_else(|e| {
        // A valid stale index beats none, but an unusable refresh must not replace
        // it before this fallback gets to read the previously published bytes.
        get_cached_validated(url, cache, true, (30, None, 0), parse, warnings).map_err(|_| e)
    })
}

fn parse_raob_stations(
    v: serde_json::Value,
    window: (f64, f64),
) -> Result<Vec<(String, f64, f64, f64)>, DataError> {
    let feats = v
        .get("features")
        .and_then(|f| f.as_array())
        .ok_or_else(|| DataError::new("RAOB network: no features"))?;
    let mut out = Vec::new();
    let mut usable = false;
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
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            && us_station)
        {
            continue;
        }
        let c = &f["geometry"]["coordinates"];
        let (lo, la) = (
            c[0].as_f64().unwrap_or(f64::NAN),
            c[1].as_f64().unwrap_or(f64::NAN),
        );
        if !((-90.0..=90.0).contains(&la) && (-180.0..=180.0).contains(&lo)) {
            continue;
        }
        // Cache validity is independent of the recording date. A real station
        // outside this window still makes the shared index usable for other dates.
        usable = true;
        if raob_station_overlaps(pr, window) {
            out.push((
                id.to_string(),
                la,
                lo,
                pr.get("elevation").and_then(|x| x.as_f64()).unwrap_or(0.0),
            ));
        }
    }
    if !usable {
        return Err(DataError::new("RAOB network: no usable US stations"));
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
        if z.is_finite()
            && let Some((we, wn)) = pressure_wind(ff * KT, dd)
        {
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
    // Wind-only knots must preserve the observed thermal profile, including humidity:
    // Column interpolates both before computing moist-air sound speed. Keep only
    // knots inside that original temperature support.
    let thermal: Vec<(f64, f64, f64)> = lv.iter().map(|l| (l.z_msl, l.t_c, l.rh)).collect();
    let mut out: Vec<Level> = Vec::new();
    for mut l in with_w {
        if !l.t_c.is_finite() {
            let k = thermal.partition_point(|t| t.0 <= l.z_msl);
            if k == 0 || k >= thermal.len() {
                continue;
            }
            let (a, b) = (thermal[k - 1], thermal[k]);
            let w = (l.z_msl - a.0) / (b.0 - a.0);
            l.t_c = a.1 + w * (b.1 - a.1);
            l.rh = a.2 + w * (b.2 - a.2);
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

#[cfg(test)]
fn parse_raob_response(v: serde_json::Value) -> Result<Vec<Level>, DataError> {
    select_raob_profile(v).map(|(levels, _)| levels)
}

fn select_raob_profile(v: serde_json::Value) -> Result<(Vec<Level>, usize), DataError> {
    let profiles = v
        .get("profiles")
        .and_then(|p| p.as_array())
        .filter(|a| !a.is_empty())
        .ok_or_else(|| DataError::new("RAOB: no sounding profile"))?;
    // A virtual station can return several historical station aliases for the
    // requested launch time. Keep the first usable sounding, without combining
    // different profiles or letting an incomplete first alias hide a complete one.
    for (index, profile) in profiles.iter().enumerate() {
        let levels = parse_raob(&profile["profile"]);
        if levels.len() >= 5 && levels.last().is_some_and(|l| l.z_msl > 5000.0) {
            return Ok((levels, index));
        }
    }
    Err(DataError::new(
        "RAOB: sounding lacks usable levels through 5 km",
    ))
}

fn raob_url(t: f64, station: &str) -> String {
    let (y, m, d) = utc_date(t);
    let hh = ((t.rem_euclid(86400.0)) / 3600.0).round() as u32;
    // IEM prefixes three-character inputs with K. Keep that spelling for existing
    // continental cache keys, but pass other US and virtual station identifiers intact.
    let station = station.strip_prefix('K').unwrap_or(station);
    format!("{IEM_RAOB}?ts={y:04}{m:02}{d:02}{hh:02}00&station={station}")
}

#[cfg(test)]
fn raob_atmosphere(
    b: &BBox,
    t_start: f64,
    t_end: f64,
    cache: &Path,
    offline: bool,
) -> Result<WeatherSource, DataError> {
    raob_atmosphere_with_progress(b, t_start, t_end, cache, offline, &|_| {})
}

fn raob_atmosphere_with_progress(
    b: &BBox,
    t_start: f64,
    t_end: f64,
    cache: &Path,
    offline: bool,
    on_profile: &dyn Fn(WeatherSource),
) -> Result<WeatherSource, DataError> {
    // Select stations for the 00Z/12Z brackets too, not just times inside the recording.
    let t0 = (t_start / 43200.0).floor() * 43200.0;
    let t1 = (t_end / 43200.0).ceil() * 43200.0;
    let (clat, clon) = b.center();
    let mut cache_warnings = Vec::new();
    let mut st = raob_stations(cache, offline, (t0, t1), &mut cache_warnings)?;
    st.sort_by(|x, y| {
        fdop_types::geo::haversine_m(clat, clon, x.1, x.2)
            .total_cmp(&fdop_types::geo::haversine_m(clat, clon, y.1, y.2))
    });
    // The preferred blend is unchanged: the nearest station plus any other within
    // 1.6x its distance (or the region's half-diagonal), at most four inside 700 km.
    // Keep the next-nearest candidates too: a nearby station with no usable archive
    // must not hide soundings farther away. Recovery shares the four-station budget.
    let d0 = st.first().map_or(f64::INFINITY, |s| {
        fdop_types::geo::haversine_m(clat, clon, s.1, s.2)
    });
    let half_diag = fdop_types::geo::haversine_m(b.south, b.west, b.north, b.east) / 2.0;
    let preferred_distance = (1.6 * d0).max(d0 + half_diag);
    let chosen: Vec<_> = st
        .into_iter()
        .filter(|s| {
            let d = fdop_types::geo::haversine_m(clat, clon, s.1, s.2);
            d <= 700_000.0
        })
        .take(4)
        .collect();
    if chosen.is_empty() {
        return Err(DataError::new("no upper-air station within 700 km"));
    }
    let mut stations = Vec::new();
    let mut attempted = Vec::new();
    let mut used = Vec::new();
    let mut recovered = false;
    let (mut n_ok, mut n_tried) = (0, 0);
    let mut alias_recoveries = 0;
    let profile_warnings = |count, cache_warnings: &[String]| {
        let mut warnings = cache_warnings.to_vec();
        if count > 0 {
            warnings.push(format!(
                "radiosondes: recovered {count} sounding(s) from later returned profiles after incomplete aliases; profiles were not combined"
            ));
        }
        warnings
    };
    for (id, la, lo, _) in &chosen {
        let outside_preferred =
            fdop_types::geo::haversine_m(clat, clon, *la, *lo) > preferred_distance;
        if outside_preferred && n_ok > 0 {
            break;
        }
        attempted.push(id.as_str());
        let mut series = Vec::new();
        let mut t = t0;
        while t <= t1 + 1.0 {
            n_tried += 1;
            if let Ok((lv, index)) = get_cached_validated(
                &raob_url(t, id),
                cache,
                offline,
                (20, None, 1),
                select_raob_profile,
                &mut cache_warnings,
            ) {
                series.push((t, lv));
                n_ok += 1;
                alias_recoveries += usize::from(index > 0);
                // A later sounding or station can stall past the pipeline deadline.
                // Publish only already validated profiles; the final blend below keeps
                // its original station selection and time interpolation.
                let mut available = stations.clone();
                available.push((*la, *lo, series.clone()));
                let mut ids = used.clone();
                ids.push(id.as_str());
                if let Ok(profile) = ScatteredAtmosphere::new(
                    available,
                    format!(
                        "NWS radiosondes {} (IEM RAOB archive, retrieval in progress: {n_ok} usable soundings)",
                        ids.join(", ")
                    ),
                ) {
                    let mut source = WeatherSource::complete(profile);
                    source.warnings = profile_warnings(alias_recoveries, &cache_warnings);
                    on_profile(source);
                }
            }
            t += 43200.0;
        }
        if !series.is_empty() {
            used.push(id.as_str());
            stations.push((*la, *lo, series));
            if outside_preferred {
                recovered = true;
                break;
            }
        }
    }
    if n_ok == 0 {
        return Err(DataError::new("no radiosonde soundings retrieved"));
    }
    let station_note = if attempted.len() > used.len() {
        format!(
            "; {} of {} stations usable, tried {}{}",
            used.len(),
            attempted.len(),
            attempted.join(", "),
            if recovered {
                "; preferred stations had no usable soundings, recovered from the nearest usable station"
            } else {
                ""
            },
        )
    } else {
        String::new()
    };
    ScatteredAtmosphere::new(
        stations,
        format!(
            "NWS radiosondes {} (IEM RAOB archive, {n_ok} of {n_tried} soundings usable{station_note})",
            used.join(", ")
        ),
    )
    .map(|profile| {
        let mut source = WeatherSource::complete(profile);
        source.warnings = profile_warnings(alias_recoveries, &cache_warnings);
        source
    })
    .map_err(DataError)
}

fn join_weather<T>(h: std::thread::JoinHandle<Result<T, DataError>>) -> Result<T, DataError> {
    h.join()
        .unwrap_or_else(|_| Err(DataError::new("weather thread panicked")))
}

/// The atmosphere a run uses, with what it lacks.
pub struct Weather {
    pub atmo: Arc<dyn Atmosphere>,
    /// Every fallback taken; empty only when all Open-Meteo pressure-level cells answered.
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
    let om = join_weather(h_om);
    let preferred_failure = om
        .as_ref()
        .err()
        .map(|e| format!("Open-Meteo pressure levels unavailable ({e})"))
        .unwrap_or_default();
    let updates = Arc::new(on_fallback);
    let raob_updates = Arc::clone(&updates);
    fetch_with_fallbacks(
        om,
        move || om_grid(OM_ARCHIVE, &b, t_start, t_end, &c2, offline, false),
        move || {
            raob_atmosphere_with_progress(&b, t_start, t_end, &c3, offline, &|mut source| {
                source.warnings.push(
                    "radiosonde retrieval is still in progress; using only the soundings already received"
                        .into(),
                );
                let _ =
                    publish_fallback(Ok(source), &preferred_failure, true, &*raob_updates);
            })
        },
        move |weather| updates(weather),
    )
}

type AtmosphereResult = Result<WeatherSource, DataError>;

// The final adjustment and a provisional result share immutable profiles. Keep the
// public AdjustedAtmosphere interface unchanged while retaining them in both places.
struct SharedAtmosphere(Arc<dyn Atmosphere>);

impl Atmosphere for SharedAtmosphere {
    fn medium(&self, lat: f64, lon: f64, t: f64, z_a: f64, z_b: f64) -> Medium {
        self.0.medium(lat, lon, t, z_a, z_b)
    }

    fn describe(&self) -> String {
        self.0.describe()
    }

    fn uniform_medium(&self) -> Option<Medium> {
        self.0.uniform_medium()
    }

    fn medium_bounds(&self) -> Option<MediumBounds> {
        self.0.medium_bounds()
    }
}

fn publish_fallback(
    result: AtmosphereResult,
    preferred_failure: &str,
    winds_aloft: bool,
    on_fallback: &dyn Fn(Weather),
) -> AtmosphereResult {
    if let Ok(a) = &result {
        let mut warnings = vec![preferred_failure.into()];
        warnings.extend(a.warnings.iter().cloned());
        warnings.push(
            if winds_aloft {
                "hourly surface temperatures not applied; boundary layer from the 12-hourly soundings"
            } else {
                "no upper-air data: standard lapse above the surface and no winds aloft"
            }
            .into(),
        );
        on_fallback(Weather {
            atmo: Arc::clone(&a.atmo),
            warnings,
            winds_aloft,
        });
    }
    result
}

fn fetch_with_fallbacks(
    om: AtmosphereResult,
    surface: impl FnOnce() -> AtmosphereResult + Send + 'static,
    raob: impl FnOnce() -> AtmosphereResult + Send + 'static,
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
            publish_fallback(
                raob(),
                &raob_failure,
                true,
                &*on_fallback,
            )
        });
        (join_weather(h_sf), join_weather(h_rb))
    })
}

fn choose_atmosphere(
    om: AtmosphereResult,
    fallbacks: impl FnOnce(&str) -> (AtmosphereResult, AtmosphereResult),
) -> Weather {
    let weather = |atmo: Arc<dyn Atmosphere>, warnings: Vec<String>, winds_aloft: bool| Weather {
        atmo,
        warnings,
        winds_aloft,
    };
    let mut warn = Vec::new();
    match om {
        Ok(a) => return weather(a.atmo, a.warnings, true),
        Err(e) => warn.push(format!("Open-Meteo pressure levels unavailable ({e})")),
    }
    let (surface, raob) = fallbacks(&warn[0]);
    match raob {
        Ok(r) => {
            warn.extend(r.warnings);
            // radiosondes give real temperature and wind profiles; with hourly surface
            // temperatures the boundary layer follows the diurnal cycle too
            return match surface {
                Ok(s) => {
                    warn.extend(s.warnings);
                    weather(
                        Arc::new(AdjustedAtmosphere {
                            base: Box::new(SharedAtmosphere(r.atmo)),
                            surface: Box::new(SharedAtmosphere(s.atmo)),
                            depth: 1500.0,
                        }),
                        warn,
                        true,
                    )
                }
                Err(e) => {
                    warn.push(format!(
                        "hourly surface temperatures unavailable ({e}); boundary layer from the 12-hourly soundings"
                    ));
                    weather(r.atmo, warn, true)
                }
            };
        }
        Err(e) => warn.push(format!("radiosondes unavailable ({e})")),
    }
    match surface {
        Ok(a) => {
            warn.extend(a.warnings);
            warn.push(
                "no upper-air data: standard lapse above the surface and no winds aloft".into(),
            );
            return weather(a.atmo, warn, false);
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

    fn test_grid(source: &str, t_c: f64) -> WeatherSource {
        WeatherSource::complete(
            GriddedAtmosphere::new(
                vec![0.0],
                vec![0.0],
                0.0,
                3600.0,
                &[standard_levels(t_c, 0.0, 0.5, 1.0, 2.0)],
                source.into(),
            )
            .unwrap(),
        )
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
    fn shared_profiles_preserve_certified_search_bounds() {
        let constant: Arc<dyn Atmosphere> = Arc::new(fdop_types::atmo::UniformAtmosphere(Medium {
            c: 340.0,
            wind_e: 3.0,
            wind_n: -4.0,
        }));
        let soundings: Arc<dyn Atmosphere> = Arc::new(test_soundings());
        for original in [constant, soundings] {
            let shared = SharedAtmosphere(original.clone());
            assert!(original.medium_bounds().is_some());
            assert_eq!(shared.medium_bounds(), original.medium_bounds());
            assert_eq!(shared.uniform_medium(), original.uniform_medium());
            assert_eq!(
                shared.medium(0.0, 0.0, 0.0, 1.5, 2000.0),
                original.medium(0.0, 0.0, 0.0, 1.5, 2000.0)
            );
        }
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
                        Ok(WeatherSource::complete(test_soundings()))
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
                Ok(test_grid("test surface", 26.0)),
                Ok(WeatherSource::complete(test_soundings())),
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
                Ok(WeatherSource::complete(test_soundings())),
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
                Ok(test_grid("test surface", 26.0)),
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

    #[test]
    fn partial_model_grids_recover_complete_columns_without_changing_full_grids() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let (t0, t1) = (day + 4.25 * 3600.0, day + 5.75 * 3600.0);
        for levels in [false, true] {
            let left = open_meteo_fixture(day as i64, 24);
            let mut right = left.clone();
            for (name, values) in right["hourly"].as_object_mut().unwrap() {
                let delta = if name.starts_with("temperature_") {
                    8.0
                } else if name.starts_with("wind_speed_") {
                    6.0
                } else {
                    continue;
                };
                for value in values.as_array_mut().unwrap() {
                    *value = serde_json::json!(value.as_f64().unwrap() + delta);
                }
            }
            let complete = serde_json::json!([left, left, right]);
            let parse = |value| parse_om_grid(value, &[0.0], &[0.0, 1.0, 2.0], t0, t1, levels);
            let all = parse(complete.clone()).unwrap();
            let columns: Vec<_> = complete
                .as_array()
                .unwrap()
                .iter()
                .map(|loc| parse_location(loc, levels, 4..7).unwrap().profiles)
                .collect();
            let profiles: Vec<_> = (0..3)
                .flat_map(|hour| columns.iter().map(move |column| column[hour].clone()))
                .collect();
            let expected = GriddedAtmosphere::new(
                vec![0.0],
                vec![0.0, 1.0, 2.0],
                day + 4.0 * 3600.0,
                3600.0,
                &profiles,
                "complete".into(),
            )
            .unwrap();
            assert!(all.warnings.is_empty());
            assert!(all.partial_cells.is_none());
            for lon in [0.0, 0.37, 1.0, 1.89, 2.0] {
                for t in [t0, day + 5.0 * 3600.0, t1] {
                    assert_eq!(
                        all.medium(0.0, lon, t, 30.0, 5000.0),
                        expected.medium(0.0, lon, t, 30.0, 5000.0)
                    );
                }
            }
            let mut partial = complete.clone();
            partial[1]["hourly"]["temperature_2m"][5] = serde_json::Value::Null;
            let recovered = parse(partial.clone()).unwrap();
            assert_eq!(recovered.partial_cells, Some(2));
            assert_eq!(recovered.warnings.len(), 1);
            assert!(recovered.warnings[0].contains("2 of 3 grid cells"));
            assert!(recovered.warnings[0].contains("cell 2"));
            assert!(recovered.describe().contains("partial grid: 2 of 3 cells"));
            // A valid hourly axis can still lack either requested interpolation
            // endpoint. It must drop only that cell, just like a missing profile.
            for rows in [0..6, 5..24, 12..24] {
                let mut cropped = complete.clone();
                for values in cropped[1]["hourly"].as_object_mut().unwrap().values_mut() {
                    *values = serde_json::json!(&values.as_array().unwrap()[rows.clone()]);
                }
                let usable = parse(cropped).unwrap();
                assert_eq!(usable.partial_cells, Some(2));
                assert!(usable.warnings[0].contains("cell 2"));
                assert!(usable.warnings[0].contains("do not cover the requested window"));
                for lon in [0.0, 0.5, 1.0, 2.0] {
                    for t in [t0, day + 5.0 * 3600.0, t1] {
                        assert_eq!(
                            usable.medium(0.0, lon, t, 30.0, 5000.0),
                            recovered.medium(0.0, lon, t, 30.0, 5000.0)
                        );
                    }
                }
            }
            // An unusable first column cannot set a foreign phase that then
            // rejects all later columns with valid UTC-hour profiles.
            let mut bad_first = complete.clone();
            bad_first[0]["hourly"]["temperature_2m"] = serde_json::json!(vec![None::<f64>; 24]);
            let expected = parse(bad_first.clone()).unwrap();
            for t in bad_first[0]["hourly"]["time"].as_array_mut().unwrap() {
                *t = serde_json::json!(t.as_i64().unwrap() + 1800);
            }
            let usable = parse(bad_first).unwrap();
            assert_eq!(usable.partial_cells, Some(2));
            assert!(usable.warnings[0].contains("cell 1"));
            for lon in [0.0, 0.5, 1.0, 2.0] {
                for t in [t0, day + 5.0 * 3600.0, t1] {
                    assert_eq!(
                        usable.medium(0.0, lon, t, 30.0, 5000.0),
                        expected.medium(0.0, lon, t, 30.0, 5000.0)
                    );
                }
            }
            let cell = |index: usize| {
                GriddedAtmosphere::new(
                    vec![0.0],
                    vec![index as f64],
                    day + 4.0 * 3600.0,
                    3600.0,
                    &columns[index],
                    "cell".into(),
                )
                .unwrap()
            };
            let (a, b) = (cell(0), cell(2));
            // At the equator these distances are 1:3 or 1:1. Check the existing
            // inverse-square spatial weights and harmonic sound speed independently.
            for (lon, wa) in [(0.5, 0.9), (1.0, 0.5)] {
                for t in [t0, day + 5.5 * 3600.0, t1] {
                    let (a, b) = (
                        a.medium(0.0, lon, t, 30.0, 5000.0),
                        b.medium(0.0, lon, t, 30.0, 5000.0),
                    );
                    let got = recovered.medium(0.0, lon, t, 30.0, 5000.0);
                    assert!((got.c - 1.0 / (wa / a.c + (1.0 - wa) / b.c)).abs() < 1e-10);
                    assert!((got.wind_e - (wa * a.wind_e + (1.0 - wa) * b.wind_e)).abs() < 1e-10);
                    assert!((got.wind_n - (wa * a.wind_n + (1.0 - wa) * b.wind_n)).abs() < 1e-10);
                }
            }
            // A complete column retains priority. Without one, complementary
            // pressure-level cell-hours can still cover the whole interval.
            partial[2]["hourly"]["temperature_2m"][6] = serde_json::Value::Null;
            assert_eq!(parse(partial.clone()).unwrap().partial_cells, Some(1));
            partial[0]["hourly"]["temperature_2m"][4] = serde_json::Value::Null;
            if levels {
                assert_eq!(parse(partial).unwrap().partial_cells, Some(0));
            } else {
                assert!(parse(partial).is_err());
            }
            if levels {
                let weather = fetch_with_fallbacks(
                    Ok(recovered),
                    || panic!("usable pressure cells must not fetch surface weather"),
                    || panic!("usable pressure cells must not fetch radiosondes"),
                    |_| panic!("pressure recovery is the final source"),
                );
                assert!(weather.winds_aloft);
                assert_eq!(weather.warnings.len(), 1);
            } else {
                let warning = recovered.warnings[0].clone();
                let (tx, rx) = std::sync::mpsc::channel();
                let weather = fetch_with_fallbacks(
                    Err(DataError::new("pressure failed")),
                    move || Ok(recovered),
                    || Ok(WeatherSource::complete(test_soundings())),
                    move |weather| tx.send(weather).unwrap(),
                );
                assert!(weather.winds_aloft);
                assert!(weather.atmo.describe().contains("boundary layer adjusted"));
                assert_eq!(
                    weather.warnings.iter().filter(|w| **w == warning).count(),
                    1
                );
                let provisional = rx.try_iter().find(|w| !w.winds_aloft).unwrap();
                assert!(provisional.warnings.contains(&warning));
            }
        }
    }

    fn staggered_pressure_grid(day: i64) -> serde_json::Value {
        let cells: Vec<_> = (0..3)
            .map(|cell| {
                let mut loc = open_meteo_fixture(day, 3);
                loc["hourly"]["relative_humidity_2m"] = serde_json::json!(vec![30.0; 3]);
                // Constant temperature, humidity and wind with height give known
                // column means, independent of the hypsometric height calculation.
                for hour in 0..3 {
                    let temp = 10.0 + 5.0 * cell as f64 + 7.0 * hour as f64;
                    let speed = 12.0 + 6.0 * cell as f64 + 3.0 * hour as f64;
                    loc["hourly"]["temperature_2m"][hour] = serde_json::json!(temp);
                    for p in LEVELS_HPA {
                        loc["hourly"][format!("temperature_{p}hPa")][hour] =
                            serde_json::json!(temp);
                    }
                    for suffix in std::iter::once("10m".to_string())
                        .chain(LEVELS_HPA.into_iter().map(|p| format!("{p}hPa")))
                    {
                        loc["hourly"][format!("wind_speed_{suffix}")][hour] =
                            serde_json::json!(speed);
                        loc["hourly"][format!("wind_direction_{suffix}")][hour] =
                            serde_json::json!([270.0, 180.0, 90.0][hour]);
                    }
                }
                match cell {
                    0 => loc["hourly"]["temperature_2m"][0] = serde_json::Value::Null,
                    1 => loc["hourly"]["surface_pressure"][1] = serde_json::Value::Null,
                    _ => {
                        for p in LEVELS_HPA {
                            loc["hourly"][format!("wind_speed_{p}hPa")][2] =
                                serde_json::Value::Null;
                        }
                    }
                }
                loc
            })
            .collect();
        serde_json::json!(cells)
    }

    #[test]
    fn complementary_cell_hours_recover_measured_pressure_weather() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let source = parse_om_grid(
            staggered_pressure_grid(day as i64),
            &[0.0],
            &[0.0, 1.0, 2.0],
            day + 900.0,
            day + 6300.0,
            true,
        )
        .unwrap();
        assert_eq!(source.partial_cells, Some(0));
        assert_eq!(source.warnings.len(), 1);
        assert!(source.warnings[0].contains("6 of 9 observed cell-hours"));
        assert!(source.warnings[0].contains("no winds aloft"));
        assert!(source.atmo.medium_bounds().is_none());
        // At longitude 0.5 on the equator, the retained station distances
        // independently give 9:1, 9:1 and 1:1 inverse-square weights by hour.
        let support = [
            [(1, 0.9), (2, 0.1)],
            [(0, 0.9), (2, 0.1)],
            [(0, 0.5), (1, 0.5)],
        ];
        let sounds: Vec<_> = support
            .iter()
            .enumerate()
            .map(|(hour, cells)| {
                1.0 / cells
                    .iter()
                    .map(|&(cell, w)| {
                        w / fdop_types::atmo::sound_speed(
                            10.0 + 5.0 * cell as f64 + 7.0 * hour as f64,
                            0.3,
                        )
                    })
                    .sum::<f64>()
            })
            .collect();
        let winds = [[18.6, 0.0], [0.0, 16.2], [-21.0, 0.0]];
        for hour in [0.0, 0.25, 0.5, 1.0, 1.75, 2.0] {
            let a = (hour as usize).min(1);
            let w = hour - a as f64;
            let c = 1.0 / ((1.0 - w) / sounds[a] + w / sounds[a + 1]);
            let wind_e = (1.0 - w) * winds[a][0] + w * winds[a + 1][0];
            let wind_n = (1.0 - w) * winds[a][1] + w * winds[a + 1][1];
            for (za, zb) in [
                (30.0, 30.0),
                (30.0, 5000.0),
                (5000.0, 30.0),
                (-300.0, 11000.0),
            ] {
                let m = source.medium(0.0, 0.5, day + hour * 3600.0, za, zb);
                assert!((m.c - c).abs() < 1e-10, "{hour}: {m:?}, c={c}");
                assert!((m.wind_e - wind_e).abs() < 1e-10);
                assert!((m.wind_n - wind_n).abs() < 1e-10);
            }
        }
        for t in [day - 1.0, day + 7201.0, f64::NAN, f64::INFINITY] {
            let m = source.medium(0.0, 0.5, t, 30.0, 5000.0);
            assert!(m.c.is_nan() && m.wind_e.is_nan() && m.wind_n.is_nan());
        }
        let weather = fetch_with_fallbacks(
            Ok(source),
            || panic!("supported pressure weather must not fetch surface data"),
            || panic!("supported pressure weather must not fetch soundings"),
            |_| panic!("supported pressure weather is final"),
        );
        assert!(weather.winds_aloft);
        assert!(weather.atmo.describe().contains("partial hourly grid"));
    }

    #[test]
    fn hourly_pressure_recovery_requires_every_requested_bracket() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let parse = |v| parse_om_grid(v, &[0.0], &[0.0, 1.0, 2.0], day + 900.0, day + 6300.0, true);
        for hour in 0..3 {
            let mut missing = staggered_pressure_grid(day as i64);
            for cell in missing.as_array_mut().unwrap() {
                cell["hourly"]["temperature_2m"][hour] = serde_json::Value::Null;
            }
            assert!(parse(missing).is_err(), "uncovered hour {hour}");
        }
        let mut shifted = staggered_pressure_grid(day as i64);
        for t in shifted[0]["hourly"]["time"].as_array_mut().unwrap() {
            *t = serde_json::json!(t.as_i64().unwrap() + 1800);
        }
        assert!(
            parse(shifted).is_err(),
            "incompatible partial hourly phases"
        );
    }

    #[test]
    fn complementary_pressure_weather_reuses_the_existing_offline_cache() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let mut vars = "temperature_2m,relative_humidity_2m,wind_speed_10m,wind_direction_10m,surface_pressure".to_string();
        for p in LEVELS_HPA {
            vars.push_str(&format!(
                ",temperature_{p}hPa,wind_speed_{p}hPa,wind_direction_{p}hPa"
            ));
        }
        let url = format!(
            "{OM_HIST_FORECAST}?latitude=0.000,0.000,0.000&longitude=0.000,1.000,2.000&start_date=2026-09-08&end_date=2026-09-08&hourly={vars}&wind_speed_unit=ms&timeformat=unixtime&timezone=GMT"
        );
        let (dir, path) = weather_test_cache(&url, "complementary-hours");
        let bytes = serde_json::to_vec(&staggered_pressure_grid(day as i64)).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let weather = fetch_atmosphere(
            &BBox::new(0.0, 0.0, 0.0, 2.0),
            day + 900.0,
            day + 6300.0,
            &dir,
            true,
        );
        assert!(weather.winds_aloft);
        assert!(weather.atmo.describe().contains("partial hourly grid"));
        assert_eq!(weather.warnings.len(), 1);
        let m = weather.atmo.medium(0.0, 0.5, day + 900.0, 30.0, 5000.0);
        assert!((m.wind_e - 13.95).abs() < 1e-10);
        assert!((m.wind_n - 4.05).abs() < 1e-10);
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn model_grid_retains_independent_hourly_phases_without_order_bias() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let (t0, t1) = (day + 4.25 * 3600.0, day + 5.75 * 3600.0);
        for levels in [false, true] {
            let cells = vec![
                open_meteo_fixture(day as i64 + 1800, 24),
                open_meteo_fixture(day as i64, 24),
                open_meteo_fixture(day as i64, 24),
            ];
            let got = parse_om_grid(
                serde_json::json!(cells),
                &[0.0],
                &[0.0, 1.0, 2.0],
                t0,
                t1,
                levels,
            )
            .unwrap();
            assert!(got.partial_cells.is_none());
            assert_eq!(got.warnings.len(), 1);
            assert!(got.warnings[0].contains("all 3 complete grid cells"));
            let expected = ScatteredAtmosphere::new(
                cells
                    .iter()
                    .enumerate()
                    .map(|(index, cell)| {
                        let (rows, phase) = if index == 0 {
                            (3..7, 0.5)
                        } else {
                            (4..7, 0.0)
                        };
                        let profiles = parse_location(cell, levels, rows.clone()).unwrap().profiles;
                        (
                            0.0,
                            index as f64,
                            rows.zip(profiles)
                                .map(|(hour, profile)| {
                                    (day + (hour as f64 + phase) * 3600.0, profile)
                                })
                                .collect(),
                        )
                    })
                    .collect(),
                "independent UTC columns".into(),
            )
            .unwrap();
            let mut reversed = cells.clone();
            reversed.reverse();
            let reordered = parse_om_grid(
                serde_json::json!(reversed),
                &[0.0],
                &[2.0, 1.0, 0.0],
                t0,
                t1,
                levels,
            )
            .unwrap();
            assert!(reordered.partial_cells.is_none());
            assert!(reordered.warnings[0].contains("all 3 complete grid cells"));
            for lon in [0.0, 0.5, 1.0, 2.0] {
                for t in [t0, day + 5.5 * 3600.0, t1] {
                    let a = got.medium(0.0, lon, t, 30.0, 5000.0);
                    let b = reordered.medium(0.0, lon, t, 30.0, 5000.0);
                    assert_eq!(a, expected.medium(0.0, lon, t, 30.0, 5000.0));
                    // Reversing an inverse-distance sum can change its rounding,
                    // but must not choose a different subset of the complete cells.
                    assert!((a.c - b.c).abs() < 1e-10);
                    assert!((a.wind_e - b.wind_e).abs() < 1e-10);
                    assert!((a.wind_n - b.wind_n).abs() < 1e-10);
                }
            }
        }
    }

    #[test]
    fn partial_model_cache_repairs_online_and_survives_unusable_refreshes() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let first = open_meteo_fixture(day as i64, 24);
        let complete = serde_json::json!([first, first]);
        let mut partial = complete.clone();
        partial[1]["hourly"]["temperature_2m"][5] = serde_json::Value::Null;
        // The second cell also lacks its final required timestamp. Its valid
        // shorter axis must not make the usable cached first cell disappear.
        for values in partial[1]["hourly"].as_object_mut().unwrap().values_mut() {
            values.as_array_mut().unwrap().truncate(6);
        }
        let mut none = partial.clone();
        none[0]["hourly"]["temperature_2m"][5] = serde_json::Value::Null;
        for replacement in [complete.clone(), none] {
            let (url, server) = serve_weather_once(&replacement);
            let (dir, path) = weather_test_cache(&url, "partial-grid");
            let prior = serde_json::to_vec(&partial).unwrap();
            std::fs::write(&path, &prior).unwrap();
            let parse = |v| {
                parse_om_grid(
                    v,
                    &[0.0],
                    &[0.0, 1.0],
                    day + 4.25 * 3600.0,
                    day + 5.75 * 3600.0,
                    true,
                )
            };
            let read = |offline| {
                get_cached_recoverable(
                    &url,
                    &dir,
                    offline,
                    (2, None, 0),
                    parse,
                    |a| a.partial_cells,
                    &mut Vec::new(),
                )
            };
            let offline = read(true).unwrap();
            assert_eq!(offline.partial_cells, Some(1));
            let repaired = read(false).unwrap();
            server.join().unwrap();
            if replacement == complete {
                assert!(repaired.partial_cells.is_none());
                assert!(repaired.warnings.is_empty());
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    serde_json::to_vec(&complete).unwrap()
                );
                assert!(read(false).unwrap().partial_cells.is_none());
            } else {
                assert_eq!(repaired.partial_cells, Some(1));
                assert_eq!(std::fs::read(&path).unwrap(), prior);
                assert_eq!(
                    repaired.medium(0.0, 0.5, day + 5.0 * 3600.0, 30.0, 5000.0),
                    offline.medium(0.0, 0.5, day + 5.0 * 3600.0, 30.0, 5000.0)
                );
                // A recent transport failure also preserves the usable cached cells
                // without opening another connection to the now-closed server.
                std::fs::write(path.with_extension("fail"), b"synthetic transport failure")
                    .unwrap();
                assert_eq!(read(false).unwrap().partial_cells, Some(1));
            }
            assert_eq!(read(true).unwrap().partial_cells, repaired.partial_cells);
        }
        // A first response with incomplete spatial coverage is still worth caching.
        let (url, server) = serve_weather_once(&partial);
        let (dir, path) = weather_test_cache(&url, "first-partial-grid");
        let parse = |v| {
            parse_om_grid(
                v,
                &[0.0],
                &[0.0, 1.0],
                day + 4.25 * 3600.0,
                day + 5.75 * 3600.0,
                true,
            )
        };
        let read = |offline| {
            get_cached_recoverable(
                &url,
                &dir,
                offline,
                (2, None, 0),
                parse,
                |a| a.partial_cells,
                &mut Vec::new(),
            )
        };
        assert_eq!(read(false).unwrap().partial_cells, Some(1));
        server.join().unwrap();
        assert_eq!(
            std::fs::read(path).unwrap(),
            serde_json::to_vec(&partial).unwrap()
        );
        assert_eq!(read(true).unwrap().partial_cells, Some(1));
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
            assert!(
                request.starts_with(b"GET /weather HTTP/1.1\r\n")
                    || request.starts_with(b"GET /weather?")
            );
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
    fn fetched_weather_survives_cache_publication_failure() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let (lat, lon) = (40.876466, -73.524951);
        let bbox = BBox::new(lat, lon, lat, lon);
        let (t0, t1) = (day + 4.25 * 3600.0, day + 5.75 * 3600.0);
        for levels in [false, true] {
            for usable in [false, true] {
                let mut value = open_meteo_fixture(day as i64, 24);
                if !usable {
                    value["hourly"]["temperature_2m"][5] = serde_json::Value::Null;
                }
                let (url, server) = serve_weather_once(&value);
                let (dir, _) = weather_test_cache(&url, "write-failure");
                // A file where the cache directory belongs fails on every platform,
                // without relying on user privileges or changing filesystem permissions.
                let blocked = dir.join("blocked");
                std::fs::create_dir(&blocked).unwrap();
                let marker = blocked.join("weather");
                std::fs::write(&marker, b"keep existing data").unwrap();
                let fetched = om_grid(&url, &bbox, t0, t1, &blocked, false, levels);
                server.join().unwrap();
                assert_eq!(std::fs::read(&marker).unwrap(), b"keep existing data");
                if !usable {
                    assert!(
                        fetched.is_err(),
                        "cache recovery must still validate weather"
                    );
                    continue;
                }
                let source = fetched.expect("valid weather must survive a cache-write failure");
                assert_eq!(source.warnings.len(), 1);
                assert!(source.warnings[0].contains("could not be cached"));
                let expected = parse_om_grid(value, &[lat], &[lon], t0, t1, levels).unwrap();
                let weather = if levels {
                    choose_atmosphere(Ok(source), |_| {
                        panic!("usable pressure levels keep priority")
                    })
                } else {
                    choose_atmosphere(Err(DataError::new("pressure levels unavailable")), |_| {
                        (Ok(source), Err(DataError::new("soundings unavailable")))
                    })
                };
                assert_eq!(weather.winds_aloft, levels);
                assert!(
                    weather
                        .warnings
                        .iter()
                        .any(|w| w.contains("could not be cached"))
                );
                assert!(!weather.warnings.iter().any(|w| w.contains("using a 15 C")));
                for time in [t0, (t0 + t1) / 2.0, t1] {
                    for (za, zb) in [(30.0, 800.0), (1500.0, 5000.0)] {
                        assert_eq!(
                            weather.atmo.medium(lat, lon, time, za, zb),
                            expected.medium(lat, lon, time, za, zb)
                        );
                    }
                }
                // Failure to persist does not create an offline cache or a cached error.
                assert!(om_grid(&url, &bbox, t0, t1, &blocked, true, levels).is_err());
            }
        }
    }

    #[test]
    fn radiosonde_cache_failures_keep_valid_station_and_sounding_data() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let (url, server) = serve_weather_once(&raob_station_index_fixture());
        let (dir, _) = weather_test_cache(&url, "raob-write-failure");
        let blocked = dir.join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        let marker = blocked.join("weather");
        std::fs::write(&marker, b"keep station and sounding caches").unwrap();
        let mut warnings = Vec::new();
        let stations =
            raob_stations_from(&url, &blocked, false, (day, day + 43_200.0), &mut warnings)
                .expect("a usable live index must remain available for sounding requests");
        server.join().unwrap();
        assert_eq!(stations.len(), 1);
        assert_eq!(warnings.len(), 1);
        let profile: Vec<_> = (0..6)
            .map(|i| {
                serde_json::json!({
                    "hght": 100.0 + 1200.0 * f64::from(i),
                    "tmpc": 15.0 - 6.0 * f64::from(i),
                    "dwpc": 5.0 - 6.0 * f64::from(i),
                    "drct": 270.0, "sknt": 20.0,
                })
            })
            .collect();
        let value = serde_json::json!({"profiles": [{"profile": profile}]});
        let (url, server) = serve_weather_once(&value);
        let (levels, recovered) = get_cached_validated(
            &url,
            &blocked,
            false,
            (2, None, 0),
            select_raob_profile,
            &mut warnings,
        )
        .expect("a validated sounding must remain usable without persistence");
        server.join().unwrap();
        assert_eq!(recovered, 0);
        assert_eq!(levels, parse_raob_response(value).unwrap());
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().all(|w| w.contains("could not be cached")));
        let (_, lat, lon, _) = stations[0];
        let atmo = ScatteredAtmosphere::new(
            vec![(lat, lon, vec![(day, levels)])],
            "retained sounding".into(),
        )
        .unwrap();
        let expected = atmo.medium(lat, lon, day, 30.0, 5000.0);
        let mut source = WeatherSource::complete(atmo);
        source.warnings = warnings;
        let updates = std::cell::RefCell::new(Vec::new());
        let source = publish_fallback(
            Ok(source),
            "pressure levels unavailable",
            true,
            &|weather| {
                updates.borrow_mut().push(weather);
            },
        );
        let weather = choose_atmosphere(Err(DataError::new("pressure levels unavailable")), |_| {
            (Err(DataError::new("surface unavailable")), source)
        });
        let updates = updates.borrow();
        assert_eq!(updates.len(), 1);
        for result in [&updates[0], &weather] {
            assert!(result.winds_aloft);
            assert_eq!(result.atmo.medium(lat, lon, day, 30.0, 5000.0), expected);
            assert_eq!(
                result
                    .warnings
                    .iter()
                    .filter(|w| w.contains("could not be cached"))
                    .count(),
                2
            );
        }
        assert_eq!(
            std::fs::read(&marker).unwrap(),
            b"keep station and sounding caches"
        );
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
                get_cached_validated(&url, &dir, offline, (2, None, 0), parse, &mut Vec::new())
            };
            let error = read(true).err().expect("offline must refuse the missing hour");
            assert!(error.0.contains("missing surface temperature"), "{error}");
            assert_eq!(std::fs::read(&path).unwrap(), prior);
            let recovered = read(false).expect("refresh restores the required hour");
            server.join().unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), serde_json::to_vec(&complete).unwrap());
            let expected = parse(complete).unwrap();
            let sample = |a: &WeatherSource| {
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
        let error = get_cached_validated(
            &url,
            &dir,
            false,
            (2, None, 0),
            |v| parse(v, 6.0),
            &mut Vec::new(),
        )
        .err()
        .expect("an unusable replacement must fail");
        server.join().unwrap();
        assert!(error.0.contains("no time axis"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), prior);
        assert!(!path.with_extension("fail").exists());
        // An error for one hour must not block the old response's usable hours.
        let old = get_cached_validated(
            &url,
            &dir,
            false,
            (2, None, 0),
            |v| parse(v, 2.0),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(
            old.medium(lat, lon, day + 7200.0, 30.0, 5000.0),
            parse(partial, 2.0).unwrap().medium(lat, lon, day + 7200.0, 30.0, 5000.0)
        );
    }

    fn raob_station_index_fixture() -> serde_json::Value {
        serde_json::json!({
            "features": [{
                "id": "KOLD",
                "properties": {
                    "country": "US", "elevation": 100.0,
                    "archive_begin": "2020-01-01", "archive_end": "2026-09-08",
                },
                "geometry": {"coordinates": [-73.524951, 40.876466]},
            }],
        })
    }

    #[test]
    fn cached_station_indexes_refresh_unusable_metadata() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let window = (day, day + 43_200.0);
        let complete = raob_station_index_fixture();
        let expected = vec![("KOLD".to_string(), 40.876466, -73.524951, 100.0)];
        for incomplete in [
            serde_json::json!({"features": null}),
            serde_json::json!({"features": []}),
            serde_json::json!({"features": [null, false, 17]}),
            serde_json::json!({"features": [{
                "id": "KAAA", "properties": {"country": "US"},
                "geometry": {"coordinates": [-73.524951, 91.0]},
            }]}),
        ] {
            let (url, server) = serve_weather_once(&complete);
            let (dir, path) = weather_test_cache(&url, "station-index");
            let prior = serde_json::to_vec(&incomplete).unwrap();
            std::fs::write(&path, &prior).unwrap();
            assert!(raob_stations_from(&url, &dir, true, window, &mut Vec::new()).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), prior);
            assert_eq!(
                raob_stations_from(&url, &dir, false, window, &mut Vec::new()).unwrap(),
                expected
            );
            server.join().unwrap();
            assert_eq!(
                std::fs::read(&path).unwrap(),
                serde_json::to_vec(&complete).unwrap()
            );
            assert!(!path.with_extension("fail").exists());
            // The one-shot server is gone. A valid index is reused without a request,
            // even for dates outside every station's lifetime, and still serves history.
            for offline in [false, true] {
                assert!(
                    raob_stations_from(
                        &url,
                        &dir,
                        offline,
                        (day + 86_400.0, day + 129_600.0),
                        &mut Vec::new(),
                    )
                    .unwrap()
                    .is_empty()
                );
                assert_eq!(
                    raob_stations_from(&url, &dir, offline, window, &mut Vec::new()).unwrap(),
                    expected
                );
            }
        }
    }

    #[test]
    fn invalid_station_index_refresh_preserves_valid_stale_metadata() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let window = (day, day + 43_200.0);
        let complete = raob_station_index_fixture();
        let expected = vec![("KOLD".to_string(), 40.876466, -73.524951, 100.0)];
        for incomplete in [
            serde_json::json!({"features": {}}),
            serde_json::json!({"features": []}),
        ] {
            let (url, server) = serve_weather_once(&incomplete);
            let (dir, path) = weather_test_cache(&url, "station-index-stale");
            let prior = serde_json::to_vec(&complete).unwrap();
            std::fs::write(&path, &prior).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
                .unwrap();
            assert_eq!(
                raob_stations_from(&url, &dir, false, window, &mut Vec::new()).unwrap(),
                expected
            );
            server.join().unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), prior);
            assert!(!path.with_extension("fail").exists());
            assert_eq!(
                raob_stations_from(&url, &dir, true, window, &mut Vec::new()).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn station_indexes_keep_valid_rows_beside_invalid_entries() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let mut listing = raob_station_index_fixture();
        let features = listing["features"].as_array_mut().unwrap();
        let valid = features[0].clone();
        for (id, country, lat, lon) in [
            ("KLOC", "US", 40.876466, 181.0),
            ("KLOC", "US", -91.0, -73.524951),
            ("bad", "US", 40.876466, -73.524951),
            ("KCAN", "CA", 40.876466, -73.524951),
        ] {
            let mut invalid = valid.clone();
            invalid["id"] = serde_json::json!(id);
            invalid["properties"]["country"] = serde_json::json!(country);
            invalid["geometry"]["coordinates"] = serde_json::json!([lon, lat]);
            features.push(invalid);
        }
        assert_eq!(
            parse_raob_stations(listing, (day, day + 43_200.0)).unwrap(),
            vec![("KOLD".to_string(), 40.876466, -73.524951, 100.0)]
        );
    }

    #[test]
    fn accepted_soundings_publish_before_later_profiles_are_requested() {
        use std::sync::Mutex;

        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let (lat, lon) = (40.876466, -73.524951);
        let bbox = BBox::new(lat, lon, lat, lon);
        let (dir, index_path) = weather_test_cache(IEM_RAOB_NETWORK, "raob-progress");
        std::fs::write(&index_path, raob_station_index_fixture().to_string()).unwrap();
        let profile = |speed: f64| {
            let levels: Vec<_> = (0..6)
                .map(|i| {
                    serde_json::json!({
                        "hght": 100.0 + 1200.0 * f64::from(i),
                        "tmpc": 15.0 - 6.0 * f64::from(i),
                        "dwpc": 5.0 - 6.0 * f64::from(i),
                        "drct": 270.0,
                        "sknt": speed,
                    })
                })
                .collect();
            serde_json::json!({"profiles": [{"profile": levels}]})
        };
        let (first, second) = (profile(20.0), profile(40.0));
        let at = |time| {
            dir.join("weather")
                .join(format!("{:016x}.json", fnv(&raob_url(time, "KOLD"))))
        };
        std::fs::write(at(day), first.to_string()).unwrap();
        let later_path = at(day + 43_200.0);
        assert!(!later_path.exists());
        let first_only = ScatteredAtmosphere::new(
            vec![(lat, lon, vec![(day, parse_raob_response(first.clone()).unwrap())])],
            "first sounding".into(),
        )
        .unwrap();
        let expected = ScatteredAtmosphere::new(
            vec![(
                lat,
                lon,
                vec![
                    (day, parse_raob_response(first).unwrap()),
                    (day + 43_200.0, parse_raob_response(second.clone()).unwrap()),
                ],
            )],
            "both soundings".into(),
        )
        .unwrap();
        let snapshots = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&snapshots);
        let weather = fetch_atmosphere_with_progress(
            &bbox,
            day + 3600.0,
            day + 39_600.0,
            &dir,
            true,
            move |update| {
                let mut snapshots = observed.lock().unwrap();
                if snapshots.is_empty() {
                    // Make the later profile available only after a real accepted
                    // sounding reaches the callback. No sleeps, sockets or network:
                    // publishing only after the entire RAOB fetch loses this bracket.
                    assert!(update.winds_aloft);
                    assert!(update.warnings.iter().any(|w| w.contains("still in progress")));
                    std::fs::write(&later_path, second.to_string()).unwrap();
                }
                snapshots.push(update);
            },
        );
        let snapshots = snapshots.lock().unwrap();
        assert_eq!(snapshots.len(), 3, "two partial profiles, then the completed source");
        assert!(weather.winds_aloft);
        assert!(weather.atmo.describe().contains("2 of 2 soundings usable"));
        assert!(weather.warnings.iter().all(|w| !w.contains("still in progress")));
        for time in [day, day + 21_600.0, day + 43_200.0] {
            for heights in [(30.0, 1000.0), (2500.0, 6000.0)] {
                let sample = |atmo: &dyn Atmosphere| {
                    atmo.medium(lat, lon, time, heights.0, heights.1)
                };
                assert_eq!(sample(&*snapshots[0].atmo), sample(&first_only));
                for complete in [&snapshots[1], &snapshots[2], &weather] {
                    assert_eq!(sample(&*complete.atmo), sample(&expected));
                }
            }
        }
        // Finishing the same cached request keeps the previous final-result contract.
        let final_only = fetch_atmosphere(&bbox, day + 3600.0, day + 39_600.0, &dir, true);
        assert_eq!(final_only.atmo.describe(), weather.atmo.describe());
        assert_eq!(final_only.warnings, weather.warnings);
    }

    #[test]
    fn invalid_cached_soundings_do_not_publish_partial_weather() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let (lat, lon) = (40.876466, -73.524951);
        let bbox = BBox::new(lat, lon, lat, lon);
        let (dir, index_path) = weather_test_cache(IEM_RAOB_NETWORK, "raob-no-progress");
        std::fs::write(index_path, raob_station_index_fixture().to_string()).unwrap();
        for time in [day, day + 43_200.0] {
            let path = dir.join("weather").join(format!(
                "{:016x}.json",
                fnv(&raob_url(time, "KOLD"))
            ));
            std::fs::write(path, r#"{"profiles": []}"#).unwrap();
        }
        let updates = Arc::new(AtomicUsize::new(0));
        let received = Arc::clone(&updates);
        let weather = fetch_atmosphere_with_progress(
            &bbox,
            day + 3600.0,
            day + 39_600.0,
            &dir,
            true,
            move |_| {
                received.fetch_add(1, Ordering::Relaxed);
            },
        );
        assert_eq!(updates.load(Ordering::Relaxed), 0);
        assert!(!weather.winds_aloft);
        assert!(weather.warnings.iter().any(|w| w.contains("radiosondes unavailable")));
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
                get_cached_validated(
                    &url,
                    &dir,
                    offline,
                    (2, None, 0),
                    parse_raob_response,
                    &mut Vec::new(),
                )
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
                    &parse_location(&complete, levels, 0..count)
                        .unwrap()
                        .profiles,
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
    fn cached_weather_aligns_each_location_by_its_required_utc_hours() {
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
        let second = open_meteo_fixture(day as i64 + 3600, 12);
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!([first, second])).unwrap(),
        )
        .unwrap();
        for (start, end, left, right) in [(4.25, 5.75, 4..7, 3..6), (6.0, 6.0, 6..7, 5..6)] {
            let got = om_grid(
                OM_ARCHIVE,
                &bbox,
                day + start * 3600.0,
                day + end * 3600.0,
                &dir,
                true,
                false,
            )
            .unwrap();
            assert!(got.warnings.is_empty());
            assert!(got.partial_cells.is_none());
            // Their hour-4 rows have different temperatures and winds: using the
            // first cell's row index for the second gives the wrong atmosphere.
            let left_profiles = parse_location(&first, false, left.clone())
                .unwrap()
                .profiles;
            let right_profiles = parse_location(&second, false, right).unwrap().profiles;
            let profiles: Vec<_> = left_profiles
                .iter()
                .zip(&right_profiles)
                .flat_map(|(a, b)| [a.clone(), b.clone()])
                .collect();
            let expected = GriddedAtmosphere::new(
                vec![lat],
                vec![lon, lon + 0.01],
                day + left.start as f64 * 3600.0,
                3600.0,
                &profiles,
                "independently aligned columns".into(),
            )
            .unwrap();
            for x in [lon, lon + 0.0037, lon + 0.01] {
                for hour in [start, 0.5 * (start + end), end] {
                    let t = day + hour * 3600.0;
                    for height in [1000.0, 5000.0] {
                        assert_eq!(
                            got.medium(lat, x, t, 30.0, height),
                            expected.medium(lat, x, t, 30.0, height)
                        );
                    }
                }
            }
        }
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
        let p = parse_location(&loc, true, 0..t.len()).unwrap().profiles;
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
    fn pressure_profiles_recover_surface_wind_without_fabricating_calm() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let (lat, lon) = (40.876466, -73.524951);
        let bbox = BBox::new(lat, lon, lat, lon);
        let (dir, _) = weather_test_cache("surface-wind", "surface-wind");
        let mut observed = open_meteo_fixture(day as i64, 3);
        // Each hourly column has an independently known constant wind with height.
        // East, north, then west also exercise vector interpolation across UTC hours.
        for suffix in std::iter::once("10m".to_string())
            .chain(LEVELS_HPA.into_iter().map(|p| format!("{p}hPa")))
        {
            observed["hourly"][format!("wind_speed_{suffix}")] =
                serde_json::json!([12.0, 18.0, 24.0]);
            observed["hourly"][format!("wind_direction_{suffix}")] =
                serde_json::json!([270.0, 180.0, 90.0]);
        }
        let load = |loc: &serde_json::Value| {
            cache_open_meteo_fixture(&dir, true, "2026-09-08", loc);
            fetch_atmosphere(&bbox, day + 900.0, day + 6300.0, &dir, true)
        };
        let expected = load(&observed);
        assert!(expected.warnings.is_empty());
        let known = [[12.0, 0.0], [0.0, 18.0], [-24.0, 0.0]];
        for (name, speed, direction) in [
            (
                "missing speed",
                serde_json::Value::Null,
                serde_json::json!(270.0),
            ),
            (
                "missing direction",
                serde_json::json!(12.0),
                serde_json::Value::Null,
            ),
            (
                "malformed speed",
                serde_json::json!("bad"),
                serde_json::json!(270.0),
            ),
            (
                "negative speed",
                serde_json::json!(-12.0),
                serde_json::json!(270.0),
            ),
            (
                "negative direction",
                serde_json::json!(12.0),
                serde_json::json!(-90.0),
            ),
            (
                "invalid direction",
                serde_json::json!(12.0),
                serde_json::json!(450.0),
            ),
        ] {
            let mut missing = observed.clone();
            missing["hourly"]["wind_speed_10m"] = serde_json::json!(vec![speed; 3]);
            missing["hourly"]["wind_direction_10m"] = serde_json::json!(vec![direction; 3]);
            // The first aloft level can be unavailable too; the next observed one
            // is the surface anchor, rather than a zero or a reversed negative speed.
            missing["hourly"]["wind_speed_925hPa"] = serde_json::json!(vec![-1.0; 3]);
            let recovered = load(&missing);
            assert!(recovered.winds_aloft, "{name}");
            for hour in [0.25, 0.5, 1.0, 1.75] {
                let i = hour as usize;
                let w = hour - i as f64;
                let wind = [
                    known[i][0] * (1.0 - w) + known[i + 1][0] * w,
                    known[i][1] * (1.0 - w) + known[i + 1][1] * w,
                ];
                for (ground, aircraft) in [(30.0, 100.0), (30.0, 4000.0), (8000.0, -100.0)] {
                    let t = day + hour * 3600.0;
                    let got = recovered.atmo.medium(lat, lon, t, ground, aircraft);
                    let want = expected.atmo.medium(lat, lon, t, ground, aircraft);
                    assert_eq!(got.c, want.c, "temperature/humidity changed: {name}");
                    assert!((got.wind_e - wind[0]).abs() < 1e-9, "{name}: {got:?}");
                    assert!((got.wind_n - wind[1]).abs() < 1e-9, "{name}: {got:?}");
                    assert!((got.wind_e - want.wind_e).abs() < 1e-9, "{name}");
                    assert!((got.wind_n - want.wind_n).abs() < 1e-9, "{name}");
                }
            }
            assert_eq!(recovered.warnings.len(), 2, "{name}");
            assert!(
                recovered.warnings[0].contains("3 retained cell-hours"),
                "{name}"
            );
            assert!(recovered.warnings[0].contains("nearest observed pressure-level wind"));
            assert!(recovered.warnings[1].contains("3 retained pressure-level winds"));
        }
        // Malformed aloft data must report its interpolation even when the surface
        // is observed; it must not be hidden behind the surface-recovery diagnostic.
        let mut bad_aloft = observed.clone();
        bad_aloft["hourly"]["wind_direction_925hPa"] = serde_json::json!(vec![450.0; 3]);
        let recovered = load(&bad_aloft);
        assert_eq!(recovered.warnings.len(), 1);
        assert!(recovered.warnings[0].contains("3 retained pressure-level winds"));
        assert_eq!(
            recovered.atmo.medium(lat, lon, day + 900.0, 30.0, 4000.0),
            expected.atmo.medium(lat, lon, day + 900.0, 30.0, 4000.0)
        );
        // Measured zero speed is calm even with undefined direction, and must keep
        // its own surface anchor instead of taking the wind above it.
        let mut calm = observed.clone();
        calm["hourly"]["wind_speed_10m"] = serde_json::json!(vec![0.0; 3]);
        let with_direction = load(&calm);
        calm["hourly"]["wind_direction_10m"] = serde_json::json!([null, null, null]);
        let without_direction = load(&calm);
        assert!(without_direction.warnings.is_empty());
        let near_ground = without_direction
            .atmo
            .medium(lat, lon, day + 900.0, 30.0, 100.0);
        assert_eq!(
            near_ground,
            with_direction
                .atmo
                .medium(lat, lon, day + 900.0, 30.0, 100.0)
        );
        assert!(
            near_ground.wind_e.abs() < 1.0,
            "observed calm was lost: {near_ground:?}"
        );
        assert!(
            expected
                .atmo
                .medium(lat, lon, day + 900.0, 30.0, 100.0)
                .wind_e
                - near_ground.wind_e
                > 8.0,
            "the old false-calm profile must differ materially from recovered propagation"
        );
        // The surface-only source keeps its existing no-direction fallback.
        calm["hourly"]["wind_speed_10m"] = serde_json::json!(vec![12.0; 3]);
        cache_open_meteo_fixture(&dir, false, "2026-09-08", &calm);
        let surface_only = om_grid(
            OM_ARCHIVE,
            &bbox,
            day + 900.0,
            day + 6300.0,
            &dir,
            true,
            false,
        )
        .unwrap()
        .medium(lat, lon, day + 900.0, 30.0, 4000.0);
        assert_eq!((surface_only.wind_e, surface_only.wind_n), (0.0, 0.0));
    }

    #[test]
    fn surface_wind_warning_counts_only_retained_pressure_cells_and_hours() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let mut missing = open_meteo_fixture(day as i64, 4);
        missing["hourly"]["wind_speed_10m"] = serde_json::json!([null, null, null, null]);
        let mut unusable = missing.clone();
        unusable["hourly"]["temperature_2m"][1] = serde_json::Value::Null;
        let source = parse_om_grid(
            serde_json::json!([missing, unusable, open_meteo_fixture(day as i64, 4)]),
            &[0.0],
            &[0.0, 1.0, 2.0],
            day + 900.0,
            day + 6300.0,
            true,
        )
        .unwrap();
        assert_eq!(source.partial_cells, Some(2));
        assert_eq!(source.warnings.len(), 2);
        assert!(source.warnings[0].contains("2 of 3 grid cells"));
        assert!(source.warnings[1].contains("3 retained cell-hours"));
        let weather = fetch_with_fallbacks(
            Ok(source),
            || panic!("valid pressure profiles must retain source priority"),
            || panic!("valid pressure profiles must retain source priority"),
            |_| panic!("no fallback should be published"),
        );
        assert!(weather.winds_aloft);
        assert_eq!(weather.warnings.len(), 2);
    }

    fn wind_only_pressure_fixture(day: i64) -> serde_json::Value {
        let mut value = open_meteo_fixture(day, 3);
        value["hourly"]["temperature_2m"] = serde_json::json!(vec![15.0; 3]);
        value["hourly"]["relative_humidity_2m"] = serde_json::json!(vec![30.0; 3]);
        value["hourly"]["wind_speed_10m"] = serde_json::json!([null, null, null]);
        for p in LEVELS_HPA {
            value["hourly"][format!("temperature_{p}hPa")] = serde_json::json!(vec![15.0; 3]);
            value["hourly"][format!("wind_speed_{p}hPa")] = serde_json::json!([null, null, null]);
        }
        value["hourly"]["temperature_850hPa"] = serde_json::json!([null, null, null]);
        value["hourly"]["wind_speed_850hPa"] = serde_json::json!([12.0, 18.0, 0.0]);
        value["hourly"]["wind_direction_850hPa"] = serde_json::json!([270.0, 180.0, null]);
        value
    }

    #[test]
    fn cached_pressure_profiles_recover_winds_without_coincident_temperatures() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let (lat, lon) = (40.876466, -73.524951);
        let (dir, _) = weather_test_cache("wind-only", "wind-only");
        let fixture = wind_only_pressure_fixture(day as i64);
        cache_open_meteo_fixture(&dir, true, "2026-09-08", &fixture);
        let weather = fetch_atmosphere(
            &BBox::new(lat, lon, lat, lon),
            day + 900.0,
            day + 6300.0,
            &dir,
            true,
        );
        assert!(weather.winds_aloft, "{:?}", weather.warnings);
        assert!(weather.atmo.describe().contains("pressure levels"));
        assert_eq!(weather.warnings.len(), 3);
        assert!(weather.warnings[2].contains("retained 3 observed pressure-level winds"));
        let winds = [[12.0, 0.0], [0.0, 18.0], [0.0, 0.0]];
        for hour in [0.25, 0.5, 1.0, 1.75] {
            let i = hour as usize;
            let w = hour - i as f64;
            for (lo, hi) in [(-100.0, 100.0), (30.0, 4000.0), (9000.0, 500.0)] {
                let got = weather.atmo.medium(lat, lon, day + hour * 3600.0, lo, hi);
                let want = [
                    (1.0 - w) * winds[i][0] + w * winds[i + 1][0],
                    (1.0 - w) * winds[i][1] + w * winds[i + 1][1],
                ];
                assert!((got.c - fdop_types::atmo::sound_speed(15.0, 0.3)).abs() < 1e-10);
                assert!((got.wind_e - want[0]).abs() < 1e-10, "{got:?}");
                assert!((got.wind_n - want[1]).abs() < 1e-10, "{got:?}");
            }
        }
        // Isothermal hydrostatics independently locate the recovered wind. It lies
        // between two pressure temperatures, so the surface's 2 m offset is irrelevant.
        let parsed = parse_location(&fixture, true, 0..3).unwrap();
        let z = 30.0 + 287.05 * (15.0 + 273.15) / 9.80665 * (1010.0_f64 / 850.0).ln();
        let recovered = parsed.profiles[0]
            .iter()
            .find(|l| (l.z_msl - z).abs() < 1e-9)
            .expect("observed wind must be located at the 850 hPa height");
        assert_eq!(recovered.wind_e, 12.0);
        assert_eq!(recovered.t_c, 15.0);
        assert_eq!(recovered.rh, 0.3);
    }

    #[test]
    fn wind_only_pressure_levels_preserve_thermal_profiles_and_recover_shear() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let (lat, lon) = (40.876466, -73.524951);
        let (dir, _) = weather_test_cache("wind-only-shear", "wind-only-shear");
        let mut fixture = open_meteo_fixture(day as i64, 3);
        for suffix in std::iter::once("10m".to_string())
            .chain(LEVELS_HPA.into_iter().map(|p| format!("{p}hPa")))
        {
            fixture["hourly"][format!("wind_speed_{suffix}")] = serde_json::json!(vec![0.0; 3]);
        }
        for p in [925, 850, 700] {
            fixture["hourly"][format!("temperature_{p}hPa")] =
                serde_json::json!([null, null, null]);
        }
        let load = |value: &serde_json::Value| {
            cache_open_meteo_fixture(&dir, true, "2026-09-08", value);
            fetch_atmosphere(
                &BBox::new(lat, lon, lat, lon),
                day + 900.0,
                day + 6300.0,
                &dir,
                true,
            )
        };
        // No missing-temperature winds in the control: it keeps exactly the original
        // surface/500/300 hPa thermodynamic support and observed calm aloft.
        let mut control = fixture.clone();
        for p in [925, 850, 700] {
            control["hourly"][format!("wind_speed_{p}hPa")] = serde_json::json!([null, null, null]);
        }
        let baseline = load(&control);
        fixture["hourly"]["wind_speed_850hPa"] = serde_json::json!(vec![20.0; 3]);
        let recovered = load(&fixture);
        assert!(baseline.winds_aloft && recovered.winds_aloft);
        assert!(baseline.warnings.is_empty());
        assert_eq!(recovered.warnings.len(), 1);
        assert!(recovered.warnings[0].contains("retained 9 observed pressure-level winds"));
        for hour in [0.25, 0.5, 1.0, 1.75] {
            for (lo, hi) in [
                (-100.0, 30.0),
                (30.0, 200.0),
                (200.0, 2200.0),
                (5000.0, 10000.0),
            ] {
                let t = day + hour * 3600.0;
                let got = recovered.atmo.medium(lat, lon, t, lo, hi);
                let old = baseline.atmo.medium(lat, lon, t, lo, hi);
                assert!(
                    (got.c - old.c).abs() < 1e-10,
                    "thermal profile changed: {got:?} {old:?}"
                );
            }
        }
        // A 50 m-aligned interval inside the 925..850 hPa linear wind segment
        // has the analytic midpoint wind; no cumulative-grid kink is crossed.
        let layer_log_pressure = (1010.0_f64 / 500.0).ln();
        let top = 30.0 + 287.05 / 9.80665 * (273.15 + 0.5 * (15.25 - 25.75)) * layer_log_pressure;
        let height = |p: f64| 32.0 + (top - 32.0) * (1010.0_f64 / p).ln() / layer_log_pressure;
        let (calm, peak) = (height(925.0), height(850.0));
        let lo = (calm / 50.0).ceil() * 50.0;
        let hi = (peak / 50.0).floor() * 50.0;
        assert!(hi > lo);
        let want = 20.0 * (0.5 * (lo + hi) - calm) / (peak - calm);
        let got = recovered.atmo.medium(lat, lon, day + 3600.0, lo, hi);
        assert!((got.wind_e - want).abs() < 1e-10, "{got:?} expected {want}");
        assert!(got.wind_n.abs() < 1e-10);
        assert!(
            got.wind_e > 5.0,
            "the original calm profile must be materially different"
        );
    }

    #[test]
    fn wind_only_pressure_recovery_keeps_support_gates_and_partial_warnings() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let fixture = wind_only_pressure_fixture(day as i64);
        for (speed, direction) in [
            (serde_json::Value::Null, serde_json::json!(270.0)),
            (serde_json::json!(-12.0), serde_json::json!(270.0)),
            (serde_json::json!(12.0), serde_json::Value::Null),
            (serde_json::json!(12.0), serde_json::json!(450.0)),
        ] {
            let mut bad = fixture.clone();
            bad["hourly"]["wind_speed_850hPa"][0] = speed;
            bad["hourly"]["wind_direction_850hPa"][0] = direction;
            assert!(
                parse_location(&bad, true, 0..1)
                    .err()
                    .unwrap()
                    .0
                    .contains("no winds aloft")
            );
        }
        // A wind above the highest observed temperature, or below the surface,
        // cannot make a usable profile by extrapolating temperature support.
        for (p, surface_pressure) in [(300, 1010.0), (925, 900.0)] {
            let mut unsupported = fixture.clone();
            unsupported["hourly"]["wind_speed_850hPa"] = serde_json::json!([null, null, null]);
            unsupported["hourly"][format!("temperature_{p}hPa")] =
                serde_json::json!([null, null, null]);
            unsupported["hourly"][format!("wind_speed_{p}hPa")] = serde_json::json!(vec![12.0; 3]);
            unsupported["hourly"]["surface_pressure"] =
                serde_json::json!(vec![surface_pressure; 3]);
            assert!(
                parse_location(&unsupported, true, 0..1)
                    .err()
                    .unwrap()
                    .0
                    .contains("no winds aloft")
            );
        }
        let mut short = fixture.clone();
        for p in [925, 700, 500] {
            short["hourly"][format!("temperature_{p}hPa")] = serde_json::json!([null, null, null]);
        }
        assert!(
            parse_location(&short, true, 0..1)
                .err()
                .unwrap()
                .0
                .contains("too few pressure levels")
        );
        let mut missing_hour = fixture.clone();
        missing_hour["hourly"]["temperature_2m"][1] = serde_json::Value::Null;
        let source = parse_om_grid(
            serde_json::json!([fixture, missing_hour]),
            &[0.0],
            &[0.0, 1.0],
            day + 900.0,
            day + 6300.0,
            true,
        )
        .unwrap();
        assert_eq!(source.partial_cells, Some(1));
        assert!(source.warnings[0].contains("1 of 2 grid cells"));
        assert!(
            source
                .warnings
                .last()
                .unwrap()
                .contains("retained 3 observed pressure-level winds")
        );
        let weather = fetch_with_fallbacks(
            Ok(source),
            || panic!("recovered wind-only pressure profiles keep source priority"),
            || panic!("recovered wind-only pressure profiles keep source priority"),
            |_| panic!("recovered wind-only pressure profiles need no fallback"),
        );
        assert!(weather.winds_aloft);
        assert!(
            weather
                .warnings
                .last()
                .unwrap()
                .contains("retained 3 observed pressure-level winds")
        );
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
        let p = parse_location(&loc(&hourly), true, 0..2).unwrap().profiles;
        let (surface, top) = (p[0].first().unwrap(), p[0].last().unwrap());
        assert!(surface.wind_e > 1.9 && surface.wind_e < 2.1, "{surface:?}");
        assert!(top.wind_e > 14.0 && top.wind_n < -14.0, "{top:?}");
        for speed in [-1.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            assert!(pressure_wind(speed, 270.0).is_none());
            hourly["wind_speed_500hPa"] = serde_json::json!([speed, speed]);
            let err = parse_location(&loc(&hourly), true, 0..2).err().unwrap();
            assert!(err.0.contains("no winds aloft"), "{err}");
        }
        // A reported calm aloft remains usable without a direction; absence or
        // malformed speed above must never earn this observed-calm interpretation.
        hourly["wind_speed_500hPa"] = serde_json::json!([0.0, 0.0]);
        hourly["wind_direction_500hPa"] = serde_json::json!([null, null]);
        let p = parse_location(&loc(&hourly), true, 0..2).unwrap().profiles;
        let top = p[0].last().unwrap();
        assert_eq!((top.wind_e, top.wind_n), (0.0, 0.0));
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
    fn cached_radiosondes_retain_observed_calm_without_a_direction() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let (lat, lon) = (40.876466, -73.524951);
        let bbox = BBox::new(lat, lon, lat, lon);
        let (dir, index_path) = weather_test_cache(IEM_RAOB_NETWORK, "raob-calm");
        std::fs::write(index_path, raob_station_index_fixture().to_string()).unwrap();
        let profile = |speed: f64, direction: Option<f64>| {
            (0..6)
                .map(|i| {
                    serde_json::json!({
                        "hght": 100.0 + 1200.0 * f64::from(i),
                        "tmpc": 10.0, "dwpc": 10.0,
                        "drct": direction, "sknt": speed,
                    })
                })
                .collect::<Vec<_>>()
        };
        let put = |hour, rows: &[serde_json::Value]| {
            let path = dir.join("weather").join(format!(
                "{:016x}.json",
                fnv(&format!("{IEM_RAOB}?ts=20260908{hour:02}00&station=OLD"))
            ));
            std::fs::write(
                path,
                serde_json::json!({"profiles": [{"profile": rows}]}).to_string(),
            )
            .unwrap();
        };
        let load = || fetch_atmosphere(&bbox, day + 3600.0, day + 39_600.0, &dir, true);
        // The calm 00Z sounding must remain an interpolation anchor, not disappear
        // and let the later 12Z wind be extrapolated over the entire recording.
        for later_speed in [0.0, 20.0] {
            put(0, &profile(0.0, None));
            put(
                12,
                &profile(later_speed, (later_speed > 0.0).then_some(270.0)),
            );
            let weather = load();
            assert!(weather.winds_aloft, "{:?}", weather.warnings);
            assert!(weather.atmo.describe().contains("2 of 2 soundings usable"));
            assert!(
                weather
                    .warnings
                    .iter()
                    .all(|w| !w.contains("radiosondes unavailable"))
            );
            for hour in [1.0, 6.0, 11.0] {
                for heights in [(30.0, 1000.0), (2500.0, 6000.0)] {
                    let m =
                        weather
                            .atmo
                            .medium(lat, lon, day + hour * 3600.0, heights.0, heights.1);
                    assert!(
                        (m.c - fdop_types::atmo::sound_speed(10.0, 1.0)).abs() < 1e-8,
                        "{m:?}"
                    );
                    assert!(
                        (m.wind_e - later_speed * KT * hour / 12.0).abs() < 1e-8,
                        "{m:?}"
                    );
                    assert!(m.wind_n.abs() < 1e-8, "{m:?}");
                }
            }
        }
        // A wind-only calm layer must also survive between windier rows.
        // Its triangular wind profile integrates to half the surrounding speed.
        let mut rows = profile(20.0, Some(270.0));
        rows.insert(3, serde_json::json!({"hght": 3100.0, "sknt": 0.0}));
        put(0, &rows);
        put(12, &rows);
        let weather = load();
        assert!(weather.winds_aloft, "{:?}", weather.warnings);
        let m = weather
            .atmo
            .medium(lat, lon, day + 21_600.0, 2500.0, 3700.0);
        assert!(
            (m.c - fdop_types::atmo::sound_speed(10.0, 1.0)).abs() < 1e-8,
            "a wind-only calm layer must preserve the observed saturated air: {m:?}"
        );
        assert!((m.wind_e - 10.0 * KT).abs() < 1e-8, "{m:?}");
        assert!(m.wind_n.abs() < 1e-8, "{m:?}");
    }

    #[test]
    fn wind_only_radiosondes_preserve_observed_thermal_profiles() {
        for temperature_shift in [0.0, 2.0] {
            let original: Vec<_> = (0..6)
                .map(|i| {
                    let t = 28.0 - 5.0 * f64::from(i) + temperature_shift;
                    serde_json::json!({
                        "hght": 100.0 + 1200.0 * f64::from(i),
                        "tmpc": t, "dwpc": t - 1.0 - 2.0 * f64::from(i),
                        "drct": 270.0, "sknt": 20.0,
                    })
                })
                .collect();
            let mut rows = original.clone();
            rows.extend([
                serde_json::json!({"hght": 700.0, "sknt": 40.0, "drct": 270.0}),
                serde_json::json!({"hght": 3100.0, "sknt": 0.0}),
                // Wind-only rows outside the observed temperature range stay unusable.
                serde_json::json!({"hght": -200.0, "sknt": 100.0, "drct": 90.0}),
                serde_json::json!({"hght": 8000.0, "sknt": 100.0, "drct": 90.0}),
            ]);
            let retained = parse_raob(&serde_json::json!(rows));
            let observed_levels = parse_raob(&serde_json::json!(original));
            assert_eq!(observed_levels.len(), 6);
            assert_eq!(retained.len(), 8);
            for observed in &observed_levels {
                let same = retained.iter().find(|l| l.z_msl == observed.z_msl).unwrap();
                assert_eq!(same.t_c, observed.t_c);
                assert_eq!(same.rh, observed.rh);
                assert_eq!(same.wind_e, observed.wind_e);
                assert_eq!(same.wind_n, observed.wind_n);
            }
            let baseline = fdop_types::atmo::Column::from_levels(&observed_levels);
            let recovered = fdop_types::atmo::Column::from_levels(&retained);
            for (lo, hi) in [
                (-100.0, 30.0),
                (100.0, 1300.0),
                (400.0, 1000.0),
                (2500.0, 3700.0),
                (100.0, 6100.0),
                (6200.0, 9000.0),
            ] {
                let expected = baseline.mean(lo, hi);
                let actual = recovered.mean(lo, hi);
                assert!(
                    (actual.c - expected.c).abs() < 1e-10,
                    "wind-only rows changed sound speed for temperature shift {temperature_shift}, heights {lo}..{hi}: {actual:?} vs {expected:?}"
                );
            }
            // Each extra wind knot forms a triangle between unchanged thermal rows.
            // The sampled integration grid includes these 50 m-aligned endpoints.
            for (lo, hi, factor) in [(100.0, 1300.0, 1.5), (2500.0, 3700.0, 0.5)] {
                let m = recovered.mean(lo, hi);
                assert!((m.wind_e - factor * 20.0 * KT).abs() < 1e-8, "{m:?}");
                assert!(m.wind_n.abs() < 1e-8, "{m:?}");
            }
        }
    }

    #[test]
    fn radiosondes_recover_invalid_wind_rows_only_from_observed_vectors() {
        for (speed, direction) in [
            (serde_json::Value::Null, serde_json::json!(270.0)),
            (serde_json::json!("missing"), serde_json::json!(270.0)),
            (serde_json::json!(-1.0), serde_json::json!(270.0)),
            (serde_json::json!(20.0), serde_json::Value::Null),
            (serde_json::json!(20.0), serde_json::json!("missing")),
            (serde_json::json!(20.0), serde_json::json!(-1.0)),
            (serde_json::json!(20.0), serde_json::json!(361.0)),
        ] {
            let mut rows: Vec<_> = (0..6)
                .map(|i| {
                    serde_json::json!({
                        "hght": 100.0 + 1200.0 * f64::from(i), "tmpc": 10.0,
                        "sknt": speed, "drct": direction,
                    })
                })
                .collect();
            assert!(
                parse_raob(&serde_json::json!(rows)).is_empty(),
                "{speed}, {direction}"
            );
            rows[0]["sknt"] = serde_json::json!(20.0);
            rows[0]["drct"] = serde_json::json!(270.0);
            assert!(
                parse_raob(&serde_json::json!(rows)).is_empty(),
                "surface wind alone"
            );
            rows[5]["sknt"] = serde_json::json!(40.0);
            rows[5]["drct"] = serde_json::json!(270.0);
            let levels = parse_raob(&serde_json::json!(rows));
            assert_eq!(levels.len(), 6);
            for (i, level) in levels.iter().enumerate() {
                assert!(
                    (level.wind_e - (20.0 + 4.0 * i as f64) * KT).abs() < 1e-8,
                    "{level:?}"
                );
                assert!(level.wind_n.abs() < 1e-8, "{level:?}");
            }
        }
    }

    #[test]
    fn cached_radiosondes_keep_measured_calm_without_a_direction() {
        use fdop_types::atmo::sound_speed;
        let (lat, lon) = (40.876466, -73.524951);
        let day = fdop_types::time::unix_from_utc(2024, 9, 8, 0, 0, 0.0);
        let bbox = BBox::new(lat, lon, lat, lon);
        let (dir, index) = weather_test_cache(IEM_RAOB_NETWORK, "calm-raob");
        std::fs::write(
            index,
            serde_json::json!({"features": [{
                "id": "KAAA", "properties": {"country": "US"},
                "geometry": {"coordinates": [lon, lat]},
            }]})
            .to_string(),
        )
        .unwrap();
        for (label, fractions, expected_fraction) in [
            ("calm", [0.0; 6], 0.0),
            ("calm layer", [1.0, 1.0, 0.0, 0.0, 1.0, 1.0], 0.6),
            ("ordinary wind", [1.0; 6], 1.0),
        ] {
            for (hour, speed) in [("00", 20.0), ("12", 40.0)] {
                let profile: Vec<_> = fractions
                    .iter()
                    .enumerate()
                    .map(|(i, &fraction)| {
                        serde_json::json!({
                            "hght": 1200.0 * i as f64,
                            "tmpc": 10.0, "dwpc": 10.0,
                            "sknt": speed * fraction,
                            "drct": (fraction > 0.0).then_some(270.0),
                        })
                    })
                    .collect();
                let url = format!("{IEM_RAOB}?ts=20240908{hour}00&station=AAA");
                let path = dir.join("weather").join(format!("{:016x}.json", fnv(&url)));
                std::fs::write(
                    path,
                    serde_json::json!({"profiles": [{"profile": profile}]}).to_string(),
                )
                .unwrap();
            }
            let weather = fetch_atmosphere(&bbox, day + 3600.0, day + 39_600.0, &dir, true);
            let source = weather.atmo.describe();
            assert!(
                weather.winds_aloft,
                "{label}: {source}; {:?}",
                weather.warnings
            );
            assert!(source.contains("KAAA") && source.contains("2 of 2 soundings usable"));
            assert!(
                !weather
                    .warnings
                    .iter()
                    .any(|w| w.contains("standard atmosphere"))
            );
            // Equal-height trapezoids give 0, 3/5 or 1 of the observed wind;
            // the halfway clock gives 30 knots before this height average.
            for t in [day, day + 21_600.0, day + 43_200.0] {
                let speed = 20.0 + 20.0 * (t - day) / 43_200.0;
                let medium = weather.atmo.medium(lat, lon, t, 0.0, 6000.0);
                assert!(
                    (medium.c - sound_speed(10.0, 1.0)).abs() < 1e-8,
                    "{label}: {medium:?}"
                );
                assert!(
                    (medium.wind_e - expected_fraction * speed * KT).abs() < 1e-8,
                    "{label}: {medium:?}"
                );
                assert!(medium.wind_n.abs() < 1e-8, "{label}: {medium:?}");
            }
        }
    }

    #[test]
    fn radiosonde_calm_does_not_turn_missing_or_malformed_vectors_into_wind() {
        let profile = |speed: serde_json::Value, direction: serde_json::Value| {
            serde_json::json!((0..6).map(|i| serde_json::json!({
                "hght": 1200.0 * f64::from(i), "tmpc": 10.0, "dwpc": 10.0,
                "sknt": speed, "drct": direction,
            })).collect::<Vec<_>>())
        };
        // A direction tag is optional only for measured zero-speed calm.
        for direction in [serde_json::json!(null), serde_json::json!(270.0)] {
            let rows = profile(serde_json::json!(0.0), direction);
            let parsed = parse_raob(&rows);
            assert_eq!(parsed.len(), 6);
            assert!(parsed.iter().all(|l| l.wind_e == 0.0 && l.wind_n == 0.0));
        }
        for (speed, direction) in [
            (serde_json::json!(null), serde_json::json!(null)),
            (serde_json::json!(20.0), serde_json::json!(null)),
            (serde_json::json!(-1.0), serde_json::json!(270.0)),
            (serde_json::json!(20.0), serde_json::json!(-1.0)),
            (serde_json::json!(20.0), serde_json::json!(361.0)),
            (serde_json::json!("bad"), serde_json::json!(270.0)),
        ] {
            let mut rows = profile(speed, direction);
            assert!(parse_raob(&rows).is_empty(), "{rows}");
            // Surface-only observations still cannot supply winds aloft.
            rows[0]["sknt"] = serde_json::json!(20.0);
            rows[0]["drct"] = serde_json::json!(270.0);
            assert!(parse_raob(&rows).is_empty(), "{rows}");
        }
        // A wind-only calm level must remain an anchor between real temperature levels.
        let mut rows = profile(serde_json::json!(20.0), serde_json::json!(270.0));
        rows[2]["tmpc"] = serde_json::Value::Null;
        rows[2]["sknt"] = serde_json::json!(0.0);
        rows[2]["drct"] = serde_json::Value::Null;
        let parsed = parse_raob(&rows);
        assert_eq!(parsed.len(), 6);
        assert_eq!((parsed[2].z_msl, parsed[2].t_c), (2400.0, 10.0));
        assert_eq!((parsed[2].wind_e, parsed[2].wind_n), (0.0, 0.0));
        for (i, level) in parsed.iter().enumerate().filter(|(i, _)| *i != 2) {
            let expected = wind_en(20.0 * KT, 270.0);
            assert_eq!(
                (level.wind_e.to_bits(), level.wind_n.to_bits()),
                (expected.0.to_bits(), expected.1.to_bits()),
                "level {i}"
            );
        }
    }

    #[test]
    fn radiosonde_request_ids_preserve_us_prefixes_and_continental_cache_keys() {
        let t = fdop_types::time::unix_from_utc(2024, 9, 8, 12, 0, 0.0);
        for (id, requested) in [
            ("KAAA", "AAA"),
            ("PAAA", "PAAA"),
            ("PHAA", "PHAA"),
            ("_AAA", "_AAA"),
        ] {
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
            (
                "virtual",
                "_AAA",
                "_AAA",
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
                    feature("_ALL", serde_json::json!({"country": "CA"})),
                    feature("_LONG", serde_json::json!({"country": "US"})),
                    feature("_A&B", serde_json::json!({"country": "US"})),
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
                let selected = serde_json::json!({"station": "KAAA", "profile": profile});
                let profiles = if label == "virtual" {
                    vec![serde_json::json!({"station": "KOLD", "profile": []}), selected]
                } else {
                    vec![selected]
                };
                put(&url, serde_json::json!({"profiles": profiles}));
            }
            let chosen = raob_stations(&dir, true, (day, day + 43_200.0), &mut Vec::new()).unwrap();
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
            assert_eq!(
                weather.warnings.iter().any(|w| w.contains(
                    "recovered 2 sounding(s) from later returned profiles"
                )),
                label == "virtual",
                "{:?}",
                weather.warnings
            );
            assert!(
                weather
                    .warnings
                    .iter()
                    .any(|w| w.contains("hourly surface temperatures unavailable"))
            );
        }
    }

    #[test]
    fn radiosonde_alias_selection_preserves_the_first_usable_profile() {
        let rows = |speed: serde_json::Value| {
            (0..6)
                .map(|i| {
                    serde_json::json!({
                        "hght": 100.0 + 1200.0 * f64::from(i),
                        "tmpc": 10.0, "dwpc": 10.0,
                        "drct": null, "sknt": speed,
                    })
                })
                .collect::<Vec<_>>()
        };
        let calm_rows = rows(serde_json::json!(0.0));
        let calm = serde_json::json!({"station": "KAAA", "profile": calm_rows});
        let mut windy = calm.clone();
        for level in windy["profile"].as_array_mut().unwrap() {
            level["drct"] = serde_json::json!(270.0);
            level["sknt"] = serde_json::json!(20.0);
        }
        let (expected, index) = select_raob_profile(serde_json::json!({
            "profiles": [calm, windy]
        }))
        .unwrap();
        assert_eq!(
            index, 0,
            "measured calm remains authoritative without direction"
        );
        assert_eq!(expected.len(), 6);
        assert!(expected.iter().all(|l| l.wind_e == 0.0 && l.wind_n == 0.0));
        let invalid = vec![
            serde_json::Value::Null,
            serde_json::json!({"profile": []}),
            serde_json::json!({"profile": &calm_rows[..4]}),
            serde_json::json!({"profile": rows(serde_json::Value::Null)}),
            serde_json::json!({"profile": rows(serde_json::json!(20.0))}),
        ];
        for partial in &invalid {
            let (actual, index) = select_raob_profile(serde_json::json!({
                "profiles": [partial, calm, windy]
            }))
            .unwrap();
            assert_eq!(actual, expected);
            assert_eq!(index, 1);
        }
        assert!(select_raob_profile(serde_json::json!({"profiles": invalid})).is_err());
        assert!(select_raob_profile(serde_json::json!({"profiles": []})).is_err());
    }

    #[test]
    fn virtual_radiosonde_ids_keep_country_geometry_and_lifetime_checks() {
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let mut index = raob_station_index_fixture();
        index["features"][0]["id"] = serde_json::json!("_AAA");
        let selected = parse_raob_stations(index.clone(), (day, day + 43_200.0)).unwrap();
        assert_eq!(
            selected,
            vec![("_AAA".to_string(), 40.876466, -73.524951, 100.0)]
        );
        for bad_id in ["_AA", "_LONG", "_A&B", "_A/B", "_A?B", "_A B", "_éA", ""] {
            let mut bad = index.clone();
            bad["features"][0]["id"] = serde_json::json!(bad_id);
            assert!(parse_raob_stations(bad, (day, day)).is_err(), "{bad_id}");
        }
        for country in [serde_json::Value::Null, serde_json::json!("CA")] {
            let mut bad = index.clone();
            bad["features"][0]["properties"]["country"] = country;
            assert!(parse_raob_stations(bad, (day, day)).is_err());
        }
        let mut bad = index.clone();
        bad["features"][0]["geometry"]["coordinates"][1] = serde_json::json!(91.0);
        assert!(parse_raob_stations(bad, (day, day)).is_err());
        assert!(
            parse_raob_stations(index, (day + 86_400.0, day + 129_600.0))
                .unwrap()
                .is_empty()
        );
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
                &mut Vec::new(),
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
    fn radiosondes_recover_usable_nearby_archives_within_the_station_budget() {
        #[derive(Clone, Copy)]
        enum Profile {
            Empty,
            Short,
            Stale,
            Valid,
        }
        use Profile::{Empty, Short, Stale, Valid};
        let day = fdop_types::time::unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let (lat, lon) = (40.876466, -73.524951);
        let bbox = BBox::new(lat - 0.001, lon - 0.001, lat + 0.001, lon + 0.001);
        let ids = ["KAAA", "KBBB", "KCCC", "KDDD", "KEEE"];
        for (label, profiles, offsets, expected, tried) in [
            (
                "recovery",
                [Empty, Short, Valid, Valid, Valid],
                [0.1, 0.3, 0.5, 0.6, 0.7],
                Some(2),
                3,
            ),
            (
                "preferred",
                [Valid; 5],
                [0.1, 0.3, 0.5, 0.6, 0.7],
                Some(0),
                1,
            ),
            (
                "preferred-blend",
                [Valid; 5],
                [0.1, 0.15, 0.5, 0.6, 0.7],
                Some(0),
                2,
            ),
            (
                "stale-day",
                [Empty, Stale, Valid, Valid, Valid],
                [0.1, 0.3, 0.5, 0.6, 0.7],
                Some(2),
                3,
            ),
            (
                "budget",
                [Empty, Short, Empty, Short, Valid],
                [0.1, 0.3, 0.5, 0.6, 0.7],
                None,
                4,
            ),
            (
                "distance",
                [Empty, Valid, Valid, Valid, Valid],
                [0.1, 7.0, 7.2, 7.4, 7.6],
                None,
                1,
            ),
            (
                "unavailable",
                [Empty; 5],
                [0.1, 0.3, 0.5, 0.6, 0.7],
                None,
                4,
            ),
        ] {
            let (dir, _) = weather_test_cache(IEM_RAOB_NETWORK, label);
            let put = |url: &str, value: serde_json::Value| {
                let path = dir.join("weather").join(format!("{:016x}.json", fnv(url)));
                std::fs::write(path, value.to_string()).unwrap();
            };
            let mut features: Vec<_> = ids
                .iter()
                .zip(offsets)
                .map(|(id, offset)| {
                    serde_json::json!({
                        "id": id, "properties": {"country": "US"},
                        "geometry": {"coordinates": [lon, lat + offset]},
                    })
                })
                .collect();
            // An even nearer, well-populated station closed before the requested date.
            features.push(serde_json::json!({
                "id": "KOLD", "properties": {"country": "US", "archive_end": "2026-09-07"},
                "geometry": {"coordinates": [lon, lat + 0.01]},
            }));
            put(IEM_RAOB_NETWORK, serde_json::json!({"features": features}));
            for (i, (id, profile)) in ids
                .iter()
                .zip(profiles)
                .chain([(&"KOLD", Valid)])
                .enumerate()
            {
                for hour in [0.0, 43_200.0] {
                    let levels: Vec<_> = (0..match profile {
                        Empty => 0,
                        Short => 3,
                        _ => 6,
                    })
                        .map(|k| {
                            serde_json::json!({
                                "hght": 100.0 + 1200.0 * f64::from(k),
                                "tmpc": 10.0, "dwpc": 10.0, "drct": 270.0,
                                "sknt": 20.0 + 10.0 * i as f64 + hour / 4320.0,
                            })
                        })
                        .collect();
                    let t = day + hour
                        - if matches!(profile, Stale) {
                            86_400.0
                        } else {
                            0.0
                        };
                    put(
                        &raob_url(t, id),
                        serde_json::json!({"profiles": [{"profile": levels}]}),
                    );
                }
            }
            let result = raob_atmosphere(&bbox, day + 3600.0, day + 39_600.0, &dir, true);
            let Some(index) = expected else {
                assert!(
                    result
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("no radiosonde soundings"),
                    "{label}"
                );
                continue;
            };
            let atmosphere = result.unwrap_or_else(|e| panic!("{label}: {e}"));
            let source = atmosphere.describe();
            let (names, usable, expected_wind) = if label == "preferred-blend" {
                // The two stations are on the same meridian at a 2:3 distance ratio.
                ("KAAA, KBBB", 4, (25.0 * 9.0 + 35.0 * 4.0) / 13.0 * KT)
            } else {
                (ids[index], 2, (25.0 + 10.0 * index as f64) * KT)
            };
            assert!(
                source.starts_with(&format!("NWS radiosondes {names} (")),
                "{label}: {source}"
            );
            assert!(
                source.contains(&format!("{usable} of {} soundings usable", 2 * tried)),
                "{label}: {source}"
            );
            assert_eq!(
                source.contains("recovered from the nearest usable station"),
                index > 0,
                "{label}: {source}"
            );
            assert!(
                !source.contains("KOLD") && !source.contains("KDDD"),
                "{label}: {source}"
            );
            let medium = atmosphere.medium(lat, lon, day + 21_600.0, 100.0, 5000.0);
            assert!(
                (medium.wind_e - expected_wind).abs() < 1e-8,
                "{label}: {medium:?}"
            );
            assert!(medium.wind_n.abs() < 1e-8, "{label}: {medium:?}");
            // Exercise the actual fallback chain too: recovered winds must reach locate.
            let weather = fetch_atmosphere(&bbox, day + 3600.0, day + 39_600.0, &dir, true);
            assert!(weather.winds_aloft, "{label}: {:?}", weather.warnings);
            assert_eq!(weather.atmo.describe(), source);
            assert!(
                weather
                    .warnings
                    .iter()
                    .any(|w| w.contains("Open-Meteo pressure levels unavailable"))
            );
            assert!(
                weather
                    .warnings
                    .iter()
                    .any(|w| w.contains("hourly surface temperatures unavailable"))
            );
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
