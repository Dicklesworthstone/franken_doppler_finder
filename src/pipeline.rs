//! `fdop locate`: media file + dates + coarse US region -> location, radius, map, place.

use fdop_core::detect::{self, DetectParams, Detection};
use fdop_core::measure::{self, MeasureParams, MeasuredEvent};
use fdop_core::search::{self, SearchEvent, SearchInputs, SearchParams, SearchResult};
use fdop_core::spectro::{Axes, Overlay, Spec, render_png_axes};
use fdop_core::{audio, physics, track};
use fdop_geo::region::{Region, RegionContext, parse_region};
use fdop_geo::report::{Answer, CalibRow, CandidateRow, EventRow, Heatmap, MatchRow, ReportInput};
use fdop_types::time::{iso_utc, parse_date, parse_iso_utc, unix_from_utc};
use fdop_types::{BBox, Elevation};
use rayon::prelude::*;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct LocateArgs {
    pub media: PathBuf,
    /// "YYYY-MM-DD" or "YYYY-MM-DD..YYYY-MM-DD" (local dates at the region).
    pub dates: String,
    /// Coarse region: state, "City, ST", county, ZIP, "lat,lon[,km]", "S,W,N,E", "CONUS".
    pub near: String,
    pub out: Option<PathBuf>,
    pub cache: PathBuf,
    pub offline: bool,
    /// Exact recording start (ISO 8601) if known; narrows the clock search.
    pub start: Option<String>,
    pub clock_tol_s: f64,
    /// Ignore the container's creation_time metadata.
    pub ignore_metadata_time: bool,
    pub max_events: usize,
    pub sr: u32,
    pub quiet: bool,
}

#[derive(Debug)]
pub struct LocateOutcome {
    pub run_dir: PathBuf,
    pub report: ReportInput,
    pub found: bool,
}

pub struct Log {
    t0: Instant,
    quiet: bool,
    lines: std::sync::Mutex<Vec<String>>,
}

impl Log {
    pub fn new(quiet: bool) -> Arc<Self> {
        Arc::new(Self {
            t0: Instant::now(),
            quiet,
            lines: Default::default(),
        })
    }
    pub fn say(&self, s: &str) {
        let line = format!("[{:7.1}s] {s}", self.t0.elapsed().as_secs_f64());
        if !self.quiet {
            eprintln!("{line}");
        }
        if let Ok(mut l) = self.lines.lock() {
            l.push(line);
        }
    }
    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

/// A civil date (year, month, day).
pub type Ymd = (i64, u32, u32);
/// Points and colour of an extra curve on an event figure.
type ExtraCurve = (Vec<(f64, f64)>, [u8; 3]);

pub(crate) fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Parse "D0", "D0..D1", "D0 to D1", "D0,D1" into inclusive local dates.
pub fn parse_dates(s: &str) -> Result<(Ymd, Ymd), String> {
    let t = s.trim();
    let parts: Vec<&str> = if let Some((a, b)) = t.split_once("..") {
        vec![a, b]
    } else if let Some((a, b)) = t.split_once(" to ") {
        vec![a, b]
    } else if let Some((a, b)) = t.split_once(',') {
        vec![a, b]
    } else {
        vec![t]
    };
    let d0 = parse_date(parts[0])?;
    let d1 = if parts.len() > 1 {
        parse_date(parts[1])?
    } else {
        d0
    };
    let u0 = unix_from_utc(d0.0, d0.1, d0.2, 0, 0, 0.0);
    let u1 = unix_from_utc(d1.0, d1.1, d1.2, 0, 0, 0.0);
    if u1 < u0 {
        return Err(format!("date range '{s}' ends before it starts"));
    }
    if u1 - u0 > 14.0 * 86_400.0 {
        return Err(format!(
            "date range '{s}' spans more than 15 days; narrow it"
        ));
    }
    Ok((d0, d1))
}

/// UTC window for the recording START: local midnight of the first day .. local midnight after
/// the last day, using the region's UTC offset range (DST included).
pub fn start_window(d0: Ymd, d1: Ymd, off: (f64, f64)) -> (f64, f64) {
    let a = unix_from_utc(d0.0, d0.1, d0.2, 0, 0, 0.0) - off.1 * 3600.0;
    let b = unix_from_utc(d1.0, d1.1, d1.2, 0, 0, 0.0) + 86_400.0 - off.0 * 3600.0;
    (a, b)
}

/// serde_json writes non-finite floats as null, so the output is always valid JSON.
fn write_json(path: &Path, v: &impl serde::Serialize) -> Result<(), String> {
    let s = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    std::fs::write(path, s).map_err(|e| format!("{}: {e}", path.display()))
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "recording".into())
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Survey image with detections marked (log-frequency rows).
pub(crate) fn survey_png(
    ls: &detect::LogSpec,
    lin: &Spec,
    dets: &[Detection],
    events: &[MeasuredEvent],
    path: &Path,
) -> Result<(), String> {
    let rows: Vec<f64> = (0..ls.nr()).map(|r| r as f64).collect();
    let spec = Spec {
        t: ls.t.clone(),
        f: rows,
        db: ls.data.clone(),
        meta: lin.meta.clone(),
    };
    let bpo = ls.bins_per_octave as f64;
    let fmin = ls.freqs[0];
    let row = |f: f64| (f / fmin).log2() * bpo;
    let mut ovs = Vec::new();
    for d in dets.iter().filter(|d| d.harmonic_of.is_none()) {
        let t0e = d.t0r - d.d / 343.0;
        let pts: Vec<(f64, f64)> = (-30..=30)
            .map(|k| {
                let t = d.t0r + k as f64 * d.half_s / 30.0;
                (t, row(physics::cv_curve(t, d.f0, d.v, d.d, t0e, 343.0)))
            })
            .collect();
        ovs.push(Overlay {
            points: pts,
            color: [90, 200, 255],
            dots: false,
        });
    }
    for e in events.iter().filter(|e| e.quality.ok) {
        let pts: Vec<(f64, f64)> = e
            .trace
            .t
            .iter()
            .zip(&e.trace.f)
            .step_by(4)
            .map(|(&t, &f)| (t, row(f)))
            .collect();
        ovs.push(Overlay {
            points: pts,
            color: [255, 255, 255],
            dots: true,
        });
    }
    let fmax = ls.freqs[ls.freqs.len() - 1];
    let y_ticks: Vec<(f64, String)> = [25.0, 50.0, 100.0, 200.0, 400.0, 800.0, 1600.0]
        .iter()
        .filter(|&&f| f >= fmin && f <= fmax)
        .map(|&f| (row(f), format!("{f:.0}")))
        .collect();
    let axes = Axes {
        title: "survey (log frequency): detections blue, measured traces white".into(),
        y_ticks: Some(y_ticks),
    };
    render_png_axes(&spec, path, &ovs, -1.0, 6.0, 2400, 480, &axes)
}

/// Event spectrogram with its trace (white), straight-line fit (cyan) and optionally a flight's
/// predicted curve (`extra`, labelled `label`).
pub(crate) fn event_png(
    e: &MeasuredEvent,
    c: f64,
    path: &Path,
    extra: Option<ExtraCurve>,
    label: &str,
) -> Result<(), String> {
    let (Some(sp), Some(f)) = (&e.spec, &e.fit) else {
        return Err("no spectrogram/fit".into());
    };
    let b = f.best;
    let t0e = b.t0r - b.d / c;
    let curve: Vec<(f64, f64)> =
        sp.t.iter()
            .step_by(4)
            .map(|&t| (t, physics::cv_curve(t, b.f0, b.v, b.d, t0e, c)))
            .collect();
    let tr: Vec<(f64, f64)> = e
        .trace
        .t
        .iter()
        .zip(&e.trace.f)
        .map(|(a, b)| (*a, *b))
        .collect();
    let mut ov = vec![
        Overlay {
            points: curve,
            color: [0, 230, 255],
            dots: false,
        },
        Overlay {
            points: tr,
            color: [255, 255, 255],
            dots: true,
        },
    ];
    let has_extra = extra.is_some();
    if let Some((pts, col)) = extra {
        ov.push(Overlay {
            points: pts,
            color: col,
            dots: false,
        });
    }
    let title = format!(
        "{}: trace white, fit cyan (f0 {:.1} Hz, v {:.0} m/s, d {:.0} m){}",
        e.id,
        b.f0,
        b.v,
        b.d,
        if has_extra {
            format!(", {label} magenta")
        } else {
            String::new()
        }
    );
    render_png_axes(
        sp,
        path,
        &ov,
        -3.0,
        25.0,
        1600,
        640,
        &Axes {
            title,
            y_ticks: None,
        },
    )
}

/// Run the whole pipeline.
pub fn locate(a: &LocateArgs, log: &Arc<Log>) -> Result<LocateOutcome, String> {
    let t_all = Instant::now();
    let mut timings = serde_json::Map::new();
    let mut warnings: Vec<String> = Vec::new();
    let c = fdop_types::C_SOUND;
    // ---- inputs -------------------------------------------------------------------------
    if !a.media.is_file() {
        return Err(format!("{}: not a file", a.media.display()));
    }
    let info = audio::probe(&a.media)?;
    log.say(&format!(
        "media: {:.1} s, {} {} Hz x{}, creation_time {:?}",
        info.duration_s, info.audio_codec, info.sample_rate, info.channels, info.creation_time
    ));
    if let Some((la, lo)) = info.location_latlon {
        warnings.push(format!(
            "the file itself carries a location tag ({la:.5},{lo:.5}); the search below ignores it and is independent"
        ));
    }
    let ctx = RegionContext::new(a.cache.clone(), !a.offline);
    let region: Region =
        parse_region(&a.near, &ctx).map_err(|e| format!("region '{}': {e}", a.near))?;
    log.say(&format!(
        "region: {} [{:.3},{:.3},{:.3},{:.3}] {:.0} km^2, UTC offsets {:?}",
        region.label,
        region.bbox.south,
        region.bbox.west,
        region.bbox.north,
        region.bbox.east,
        region.bbox.area_km2(),
        region.utc_offset_h
    ));
    let (d0, d1) = parse_dates(&a.dates)?;
    let (mut s_lo, mut s_hi) = start_window(d0, d1, region.utc_offset_h);
    // adsb.lol publishes each UTC day a few hours after it ends, so yesterday's local date
    // usually reaches into a UTC day without an archive: search the published part, say so
    let (adsb_end, pending) = fdop_data::published_until(
        s_lo - 1200.0,
        s_hi + info.duration_s + 1200.0,
        &a.cache,
        a.offline,
        unix_now(),
    )
    .map_err(|e| format!("ADS-B availability: {e}"))?;
    let adsb_end = if pending.is_empty() {
        f64::INFINITY
    } else {
        // the last flyover needs a couple of minutes of track after the recording ends
        let hi = s_hi.min(adsb_end - info.duration_s - 300.0);
        let why = format!(
            "the ADS-B archive for UTC day(s) {} is not published yet{} (adsb.lol releases each UTC day a few hours after it ends)",
            pending.join(", "),
            if a.offline { " or not cached" } else { "" }
        );
        if hi < s_lo {
            return Err(format!(
                "{why}; no published day covers a {:.0} min recording on {}: rerun later",
                info.duration_s / 60.0,
                a.dates
            ));
        }
        warnings.push(format!(
            "{why}: only recording starts up to {} were searched; rerun after the release to cover the rest of {}",
            iso_utc(hi),
            a.dates
        ));
        s_hi = hi;
        adsb_end - 1.0
    };
    let full_window = (s_lo, s_hi);
    let mut window_note = format!(
        "recording start between {} and {} (local dates {} at UTC{:+}..{:+})",
        iso_utc(s_lo),
        iso_utc(s_hi),
        a.dates,
        region.utc_offset_h.0,
        region.utc_offset_h.1
    );
    if !pending.is_empty() {
        window_note.push_str(&format!(
            "; clipped to the published ADS-B archive ({} pending)",
            pending.join(", ")
        ));
    }
    let mut meta_window = false;
    if let Some(s) = &a.start {
        let t = parse_iso_utc(s).map_err(|e| format!("--start: {e}"))?;
        s_lo = t - a.clock_tol_s;
        s_hi = t + a.clock_tol_s;
        window_note = format!(
            "recording start {} +- {:.0} s (given)",
            iso_utc(t),
            a.clock_tol_s
        );
        if s_hi + info.duration_s > adsb_end {
            warnings.push(format!(
                "--start reaches past the published ADS-B archive (ends {}); flyovers after it cannot be matched",
                iso_utc(adsb_end)
            ));
        }
    } else if !a.ignore_metadata_time
        && let Some(ct) = info.creation_unix
        && ct >= full_window.0 - info.duration_s
        && ct <= full_window.1 + info.duration_s
    {
        // containers disagree on whether creation_time marks the start or the end
        s_lo = (ct - info.duration_s - a.clock_tol_s).max(full_window.0);
        s_hi = (ct + a.clock_tol_s).min(full_window.1);
        meta_window = true;
        window_note = format!(
            "recording start in [{}, {}] from the file's creation_time {} (start or end convention, +-{:.0} s); full-day fallback if nothing is found",
            iso_utc(s_lo),
            iso_utc(s_hi),
            iso_utc(ct),
            a.clock_tol_s
        );
    }
    log.say(&window_note);
    // weather is network-bound and independent of the audio: fetch it in the background
    let weather_rx = {
        let (tx, rx) = std::sync::mpsc::channel();
        let (bb, w0, w1, cache, offline) = (
            region.bbox.padded_km(30.0),
            full_window.0 - 3600.0,
            full_window.1 + info.duration_s + 3600.0,
            a.cache.clone(),
            a.offline,
        );
        std::thread::spawn(move || {
            let t = Instant::now();
            let r = fdop_data::fetch_atmosphere(&bb, w0, w1, &cache, offline);
            let _ = tx.send((r, t.elapsed().as_secs_f64()));
        });
        rx
    };
    let run_dir = a.out.clone().unwrap_or_else(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        PathBuf::from("fdop_runs").join(format!("{}_{}", stem(&a.media), now))
    });
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("{}: {e}", run_dir.display()))?;
    // ---- audio --------------------------------------------------------------------------
    let t = Instant::now();
    let x = audio::decode(&a.media, 0.0, None, a.sr)?;
    let dur = x.len() as f64 / a.sr as f64;
    log.say(&format!("decoded {:.1} s at {} Hz", dur, a.sr));
    timings.insert("decode_s".into(), json!(t.elapsed().as_secs_f64()));
    // ---- detection ----------------------------------------------------------------------
    let t = Instant::now();
    let dp = DetectParams::default();
    let (ls, lin) = detect::survey(&x, a.sr, &dp)?;
    let dets = detect::detect(&ls, &dp);
    let n_strong = dets
        .iter()
        .filter(|d| d.score >= dp.thr_db && d.harmonic_of.is_none())
        .count();
    log.say(&format!(
        "detection: {} candidates, {} strong primaries",
        dets.len(),
        n_strong
    ));
    timings.insert("detect_s".into(), json!(t.elapsed().as_secs_f64()));
    // ---- measurement --------------------------------------------------------------------
    let t = Instant::now();
    let mp = MeasureParams {
        c,
        ..Default::default()
    };
    let prim: Vec<&Detection> = dets
        .iter()
        .filter(|d| d.harmonic_of.is_none() && d.score >= dp.thr_db)
        .take(24)
        .collect();
    let mut events: Vec<MeasuredEvent> = prim
        .par_iter()
        .enumerate()
        .map(|(k, d)| {
            let r = detect::refine(&lin, d, c);
            measure::measure(&x, a.sr, &r, &format!("e{}", k + 1), &mp)
        })
        .collect();
    drop(x);
    events.sort_by(|p, q| q.quality.score.total_cmp(&p.quality.score));
    for (k, e) in events.iter_mut().enumerate() {
        e.id = format!("e{}", k + 1);
    }
    let n_ok = events.iter().filter(|e| e.quality.ok).count();
    log.say(&format!(
        "measured {} events, {} pass quality gates",
        events.len(),
        n_ok
    ));
    for e in &events {
        if let Some(f) = &e.fit {
            log.say(&format!(
                "  {} t_rec={:.2}s f0={:.2} v={:.1} d={:.0} rms={:.2} q={:.0} {}",
                e.id,
                f.best.t0r,
                f.best.f0,
                f.best.v,
                f.best.d,
                f.best.rmse,
                e.quality.score,
                if e.quality.ok {
                    "ok".to_string()
                } else {
                    e.quality.reasons.join("; ")
                }
            ));
        }
    }
    timings.insert("measure_s".into(), json!(t.elapsed().as_secs_f64()));
    let _ = survey_png(&ls, &lin, &dets, &events, &run_dir.join("survey.png"));
    write_json(&run_dir.join("detections.json"), &dets)?;
    write_json(&run_dir.join("events.json"), &events)?;
    for e in &events {
        if e.fit.is_some() {
            let _ = event_png(e, c, &run_dir.join(format!("{}.png", e.id)), None, "");
            let _ = e
                .trace
                .write_csv(&run_dir.join(format!("{}_trace.csv", e.id)));
        }
    }
    // ---- atmosphere (sound speed and wind profiles) ----------------------------------------
    let t = Instant::now();
    let ((atmo, wwarn), weather_fetch_s): ((Arc<dyn fdop_types::atmo::Atmosphere>, Vec<String>), f64) = match weather_rx.recv_timeout(std::time::Duration::from_secs(150)) {
        Ok(r) => r,
        Err(_) => (
            (
                Arc::new(fdop_types::atmo::StandardAtmosphere::new(15.0, 0.0, 0.5)),
                vec!["weather services did not answer in time; using a 15 C standard atmosphere (rerun to use the cached profiles once they arrive)".into()],
            ),
            f64::NAN,
        ),
    };
    let has_wind = wwarn.is_empty();
    for w in &wwarn {
        log.say(&format!("weather: {w}"));
    }
    warnings.extend(wwarn);
    let mid = 0.5 * (full_window.0 + full_window.1) + 0.5 * dur;
    let (clat, clon) = region.center;
    let m1 = atmo.medium(clat, clon, mid, 10.0, 1500.0);
    let m5 = atmo.medium(clat, clon, mid, 10.0, 5000.0);
    log.say(&format!(
        "atmosphere: {}; mid-window at region centre: c(0-1.5 km) {:.1} m/s, c(0-5 km) {:.1} m/s, wind(0-5 km) {:.1} m/s",
        atmo.describe(),
        m1.c,
        m5.c,
        m5.wind_e.hypot(m5.wind_n)
    ));
    // the fetch runs in the background from the start; this is how long the pipeline waited
    timings.insert("weather_wait_s".into(), json!(t.elapsed().as_secs_f64()));
    timings.insert("weather_fetch_s".into(), json!(weather_fetch_s));
    let atmo_text = format!(
        "{}. At the region centre mid-window: path-mean sound speed {:.1} m/s (0-1.5 km), {:.1} m/s (0-5 km); mean wind (0-5 km) {:.1} m/s from {:.0} deg.{}",
        atmo.describe(),
        m1.c,
        m5.c,
        m5.wind_e.hypot(m5.wind_n),
        (m5.wind_e.atan2(m5.wind_n).to_degrees() + 180.0).rem_euclid(360.0),
        if has_wind {
            ""
        } else {
            " No wind data: speed gates are widened by 10 m/s."
        }
    );
    // fitted speeds are mapped to ground speeds through the modelled wind; without wind
    // data the gates must absorb it
    let wind = if has_wind { 3.0 } else { 10.0 };
    let sevs: Vec<SearchEvent> = events
        .iter()
        .filter(|e| {
            e.fit
                .as_ref()
                .is_some_and(|f| f.best.v >= 28.0 && f.best.d >= 200.0)
        })
        .filter_map(|e| SearchEvent::from_measured(e, wind))
        .collect();
    log.say(&format!(
        "{} aircraft-like events go to the search",
        sevs.len()
    ));
    let heard: Vec<f64> = dets.iter().map(|d| d.t0r).collect();
    // ---- ADS-B --------------------------------------------------------------------------
    let t = Instant::now();
    let d_max = sevs
        .iter()
        .map(|e| e.d_gate[1])
        .fold(5000.0, f64::max)
        .min(30_000.0);
    // aircraft within d_gate of an observer inside the region, plus ~2 minutes of flight on
    // each side of the CPA for the curve; capped so a state-wide search stays one cache box
    let fetch_box: BBox = region.bbox.padded_km((d_max / 1000.0 + 15.0).min(30.0));
    let (geoid_n, geoid_src) = if a.offline {
        (-30.0, "offline-default")
    } else {
        fdop_data::geoid_height_or_default(region.center.0, region.center.1)
    };
    if geoid_src != "ngs" {
        warnings.push(format!(
            "geoid height N = {geoid_n:.0} m is a default ({}); across CONUS N runs from about -10 to -35 m, so ADS-B GNSS altitudes can be off by up to about 20 m",
            if a.offline {
                "--offline"
            } else {
                "the NGS GEOID lookup failed"
            }
        ));
    }
    let spec = fdop_data::IngestSpec {
        bbox: fetch_box,
        t_start: full_window.0.min(s_lo) - 1200.0,
        t_end: (full_window.1.max(s_hi) + dur + 1200.0).min(adsb_end),
        geoid_n_m: geoid_n,
        keep_unknown: true,
    };
    let lg = log.clone();
    let opts = fdop_data::FetchOptions {
        offline: a.offline,
        allow_missing: false,
        progress: Some(Arc::new(move |s: &str| lg.say(s))),
        ..Default::default()
    };
    log.say(&format!(
        "ADS-B: {} .. {} over [{:.2},{:.2},{:.2},{:.2}] (geoid N {geoid_n:.2} m, {geoid_src})",
        iso_utc(spec.t_start),
        iso_utc(spec.t_end),
        fetch_box.south,
        fetch_box.west,
        fetch_box.north,
        fetch_box.east
    ));
    let store =
        fdop_data::fetch_window(&spec, &a.cache, &opts).map_err(|e| format!("ADS-B fetch: {e}"))?;
    log.say(&format!(
        "ADS-B: {} legs, {} points",
        store.legs.len(),
        store.n_points()
    ));
    timings.insert("adsb_s".into(), json!(t.elapsed().as_secs_f64()));
    let t = Instant::now();
    let bp = track::BuildParams::default();
    let tracks: Vec<track::TrackModel> = store
        .legs
        .par_iter()
        .filter(|l| l.engine.tonal())
        .flat_map_iter(|l| track::build(l, &bp))
        .collect();
    let n_jets = store.legs.iter().filter(|l| !l.engine.tonal()).count();
    log.say(&format!(
        "track models: {} (excluded {} jet legs)",
        tracks.len(),
        n_jets
    ));
    drop(store);
    // ---- terrain ------------------------------------------------------------------------
    let coarse = fdop_data::TerrariumDem::new(&a.cache, 10)
        .with_offline(a.offline)
        .with_fallback(0.0);
    let fine = fdop_data::TerrariumDem::new(&a.cache, 13)
        .with_offline(a.offline)
        .with_fallback(0.0);
    match coarse.prefetch_bbox(&region.bbox.padded_km(d_max / 1000.0 + 2.0), 4000) {
        Ok(n) => log.say(&format!("terrain: {n} coarse tiles ready (z10)")),
        Err(e) => warnings.push(format!(
            "terrain prefetch: {e}; elevations fall back to 0 m where tiles are missing"
        )),
    }
    timings.insert("tracks_terrain_s".into(), json!(t.elapsed().as_secs_f64()));
    // ---- search -------------------------------------------------------------------------
    let t = Instant::now();
    let run_search = |lo: f64, hi: f64| -> SearchResult {
        let mut sp = SearchParams::new(lo, hi, dur, region.bbox);
        sp.c = c;
        sp.tonal_only = true;
        sp.max_events = a.max_events;
        (sp.max_hyp_per_event, sp.max_hyp_total) = search::hyp_budget(search::physical_memory());
        let inp = SearchInputs {
            events: sevs.clone(),
            tracks: &tracks,
            heard_times: heard.clone(),
            coarse_dem: &coarse,
            fine_dem: &fine,
            atmo: &*atmo,
        };
        let lg = log.clone();
        search::run(&inp, &sp, &move |s: &str| lg.say(s))
    };
    let mut res = run_search(s_lo, s_hi);
    if res.candidates.is_empty() && meta_window {
        log.say("nothing found inside the metadata window; widening to the full date window");
        warnings.push(
            "creation_time window gave no candidate; the full date window was searched".into(),
        );
        res = run_search(full_window.0, full_window.1);
        s_lo = full_window.0;
        s_hi = full_window.1;
    }
    let _ = (s_lo, s_hi);
    timings.insert("search_s".into(), json!(t.elapsed().as_secs_f64()));
    warnings.extend(res.warnings.iter().cloned());
    // a tile that cannot be read or fetched answers 0 m; say how much ground that covered
    let (lost_coarse, lost_fine) = (coarse.failures(), fine.failures());
    if lost_coarse + lost_fine > 0 {
        warnings.push(format!(
            "terrain: {lost_coarse} coarse (z10) and {lost_fine} fine (z13) tiles could not be {}; ground elevation there was taken as 0 m",
            if a.offline {
                "read from the cache (--offline)"
            } else {
                "fetched"
            }
        ));
    }
    write_json(&run_dir.join("search.json"), &res)?;
    // ---- answer -------------------------------------------------------------------------
    let t = Instant::now();
    let net = if a.offline {
        None
    } else {
        fdop_http::Net::new().ok()
    };
    let gaz = fdop_geo::gazetteer::Gazetteer::new(a.cache.clone(), !a.offline);
    let mut report = ReportInput {
        tool: "fdop".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        created_utc: iso_utc(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0),
        ),
        input_file: a.media.display().to_string(),
        dates: a.dates.clone(),
        region: region.label.clone(),
        ..Default::default()
    };
    report.events = events
        .iter()
        .map(|e| {
            let f = e.fit.as_ref();
            EventRow {
                id: e.id.clone(),
                t_rec: e.t_rec(),
                f0: f.map_or(e.detection.f0, |f| f.best.f0),
                v: f.map_or(e.detection.v, |f| f.best.v),
                d: f.map_or(e.detection.d, |f| f.best.d),
                rmse: f.map_or(f64::NAN, |f| f.best.rmse),
                quality: if e.quality.ok {
                    format!("ok (q {:.0})", e.quality.score)
                } else {
                    e.quality.reasons.join("; ")
                },
                matched: res
                    .candidates
                    .first()
                    .and_then(|c| c.events.iter().find(|m| m.event == e.id))
                    .map(|m| {
                        format!(
                            "{} {} ({}, chi {:.2}, {:.1} nats)",
                            if m.reg.is_empty() { &m.icao } else { &m.reg },
                            m.typ,
                            m.role,
                            m.chi,
                            m.nats
                        )
                    })
                    .unwrap_or_else(|| {
                        if sevs.iter().any(|s| s.id == e.id) {
                            String::new()
                        } else {
                            "not searched (not aircraft-like)".into()
                        }
                    }),
                figure: f.map(|_| format!("{}.png", e.id)),
            }
        })
        .collect();
    let found = !res.candidates.is_empty();
    if found {
        let top = &res.candidates[0];
        let (lat, lon, radius, basis, radius_lower_bound) = match &res.refine {
            Some(r) => (
                r.lat,
                r.lon,
                r.radius_m,
                r.radius_basis.clone(),
                r.radius_lower_bound,
            ),
            None => (
                top.lat,
                top.lon,
                300.0,
                "fine-grid winner (refinement unavailable); nominal 300 m".into(),
                true,
            ),
        };
        let margin_nats = res.margin.get("margin_nats").and_then(|v| v.as_f64());
        let alt_km = res.margin.get("next_distinct_km").and_then(|v| v.as_f64());
        let lock_ok = top.events.iter().all(|m| {
            let tol = sevs
                .iter()
                .find(|e| e.id == m.event)
                .map_or(4.0, search::lock_tol);
            m.dt_free.is_none_or(|d| d.abs() <= tol)
        });
        let n_core = top.events.iter().filter(|m| m.core).count();
        let gates = vec![
            (
                "core matches".to_string(),
                n_core >= 2,
                format!(
                    "{n_core} of {} matches locate the observer (chi <= 4, own clock within 4 s, trace beats chance by >= 2 nats): {}",
                    top.n_explained,
                    top.events
                        .iter()
                        .map(|m| format!(
                            "{} chi {:.2} trace {:.1} nats{}",
                            m.event,
                            m.chi,
                            m.trace_nats,
                            if m.core { " (core)" } else { "" }
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            ),
            (
                "clock lock".to_string(),
                lock_ok,
                top.events
                    .iter()
                    .map(|m| format!("{} dt_free {:+.1}s", m.event, m.dt_free.unwrap_or(f64::NAN)))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            (
                "margin".to_string(),
                margin_nats.is_none_or(|m| m >= 6.0),
                match (margin_nats, alt_km) {
                    (Some(m), Some(k)) => format!(
                        "evidence {:.1} nats vs {:.1} for the best different explanation ({}, {:.1} km away): {:.1} nats (likelihood ratio ~{:.0}:1)",
                        top.score,
                        top.score - m,
                        res.margin
                            .get("next_distinct_flights")
                            .and_then(|v| v.as_array())
                            .map(|a| a
                                .iter()
                                .filter_map(|x| x.as_str())
                                .collect::<Vec<_>>()
                                .join(" + "))
                            .unwrap_or_default(),
                        k,
                        m,
                        m.min(700.0).exp()
                    ),
                    _ => format!(
                        "evidence {:.1} nats; no distinct alternative survived",
                        top.score
                    ),
                },
            ),
            (
                "no unheard close pass".to_string(),
                top.unheard_close_passes.is_empty(),
                if top.unheard_close_passes.is_empty() {
                    "every much-closer tonal pass left a detection".into()
                } else {
                    top.unheard_close_passes.join("; ")
                },
            ),
            (
                "radius".to_string(),
                radius <= 1000.0 && !radius_lower_bound,
                if radius_lower_bound {
                    format!(
                        "{radius:.0} m is a lower bound: the 95% region reached the edge of the widest refinement window"
                    )
                } else {
                    format!("{radius:.0} m")
                },
            ),
        ];
        let passed = gates.iter().filter(|g| g.1).count();
        let confidence = if passed == gates.len() {
            "high"
        } else if n_core >= 2 && gates[1].1 && margin_nats.is_none_or(|m| m >= 3.0) {
            "medium"
        } else {
            "low"
        };
        log.say(&format!(
            "winner {lat:.6},{lon:.6} radius {radius:.0} m, {} events, confidence {confidence}",
            top.n_explained
        ));
        let place = fdop_geo::geocode::describe_location(net.as_ref(), &gaz, lat, lon);
        log.say(&format!("place: {}", place.headline));
        report.answer = Answer {
            lat,
            lon,
            radius_m: radius,
            radius_basis: basis,
            confidence: confidence.into(),
            verdict: format!(
                "{} flyovers matched to {} at one location and one recording clock (start {})",
                top.n_explained,
                top.events
                    .iter()
                    .map(|m| format!(
                        "{} {}",
                        if m.reg.is_empty() { &m.icao } else { &m.reg },
                        m.typ
                    ))
                    .collect::<Vec<_>>()
                    .join(", "),
                iso_utc(top.t_start_utc)
            ),
            gates,
        };
        report.place = Some(place.clone());
        report.candidates = res
            .candidates
            .iter()
            .take(25)
            .enumerate()
            .map(|(k, cnd)| CandidateRow {
                rank: k + 1,
                lat: cnd.lat,
                lon: cnd.lon,
                km_from_top: fdop_types::geo::haversine_m(cnd.lat, cnd.lon, top.lat, top.lon)
                    / 1000.0,
                evidence_nats: cnd.score,
                n_explained: cnd.n_explained,
                unheard: cnd.unheard_close_passes.len(),
                recording_start_utc: iso_utc(cnd.t_start_utc),
                matches: cnd
                    .events
                    .iter()
                    .map(|m| MatchRow {
                        event: m.event.clone(),
                        role: m.role.clone(),
                        flight: m.flight.clone(),
                        reg: m.reg.clone(),
                        typ: m.typ.clone(),
                        chi: m.chi,
                        nats: m.nats,
                        d_m: m.d,
                        speed_mps: m.speed,
                        alt_m: m.alt_m,
                        dt_free_s: m.dt_free,
                        cpa_heard_utc: iso_utc(m.t_cpa_heard_utc),
                        c_path: m.c_path,
                        wind_along: m.wind_along,
                        f0: m.f0,
                        f0_after: m.f0_change.map(|c| c.1),
                    })
                    .collect(),
            })
            .collect();
        if let Some(r) = &res.refine {
            report.heatmap = Some(Heatmap {
                lats: r.lats.clone(),
                lons: r.lons.clone(),
                scores: r
                    .dchi2
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|v| {
                                if v.is_finite() {
                                    -v.min(60.0)
                                } else {
                                    f64::NAN
                                }
                            })
                            .collect()
                    })
                    .collect(),
            });
        }
        // map
        let mopts = fdop_geo::staticmap::StaticMapOptions {
            cache_dir: a.cache.clone(),
            headline: Some(place.headline.clone()),
            markers: res
                .candidates
                .iter()
                .skip(1)
                .take(5)
                .map(|c| (c.lat, c.lon))
                .collect(),
            ..Default::default()
        };
        match fdop_geo::staticmap::static_map(net.as_ref(), lat, lon, radius, &run_dir, &mopts) {
            Ok(m) => {
                report
                    .figures
                    .push(("Map: answer and confidence radius".into(), "map.png".into()));
                if m.offline {
                    warnings
                        .push("map basemap tiles unavailable; map shows the circle only".into());
                }
            }
            Err(e) => warnings.push(format!("map: {e}")),
        }
        // event figures with the winning flights' predicted curves
        for m in &top.events {
            if let Some(e) = events.iter().find(|e| e.id == m.event)
                && let Some(tm) = tracks.iter().find(|t| t.key == m.flight)
                && let Some(sp) = &e.spec
            {
                let z = fine.elev_m(lat, lon);
                let xo = fdop_types::geo::geodetic_to_ecef(lat, lon, z + fdop_types::MIC_HEIGHT_M);
                let t_cpa_e = m.t_cpa_heard_utc - m.d / m.c_path;
                let (_, _, h) = fdop_types::geo::ecef_to_geodetic(tm.pos_at(t_cpa_e));
                let enu_o = fdop_types::geo::Enu::new(lat, lon, z + fdop_types::MIC_HEIGHT_M);
                let prop = track::Prop::from_medium(
                    &atmo.medium(lat, lon, t_cpa_e, z + fdop_types::MIC_HEIGHT_M, h),
                    &enu_o,
                );
                let mut pts = Vec::new();
                let mut warm = None;
                for &tr in sp.t.iter().step_by(4) {
                    let tu = top.t_start_utc + tr;
                    if let Some((te, _, ratio)) = tm.solve_emission_p(tu, xo, &prop, warm) {
                        warm = Some(te);
                        let f0 = match m.f0_change {
                            Some((t_brk, f_after)) if tr >= t_brk => f_after,
                            _ => m.f0,
                        };
                        pts.push((tr, f0 * ratio));
                    } else {
                        warm = None;
                    }
                }
                let ac = format!(
                    "{} {}",
                    if m.reg.is_empty() { &m.icao } else { &m.reg },
                    m.typ
                );
                let _ = event_png(
                    e,
                    c,
                    &run_dir.join(format!("{}_match.png", e.id)),
                    Some((pts, [255, 60, 200])),
                    &format!("{ac} ADS-B prediction"),
                );
                report.figures.push((format!("{}: trace (white), straight-line fit (cyan), ADS-B prediction for {ac} ({}, {}) at the answer (magenta)", e.id, m.flight, m.role), format!("{}_match.png", e.id)));
            }
        }
    } else {
        report.answer = Answer {
            lat: f64::NAN,
            lon: f64::NAN,
            radius_m: f64::NAN,
            radius_basis: "no candidate".into(),
            confidence: "none".into(),
            verdict: format!(
                "no location found: {} usable flyover events; {}",
                sevs.len(),
                if sevs.len() < 2 {
                    "need at least two clear propeller flyovers"
                } else {
                    "no ADS-B flight pair explains two events at one place and clock (check dates/region, or the aircraft lacked ADS-B)"
                }
            ),
            gates: vec![],
        };
        log.say(&report.answer.verdict);
    }
    report.figures.insert(
        0,
        (
            "Survey spectrogram (log frequency): detections (blue), measured traces (white)".into(),
            "survey.png".into(),
        ),
    );
    for e in &events {
        if e.fit.is_some() {
            report.figures.push((
                format!("{} spectrogram, trace and fit", e.id),
                format!("{}.png", e.id),
            ));
        }
    }
    report.funnel = serde_json::to_value(&res.funnel).unwrap_or_default();
    if let Some(h) = report.funnel.get_mut("hypotheses") {
        // None = the event's cloud was over budget: it can only confirm, not drive the join
        *h = json!(
            res.funnel
                .hypotheses
                .iter()
                .map(|(e, n)| (
                    e.clone(),
                    n.map_or_else(|| json!("confirmer only"), |v| json!(v))
                ))
                .collect::<Vec<_>>()
        );
    }
    report.calibration = res
        .nulls
        .iter()
        .map(|n| CalibRow {
            event: n.event.clone(),
            hypotheses: n.cloud,
            gate_nats: n.gate_nats,
            chi_null_p05: n.chi_null_p05,
            chi_null_median: n.chi_null_median,
            n_null: n.n_null,
        })
        .collect();
    report.atmosphere = atmo_text;
    report.margin = res.margin.clone();
    warnings.push("Use only on recordings you made or have permission to analyse; the answer is a cluster centre and radius, not an address.".into());
    report.warnings = warnings;
    timings.insert("answer_s".into(), json!(t.elapsed().as_secs_f64()));
    timings.insert("total_s".into(), json!(t_all.elapsed().as_secs_f64()));
    report.timings = serde_json::Value::Object(timings);
    report.extra = json!({
        "window": window_note,
        "media": {"duration_s": info.duration_s, "codec": info.audio_codec, "sample_rate": info.sample_rate, "creation_time": info.creation_time},
        "region_bbox": region.bbox.as_array(),
        "atmosphere": {"source": atmo.describe(), "c_0_1500m": m1.c, "c_0_5000m": m5.c, "wind_0_5000m": [m5.wind_e, m5.wind_n]},
        "search_events": sevs,
        "log": log.lines(),
    });
    write_json(&run_dir.join("result.json"), &report)?;
    std::fs::write(
        run_dir.join("report.md"),
        fdop_geo::report::render_markdown(&report),
    )
    .map_err(|e| e.to_string())?;
    if let Err(e) = fdop_geo::report::write_candidates_csv(&report, &run_dir.join("candidates.csv"))
    {
        log.say(&format!("candidates.csv: {e}"));
    }
    match fdop_geo::report::render_html(&report, &run_dir) {
        Ok(p) => log.say(&format!("report: {}", p.display())),
        Err(e) => log.say(&format!("report.html: {e}")),
    }
    Ok(LocateOutcome {
        run_dir,
        report,
        found,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_windows() {
        let (d0, d1) = parse_dates("2026-09-08").unwrap();
        assert_eq!(d0, d1);
        let (a, b) = start_window(d0, d1, (-5.0, -4.0));
        assert_eq!(iso_utc(a), "2026-09-08T04:00:00Z");
        assert_eq!(iso_utc(b), "2026-09-09T05:00:00Z");
        let (d0, d1) = parse_dates("2026-09-08..2026-09-10").unwrap();
        assert_eq!((d0.2, d1.2), (8, 10));
        assert!(parse_dates("2026-09-10..2026-09-08").is_err());
    }
}
