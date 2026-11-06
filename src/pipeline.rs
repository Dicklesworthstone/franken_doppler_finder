//! `fdop locate`: media file + dates + coarse US region -> location, radius, map, place.

use fdop_core::curvematch;
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
    pub audio_selection: audio::AudioSelection,
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
    /// Also score logged flights directly on the spectrogram (see `fdop_core::curvematch`).
    pub curve_match: bool,
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

/// `fdop_runs/<media stem>_<unix seconds>`, the run directory when `-o` is not given.
pub fn default_run_dir(media: &Path) -> PathBuf {
    PathBuf::from("fdop_runs").join(format!("{}_{}", stem(media), unix_now() as u64))
}

/// UTC window for the recording START: local midnight of the first day .. local midnight after
/// the last day, using the region's UTC offset range (DST included).
pub fn start_window(d0: Ymd, d1: Ymd, off: (f64, f64)) -> (f64, f64) {
    let a = unix_from_utc(d0.0, d0.1, d0.2, 0, 0, 0.0) - off.1 * 3600.0;
    let b = unix_from_utc(d1.0, d1.1, d1.2, 0, 0, 0.0) + 86_400.0 - off.0 * 3600.0;
    (a, b)
}

/// An explicit clock constrains every data request, not just the final search. Keep it
/// inside the date window so an inconsistent clock cannot silently search another day.
fn requested_start_window(
    dates: (f64, f64),
    start: Option<f64>,
    tolerance: f64,
) -> Result<(f64, f64), String> {
    if !tolerance.is_finite() || tolerance < 0.0 {
        return Err("--clock-tol must be finite and nonnegative".into());
    }
    let Some(t) = start else {
        return Ok(dates);
    };
    if !t.is_finite() {
        return Err("--start must be a finite timestamp".into());
    }
    let window = ((t - tolerance).max(dates.0), (t + tolerance).min(dates.1));
    if window.0 > window.1 {
        return Err("--start and --clock-tol do not overlap --dates in the selected region".into());
    }
    Ok(window)
}

/// Container clocks may mark either end of the recording. An unusable hint must not
/// produce an inverted search window; retain the full-date fallback instead.
fn metadata_start_window(
    full: (f64, f64),
    creation: f64,
    duration: f64,
    tolerance: f64,
) -> Option<(f64, f64)> {
    if !creation.is_finite() || creation < full.0 - duration || creation > full.1 + duration {
        return None;
    }
    let window = (
        (creation - duration - tolerance).max(full.0),
        (creation + tolerance).min(full.1),
    );
    (window.0 <= window.1).then_some(window)
}

/// serde_json writes non-finite floats as null, so the output is always valid JSON.
fn write_json(path: &Path, v: &impl serde::Serialize) -> Result<(), String> {
    let s = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    std::fs::write(path, s).map_err(|e| format!("{}: {e}", path.display()))
}

/// Below this low-band ratio the input has lost its low band, as a high-pass filter does.
/// Nine outdoor recordings measured +10.0 to +19.5 dB unfiltered; a cam high-passed at about
/// 125 Hz (-3 dB, 24 dB per octave) measured -26.1 dB. Speech-heavy audio, which sits at
/// 150-600 Hz, can read low too.
const LOW_BAND_WARN_DB: f64 = -15.0;

/// Mean power at 45-95 Hz over mean power at 150-600 Hz, in dB, from 1 s Hann frames.
/// A high-pass filter empties this band first. Slower propellers' fundamentals sit in it
/// (two blades at 2400 RPM: 80 Hz); turboprops' (about 95-135 Hz) sit near a typical cut.
/// `None` when the audio is shorter than one frame or the bands fall above Nyquist.
fn low_band_ratio_db(x: &[f32], sr: u32) -> Option<f64> {
    let s = fdop_core::spectro::stft_band_db(x, sr, 1.0, 1.0, 40.0, 650.0, 1.0, 0.0).ok()?;
    let (mut lo, mut n_lo, mut hi, mut n_hi) = (0.0f64, 0usize, 0.0f64, 0usize);
    for (fi, &f) in s.f.iter().enumerate() {
        let (in_lo, in_hi) = ((45.0..=95.0).contains(&f), (150.0..=600.0).contains(&f));
        if !(in_lo || in_hi) {
            continue;
        }
        let p: f64 = s.row(fi).iter().map(|&d| 10f64.powf(d as f64 / 10.0)).sum();
        if in_lo {
            lo += p;
            n_lo += s.nt();
        } else {
            hi += p;
            n_hi += s.nt();
        }
    }
    (n_lo > 0 && n_hi > 0 && hi > 0.0)
        .then(|| 10.0 * ((lo / n_lo as f64) / (hi / n_hi as f64)).log10())
}

/// The warning for audio that has lost its low band: `locate` reports it, `detect` prints it.
pub fn low_band_warning(x: &[f32], sr: u32) -> Option<String> {
    let r = low_band_ratio_db(x, sr).filter(|&r| r < LOW_BAND_WARN_DB)?;
    Some(format!(
        "the audio has little energy below 100 Hz (45-95 Hz is {:.0} dB below 150-600 Hz). A high-pass filter does this; it removes the fundamentals of slower propellers and weakens those near the cut, so expect fewer usable flyovers",
        -r
    ))
}

/// The answer's one-line verdict, shown by the CLI and both reports and stored in `result.json`.
/// A low answer still prints a location and a radius and exits 0, so it says first that it is
/// unconfirmed and which gates failed. The radius comes from the fit around this one candidate,
/// so it cannot say whether the candidate is the right place.
fn verdict_text(confidence: &str, gates: &[(String, bool, String)], matched: &str) -> String {
    let failed = gates
        .iter()
        .filter(|g| !g.1)
        .map(|g| g.0.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    match confidence {
        "low" => format!(
            "unconfirmed, failed: {failed}. This place can be close or far off, and the radius holds only if it is right. {matched}"
        ),
        "medium" => format!("failed: {failed}. {matched}"),
        _ => matched.to_string(),
    }
}

/// Longest recording-clock window the curve match searches. Decoy clocks are windows of the same
/// width, and they have to fit inside the days being fetched.
const CURVE_MAX_CLOCK_S: f64 = 1800.0;
/// Largest grid the curve match searches. Its time and memory go with cells times windows.
const CURVE_MAX_CELLS: f64 = 200_000.0;
/// A trace answer asserted this far from a curve-match answer that stands names another place.
const CURVE_CONFLICT_M: f64 = 3000.0;

/// Check eligibility before building the whole-file harmonic image. A skipped curve match
/// must not spend time and memory preparing a second spectrogram it will never read.
fn curve_match_skip_reason(
    p: &curvematch::MatchParams,
    bbox: BBox,
    clock: (f64, f64),
) -> Option<String> {
    let width = clock.1 - clock.0;
    if width > CURVE_MAX_CLOCK_S {
        return Some(format!(
            "the recording clock is known to {width:.0} s, and it needs {CURVE_MAX_CLOCK_S:.0} s or better"
        ));
    }
    let km_ns = (bbox.north - bbox.south) * 111.2;
    let km_ew =
        (bbox.east - bbox.west) * 111.2 * (0.5 * (bbox.north + bbox.south)).to_radians().cos();
    let cells = km_ns * km_ew * 1e6 / (p.cell_m * p.cell_m);
    (cells > CURVE_MAX_CELLS).then(|| {
        format!("the region needs {cells:.0} cells, and it searches {CURVE_MAX_CELLS:.0} at most")
    })
}

/// A failed trace is not a failed metadata window: the independent curve match can still
/// locate passes whose ridges could not be measured. Only widen once both have had a chance.
fn metadata_needs_fallback(
    meta_window: bool,
    trace_has_candidate: bool,
    curve: Option<&curvematch::CurveVerdict>,
) -> bool {
    meta_window && !trace_has_candidate && !curve.is_some_and(|v| v.stands)
}

/// Decoys calibrate one searched clock window. A verdict retained from an earlier metadata
/// attempt remains diagnostic after widening, until the matcher has searched that window too.
fn curve_judges_window(curve_clock: (f64, f64), search_clock: (f64, f64), n_decoys: usize) -> bool {
    curve_clock == search_clock && n_decoys >= curvematch::MIN_DECOYS
}

/// What the curve match does to the trace search's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CurveCall {
    /// No verdict, or one that does not stand, on an answer `Unsupported` does not cover: the trace
    /// search's answer is untouched.
    Silent,
    /// Trace answer asserted, curve match stands at the same place.
    Agrees,
    /// Trace answer asserted, curve match stands somewhere else: nobody gets to assert.
    Conflicts,
    /// Trace answer low, curve match stands at the same place: asserted at medium, trace position.
    Confirms,
    /// Trace answer low or absent, curve match stands elsewhere: asserted at medium, its position.
    Adopts,
    /// Trace answer medium, and the curve match ran with enough decoy clocks to judge but stands
    /// nowhere: lowered to low. The medium grade's margin has no calibrated false-alarm rate, and
    /// it has named wrong-date places (`nd_o31_1101` under 069c971, 9087ec4 and 6914ab7). The curve
    /// match's decoy test has one: with a wrong date or place the real window is one of at least
    /// `MIN_DECOYS` + 1 interchangeable clocks, so it stands by chance at most about once in 41,
    /// whatever the trace margin did. A high answer passed every gate and keeps its grade; where
    /// the curve match did not run or had too few decoys, nothing changes.
    Unsupported,
}

fn curve_call(
    trace_confidence: &str,
    trace_at: Option<(f64, f64)>,
    verdict: Option<&curvematch::CurveVerdict>,
    judged: bool,
) -> CurveCall {
    let Some(v) = verdict.filter(|v| v.stands) else {
        return if judged && trace_confidence == "medium" {
            CurveCall::Unsupported
        } else {
            CurveCall::Silent
        };
    };
    let apart = trace_at.map(|(la, lo)| fdop_types::geo::haversine_m(la, lo, v.lat, v.lon));
    if matches!(trace_confidence, "high" | "medium") {
        if apart.is_some_and(|d| d <= CURVE_CONFLICT_M) {
            CurveCall::Agrees
        } else {
            CurveCall::Conflicts
        }
    } else if apart.is_some_and(|d| d <= v.radius_m) {
        CurveCall::Confirms
    } else {
        CurveCall::Adopts
    }
}

/// Whether the measurement at rank `k`, of detection `d`, is one the old measurement line would not
/// have made: past the strongest `max_measured`, a primary whose detection scored under `thr_db`, or
/// one found only on the contrast image (`Detection::from_contrast`), which no null run has tested.
fn past_the_old_line(k: usize, d: &Detection, max_measured: usize, thr_db: f64) -> bool {
    k >= max_measured || d.score < thr_db || d.from_contrast
}

/// A trace answer graded medium whose core matches include an event the old measurement line would
/// not have measured, with no curve match able to judge it. Measuring further down the list gives
/// chance alignments more events to use, and the medium margin does not count how many were
/// searched; the curve match's decoy test does, so where it cannot judge, such an answer stays low.
fn leans_on_added(
    trace_confidence: &str,
    call: CurveCall,
    judged: bool,
    core_events: &[&str],
    added: &std::collections::HashSet<String>,
) -> bool {
    trace_confidence == "medium"
        && call == CurveCall::Silent
        && !judged
        && core_events.iter().any(|id| added.contains(*id))
}

/// Radius of a trace answer the curve match confirms. The curve match's radius is the spread of its
/// tied places around its own best one, so from the trace answer the circle must reach that far
/// beyond the distance between the two.
fn confirmed_radius(trace_radius_m: f64, at: (f64, f64), v: &curvematch::CurveVerdict) -> f64 {
    trace_radius_m.max(v.radius_m + fdop_types::geo::haversine_m(at.0, at.1, v.lat, v.lon))
}

/// The flights the winning curve-match mode heard, for the verdict line.
fn curve_flights(r: &curvematch::MatchResult) -> String {
    r.modes.first().map_or_else(String::new, |m| {
        m.passes
            .iter()
            .filter(|p| p.value >= 1.0)
            .map(|p| {
                format!(
                    "{} {}",
                    if p.reg.is_empty() { &p.icao } else { &p.reg },
                    p.typ
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    })
}

/// Fetch the flights of the decoy clocks too and run the curve match. Its own ADS-B fetch keeps the
/// trace search's tracks exactly as they were. Returns None when this recording's clock or region
/// is too wide for it, which says nothing about the recording.
#[allow(clippy::too_many_arguments)]
fn run_curve_match(
    a: &LocateArgs,
    p: &curvematch::MatchParams,
    image: &curvematch::CombImage,
    bbox: BBox,
    fetch_box: BBox,
    clock: (f64, f64),
    dur: f64,
    adsb_end: f64,
    geoid_n_m: f64,
    coarse: &dyn fdop_types::Elevation,
    fine: &dyn fdop_types::Elevation,
    atmo: &dyn fdop_types::atmo::Atmosphere,
    log: &Arc<Log>,
) -> Result<Option<curvematch::MatchResult>, String> {
    let (s_lo, s_hi) = clock;
    if let Some(why) = curve_match_skip_reason(p, bbox, clock) {
        log.say(&format!("curve match skipped: {why}"));
        return Ok(None);
    }
    let day = 86_400.0;
    let limits = (
        ((s_lo - 1200.0) / day).floor() * day,
        (((s_hi + dur + 1200.0) / day).ceil() * day).min(adsb_end),
    );
    let (t_start, t_end) = curvematch::catalog_span(p, s_lo, s_hi, dur, limits);
    let spec = fdop_data::IngestSpec {
        bbox: fetch_box,
        t_start,
        t_end,
        geoid_n_m,
        keep_unknown: true,
    };
    let lg = log.clone();
    let opts = fdop_data::FetchOptions {
        offline: a.offline,
        allow_missing: false,
        progress: Some(Arc::new(move |s: &str| lg.say(s))),
        ..Default::default()
    };
    let store =
        fdop_data::fetch_window(&spec, &a.cache, &opts).map_err(|e| format!("ADS-B fetch: {e}"))?;
    let bp = track::BuildParams::default();
    let tracks: Vec<track::TrackModel> = store
        .legs
        .par_iter()
        .filter(|l| l.engine.tonal())
        .flat_map_iter(|l| track::build(l, &bp))
        .collect();
    drop(store);
    let inp = curvematch::MatchInputs {
        image,
        tracks: &tracks,
        region: bbox,
        start_lo: s_lo,
        start_hi: s_hi,
        duration_s: dur,
        catalog: (t_start, t_end),
        dem: coarse,
        fine_dem: fine,
        atmo,
    };
    let lg = log.clone();
    Ok(Some(curvematch::run(&inp, p, &move |s: &str| lg.say(s))))
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
    let info = audio::probe_with_selection(&a.media, a.audio_selection)?;
    log.say(&format!(
        "media: {:.1} s, {} {} Hz x{}, audio stream {}, channel {}, creation_time {:?}",
        info.duration_s,
        info.audio_codec,
        info.sample_rate,
        info.channels,
        info.audio_stream,
        info.audio_channel
            .map_or_else(|| "mix".into(), |c| c.to_string()),
        info.creation_time
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
    let given_start = a
        .start
        .as_deref()
        .map(parse_iso_utc)
        .transpose()
        .map_err(|e| format!("--start: {e}"))?;
    let (mut s_lo, mut s_hi) = requested_start_window(
        start_window(d0, d1, region.utc_offset_h),
        given_start,
        a.clock_tol_s,
    )?;
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
                "{why}; no published day covers a {:.0} min recording in the requested start window on {}: rerun later",
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
    let mut window_note = if let Some(t) = given_start {
        format!(
            "recording start between {} and {} (--start {} +- {:.0} s, within local dates {})",
            iso_utc(s_lo),
            iso_utc(s_hi),
            iso_utc(t),
            a.clock_tol_s,
            a.dates
        )
    } else {
        format!(
            "recording start between {} and {} (local dates {} at UTC{:+}..{:+})",
            iso_utc(s_lo),
            iso_utc(s_hi),
            a.dates,
            region.utc_offset_h.0,
            region.utc_offset_h.1
        )
    };
    if !pending.is_empty() {
        window_note.push_str(&format!(
            "; clipped to the published ADS-B archive ({} pending)",
            pending.join(", ")
        ));
    }
    let mut meta_window = false;
    if given_start.is_none()
        && !a.ignore_metadata_time
        && let Some(ct) = info.creation_unix
        && let Some((lo, hi)) =
            metadata_start_window(full_window, ct, info.duration_s, a.clock_tol_s)
    {
        s_lo = lo;
        s_hi = hi;
        meta_window = true;
        window_note = format!(
            "recording start in [{}, {}] from the file's creation_time {} (start or end convention, +-{:.0} s); full-day fallback if neither search finds an answer",
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
    let run_dir = a.out.clone().unwrap_or_else(|| default_run_dir(&a.media));
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("{}: {e}", run_dir.display()))?;
    // ---- audio --------------------------------------------------------------------------
    let t = Instant::now();
    let x = audio::decode_with_selection(&a.media, 0.0, None, a.sr, a.audio_selection)?;
    let dur = x.len() as f64 / a.sr as f64;
    log.say(&format!("decoded {:.1} s at {} Hz", dur, a.sr));
    warnings.extend(low_band_warning(&x, a.sr));
    timings.insert("decode_s".into(), json!(t.elapsed().as_secs_f64()));
    let curve_params = curvematch::MatchParams::default();
    let mut curve_clock = (s_lo, s_hi);
    let curve_skip = curve_match_skip_reason(&curve_params, region.bbox, curve_clock);
    if a.curve_match
        && let Some(why) = &curve_skip
    {
        log.say(&format!("curve match skipped: {why}"));
    }
    let t = Instant::now();
    let curve_image = if a.curve_match && curve_skip.is_none() {
        match curvematch::build_image(&x, a.sr, &curve_params) {
            Ok(im) => Some(im),
            Err(e) => {
                warnings.push(format!("curve match skipped: {e}"));
                None
            }
        }
    } else {
        None
    };
    let curve_image_s = t.elapsed().as_secs_f64();
    // ---- detection ----------------------------------------------------------------------
    let t = Instant::now();
    let dp = DetectParams::default();
    let (ls, lin) = detect::survey(&x, a.sr, &dp)?;
    let (dets, det_cut) = detect::detect_counted(&ls, &dp);
    let n_strong = dets
        .iter()
        .filter(|d| d.score >= dp.thr_db && d.harmonic_of.is_none())
        .count();
    log.say(&format!(
        "detection: {} candidates, {} strong primaries; survey image spread {:.3}",
        dets.len(),
        n_strong,
        ls.scale
    ));
    if ls.gain > 1.0 {
        warnings.push(format!(
            "the survey image's spread is {:.2}, under the {:.2} the detection lines were set at (a recording loud mostly while a pass goes over): its image was raised x{:.2} before detection",
            ls.scale, dp.min_image_scale, ls.gain
        ));
    }
    if det_cut > 0 {
        warnings.push(format!(
            "detection kept the {} strongest of {}{} candidate curves; the weaker ones cannot explain an event or mark a pass as heard",
            dp.max_detections,
            dp.max_detections + det_cut,
            if det_cut >= 2 * dp.max_detections { "+" } else { "" },
        ));
    }
    timings.insert("detect_s".into(), json!(t.elapsed().as_secs_f64()));
    // ---- measurement --------------------------------------------------------------------
    let t = Instant::now();
    let mp = MeasureParams {
        c,
        ..Default::default()
    };
    // Every primary detection from the silence line up is a candidate, strongest first: the
    // detection score does not separate flyovers from tones (pure noise peaks near 3), the
    // measurement gates do. Duplicate or non-aircraft fits do not use up the allowance:
    // measuring goes down the list until MAX_MEASURED independent aircraft events survive.
    let max_measured = measure::MAX_MEASURED;
    let prim: Vec<&Detection> = dets
        .iter()
        .filter(|d| d.harmonic_of.is_none() && d.score >= dp.thr_silence_db)
        .collect();
    let mut events: Vec<MeasuredEvent> = measure::measure_ranked_counted(
        prim.len(),
        max_measured,
        |ranks| {
            prim[ranks.clone()]
                .par_iter()
                .enumerate()
                .map(|(i, d)| {
                    let r = detect::refine(&lin, d, c);
                    measure::measure(&x, a.sr, &r, &format!("e{}", ranks.start + i + 1), &mp)
                })
                .collect()
        },
        |events| search::independent_measured_events(events, 0.0).len(),
    );
    if prim.len() > events.len() {
        warnings.push(format!(
            "measured the {} strongest of {} flyover detections, reaching the limit of {max_measured} independent aircraft events; the rest neither drive nor confirm the answer, though they still count as heard",
            events.len(),
            prim.len(),
        ));
    }
    // Events the old measurement line would not have measured (`past_the_old_line`). Without a
    // curve match to judge it, an answer that rests on one of them cannot be medium
    // (`leans_on_added`).
    let added_ranks: std::collections::HashSet<String> = (0..events.len())
        .filter(|&k| past_the_old_line(k, prim[k], max_measured, dp.thr_db))
        .map(|k| format!("e{}", k + 1))
        .collect();
    drop(x);
    events.sort_by(|p, q| q.quality.score.total_cmp(&p.quality.score));
    let mut added = std::collections::HashSet::new();
    for (k, e) in events.iter_mut().enumerate() {
        let id = format!("e{}", k + 1);
        if added_ranks.contains(&e.id) {
            added.insert(id.clone());
        }
        e.id = id;
    }
    log.say(&format!(
        "{} of them measured past the old line (detection score under {:.1}, past the strongest {max_measured}, or found only on the contrast image)",
        added.len(),
        dp.thr_db
    ));
    let n_ok = events.iter().filter(|e| e.quality.ok).count();
    let n_independent = search::independent_measured_events(&events, 0.0).len();
    log.say(&format!(
        "measured {} events, {} pass quality gates, {} independent aircraft events",
        events.len(),
        n_ok,
        n_independent
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
    let (wx, weather_fetch_s) =
        match weather_rx.recv_timeout(fdop_data::weather::FETCH_WAIT_TIMEOUT) {
            Ok(r) => r,
            Err(_) => (
                fdop_data::Weather {
                    atmo: Arc::new(fdop_types::atmo::StandardAtmosphere::new(15.0, 0.0, 0.5)),
                    warnings: vec!["weather services did not answer in time; using a 15 C standard atmosphere (rerun to use the cached profiles once they arrive)".into()],
                    winds_aloft: false,
                },
                f64::NAN,
            ),
        };
    let fdop_data::Weather {
        atmo,
        warnings: wwarn,
        winds_aloft: has_wind,
    } = wx;
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
    let mut sevs: Vec<SearchEvent> = search::independent_measured_events(&events, wind);
    // A newly measured, better representative can change the greedy harmonic grouping of
    // earlier events. Enforce the same independent-event limit even if that last batch
    // releases more than its number of new slots; keep every measurement in the report.
    let excluded_by_event_limit: std::collections::HashSet<String> = sevs
        .iter()
        .skip(max_measured)
        .map(|e| e.id.clone())
        .collect();
    if sevs.len() > max_measured {
        warnings.push(format!(
            "measurement produced {} independent aircraft events after regrouping; the {max_measured} strongest were searched and {} omitted by the event limit",
            sevs.len(),
            sevs.len() - max_measured,
        ));
        sevs.truncate(max_measured);
    }
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
    let (geoid_n, geoid_src) =
        fdop_data::geoid_height_with_options(&a.cache, region.center.0, region.center.1, a.offline);
    if geoid_src != "ngs" {
        warnings.push(format!(
            "geoid height N = {geoid_n:.0} m is a default ({}); across CONUS N runs from about -10 to -35 m, so ADS-B GNSS altitudes can be off by up to about 20 m",
            if a.offline {
                "--offline and no valid cached NGS height"
            } else {
                "the NGS GEOID lookup failed"
            }
        ));
    }
    let spec = fdop_data::IngestSpec {
        bbox: fetch_box,
        t_start: full_window.0 - 1200.0,
        t_end: (full_window.1 + dur + 1200.0).min(adsb_end),
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
    // a day built from a lesser feed can lack aircraft: those can neither match an event nor
    // count as unheard close passes
    if let Some(notes) = store.meta.stats["notes"].as_array() {
        warnings.extend(
            notes
                .iter()
                .filter_map(|n| n.as_str())
                .map(|n| format!("ADS-B {n}")),
        );
    }
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
    let mut search_s = t.elapsed().as_secs_f64();
    // ---- curve match --------------------------------------------------------------------
    let t = Instant::now();
    let run_curve = |clock| {
        let Some(im) = curve_image.as_ref() else {
            return Ok(None);
        };
        run_curve_match(
            a,
            &curve_params,
            im,
            region.bbox,
            fetch_box,
            clock,
            dur,
            adsb_end,
            geoid_n,
            &coarse,
            &fine,
            &*atmo,
            log,
        )
    };
    let mut curve = match run_curve(curve_clock) {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("curve match skipped: {e}"));
            None
        }
    };
    let mut curve_verdict = curve.as_ref().and_then(curvematch::verdict);
    let mut curve_match_s = curve_image_s + t.elapsed().as_secs_f64();
    let metadata_fallback = metadata_needs_fallback(
        meta_window,
        !res.candidates.is_empty(),
        curve_verdict.as_ref(),
    );
    if metadata_fallback {
        log.say("neither search located the recording inside the metadata window; widening to the full date window");
        warnings.push(
            "creation_time window gave no trace candidate or standing curve match; the full date window was searched. A curve match in the metadata window does not judge the wider search".into(),
        );
        let t = Instant::now();
        res = run_search(full_window.0, full_window.1);
        search_s += t.elapsed().as_secs_f64();
        s_lo = full_window.0;
        s_hi = full_window.1;
        window_note = format!(
            "recording start between {} and {} (full date window after neither search found an answer inside the creation_time window)",
            iso_utc(s_lo),
            iso_utc(s_hi)
        );
        // Usually the full date is too wide. Publication clipping can leave a shorter
        // window, in which case the curve match must try it too, with its own decoys.
        if curve_image.is_some()
            && curve_match_skip_reason(&curve_params, region.bbox, full_window).is_none()
        {
            let t = Instant::now();
            curve_clock = full_window;
            curve = match run_curve(curve_clock) {
                Ok(r) => r,
                Err(e) => {
                    warnings.push(format!("curve match in the full date window skipped: {e}"));
                    None
                }
            };
            curve_verdict = curve.as_ref().and_then(curvematch::verdict);
            curve_match_s += t.elapsed().as_secs_f64();
        }
    }
    drop(curve_image);
    timings.insert("search_s".into(), json!(search_s));
    timings.insert("curve_match_s".into(), json!(curve_match_s));
    warnings.extend(res.warnings.iter().cloned());
    // the curve match ran and had decoy clocks enough to judge, whether or not it found a place
    // Its narrow clock test says nothing about a trace answer found only after widening.
    let curve_judged = curve
        .as_ref()
        .is_some_and(|r| curve_judges_window(curve_clock, (s_lo, s_hi), r.decoys.len()));
    if let Some(r) = &curve
        && let Err(e) = write_json(&run_dir.join("curvematch.json"), r)
    {
        // the curve match is optional: losing its record must not lose the answer
        warnings.push(format!("curvematch.json not written: {e}"));
    }
    if let Some(v) = &curve_verdict {
        log.say(&format!(
            "curve match {}: {}",
            if v.stands { "stands" } else { "does not stand" },
            v.why
        ));
    }
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
    write_json(&run_dir.join("search.json"), &res.dump())?;
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
                        } else if excluded_by_event_limit.contains(&e.id) {
                            format!("not searched ({max_measured}-event limit)")
                        } else {
                            "not searched (not an independent aircraft event)".into()
                        }
                    }),
                figure: f.map(|_| format!("{}.png", e.id)),
            }
        })
        .collect();
    let mut found = !res.candidates.is_empty();
    let mut curve_used = CurveCall::Silent;
    let draw_map = |report: &mut ReportInput,
                    warnings: &mut Vec<String>,
                    headline: String,
                    (lat, lon, radius): (f64, f64, f64),
                    markers: Vec<(f64, f64)>| {
        let mopts = fdop_geo::staticmap::StaticMapOptions {
            cache_dir: a.cache.clone(),
            headline: Some(headline),
            markers,
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
    };
    if found {
        let top = &res.candidates[0];
        let (lat, lon, radius, basis, radius_unresolved) = match &res.refine {
            Some(r) => (
                r.lat,
                r.lon,
                r.radius_m,
                r.radius_basis.clone(),
                r.radius_lower_bound || r.region_cells == 0,
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
                res.margin_passes(6.0),
                match margin_nats {
                    Some(m) => {
                        let flights = res
                            .margin
                            .get("next_distinct_flights")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str())
                                    .collect::<Vec<_>>()
                                    .join(" + ")
                            })
                            .unwrap_or_default();
                        let rival = match alt_km {
                            Some(k) => format!(" ({flights}, {k:.1} km away)"),
                            None if !flights.is_empty() => format!(" ({flights})"),
                            None => String::new(),
                        };
                        let qualifier = if res.funnel.truncated.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "; provisional, comparison truncated ({})",
                                res.funnel.truncated.join("; ")
                            )
                        };
                        let compared = |key: &str, otherwise: f64| {
                            res.margin
                                .get(key)
                                .and_then(|v| v.as_f64())
                                .unwrap_or(otherwise)
                        };
                        let basis = match res.margin.get("margin_basis").and_then(|v| v.as_str()) {
                            Some("fine_grid") => {
                                "; both at their fine-grid cells, because one could not be refined"
                            }
                            _ => "",
                        };
                        format!(
                            "evidence {:.2} nats vs {:.2} for the best evaluated different explanation{rival}: {m:.2} nats{basis}{qualifier}",
                            compared("margin_top_score", top.score),
                            compared("margin_rival_score", top.score - m),
                        )
                    }
                    None if !res.margin_passes(6.0) => format!(
                        "evidence {:.1} nats; no verified margin: {}",
                        top.score,
                        if res.funnel.truncated.is_empty() {
                            "margin data unavailable".to_string()
                        } else {
                            format!("comparison truncated ({})", res.funnel.truncated.join("; "))
                        }
                    ),
                    None => format!(
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
                radius <= 1000.0 && !radius_unresolved,
                if radius_unresolved {
                    format!("{radius:.0} m radius is unresolved: {basis}")
                } else {
                    format!("{radius:.0} m")
                },
            ),
        ];
        let passed = gates.iter().filter(|g| g.1).count();
        let confidence = if passed == gates.len() {
            "high"
        } else if n_core >= 2 && gates[1].1 && res.margin_passes(3.0) {
            "medium"
        } else {
            "low"
        };
        // The curve match is a second, independent reading of the recording: it can confirm a
        // low trace answer, replace it, or stop an asserted one that names another place.
        let trace_confidence = confidence;
        let call = curve_call(
            confidence,
            Some((lat, lon)),
            curve_verdict.as_ref(),
            curve_judged,
        );
        curve_used = call;
        let on_trace = call != CurveCall::Adopts;
        let standing = curve_verdict.as_ref().filter(|v| v.stands);
        let mut gates = gates;
        if let Some(v) = standing {
            let apart = fdop_types::geo::haversine_m(lat, lon, v.lat, v.lon);
            gates.push((
                "curve match".to_string(),
                call != CurveCall::Conflicts,
                if call == CurveCall::Conflicts {
                    format!(
                        "another place stands {:.1} km away: {}",
                        apart / 1000.0,
                        v.why
                    )
                } else {
                    v.why.clone()
                },
            ));
        }
        if call == CurveCall::Unsupported {
            gates.push((
                "curve match".to_string(),
                false,
                format!(
                    "a medium answer needs the curve match to stand where it ran, and it does not: {}",
                    curve_verdict
                        .as_ref()
                        .map_or_else(|| "it found no place".to_string(), |v| v.why.clone())
                ),
            ));
        }
        let core_ids: Vec<&str> = top
            .events
            .iter()
            .filter(|m| m.core)
            .map(|m| m.event.as_str())
            .collect();
        let unchecked = leans_on_added(trace_confidence, call, curve_judged, &core_ids, &added);
        if unchecked {
            let on: Vec<&str> = core_ids
                .iter()
                .copied()
                .filter(|id| added.contains(*id))
                .collect();
            gates.push((
                "measurement line".to_string(),
                false,
                format!(
                    "a medium answer rests on {}, measured past the old line or found only on the contrast image, and no curve match ran with enough decoy clocks to check it",
                    on.join(", ")
                ),
            ));
        }
        let (lat, lon, radius, basis, confidence) = match (call, standing) {
            (CurveCall::Adopts, Some(v)) => (
                v.lat,
                v.lon,
                v.radius_m,
                format!(
                    "curve match: places within {:.0} nats of the best span {:.0} m (1 km floor, its modes are 1 km apart)",
                    curvematch::TIED_NATS,
                    v.radius_m
                ),
                "medium",
            ),
            (CurveCall::Confirms, Some(v)) => {
                // The curve radius is measured from the curve's own best place. Kept at the trace
                // answer, the circle must still reach every place the curve could not rule out.
                let apart = fdop_types::geo::haversine_m(lat, lon, v.lat, v.lon);
                let r = confirmed_radius(radius, (lat, lon), v);
                (
                    lat,
                    lon,
                    r,
                    format!(
                        "trace answer confirmed by the curve match {apart:.0} m away; radius {r:.0} m covers every place within {:.0} nats of the curve match's best ({:.0} m from it)",
                        curvematch::TIED_NATS,
                        v.radius_m
                    ),
                    "medium",
                )
            }
            (CurveCall::Conflicts, Some(_)) | (CurveCall::Unsupported, _) => {
                (lat, lon, radius, basis, "low")
            }
            (CurveCall::Silent, _) if unchecked => (lat, lon, radius, basis, "low"),
            _ => (lat, lon, radius, basis, confidence),
        };
        log.say(&format!(
            "winner {lat:.6},{lon:.6} radius {radius:.0} m, {} events, confidence {confidence}",
            top.n_explained
        ));
        let place = fdop_geo::geocode::describe_location(net.as_ref(), &gaz, lat, lon);
        log.say(&format!("place: {}", place.headline));
        if let Some(w) =
            region.outside_states_warning(place.state_abbr.as_deref(), place.source == "census-api")
        {
            warnings.push(w);
        }
        report.answer = Answer {
            lat,
            lon,
            radius_m: radius,
            radius_basis: basis,
            confidence: confidence.into(),
            verdict: if let (false, Some(v), Some(r)) = (on_trace, standing, curve.as_ref()) {
                format!(
                    "from the curve match, not the trace search, which graded its own answer {trace_confidence} {:.1} km away. Logged Doppler curves of {} at one place and one recording clock (start {}): {}",
                    fdop_types::geo::haversine_m(top.lat, top.lon, v.lat, v.lon) / 1000.0,
                    curve_flights(r),
                    iso_utc(v.clock_unix),
                    v.why
                )
            } else {
                verdict_text(
                    confidence,
                    &gates,
                    &format!(
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
                )
            },
            gates,
        };
        if !on_trace {
            // the events and candidates below are the trace search's, not this answer's
            for e in &mut report.events {
                e.matched.clear();
            }
            warnings.push("The answer comes from the curve match. The candidates table lists what the trace search found on its own, and none of it supports this answer.".into());
        }
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
        if let (true, Some(r)) = (on_trace, &res.refine) {
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
        draw_map(
            &mut report,
            &mut warnings,
            place.headline.clone(),
            (lat, lon, radius),
            res.candidates
                .iter()
                .skip(usize::from(on_trace))
                .take(5)
                .map(|c| (c.lat, c.lon))
                .collect(),
        );
        // event figures with the winning flights' predicted curves
        let drawn: &[search::EventMatch] = if on_trace { &top.events } else { &[] };
        for m in drawn {
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
    } else if let (CurveCall::Adopts, Some(v), Some(r)) = (
        curve_call("none", None, curve_verdict.as_ref(), curve_judged),
        curve_verdict.as_ref(),
        curve.as_ref(),
    ) {
        found = true;
        curve_used = CurveCall::Adopts;
        log.say(&format!(
            "winner {:.6},{:.6} radius {:.0} m from the curve match alone, confidence medium",
            v.lat, v.lon, v.radius_m
        ));
        let place = fdop_geo::geocode::describe_location(net.as_ref(), &gaz, v.lat, v.lon);
        log.say(&format!("place: {}", place.headline));
        if let Some(w) =
            region.outside_states_warning(place.state_abbr.as_deref(), place.source == "census-api")
        {
            warnings.push(w);
        }
        report.answer = Answer {
            lat: v.lat,
            lon: v.lon,
            radius_m: v.radius_m,
            radius_basis: format!(
                "curve match: places within {:.0} nats of the best span {:.0} m (1 km floor, its modes are 1 km apart)",
                curvematch::TIED_NATS,
                v.radius_m
            ),
            confidence: "medium".into(),
            verdict: format!(
                "from the curve match; the trace search found no candidate ({} usable flyover events). Logged Doppler curves of {} at one place and one recording clock (start {}): {}",
                sevs.len(),
                curve_flights(r),
                iso_utc(v.clock_unix),
                v.why
            ),
            gates: vec![("curve match".to_string(), true, v.why.clone())],
        };
        report.place = Some(place.clone());
        draw_map(
            &mut report,
            &mut warnings,
            place.headline.clone(),
            (v.lat, v.lon, v.radius_m),
            vec![],
        );
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
        "media": {"duration_s": info.duration_s, "codec": info.audio_codec, "sample_rate": info.sample_rate, "creation_time": info.creation_time, "audio_stream": info.audio_stream, "audio_channel": info.audio_channel},
        "region_bbox": region.bbox.as_array(),
        "atmosphere": {"source": atmo.describe(), "c_0_1500m": m1.c, "c_0_5000m": m5.c, "wind_0_5000m": [m5.wind_e, m5.wind_n]},
        "search_events": sevs,
        "curve_match": curve_verdict.as_ref().map(|v| json!({
            "call": format!("{curve_used:?}"),
            "clock_window": [curve_clock.0, curve_clock.1],
            "judged_search_window": curve_judged,
            "verdict": v,
            "flights": curve.as_ref().map(curve_flights),
        })),
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

    /// A tone mix with power below 100 Hz reads well above zero; the same mix through a
    /// 110 Hz high-pass (the low tones removed) reads far below the warning bar.
    #[test]
    fn low_band_ratio_flags_high_passed_audio() {
        let sr = 4000u32;
        let n = (sr * 20) as usize;
        let tone = |f: f64, a: f64, i: usize| {
            a * (2.0 * std::f64::consts::PI * f * i as f64 / sr as f64).sin()
        };
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut noise = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        };
        let full: Vec<f32> = (0..n)
            .map(|i| {
                (tone(70.0, 0.5, i) + tone(85.0, 0.3, i) + tone(300.0, 0.1, i) + 0.01 * noise())
                    as f32
            })
            .collect();
        let passed: Vec<f32> = (0..n)
            .map(|i| (tone(300.0, 0.1, i) + 0.01 * noise()) as f32)
            .collect();
        let r_full = low_band_ratio_db(&full, sr).expect("ratio");
        let r_hp = low_band_ratio_db(&passed, sr).expect("ratio");
        assert!(r_full > 0.0, "{r_full}");
        assert!(r_hp < LOW_BAND_WARN_DB, "{r_hp}");
        assert!(low_band_warning(&full, sr).is_none());
        let warning = low_band_warning(&passed, sr).expect("warning");
        assert!(
            warning.contains(&format!("{:.0} dB below", -r_hp)),
            "{warning}"
        );
        assert!(
            low_band_ratio_db(&full[..1000], sr).is_none(),
            "shorter than one frame"
        );
        let silent = vec![0f32; n];
        assert!(
            low_band_ratio_db(&silent, sr).is_some_and(|r| r.abs() < 1e-6),
            "silence is not high-passed"
        );
    }

    /// A low answer prints a location and a radius, so its verdict must say it is unconfirmed and
    /// name only the gates that failed. A high answer keeps the bare match.
    #[test]
    fn verdict_says_when_an_answer_is_unconfirmed() {
        let gates = |failed: &[&str]| -> Vec<(String, bool, String)> {
            ["core matches", "clock lock", "margin", "radius"]
                .iter()
                .map(|g| (g.to_string(), !failed.contains(g), String::new()))
                .collect()
        };
        let matched = "2 flyovers matched to AAA C172, BBB PA28 at one location";
        assert_eq!(verdict_text("high", &gates(&[]), matched), matched);
        assert_eq!(
            verdict_text("medium", &gates(&["margin"]), matched),
            format!("failed: margin. {matched}")
        );
        let low = verdict_text("low", &gates(&["core matches", "margin"]), matched);
        assert!(
            low.starts_with("unconfirmed, failed: core matches, margin. "),
            "{low}"
        );
        assert!(low.contains("close or far off"), "{low}");
        assert!(low.ends_with(matched), "{low}");
        assert!(!low.contains("clock lock"), "{low}");
    }

    #[test]
    fn known_clock_requests_only_its_archive_day() {
        let (d0, d1) = parse_dates("2026-09-07..2026-09-09").unwrap();
        let dates = start_window(d0, d1, (-5.0, -4.0));
        let start = parse_iso_utc("2026-09-08T16:17:30-04:00").unwrap();
        let requested = requested_start_window(dates, Some(start), 180.0).unwrap();
        assert_eq!(requested, (start - 180.0, start + 180.0));
        // The availability check, weather and store load all use this outer window.
        // Even with a one-hour recording and track padding, no adjacent UTC day is needed.
        assert_eq!(
            fdop_data::fetch::utc_days(requested.0 - 1200.0, requested.1 + 3600.0 + 1200.0),
            vec![(2026, 9, 8)]
        );
        assert!(fdop_data::fetch::utc_days(dates.0 - 1200.0, dates.1 + 4800.0).len() > 1);
        assert_eq!(requested_start_window(dates, None, 180.0).unwrap(), dates);
    }

    #[test]
    fn known_clock_keeps_midnight_track_coverage() {
        let (d0, d1) = parse_dates("2026-09-08").unwrap();
        let dates = start_window(d0, d1, (-5.0, -4.0));
        let start = parse_iso_utc("2026-09-08T23:50:00Z").unwrap();
        let (lo, hi) = requested_start_window(dates, Some(start), 60.0).unwrap();
        assert_eq!(
            fdop_data::fetch::utc_days(lo - 1200.0, hi + 3600.0 + 1200.0),
            vec![(2026, 9, 8), (2026, 9, 9)]
        );
    }

    #[test]
    fn known_clock_stays_inside_dates_and_rejects_bad_inputs() {
        let dates = (1000.0, 2000.0);
        assert_eq!(
            requested_start_window(dates, Some(1050.0), 180.0).unwrap(),
            (1000.0, 1230.0)
        );
        assert_eq!(
            requested_start_window(dates, Some(1950.0), 180.0).unwrap(),
            (1770.0, 2000.0)
        );
        assert_eq!(
            requested_start_window(dates, Some(1500.0), 0.0).unwrap(),
            (1500.0, 1500.0)
        );
        assert!(requested_start_window(dates, Some(3000.0), 180.0).is_err());
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(requested_start_window(dates, Some(bad), 180.0).is_err());
            assert!(requested_start_window(dates, Some(1500.0), bad).is_err());
            assert!(requested_start_window(dates, None, bad).is_err());
        }
        assert!(requested_start_window(dates, None, -1.0).is_err());
    }

    #[test]
    fn metadata_hints_support_both_conventions_without_inverting_the_window() {
        let full = (1000.0, 2000.0);
        // The timestamp can mark the start or end of this 300-second recording.
        assert_eq!(
            metadata_start_window(full, 1500.0, 300.0, 10.0),
            Some((1190.0, 1510.0))
        );
        assert_eq!(
            metadata_start_window(full, 2200.0, 300.0, 10.0),
            Some((1890.0, 2000.0))
        );
        // A creation time before the earliest allowed start used to produce lo > hi.
        assert_eq!(metadata_start_window(full, 800.0, 300.0, 10.0), None);
        assert_eq!(
            metadata_start_window(full, f64::INFINITY, 300.0, 10.0),
            None
        );
        assert_eq!(metadata_start_window(full, f64::NAN, 300.0, 10.0), None);
    }

    fn stands_at(lat: f64, lon: f64, stands: bool) -> curvematch::CurveVerdict {
        curvematch::CurveVerdict {
            stands,
            lat,
            lon,
            clock_unix: 0.0,
            radius_m: 1000.0,
            evidence: 20.0,
            margin: 10.0,
            n_decoys: 48,
            p: 0.02,
            n_heard: 3,
            why: String::new(),
        }
    }

    #[test]
    fn a_standing_curve_match_rescues_an_empty_metadata_trace_search() {
        let good = stands_at(40.876466, -73.524951, true);
        let weak = stands_at(40.876466, -73.524951, false);
        // A usable metadata clock must reach the independent matcher even when no trace
        // survived measurement. Its standing answer follows the existing adoption gates.
        assert!(!metadata_needs_fallback(true, false, Some(&good)));
        assert_eq!(
            curve_call("none", None, Some(&good), true),
            CurveCall::Adopts
        );
        for curve in [None, Some(&weak)] {
            assert!(metadata_needs_fallback(true, false, curve));
            assert!(!metadata_needs_fallback(true, true, curve));
            // An explicit --start never widens, whether or not either search succeeded.
            assert!(!metadata_needs_fallback(false, false, curve));
        }
        // A negative curve test inside the old metadata window cannot veto a medium
        // trace answer that was found only after widening to the full date window.
        let judged = curve_judges_window((1000.0, 1200.0), (0.0, 86_400.0), weak.n_decoys);
        assert_eq!(
            curve_call("medium", Some((40.876466, -73.524951)), Some(&weak), judged),
            CurveCall::Silent
        );
    }

    #[test]
    fn only_a_new_curve_attempt_can_judge_the_widened_clock() {
        let narrow = (1000.25, 1100.25);
        let wider = (900.25, 1200.25);
        assert!(curve_judges_window(narrow, narrow, 40));
        assert!(!curve_judges_window(narrow, narrow, 39));
        assert!(!curve_judges_window(narrow, wider, 48));
        assert!(curve_judges_window(wider, wider, 48));
        // A rerun that failed or had too few decoys has no authority to grade it.
        assert!(!curve_judges_window(wider, wider, 0));
        assert!(!curve_judges_window(wider, wider, 39));
        assert!(!curve_judges_window(narrow, (1001.25, 1101.25), 48));
    }

    #[test]
    fn curve_image_is_only_needed_inside_the_clock_and_region_limits() {
        let p = curvematch::MatchParams::default();
        let small = BBox::new(40.8, -73.6, 40.9, -73.5);
        assert!(curve_match_skip_reason(&p, small, (1000.0, 1000.0)).is_none());
        assert!(curve_match_skip_reason(&p, small, (1000.0, 2800.0)).is_none());
        assert!(
            curve_match_skip_reason(&p, small, (1000.0, 2800.1))
                .unwrap()
                .contains("recording clock")
        );
        assert!(
            curve_match_skip_reason(&p, small, (0.0, 86_400.0))
                .unwrap()
                .contains("recording clock")
        );
        let large = BBox::new(40.0, -74.0, 42.0, -72.0);
        assert!(
            curve_match_skip_reason(&p, large, (1000.0, 1000.0))
                .unwrap()
                .contains("cells")
        );
    }

    /// Every combination of the trace grade and where the curve match stands. km east of 40 N.
    #[test]
    fn curve_call_table() {
        let east = |km: f64| -73.5 + km / (111.2 * 40f64.to_radians().cos());
        let v = stands_at(40.0, east(0.0), true);
        let at = |km: f64| Some((40.0, east(km)));
        // does not stand, or no verdict: where the curve match could not judge, whatever the trace
        // search said holds; where it could, only a medium answer is lowered
        let weak = stands_at(40.0, east(50.0), false);
        for conf in ["high", "medium", "low", "none"] {
            for verdict in [Some(&weak), None] {
                assert_eq!(curve_call(conf, at(0.0), verdict, false), CurveCall::Silent);
                let judged = if conf == "medium" {
                    CurveCall::Unsupported
                } else {
                    CurveCall::Silent
                };
                assert_eq!(curve_call(conf, at(0.0), verdict, true), judged, "{conf}");
            }
        }
        // asserted answers, judged or not: standing settles them
        for judged in [false, true] {
            for conf in ["high", "medium"] {
                assert_eq!(
                    curve_call(conf, at(0.5), Some(&v), judged),
                    CurveCall::Agrees
                );
                assert_eq!(
                    curve_call(conf, at(2.9), Some(&v), judged),
                    CurveCall::Agrees
                );
                assert_eq!(
                    curve_call(conf, at(3.1), Some(&v), judged),
                    CurveCall::Conflicts
                );
            }
            // a low answer inside the curve match's radius is confirmed, outside it is replaced
            assert_eq!(
                curve_call("low", at(0.9), Some(&v), judged),
                CurveCall::Confirms
            );
            assert_eq!(
                curve_call("low", at(1.1), Some(&v), judged),
                CurveCall::Adopts
            );
            assert_eq!(
                curve_call("low", at(100.0), Some(&v), judged),
                CurveCall::Adopts
            );
            // the trace search found nothing
            assert_eq!(
                curve_call("none", None, Some(&v), judged),
                CurveCall::Adopts
            );
        }
    }

    /// The old line measured the strongest `max_measured` primaries scoring `thr_db` or more on the
    /// survey image; anything else, including a contrast-stage find however strong, is added.
    #[test]
    fn contrast_stage_finds_are_measured_past_the_old_line() {
        let det = |score: f64, from_contrast: bool| Detection {
            id: 0,
            t0r: 30.0,
            f0: 200.0,
            v: 60.0,
            d: 900.0,
            score,
            score_approach: score,
            score_recede: score,
            half_s: 30.0,
            harmonic_of: None,
            harmonics: Vec::new(),
            from_contrast,
        };
        assert!(!past_the_old_line(0, &det(6.0, false), 24, 4.5));
        assert!(!past_the_old_line(23, &det(4.5, false), 24, 4.5));
        assert!(past_the_old_line(24, &det(9.0, false), 24, 4.5));
        assert!(past_the_old_line(0, &det(4.4, false), 24, 4.5));
        assert!(past_the_old_line(0, &det(9.0, true), 24, 4.5));
    }

    /// Events measured past the old line hold up a medium answer only where the curve match judged it.
    #[test]
    fn a_medium_on_events_past_the_old_line_needs_a_curve_match_that_judged() {
        let added: std::collections::HashSet<String> = ["e7".to_string()].into();
        // rests on an added event and nothing judged it: lowered
        assert!(leans_on_added(
            "medium",
            CurveCall::Silent,
            false,
            &["e1", "e7"],
            &added
        ));
        // only events the old line measured: kept
        assert!(!leans_on_added(
            "medium",
            CurveCall::Silent,
            false,
            &["e1", "e2"],
            &added
        ));
        assert!(!leans_on_added(
            "medium",
            CurveCall::Silent,
            false,
            &[],
            &added
        ));
        // the curve match judged it: it stands and agrees, or `Unsupported` already lowered it
        for call in [CurveCall::Agrees, CurveCall::Unsupported] {
            assert!(!leans_on_added("medium", call, true, &["e1", "e7"], &added));
        }
        // high and low answers are not this rule's
        for conf in ["high", "low", "none"] {
            assert!(!leans_on_added(
                conf,
                CurveCall::Silent,
                false,
                &["e7"],
                &added
            ));
        }
    }

    /// A confirmed trace answer 0.9 km west of the curve match's best place, with a place 4 km east
    /// of that best still tied: the circle around the trace answer must reach 4.9 km.
    #[test]
    fn a_confirmed_answer_keeps_every_tied_curve_place_inside_its_radius() {
        let east = |km: f64| -73.5 + km / (111.2 * 40f64.to_radians().cos());
        let mut v = stands_at(40.0, east(0.0), true);
        v.radius_m = 4000.0;
        let trace = (40.0, east(-0.9));
        assert_eq!(
            curve_call("low", Some(trace), Some(&v), true),
            CurveCall::Confirms
        );
        let r = confirmed_radius(250.0, trace, &v);
        let far = fdop_types::geo::haversine_m(trace.0, trace.1, 40.0, east(4.0));
        assert!(r >= far - 1.0, "radius {r} m, tied place {far} m away");
        // a trace radius already wider is kept
        assert_eq!(confirmed_radius(9000.0, trace, &v), 9000.0);
    }
}
