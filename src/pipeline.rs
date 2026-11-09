//! `fdop locate`: media file + dates + coarse US region -> location, radius, map, place.

use fdop_core::curvematch;
use fdop_core::detect::{self, DetectParams, Detection};
use fdop_core::measure::{self, MeasureParams, MeasuredEvent};
use fdop_core::search::{self, SearchInputs, SearchParams};
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
use std::time::{Duration, Instant};

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
    /// The trace search's top evidence (nats, the margin gate's "evidence"); None when it found no
    /// candidate. A decoy-clock run hands it back to the run that asked for it.
    pub trace_evidence: Option<f64>,
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

/// The creation tag may mark the container's end even when its selected audio ends earlier.
/// Missing or invalid duration tags cannot shorten the decoded timeline or widen it to infinity.
fn media_metadata_start_window(
    full: (f64, f64),
    info: &audio::MediaInfo,
    decoded_duration: f64,
    tolerance: f64,
) -> Option<(f64, f64)> {
    let creation = info.creation_unix?;
    let duration = if info.duration_s.is_finite() && info.duration_s > 0.0 {
        info.duration_s.max(decoded_duration)
    } else {
        decoded_duration
    };
    metadata_start_window(full, creation, duration, tolerance)
}

struct PublishedWindow {
    clock: (f64, f64),
    adsb_end: f64,
    pending: Vec<String>,
}

impl PublishedWindow {
    fn track_spec(&self, bbox: BBox, duration: f64, geoid_n_m: f64) -> fdop_data::IngestSpec {
        fdop_data::IngestSpec {
            bbox,
            t_start: self.clock.0 - 1200.0,
            t_end: (self.clock.1 + duration + 1200.0).min(self.adsb_end),
            geoid_n_m,
            keep_unknown: true,
        }
    }

    fn weather_span(
        &self,
        bbox: BBox,
        duration: f64,
        curve: Option<&curvematch::MatchParams>,
    ) -> (f64, f64) {
        let mut span = (self.clock.0, self.clock.1 + duration);
        if let Some(p) = curve
            && curve_match_skip_reason(p, bbox, self.clock).is_none()
        {
            let catalog = curve_catalog_span(p, self.clock, duration, self.adsb_end);
            span = (span.0.min(catalog.0), span.1.max(catalog.1));
        }
        // Soundings must bracket the decoys too. Unlike Open-Meteo's whole-day responses,
        // a narrow radiosonde request can otherwise miss the previous 00Z/12Z profile.
        (span.0 - 3600.0, span.1 + 3600.0)
    }
}

/// Check only the clock currently being tried. In particular, a metadata hint must not
/// require the full date range's archive before either search has tried that hint.
fn published_start_window(
    a: &LocateArgs,
    clock: (f64, f64),
    duration: f64,
    now: f64,
    warnings: &mut Vec<String>,
) -> Result<PublishedWindow, String> {
    let (end, pending) = fdop_data::published_until(
        clock.0 - 1200.0,
        clock.1 + duration + 1200.0,
        &a.cache,
        a.offline,
        now,
    )
    .map_err(|e| format!("ADS-B availability: {e}"))?;
    if pending.is_empty() {
        return Ok(PublishedWindow {
            clock,
            adsb_end: f64::INFINITY,
            pending,
        });
    }
    // The last flyover needs a couple of minutes of track after the recording ends.
    let hi = clock.1.min(end - duration - 300.0);
    let why = format!(
        "the ADS-B archive for UTC day(s) {} is not published yet{} (adsb.lol releases each UTC day a few hours after it ends)",
        pending.join(", "),
        if a.offline { " or not cached" } else { "" }
    );
    if hi < clock.0 {
        return Err(format!(
            "{why}; no published day covers a {:.0} min recording in the requested start window on {}: rerun later",
            duration / 60.0,
            a.dates
        ));
    }
    warnings.push(format!(
        "{why}: only recording starts up to {} were searched; rerun after the release to cover the rest of {}",
        iso_utc(hi),
        a.dates
    ));
    Ok(PublishedWindow {
        clock: (clock.0, hi),
        adsb_end: end - 1.0,
        pending,
    })
}

/// A hint whose day is not published cannot rule out the published part of the dates.
/// An explicit clock arrives here without a metadata window and remains binding.
fn first_published_window(
    a: &LocateArgs,
    full: (f64, f64),
    metadata: Option<(f64, f64)>,
    duration: f64,
    now: f64,
    warnings: &mut Vec<String>,
) -> Result<(PublishedWindow, bool), String> {
    match published_start_window(a, metadata.unwrap_or(full), duration, now, warnings) {
        Ok(w) => Ok((w, metadata.is_some())),
        Err(e) if metadata.is_some() => {
            warnings.push(format!(
                "creation_time window could not be searched ({e}); trying the full date window"
            ));
            published_start_window(a, full, duration, now, warnings).map(|w| (w, false))
        }
        Err(e) => Err(e),
    }
}

enum WeatherUpdate {
    Fallback(fdop_data::Weather),
    Complete(fdop_data::Weather, f64),
}

/// Keep usable weather while waiting for the final combination. Updates share one wait
/// budget, and a later surface-only result cannot replace already retrieved winds aloft.
fn wait_for_weather(
    rx: &std::sync::mpsc::Receiver<WeatherUpdate>,
    timeout: Duration,
) -> (fdop_data::Weather, f64) {
    let start = Instant::now();
    let mut fallback: Option<fdop_data::Weather> = None;
    let stopped = loop {
        match rx.recv_timeout(timeout.saturating_sub(start.elapsed())) {
            Ok(WeatherUpdate::Complete(weather, elapsed)) => return (weather, elapsed),
            Ok(WeatherUpdate::Fallback(weather)) => {
                if fallback
                    .as_ref()
                    .is_none_or(|old| weather.winds_aloft || !old.winds_aloft)
                {
                    fallback = Some(weather);
                }
            }
            Err(e) => break e,
        }
    };
    let reason = match stopped {
        std::sync::mpsc::RecvTimeoutError::Timeout => "weather services did not all answer in time",
        std::sync::mpsc::RecvTimeoutError::Disconnected => {
            "weather fetch ended before a final result was available"
        }
    };
    let mut weather = fallback.unwrap_or_else(|| fdop_data::Weather {
        atmo: Arc::new(fdop_types::atmo::StandardAtmosphere::new(15.0, 0.0, 0.5)),
        warnings: vec!["no weather profile arrived; using a 15 C standard atmosphere".into()],
        winds_aloft: false,
    });
    weather.warnings.push(format!(
        "{reason}; using the best available profile{}",
        if stopped == std::sync::mpsc::RecvTimeoutError::Timeout {
            " (rerun to use any additional cached profiles once they arrive)"
        } else {
            ""
        },
    ));
    // The selected profile may be ready, but the full fetch has not completed.
    (weather, f64::NAN)
}

type WeatherReceiver = std::sync::mpsc::Receiver<WeatherUpdate>;

/// Weather and audio/track preparation are independent. Each attempted clock gets its own
/// profiles; a wider retry must not extrapolate the metadata window's weather across days.
fn start_weather(
    a: &LocateArgs,
    bbox: BBox,
    window: &PublishedWindow,
    duration: f64,
    curve_params: &curvematch::MatchParams,
) -> WeatherReceiver {
    let (tx, rx) = std::sync::mpsc::channel();
    let (cache, offline) = (a.cache.clone(), a.offline);
    let (w0, w1) = window.weather_span(bbox, duration, a.curve_match.then_some(curve_params));
    std::thread::spawn(move || {
        let t = Instant::now();
        let fallback_tx = tx.clone();
        let r = fdop_data::weather::fetch_atmosphere_with_progress(
            &bbox.padded_km(30.0),
            w0,
            w1,
            &cache,
            offline,
            move |weather| {
                let _ = fallback_tx.send(WeatherUpdate::Fallback(weather));
            },
        );
        let _ = tx.send(WeatherUpdate::Complete(r, t.elapsed().as_secs_f64()));
    });
    rx
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
const CURVE_MAX_CELLS: usize = 200_000;
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
    let Some(cells) = curvematch::grid_cell_count(p, &bbox) else {
        return Some("the requested region cannot form a finite local curve grid".into());
    };
    (cells > CURVE_MAX_CELLS).then(|| {
        format!("the region needs {cells} cells, and it searches {CURVE_MAX_CELLS} at most")
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

/// Where the clock window the curve match searched came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClockSource {
    /// `--start` +- `--clock-tol`.
    Start,
    /// The file's `creation_time`, read as the start or the end, +- `--clock-tol`.
    CreationTime,
    /// The local dates, clipped by the published archive to under `CURVE_MAX_CLOCK_S`.
    Dates,
}

/// Why a curve match that ran cannot judge, and what would let it. It gets fewer than `MIN_DECOYS`
/// decoy windows either because its clock window is wider than `max_judged_window_s`, or, for a
/// narrower one, because its decoys must come from the UTC days the recording needs, before the
/// published archive ends (`curve_catalog_span`), and those end too close to it. The limits are
/// printed rounded down and the width rounded up, so the advice never names a window past the line.
fn too_few_decoys_warning(
    p: &curvematch::MatchParams,
    clock: (f64, f64),
    n_decoys: usize,
    source: ClockSource,
    duration: f64,
) -> Option<String> {
    let need = curvematch::MIN_DECOYS;
    if n_decoys >= need {
        return None;
    }
    let (width, max) = (clock.1 - clock.0, curvematch::max_judged_window_s(p));
    let (shown_width, shown_max, shown_tol) = (width.ceil(), max.floor(), (0.5 * max).floor());
    let span_h = p.decoy_span_s / 3600.0;
    let cause = if width <= max {
        "its decoys come only from the UTC days this recording needs, before the published ADS-B archive ends, and those leave no room for more around this clock".to_string()
    } else {
        match source {
            ClockSource::Start => format!(
                "a {shown_width:.0} s clock window gets no more within about +-{span_h:.0} h, and one of {shown_max:.0} s or less gets {need} (--clock-tol {shown_tol:.0} or less with --start)"
            ),
            ClockSource::CreationTime => format!(
                "the creation_time window is {shown_width:.0} s wide, since it spans the recording's {duration:.0} s as well as the tolerance, and only one of {shown_max:.0} s or less gets {need}"
            ),
            ClockSource::Dates => format!(
                "a {shown_width:.0} s clock window gets no more within about +-{span_h:.0} h, and only one of {shown_max:.0} s or less gets {need} (--start with --clock-tol {shown_tol:.0} or less)"
            ),
        }
    };
    Some(format!(
        "the curve match ran with {n_decoys} decoy clocks, under the {need} it needs to judge: {cause}. It can neither stand nor overrule the trace search's grade"
    ))
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
/// have made: past the strongest `max_measured`, a primary whose detection scored under `thr_db`,
/// one found only on the contrast image, a faster supplemental seed, or a harmonic recovery.
fn past_the_old_line(
    k: usize,
    primary: &Detection,
    measured: &Detection,
    max_measured: usize,
    thr_db: f64,
) -> bool {
    k >= max_measured
        || primary.score < thr_db
        || primary.from_contrast
        || primary.v > detect::PRIMARY_MAX_SPEED_MPS
        || measured.harmonic_of.is_some()
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

/// The catalogue includes the same decoy clocks for ADS-B and weather, including the
/// extra clocks placed on one side near UTC midnight or the published archive's end.
fn curve_catalog_span(
    p: &curvematch::MatchParams,
    clock: (f64, f64),
    duration: f64,
    adsb_end: f64,
) -> (f64, f64) {
    let day = 86_400.0;
    // Fetch windows include both endpoints. End inside the final required UTC day,
    // not at the following midnight, which would demand an unrelated extra store.
    // A real padded endpoint exactly at midnight does require that new day.
    let last_day = ((clock.1 + duration + 1200.0) / day).floor();
    let limits = (
        ((clock.0 - 1200.0) / day).floor() * day,
        ((last_day + 1.0) * day).next_down().min(adsb_end),
    );
    curvematch::catalog_span(p, clock.0, clock.1, duration, limits)
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
    warnings: &mut Vec<String>,
) -> Result<Option<curvematch::MatchResult>, String> {
    let (s_lo, s_hi) = clock;
    if let Some(why) = curve_match_skip_reason(p, bbox, clock) {
        log.say(&format!("curve match skipped: {why}"));
        return Ok(None);
    }
    let (t_start, t_end) = curve_catalog_span(p, clock, dur, adsb_end);
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
    // The independent real/decoy catalogue can have different coverage from the trace
    // catalogue. Keep its limitations in the final report as well as fetch progress.
    if let Some(notes) = store.meta.stats["notes"].as_array() {
        warnings.extend(
            notes
                .iter()
                .filter_map(|n| n.as_str())
                .map(|n| format!("curve match ADS-B {n}")),
        );
    }
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

/// Decoy clocks, in [`decoy_unit_s`] steps from the searched clock window's centre, in the order
/// tried; the first [`DECOY_CLOCKS`] whose whole window lies inside the dates are used. Hours
/// apart, no true pass lines up at a decoy, and the traffic is that of the same place and day.
const DECOY_STEPS: [f64; 8] = [-2.0, -1.0, 1.0, 2.0, -3.0, 3.0, -4.0, 4.0];
/// The least gap between a decoy's clock window and the searched one.
const DECOY_GAP_S: f64 = 600.0;
/// Decoy clocks a high or medium trace answer is checked against.
const DECOY_CLOCKS: usize = 4;
/// Nats a high or medium trace answer must lead its best decoy clock by: the medium margin line.
const DECOY_LEAD_NATS: f64 = 3.0;

/// The decoy step for a clock window `centre +- half`: whole hours, at least the window's width
/// plus [`DECOY_GAP_S`], so no decoy window reaches the true start. A `--start` window
/// (`--clock-tol` up to 1500 s) gets 1 h; a creation_time window, which spans the recording, gets
/// more (a one-hour recording's 3840 s window gets 2 h).
fn decoy_unit_s(half: f64) -> f64 {
    3600.0 * ((2.0 * half + DECOY_GAP_S) / 3600.0).ceil().max(1.0)
}

/// Decoy shifts for a clock window `t0 +- tol`, in the order tried: those whose window
/// `t0 + shift +- tol` fits inside the date window.
fn decoy_shifts(t0: f64, dates: (f64, f64), tol: f64) -> Vec<f64> {
    let unit = decoy_unit_s(tol);
    DECOY_STEPS
        .iter()
        .map(|k| k * unit)
        .filter(|s| t0 + s - tol >= dates.0 && t0 + s + tol <= dates.1)
        .collect()
}

/// The decoy-clock gate. Each run is a shift and the trace search's top evidence there (None when
/// it found no candidate, which adds no evidence). It passes when [`DECOY_CLOCKS`] decoys ran and
/// `evidence` leads the best of them by [`DECOY_LEAD_NATS`].
fn decoy_gate(evidence: f64, runs: &[(f64, Option<f64>)]) -> (bool, String) {
    let list = runs
        .iter()
        .map(|(s, e)| {
            format!(
                "{:+.0} h {}",
                s / 3600.0,
                e.map_or_else(|| "none".to_string(), |e| format!("{e:.2}"))
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    if runs.len() < DECOY_CLOCKS {
        return (
            false,
            format!(
                "only {} of {DECOY_CLOCKS} decoy clocks ran inside the dates ({list}); a high or medium answer needs all of them",
                runs.len()
            ),
        );
    }
    let best = runs
        .iter()
        .filter_map(|r| r.1)
        .reduce(f64::max)
        .unwrap_or(0.0);
    let lead = evidence - best;
    (
        lead >= DECOY_LEAD_NATS,
        format!(
            "evidence {evidence:.2} nats vs {best:.2} at the best of {DECOY_CLOCKS} decoy clocks, the same search with its clock window moved ({list}): {lead:+.2} nats, needs {DECOY_LEAD_NATS:.1}"
        ),
    )
}

/// Arguments for one decoy-clock run: `a` with the clock window `t0 +- tol` moved by `shift` and
/// given as `--start` and `--clock-tol` (a creation_time window becomes the same window as a start,
/// so the decoy reads no metadata), its own run directory, quiet, and no curve match. The gate
/// reads only the trace search's evidence, which with an explicit start is computed before the
/// curve match and does not read it. Without the curve match that evidence came out identical on
/// four decoys from both development harnesses, at a fifth to a tenth of the time (LOG
/// 2026-11-08T17:51Z).
fn decoy_args(a: &LocateArgs, t0: f64, tol: f64, shift: f64, run_dir: &Path) -> LocateArgs {
    let mut d = a.clone();
    d.start = Some(format!("{:.3}", t0 + shift));
    d.clock_tol_s = tol;
    d.out = Some(run_dir.join(format!("decoy_clock_{:+.0}h", shift / 3600.0)));
    d.quiet = true;
    d.curve_match = false;
    d
}

/// Audio measurements use the fixed reference sound speed, independent of the searched clock.
/// Borrow the original observations for decoys; their tracks, atmosphere and speed gates still
/// belong to each shifted clock. In particular, do not clone the per-event spectrograms.
struct PreparedRecording<'a> {
    info: &'a audio::MediaInfo,
    duration_s: f64,
    low_band_warning: Option<&'a str>,
    measurements: &'a AudioMeasurements,
}

struct AudioMeasurements {
    detections: Vec<Detection>,
    events: Vec<MeasuredEvent>,
    added: std::collections::HashSet<String>,
    warnings: Vec<String>,
    detect_s: f64,
    measure_s: f64,
}

fn measure_recording(
    x: &[f32],
    sr: u32,
    log: &Arc<Log>,
    survey_path: Option<&Path>,
) -> Result<AudioMeasurements, String> {
    let mut warnings = Vec::new();
    let t = Instant::now();
    let dp = DetectParams::default();
    let (ls, lin) = detect::survey(x, sr, &dp)?;
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
    let detect_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let mp = MeasureParams::default();
    // Every primary from the silence line up is measured strongest first. Refused, duplicate
    // and non-aircraft fits leave room for later detections; a family contributes one event.
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
                    measure::measure_family(
                        x,
                        sr,
                        &lin,
                        d,
                        &dets,
                        &format!("e{}", ranks.start + i + 1),
                        &mp,
                    )
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
    let added_ranks: std::collections::HashSet<String> = (0..events.len())
        .filter(|&k| past_the_old_line(k, prim[k], &events[k].detection, max_measured, dp.thr_db))
        .map(|k| format!("e{}", k + 1))
        .collect();
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
        "{} of them measured past the old line (detection score under {:.1}, past the strongest {max_measured}, found only on the contrast image, from the fast bank, or recovered from a harmonic)",
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
    let measure_s = t.elapsed().as_secs_f64();
    if let Some(path) = survey_path {
        let _ = survey_png(&ls, &lin, &dets, &events, path);
    }
    Ok(AudioMeasurements {
        detections: dets,
        events,
        added,
        warnings,
        detect_s,
        measure_s,
    })
}

/// Run the same locate at decoy clocks for the clock window `t0 +- tol`, each into
/// `run_dir/decoy_clock_<shift>h`, until [`DECOY_CLOCKS`] have run. A decoy that fails (an
/// unpublished day, say) is logged and the next shift is tried.
fn decoy_clock_runs(
    a: &LocateArgs,
    now: &dyn Fn() -> f64,
    (t0, tol): (f64, f64),
    dates: (f64, f64),
    run_dir: &Path,
    log: &Arc<Log>,
    recording: &PreparedRecording<'_>,
) -> Vec<(f64, Option<f64>)> {
    let mut runs = Vec::new();
    for s in decoy_shifts(t0, dates, tol) {
        if runs.len() == DECOY_CLOCKS {
            break;
        }
        let d = decoy_args(a, t0, tol, s, run_dir);
        match locate_with_clock(&d, &Log::new(true), now, true, Some(recording)) {
            Ok(o) => {
                log.say(&format!(
                    "decoy clock {:+.0} h: top evidence {}",
                    s / 3600.0,
                    o.trace_evidence
                        .map_or_else(|| "none".to_string(), |e| format!("{e:.2} nats"))
                ));
                runs.push((s, o.trace_evidence));
            }
            Err(e) => log.say(&format!(
                "decoy clock {:+.0} h did not run: {e}",
                s / 3600.0
            )),
        }
    }
    runs
}

/// Run the whole pipeline.
pub fn locate(a: &LocateArgs, log: &Arc<Log>) -> Result<LocateOutcome, String> {
    locate_with_clock(a, log, &unix_now, false, None)
}

/// `decoy` marks a run made by [`decoy_clock_runs`], which checks no decoys of its own.
fn locate_with_clock(
    a: &LocateArgs,
    log: &Arc<Log>,
    now: &dyn Fn() -> f64,
    decoy: bool,
    recording: Option<&PreparedRecording<'_>>,
) -> Result<LocateOutcome, String> {
    assert!(
        recording.is_none() || (decoy && !a.curve_match),
        "shared audio is only used for trace-only decoys"
    );
    let t_all = Instant::now();
    let mut timings = serde_json::Map::new();
    let mut warnings: Vec<String> = Vec::new();
    let c = fdop_types::C_SOUND;
    // A decoy run is read only for its trace evidence. It draws no figures or map and asks the
    // Census geocoder nothing, so it writes a few megabytes instead of its answer's hundred-odd
    // and requests no tiles or place names for a place nobody asked about.
    let extras = !decoy;
    // ---- inputs -------------------------------------------------------------------------
    if !a.media.is_file() {
        return Err(format!("{}: not a file", a.media.display()));
    }
    let info = match recording {
        Some(r) => r.info.clone(),
        None => audio::probe_with_selection(&a.media, a.audio_selection)?,
    };
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
    let full_window = requested_start_window(
        start_window(d0, d1, region.utc_offset_h),
        given_start,
        a.clock_tol_s,
    )?;
    // Archive and weather coverage follow the selected audio, not a longer video or a
    // missing/incorrect duration tag. Decode before publication clipping; weather still
    // overlaps detection and measurement. Metadata retains both start/end conventions.
    let t = Instant::now();
    let x = if recording.is_none() {
        audio::decode_with_selection(&a.media, 0.0, None, a.sr, a.audio_selection)?
    } else {
        Vec::new()
    };
    let dur = recording.map_or_else(|| x.len() as f64 / a.sr as f64, |r| r.duration_s);
    let low_band = match recording {
        Some(r) => r.low_band_warning.map(str::to_owned),
        None => low_band_warning(&x, a.sr),
    };
    warnings.extend(low_band.iter().cloned());
    if recording.is_some() {
        log.say("reusing the original recording's decoded duration, detections and measurements");
        timings.insert("decode_s".into(), json!(0.0));
    } else {
        log.say(&format!("decoded {:.1} s at {} Hz", dur, a.sr));
        timings.insert("decode_s".into(), json!(t.elapsed().as_secs_f64()));
    }
    let metadata_window = if given_start.is_none() && !a.ignore_metadata_time {
        media_metadata_start_window(full_window, &info, dur, a.clock_tol_s)
    } else {
        None
    };
    let (mut archive, mut meta_window) =
        first_published_window(a, full_window, metadata_window, dur, now(), &mut warnings)?;
    let mut metadata_fallback = metadata_window.is_some() && !meta_window;
    let describe_window = |w: &PublishedWindow, metadata: bool, fallback: bool| {
        let (lo, hi) = w.clock;
        let mut note = if metadata {
            format!(
                "recording start in [{}, {}] from the file's creation_time {} (start or end convention, +-{:.0} s); full-day fallback if neither search finds an answer",
                iso_utc(lo),
                iso_utc(hi),
                iso_utc(info.creation_unix.unwrap()),
                a.clock_tol_s
            )
        } else if let Some(t) = given_start {
            format!(
                "recording start between {} and {} (--start {} +- {:.0} s, within local dates {})",
                iso_utc(lo),
                iso_utc(hi),
                iso_utc(t),
                a.clock_tol_s,
                a.dates
            )
        } else if fallback {
            format!(
                "recording start between {} and {} (full date window after the creation_time window could not locate the recording)",
                iso_utc(lo),
                iso_utc(hi)
            )
        } else {
            format!(
                "recording start between {} and {} (local dates {} at UTC{:+}..{:+})",
                iso_utc(lo),
                iso_utc(hi),
                a.dates,
                region.utc_offset_h.0,
                region.utc_offset_h.1
            )
        };
        if !w.pending.is_empty() {
            note.push_str(&format!(
                "; clipped to the published ADS-B archive ({} pending)",
                w.pending.join(", ")
            ));
        }
        note
    };
    log.say(&describe_window(&archive, meta_window, metadata_fallback));
    let curve_params = curvematch::MatchParams::default();
    let mut weather_rx = start_weather(a, region.bbox, &archive, dur, &curve_params);
    let run_dir = a.out.clone().unwrap_or_else(|| default_run_dir(&a.media));
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("{}: {e}", run_dir.display()))?;
    let mut curve_clock = archive.clock;
    // whether `curve_clock` is the creation_time window; it stays so if a wider retry keeps the
    // metadata attempt's curve match
    let mut curve_from_metadata = meta_window;
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
    // ---- detection and measurement: one immutable observation set for every clock ---------
    let measured;
    let measurements = match recording {
        Some(r) => r.measurements,
        None => {
            let survey_path = run_dir.join("survey.png");
            measured = measure_recording(&x, a.sr, log, extras.then_some(survey_path.as_path()))?;
            &measured
        }
    };
    drop(x);
    warnings.extend(measurements.warnings.iter().cloned());
    timings.insert(
        "detect_s".into(),
        json!(if recording.is_some() {
            0.0
        } else {
            measurements.detect_s
        }),
    );
    timings.insert(
        "measure_s".into(),
        json!(if recording.is_some() {
            0.0
        } else {
            measurements.measure_s
        }),
    );
    let dets = &measurements.detections;
    let events = &measurements.events;
    let added = &measurements.added;
    let max_measured = measure::MAX_MEASURED;
    write_json(&run_dir.join("detections.json"), dets)?;
    write_json(&run_dir.join("events.json"), events)?;
    for e in events {
        if e.fit.is_some() {
            if extras {
                let _ = event_png(e, c, &run_dir.join(format!("{}.png", e.id)), None, "");
            }
            let _ = e
                .trace
                .write_csv(&run_dir.join(format!("{}_trace.csv", e.id)));
        }
    }
    // Wind changes speed gates, not event eligibility, order, distance gates or grouping.
    // Select the capped event IDs once; rebuild their speed gates for each attempted atmosphere.
    let mut base_events = search::independent_measured_events(events, 0.0);
    let excluded_by_event_limit: std::collections::HashSet<String> = base_events
        .iter()
        .skip(max_measured)
        .map(|e| e.id.clone())
        .collect();
    if base_events.len() > max_measured {
        warnings.push(format!(
            "measurement produced {} independent aircraft events after regrouping; the {max_measured} strongest were searched and {} omitted by the event limit",
            base_events.len(),
            base_events.len() - max_measured,
        ));
        base_events.truncate(max_measured);
    }
    log.say(&format!(
        "{} aircraft-like events go to the search",
        base_events.len()
    ));
    let heard: Vec<f64> = dets.iter().map(|d| d.t0r).collect();
    let d_max = base_events
        .iter()
        .map(|e| e.d_gate[1])
        .fold(5000.0, f64::max)
        .min(30_000.0);
    // Aircraft within d_gate of an observer, plus ~2 minutes on each side of the CPA.
    let fetch_box = region.bbox.padded_km((d_max / 1000.0 + 15.0).min(30.0));
    let t = Instant::now();
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
    let mut adsb_s = t.elapsed().as_secs_f64();
    let lg = log.clone();
    let opts = fdop_data::FetchOptions {
        offline: a.offline,
        allow_missing: false,
        progress: Some(Arc::new(move |s: &str| lg.say(s))),
        ..Default::default()
    };
    // Terrain is the same for both clocks and is shared across the attempts.
    let t = Instant::now();
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
    let mut tracks_terrain_s = t.elapsed().as_secs_f64();
    let (mut weather_fetch_s, mut weather_wait_s, mut search_s) = (0.0, 0.0, 0.0);
    let mut curve_match_s = curve_image_s;
    let mut curve = None;
    let (tracks, atmo, has_wind, sevs, res) = loop {
        // ---- ADS-B for this attempt ------------------------------------------------------
        let spec = archive.track_spec(fetch_box, dur, geoid_n);
        log.say(&format!(
            "ADS-B: {} .. {} over [{:.2},{:.2},{:.2},{:.2}] (geoid N {geoid_n:.2} m, {geoid_src})",
            iso_utc(spec.t_start),
            iso_utc(spec.t_end),
            fetch_box.south,
            fetch_box.west,
            fetch_box.north,
            fetch_box.east
        ));
        let t = Instant::now();
        let fetched = fdop_data::fetch_window(&spec, &a.cache, &opts);
        adsb_s += t.elapsed().as_secs_f64();
        let store = match fetched {
            Ok(s) => s,
            Err(e) if meta_window => {
                let why = format!(
                    "creation_time window could not load ADS-B data ({e}); trying the full date window"
                );
                log.say(&why);
                warnings.push(why);
                meta_window = false;
                metadata_fallback = true;
                archive = published_start_window(a, full_window, dur, now(), &mut warnings)?;
                weather_rx = start_weather(a, region.bbox, &archive, dur, &curve_params);
                log.say(&describe_window(&archive, false, true));
                continue;
            }
            Err(e) => return Err(format!("ADS-B fetch: {e}")),
        };
        log.say(&format!(
            "ADS-B: {} legs, {} points",
            store.legs.len(),
            store.n_points()
        ));
        // A lesser feed can lack aircraft: neither matches nor unheard close passes.
        if let Some(notes) = store.meta.stats["notes"].as_array() {
            warnings.extend(
                notes
                    .iter()
                    .filter_map(|n| n.as_str())
                    .map(|n| format!("ADS-B {n}")),
            );
        }
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
        tracks_terrain_s += t.elapsed().as_secs_f64();
        // ---- atmosphere for this attempt ------------------------------------------------
        let t = Instant::now();
        let (wx, fetch_s) = wait_for_weather(&weather_rx, fdop_data::weather::FETCH_WAIT_TIMEOUT);
        weather_wait_s += t.elapsed().as_secs_f64();
        weather_fetch_s += fetch_s;
        let fdop_data::Weather {
            atmo,
            warnings: wwarn,
            winds_aloft: has_wind,
        } = wx;
        for w in &wwarn {
            log.say(&format!("weather: {w}"));
        }
        warnings.extend(wwarn);
        let wind = if has_wind { 3.0 } else { 10.0 };
        let mut sevs = search::independent_measured_events(events, wind);
        sevs.truncate(max_measured);
        // ---- search ---------------------------------------------------------------------
        let t = Instant::now();
        let mut sp = SearchParams::new(archive.clock.0, archive.clock.1, dur, region.bbox);
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
        let res = search::run(&inp, &sp, &|s: &str| log.say(s));
        search_s += t.elapsed().as_secs_f64();
        // The curve matcher still loads its own decoy-clock catalogue. A narrow verdict
        // remains diagnostic after a wider retry unless that wider clock is eligible too.
        if let Some(im) = &curve_image
            && curve_match_skip_reason(&curve_params, region.bbox, archive.clock).is_none()
        {
            let t = Instant::now();
            curve_clock = archive.clock;
            curve_from_metadata = meta_window;
            curve = match run_curve_match(
                a,
                &curve_params,
                im,
                region.bbox,
                fetch_box,
                curve_clock,
                dur,
                archive.adsb_end,
                geoid_n,
                &coarse,
                &fine,
                &*atmo,
                log,
                &mut warnings,
            ) {
                Ok(r) => r,
                Err(e) => {
                    warnings.push(format!("curve match skipped: {e}"));
                    None
                }
            };
            curve_match_s += t.elapsed().as_secs_f64();
        }
        let verdict = curve.as_ref().and_then(curvematch::verdict);
        if metadata_needs_fallback(meta_window, !res.candidates.is_empty(), verdict.as_ref()) {
            log.say("neither search located the recording inside the metadata window; widening to the full date window");
            warnings.push(
                "creation_time window gave no trace candidate or standing curve match; the full date window was searched. A curve match in the metadata window does not judge the wider search".into(),
            );
            meta_window = false;
            metadata_fallback = true;
            archive = published_start_window(a, full_window, dur, now(), &mut warnings)?;
            weather_rx = start_weather(a, region.bbox, &archive, dur, &curve_params);
            log.say(&describe_window(&archive, false, true));
            continue;
        }
        break (tracks, atmo, has_wind, sevs, res);
    };
    drop(curve_image);
    let curve_verdict = curve.as_ref().and_then(curvematch::verdict);
    let (s_lo, s_hi) = archive.clock;
    let window_note = describe_window(&archive, meta_window, metadata_fallback);
    let mid = 0.5 * (s_lo + s_hi) + 0.5 * dur;
    let (clat, clon) = region.center;
    let m1 = atmo.medium(clat, clon, mid, 10.0, 1500.0);
    let m5 = atmo.medium(clat, clon, mid, 10.0, 5000.0);
    log.say(&format!(
        "atmosphere: {}; mid-window at region centre: c(0-1.5 km) {:.1} m/s, c(0-5 km) {:.1} m/s, wind(0-5 km) {:.1} m/s",
        atmo.describe(), m1.c, m5.c, m5.wind_e.hypot(m5.wind_n)
    ));
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
    timings.insert("weather_wait_s".into(), json!(weather_wait_s));
    timings.insert("weather_fetch_s".into(), json!(weather_fetch_s));
    timings.insert("adsb_s".into(), json!(adsb_s));
    timings.insert("tracks_terrain_s".into(), json!(tracks_terrain_s));
    timings.insert("search_s".into(), json!(search_s));
    timings.insert("curve_match_s".into(), json!(curve_match_s));
    warnings.extend(res.warnings.iter().cloned());
    // the curve match ran and had decoy clocks enough to judge, whether or not it found a place
    // Its narrow clock test says nothing about a trace answer found only after widening.
    let curve_judged = curve
        .as_ref()
        .is_some_and(|r| curve_judges_window(curve_clock, (s_lo, s_hi), r.decoys.len()));
    let curve_source = if a.start.is_some() {
        ClockSource::Start
    } else if curve_from_metadata {
        ClockSource::CreationTime
    } else {
        ClockSource::Dates
    };
    if let Some(r) = &curve
        && let Some(w) = too_few_decoys_warning(
            &curve_params,
            curve_clock,
            r.decoys.len(),
            curve_source,
            dur,
        )
    {
        warnings.push(w);
    }
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
                figure: f.filter(|_| extras).map(|_| format!("{}.png", e.id)),
            }
        })
        .collect();
    let mut found = !res.candidates.is_empty();
    let mut trace_evidence: Option<f64> = None;
    let mut curve_used = CurveCall::Silent;
    let draw_map = |report: &mut ReportInput,
                    warnings: &mut Vec<String>,
                    headline: String,
                    (lat, lon, radius): (f64, f64, f64),
                    markers: Vec<(f64, f64)>| {
        if !extras {
            return;
        }
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
        let evidence = match margin_nats {
            Some(_) => res
                .margin
                .get("margin_top_score")
                .and_then(|v| v.as_f64())
                .unwrap_or(top.score),
            None => top.score,
        };
        trace_evidence = Some(evidence);
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
        let unchecked = leans_on_added(trace_confidence, call, curve_judged, &core_ids, added);
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
        // A high or medium trace answer must lead what the same recording reaches at decoy clocks.
        // Its margin compares explanations inside one clock window; a wrong date or place can
        // still find two chance fits to a recording's strong passes there (nd_o02_1031 on
        // c2c7578, np_o02_p3 without 8be452d's chirp factor), and the decoys measure that level.
        // The window moved is `--start +- --clock-tol`, or the creation_time window the answer
        // came from; the full date window, after a fallback or with neither, compares every clock.
        let decoy_window = match given_start {
            Some(t0) => Some((t0, a.clock_tol_s)),
            None if meta_window => Some((
                0.5 * (archive.clock.0 + archive.clock.1),
                0.5 * (archive.clock.1 - archive.clock.0),
            )),
            None => None,
        };
        let confidence = match decoy_window {
            Some(window) if !decoy && on_trace && matches!(confidence, "high" | "medium") => {
                let t = Instant::now();
                let runs = decoy_clock_runs(
                    a,
                    now,
                    window,
                    start_window(d0, d1, region.utc_offset_h),
                    &run_dir,
                    log,
                    &PreparedRecording {
                        info: &info,
                        duration_s: dur,
                        low_band_warning: low_band.as_deref(),
                        measurements,
                    },
                );
                timings.insert("decoy_clocks_s".into(), json!(t.elapsed().as_secs_f64()));
                let (passes, why) = decoy_gate(evidence, &runs);
                log.say(&format!("decoy clocks: {why}"));
                gates.push(("decoy clocks".to_string(), passes, why));
                if passes { confidence } else { "low" }
            }
            _ => confidence,
        };
        log.say(&format!(
            "winner {lat:.6},{lon:.6} radius {radius:.0} m, {} events, confidence {confidence}",
            top.n_explained
        ));
        let place =
            fdop_geo::geocode::describe_location(net.as_ref().filter(|_| extras), &gaz, lat, lon);
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
        let drawn: &[search::EventMatch] = if on_trace && extras { &top.events } else { &[] };
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
        let place = fdop_geo::geocode::describe_location(
            net.as_ref().filter(|_| extras),
            &gaz,
            v.lat,
            v.lon,
        );
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
    if extras {
        report.figures.insert(
            0,
            (
                "Survey spectrogram (log frequency): detections (blue), measured traces (white)"
                    .into(),
                "survey.png".into(),
            ),
        );
        for e in events {
            if e.fit.is_some() {
                report.figures.push((
                    format!("{} spectrogram, trace and fit", e.id),
                    format!("{}.png", e.id),
                ));
            }
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
        "media": {"duration_s": info.duration_s, "decoded_duration_s": dur, "codec": info.audio_codec, "sample_rate": info.sample_rate, "creation_time": info.creation_time, "audio_stream": info.audio_stream, "audio_channel": info.audio_channel},
        "region_bbox": region.bbox.as_array(),
        "atmosphere": {"source": atmo.describe(), "c_0_1500m": m1.c, "c_0_5000m": m5.c, "wind_0_5000m": [m5.wind_e, m5.wind_n]},
        "search_events": sevs,
        "audio_analysis_reused": recording.is_some(),
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
        trace_evidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_weather(source: &str, t_c: f64, winds_aloft: bool) -> fdop_data::Weather {
        use fdop_types::atmo::{GriddedAtmosphere, standard_levels};
        fdop_data::Weather {
            atmo: Arc::new(
                GriddedAtmosphere::new(
                    vec![0.0],
                    vec![0.0],
                    0.0,
                    3600.0,
                    &[standard_levels(t_c, 0.0, 0.5, 1.0, 2.0)],
                    source.into(),
                )
                .unwrap(),
            ),
            warnings: vec![source.into()],
            winds_aloft,
        }
    }

    #[test]
    fn weather_deadline_keeps_available_surface_profile() {
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(
            tx.send(WeatherUpdate::Fallback(test_weather(
                "cold surface",
                -10.0,
                false,
            )))
            .is_ok()
        );
        // The sender remains alive, representing a blocked radiosonde request.
        let (weather, elapsed) = wait_for_weather(&rx, Duration::ZERO);
        drop(tx);
        let expected = test_weather("cold surface", -10.0, false);
        for z in [0.0, 1000.0, 5000.0] {
            assert_eq!(
                weather.atmo.medium(0.0, 0.0, 0.0, 0.0, z),
                expected.atmo.medium(0.0, 0.0, 0.0, 0.0, z)
            );
        }
        assert!(!weather.winds_aloft);
        assert!(elapsed.is_nan(), "the full fetch has not completed");
        assert_eq!(weather.warnings[0], "cold surface");
        assert!(weather.warnings[1].contains("did not all answer in time"));
        assert!(!weather.warnings.iter().any(|w| w.contains("15 C")));
    }

    #[test]
    fn weather_deadline_keeps_winds_in_either_arrival_order() {
        for surface_first in [true, false] {
            let (tx, rx) = std::sync::mpsc::channel();
            let surface = WeatherUpdate::Fallback(test_weather("surface", -10.0, false));
            let sounding = WeatherUpdate::Fallback(test_weather("soundings", 18.0, true));
            let updates = if surface_first {
                [surface, sounding]
            } else {
                [sounding, surface]
            };
            for update in updates {
                assert!(tx.send(update).is_ok());
            }
            let (weather, _) = wait_for_weather(&rx, Duration::ZERO);
            drop(tx);
            let expected = test_weather("soundings", 18.0, true);
            assert_eq!(weather.atmo.describe(), expected.atmo.describe());
            assert_eq!(
                weather.atmo.medium(0.0, 0.0, 0.0, 0.0, 2000.0),
                expected.atmo.medium(0.0, 0.0, 0.0, 0.0, 2000.0)
            );
            assert!(weather.winds_aloft);
        }
    }

    #[test]
    fn weather_completion_replaces_provisional_profiles() {
        let (tx, rx) = std::sync::mpsc::channel();
        for update in [
            WeatherUpdate::Fallback(test_weather("surface", -10.0, false)),
            WeatherUpdate::Fallback(test_weather("soundings", 18.0, true)),
            WeatherUpdate::Complete(test_weather("combined", 8.0, true), 42.0),
        ] {
            assert!(tx.send(update).is_ok());
        }
        let (weather, elapsed) = wait_for_weather(&rx, Duration::ZERO);
        let expected = test_weather("combined", 8.0, true);
        assert_eq!(weather.atmo.describe(), expected.atmo.describe());
        assert_eq!(
            weather.atmo.medium(0.0, 0.0, 0.0, 0.0, 2000.0),
            expected.atmo.medium(0.0, 0.0, 0.0, 0.0, 2000.0)
        );
        assert_eq!(weather.warnings, ["combined"]);
        assert!(weather.winds_aloft);
        assert_eq!(elapsed, 42.0);
    }

    #[test]
    fn weather_without_profiles_uses_standard_and_reports_why() {
        use fdop_types::atmo::{Atmosphere, StandardAtmosphere};
        for disconnected in [false, true] {
            let (tx, rx) = std::sync::mpsc::channel();
            let tx = (!disconnected).then_some(tx);
            let (weather, elapsed) = wait_for_weather(&rx, Duration::ZERO);
            drop(tx);
            let expected = StandardAtmosphere::new(15.0, 0.0, 0.5);
            assert_eq!(
                weather.atmo.medium(0.0, 0.0, 0.0, 0.0, 2000.0),
                expected.medium(0.0, 0.0, 0.0, 0.0, 2000.0)
            );
            assert!(!weather.winds_aloft);
            assert!(elapsed.is_nan());
            assert!(weather.warnings[0].contains("using a 15 C standard atmosphere"));
            if disconnected {
                assert!(weather.warnings[1].contains("ended before a final result"));
                assert!(!weather.warnings[1].contains("rerun"));
            } else {
                assert!(weather.warnings[1].contains("did not all answer in time"));
            }
        }
    }

    #[test]
    fn weather_disconnect_keeps_a_received_profile() {
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(
            tx.send(WeatherUpdate::Fallback(test_weather(
                "surface", -10.0, false
            )))
            .is_ok()
        );
        drop(tx);
        let (weather, elapsed) = wait_for_weather(&rx, Duration::ZERO);
        let expected = test_weather("surface", -10.0, false);
        assert_eq!(
            weather.atmo.medium(0.0, 0.0, 0.0, 0.0, 2000.0),
            expected.atmo.medium(0.0, 0.0, 0.0, 0.0, 2000.0)
        );
        assert!(!weather.winds_aloft);
        assert!(elapsed.is_nan());
        assert!(weather.warnings[1].contains("ended before a final result"));
    }

    #[test]
    fn decoy_gate_reads_rows_from_real_runs() {
        // c2c7578 on the Mac, xdet/dclock_c2c7578 (LOG 2026-11-08T15:48Z): the wrong date
        // nd_o02_1031 graded high, and right answers with their own four decoy clocks.
        let shifts = [-7_200.0, -3_600.0, 3_600.0, 7_200.0];
        let runs = |e: [Option<f64>; 4]| shifts.iter().copied().zip(e).collect::<Vec<_>>();
        let (pass, why) = decoy_gate(
            18.41,
            &runs([Some(15.99), Some(17.34), Some(14.70), Some(14.99)]),
        );
        assert!(
            !pass && why.contains("vs 17.34") && why.contains("+1.07 nats"),
            "{why}"
        );
        let (pass, why) = decoy_gate(
            26.31,
            &runs([Some(11.69), Some(12.64), Some(11.12), Some(6.31)]),
        );
        assert!(pass && why.contains("+13.67 nats"), "{why}");
        // a decoy where the search found no candidate adds no evidence (a_ref at +2 h)
        let (pass, why) = decoy_gate(29.58, &runs([Some(13.32), Some(11.80), Some(13.56), None]));
        assert!(
            pass && why.contains("+2 h none") && why.contains("vs 13.56"),
            "{why}"
        );
        // the line is the medium margin line, and it is inclusive
        assert!(decoy_gate(DECOY_LEAD_NATS, &runs([None; 4])).0);
        assert!(!decoy_gate(DECOY_LEAD_NATS - 0.01, &runs([None; 4])).0);
        // three decoys cannot clear a high or medium answer, however far it leads
        let (pass, why) = decoy_gate(
            84.81,
            &runs([Some(14.88), Some(14.64), Some(14.91), Some(16.88)])[..3],
        );
        assert!(!pass && why.contains("only 3 of 4"), "{why}");
    }

    #[test]
    fn decoy_runs_move_only_the_start_and_skip_the_curve_match() {
        let mut a = offline_test_args("decoy-args");
        a.curve_match = true;
        a.quiet = false;
        a.start = Some("2026-09-08T20:17:00Z".into());
        let t0 = parse_iso_utc("2026-09-08T20:17:00Z").unwrap();
        let d = decoy_args(&a, t0, a.clock_tol_s, -7_200.0, Path::new("/runs/x"));
        assert_eq!(
            parse_iso_utc(d.start.as_deref().unwrap()).unwrap(),
            parse_iso_utc("2026-09-08T18:17:00Z").unwrap()
        );
        assert_eq!(d.out.as_deref(), Some(Path::new("/runs/x/decoy_clock_-2h")));
        assert!(d.quiet && !d.curve_match);
        assert_eq!(
            (
                &d.media,
                &d.dates,
                &d.near,
                &d.cache,
                d.offline,
                d.clock_tol_s,
                d.max_events,
                d.sr
            ),
            (
                &a.media,
                &a.dates,
                &a.near,
                &a.cache,
                a.offline,
                a.clock_tol_s,
                a.max_events,
                a.sr
            )
        );
        let d = decoy_args(&a, t0, a.clock_tol_s, 3_600.0, Path::new("/runs/x"));
        assert_eq!(d.out.as_deref(), Some(Path::new("/runs/x/decoy_clock_+1h")));
        // A creation_time window (no --start) becomes the same window as a start: its centre
        // moved, and its half-width as the tolerance.
        a.start = None;
        let (lo, hi) = (t0 - 3_720.0, t0 + 120.0);
        let d = decoy_args(
            &a,
            0.5 * (lo + hi),
            0.5 * (hi - lo),
            7_200.0,
            Path::new("/runs/x"),
        );
        let start = parse_iso_utc(d.start.as_deref().unwrap()).unwrap();
        assert_eq!(
            (start - d.clock_tol_s, start + d.clock_tol_s),
            (lo + 7_200.0, hi + 7_200.0)
        );
        assert_eq!(d.out.as_deref(), Some(Path::new("/runs/x/decoy_clock_+2h")));
    }

    #[test]
    fn decoy_shifts_keep_each_clock_window_inside_the_dates() {
        let day = (0.0, 86_400.0);
        let midday = decoy_shifts(43_200.0, day, 120.0);
        assert_eq!(midday.len(), DECOY_STEPS.len());
        assert_eq!(
            midday[..DECOY_CLOCKS],
            [-7_200.0, -3_600.0, 3_600.0, 7_200.0]
        );
        // 00:30: no decoy window may open before the dates, so later clocks stand in
        assert_eq!(
            decoy_shifts(1_800.0, day, 120.0),
            [3_600.0, 7_200.0, 10_800.0, 14_400.0]
        );
        // 23:30
        assert_eq!(
            decoy_shifts(84_600.0, day, 120.0),
            [-7_200.0, -3_600.0, -10_800.0, -14_400.0]
        );
        // the tolerance counts: at 01:01 an hour earlier leaves 60 s, under 120 s
        assert!(!decoy_shifts(3_660.0, day, 120.0).contains(&-3_600.0));
        assert!(decoy_shifts(3_660.0, day, 60.0).contains(&-3_600.0));
        // a one-hour recording's creation_time window (3840 s) steps by 2 h
        assert_eq!(
            decoy_shifts(43_200.0, day, 1_920.0)[..DECOY_CLOCKS],
            [-14_400.0, -7_200.0, 7_200.0, 14_400.0]
        );
    }

    #[test]
    fn decoy_windows_stay_clear_of_the_searched_window() {
        // whole hours, 1 h for any --start window up to 1500 s each side
        assert_eq!(decoy_unit_s(0.0), 3_600.0);
        assert_eq!(decoy_unit_s(120.0), 3_600.0);
        assert_eq!(decoy_unit_s(1_500.0), 3_600.0);
        assert_eq!(decoy_unit_s(1_501.0), 7_200.0);
        assert_eq!(decoy_unit_s(1_920.0), 7_200.0);
        // Metamorphic: whatever the window, every decoy window keeps DECOY_GAP_S from the
        // searched one, so none can hold the true start; and no step is wider than it needs to be.
        let mut half = 0.0;
        while half <= 20_000.0 {
            let unit = decoy_unit_s(half);
            assert!(unit - 2.0 * half >= DECOY_GAP_S, "half {half}: unit {unit}");
            assert!(
                unit == 3_600.0 || unit - 3_600.0 < 2.0 * half + DECOY_GAP_S,
                "half {half}: unit {unit}"
            );
            for s in decoy_shifts(5.0 * 86_400.0, (0.0, 10.0 * 86_400.0), half) {
                assert!(
                    s.abs() - 2.0 * half >= DECOY_GAP_S,
                    "half {half}: shift {s}"
                );
            }
            half += 37.5;
        }
    }

    fn offline_test_args(name: &str) -> LocateArgs {
        let dir = std::env::temp_dir().join(format!(
            "fdop-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        LocateArgs {
            media: dir.join("recording.mka"),
            audio_selection: audio::AudioSelection::default(),
            dates: "2026-09-07..2026-09-09".into(),
            near: "40.8765,-73.5250,20".into(),
            out: Some(dir.join("run")),
            cache: dir.join("cache"),
            offline: true,
            start: None,
            clock_tol_s: 60.0,
            ignore_metadata_time: false,
            max_events: 12,
            curve_match: false,
            sr: 4000,
            quiet: true,
        }
    }

    fn cache_test_day(
        a: &LocateArgs,
        bbox: BBox,
        day: Ymd,
        legs: Vec<fdop_types::store::FlightLeg>,
    ) {
        use fdop_types::store::{StoreMeta, TrackStore};
        let (bbox, key) = fdop_data::fetch::bbox_key(&bbox);
        let start = unix_from_utc(day.0, day.1, day.2, 0, 0, 0.0);
        TrackStore {
            meta: StoreMeta {
                version: 1,
                bbox,
                t_start: start - 3600.0,
                t_end: start + 86_400.0 + 3600.0,
                geoid_n_m: fdop_data::geoid::DEFAULT_GEOID_M,
                sources: vec!["synthetic".into()],
                stats: json!({}),
                created_unix: start,
            },
            legs,
        }
        .save(
            &a.cache
                .join("adsb")
                .join(format!("{:04}-{:02}-{:02}", day.0, day.1, day.2))
                .join(format!("prod+mlatonly_{key}.fdtrk")),
        )
        .unwrap();
    }

    #[test]
    fn offline_metadata_locates_with_only_its_archive_day() {
        offline_location_with_selected_audio(false);
    }

    #[test]
    fn selected_audio_does_not_require_the_longer_videos_unpublished_day() {
        offline_location_with_selected_audio(true);
    }

    fn offline_location_with_selected_audio(long_video: bool) {
        use fdop_types::geo::{Enu, ecef_to_geodetic, haversine_m};
        use fdop_types::store::{FlightLeg, TrackPoint, alt_src};
        let mut a = offline_test_args(if long_video {
            "selected-audio"
        } else {
            "metadata-locate"
        });
        // Generate in exactly the atmosphere an uncached offline run will use. The selftest
        // itself keeps its warm/windy atmosphere and otherwise uses this same synthesis.
        let atmo = fdop_types::atmo::StandardAtmosphere::new(15.0, 0.0, 0.5);
        let fixture =
            crate::selftest::synthesize(a.media.parent().unwrap(), &atmo, &|_| {}).unwrap();
        let mut mux = std::process::Command::new("ffmpeg");
        mux.args(["-nostdin", "-v", "error", "-i"])
            .arg(&fixture.wav);
        let creation = if long_video {
            a.media.set_extension("mkv");
            a.start = Some(iso_utc(fixture.start));
            // Eight sparse black video frames extend into the next UTC day. The actual
            // audio remains the same 1300-second known-answer recording, copied losslessly.
            mux.args([
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=16x16:r=1/3600:d=28800",
                "-map",
                "0:a:0",
                "-map",
                "1:v:0",
                "-c:v",
                "ffv1",
            ]);
            fixture.start + 28_800.0
        } else {
            fixture.start
        };
        let mux = mux
            .args(["-c:a", "copy", "-metadata"])
            .arg(format!("creation_time={}", iso_utc(creation)))
            .arg(&a.media)
            .output()
            .expect("ffmpeg is required by locate and the selftest");
        assert!(
            mux.status.success(),
            "{}",
            String::from_utf8_lossy(&mux.stderr)
        );
        let info = audio::probe(&a.media).unwrap();
        assert_eq!(info.creation_unix, Some(creation));
        if long_video {
            let midnight = unix_from_utc(2026, 9, 9, 0, 0, 0.0);
            assert!(fixture.start + info.duration_s > midnight, "{info:?}");
        }
        let legs = fixture
            .tracks
            .iter()
            .map(|tr| FlightLeg {
                key: tr.key.clone(),
                icao: tr.icao.clone(),
                reg: tr.reg.clone(),
                typ: tr.typ.clone(),
                source: "synthetic".into(),
                engine: tr.engine,
                points: tr
                    .t
                    .iter()
                    .zip(&tr.pos)
                    .zip(&tr.vel)
                    .map(|((&t, &p), &v)| {
                        let (lat, lon, h) = ecef_to_geodetic(p);
                        let vel = Enu::new(lat, lon, 0.0).vec_from_ecef(v);
                        TrackPoint {
                            t,
                            lat,
                            lon,
                            alt_m: h as f32,
                            gs_mps: vel[0].hypot(vel[1]) as f32,
                            track_deg: vel[0].atan2(vel[1]).to_degrees().rem_euclid(360.0) as f32,
                            vrate_mps: vel[2] as f32,
                            alt_src: alt_src::GEOM,
                            flags: 0,
                        }
                    })
                    .collect(),
            })
            .collect();
        let region = parse_region(&a.near, &RegionContext::new(a.cache.clone(), false)).unwrap();
        let bbox = region.bbox.padded_km(30.0);
        cache_test_day(&a, bbox, (2026, 9, 8), legs);
        let (d0, d1) = parse_dates(&a.dates).unwrap();
        let full = PublishedWindow {
            clock: start_window(d0, d1, region.utc_offset_h),
            adsb_end: f64::INFINITY,
            pending: Vec::new(),
        };
        // Prove that the old eager full-date request really cannot run from this cache.
        let full_error = fdop_data::fetch_window(
            &full.track_spec(bbox, fixture.duration, fdop_data::geoid::DEFAULT_GEOID_M),
            &a.cache,
            &fdop_data::FetchOptions {
                offline: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            full_error
                .to_string()
                .contains("2026-09-07 (offline, not cached)")
        );
        let outcome = if long_video {
            let now = parse_iso_utc("2026-09-09T01:00:00Z").unwrap();
            let clock = (fixture.start - a.clock_tol_s, fixture.start + a.clock_tol_s);
            // At this fixed clock the cached day has finished, but the next UTC day has
            // not. The former container-duration request really would reject the input.
            let error = published_start_window(&a, clock, info.duration_s, now, &mut Vec::new())
                .err()
                .expect("the longer container reaches an unpublished day");
            assert!(error.contains("no published day covers"), "{error}");
            locate_with_clock(&a, &Log::new(true), &|| now, false, None).unwrap()
        } else {
            locate(&a, &Log::new(true)).unwrap()
        };
        assert_eq!(
            outcome.report.extra["media"]["decoded_duration_s"],
            json!(fixture.duration)
        );
        assert_eq!(
            outcome.report.extra["media"]["duration_s"],
            json!(info.duration_s)
        );
        assert_eq!(outcome.report.extra["audio_analysis_reused"], json!(false));
        assert!(outcome.found, "{}", outcome.report.answer.verdict);
        let answer = &outcome.report.answer;
        // Metamorphic: moved by hours, the clock lines up no true pass, so the known answer must
        // lead its decoy clocks, whether its clock window came from --start (the long video) or
        // from the file's creation_time (1300 s of audio +- 60 s, so 1 h steps).
        let decoys = answer.gates.iter().find(|g| g.0 == "decoy clocks");
        assert!(
            decoys.is_some_and(|g| g.1),
            "{} {:?}: {}",
            answer.confidence,
            decoys,
            answer.verdict
        );
        // Each decoy writes its result but no figure or map; the answer keeps its figures.
        let mut results = 0;
        for dir in std::fs::read_dir(&outcome.run_dir).unwrap() {
            let dir = dir.unwrap().path();
            if !dir
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("decoy_clock_"))
            {
                continue;
            }
            results += usize::from(dir.join("result.json").is_file());
            let report: serde_json::Value =
                serde_json::from_slice(&std::fs::read(dir.join("result.json")).unwrap()).unwrap();
            assert_eq!(report["extra"]["audio_analysis_reused"], json!(true));
            for stage in ["decode_s", "detect_s", "measure_s"] {
                assert_eq!(report["timings"][stage], json!(0.0), "{stage}");
            }
            for file in ["detections.json", "events.json"] {
                assert_eq!(
                    std::fs::read(dir.join(file)).unwrap(),
                    std::fs::read(outcome.run_dir.join(file)).unwrap(),
                    "a shifted clock must retain the same {file}"
                );
            }
            let images: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|f| f.unwrap().path())
                .filter(|p| p.extension().is_some_and(|x| x == "png" || x == "svg"))
                .collect();
            assert!(images.is_empty(), "{}: {images:?}", dir.display());
        }
        assert_eq!(results, DECOY_CLOCKS);
        assert!(outcome.run_dir.join("survey.png").is_file());

        if long_video {
            // A borrowed recording must also reproduce a nonempty, correct search. The four
            // shifted clocks above have no true flights, so they alone cannot prove this.
            let pcm =
                audio::decode_with_selection(&a.media, 0.0, None, a.sr, a.audio_selection).unwrap();
            let measurements = measure_recording(&pcm, a.sr, &Log::new(true), None).unwrap();
            let low_band = low_band_warning(&pcm, a.sr);
            let recording = PreparedRecording {
                info: &info,
                duration_s: pcm.len() as f64 / a.sr as f64,
                low_band_warning: low_band.as_deref(),
                measurements: &measurements,
            };
            drop(pcm);
            let mut replay = a.clone();
            replay.out = Some(a.media.parent().unwrap().join("reused-audio-control"));
            let reused =
                locate_with_clock(&replay, &Log::new(true), &unix_now, true, Some(&recording))
                    .unwrap();
            assert!(reused.found, "{}", reused.report.answer.verdict);
            assert_eq!(reused.trace_evidence, outcome.trace_evidence);
            for file in ["detections.json", "events.json", "search.json"] {
                assert_eq!(
                    std::fs::read(reused.run_dir.join(file)).unwrap(),
                    std::fs::read(outcome.run_dir.join(file)).unwrap(),
                    "fresh and reused audio must produce identical {file}"
                );
            }
            assert_eq!(
                reused.report.extra["search_events"],
                outcome.report.extra["search_events"]
            );
            assert_eq!(reused.report.atmosphere, outcome.report.atmosphere);

            // Conversely, a fresh shifted run must equal the shared decoy, including its
            // clock-specific catalogue, atmosphere and empty search result.
            let mut fresh_decoy =
                decoy_args(&a, fixture.start, a.clock_tol_s, -7_200.0, &outcome.run_dir);
            let shared_dir = fresh_decoy.out.clone().unwrap();
            fresh_decoy.out = Some(a.media.parent().unwrap().join("fresh-decoy-control"));
            let fresh =
                locate_with_clock(&fresh_decoy, &Log::new(true), &unix_now, true, None).unwrap();
            assert_eq!(
                std::fs::read(fresh.run_dir.join("search.json")).unwrap(),
                std::fs::read(shared_dir.join("search.json")).unwrap()
            );
            let shared: serde_json::Value =
                serde_json::from_slice(&std::fs::read(shared_dir.join("result.json")).unwrap())
                    .unwrap();
            assert_eq!(
                fresh.report.extra["search_events"],
                shared["extra"]["search_events"]
            );
            assert_eq!(json!(fresh.report.atmosphere), shared["atmosphere"]);
            assert_eq!(json!(fresh.report.warnings), shared["warnings"]);
        }
        let error = haversine_m(
            answer.lat,
            answer.lon,
            fixture.observer.0,
            fixture.observer.1,
        );
        assert!(
            error <= answer.radius_m.max(100.0),
            "{error} m, radius {} m",
            answer.radius_m
        );
        let best = &outcome.report.candidates[0];
        let clock = parse_iso_utc(&best.recording_start_utc).unwrap();
        assert!(
            (clock - fixture.start).abs() < 2.0,
            "clock offset {} s",
            clock - fixture.start
        );
        // Unlike the direct selftest, this path reloads a merged day store, whose flight
        // keys are canonical {lowercase ICAO}_{UTC date}_{leg number} identities.
        for flight in ["a1dfeb_20260908_0", "ace391_20260908_0"] {
            assert!(
                best.matches.iter().any(|m| m.flight == flight),
                "missing {flight}"
            );
        }
        assert!(
            outcome.report.extra["window"]
                .as_str()
                .unwrap()
                .contains(if long_video {
                    "--start"
                } else {
                    "creation_time"
                })
        );
        assert!(
            !outcome
                .report
                .warnings
                .iter()
                .any(|w| w.contains("full date window"))
        );
    }

    #[test]
    fn unavailable_metadata_tries_published_dates_but_explicit_clocks_do_not() {
        let a = offline_test_args("metadata-pending");
        let bbox = BBox::new(40.8, -73.6, 40.9, -73.5);
        cache_test_day(&a, bbox, (2026, 9, 8), Vec::new());
        let lo = parse_iso_utc("2026-09-08T12:00:00Z").unwrap();
        let hint = parse_iso_utc("2026-09-10T12:00:00Z").unwrap();
        let now = parse_iso_utc("2026-09-10T01:00:00Z").unwrap();
        let mut warnings = Vec::new();
        let (w, metadata) = first_published_window(
            &a,
            (lo, hint + 60.0),
            Some((hint - 60.0, hint + 60.0)),
            600.0,
            now,
            &mut warnings,
        )
        .unwrap();
        assert!(!metadata);
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("creation_time window could not be searched"))
        );
        let opts = fdop_data::FetchOptions {
            offline: true,
            ..Default::default()
        };
        let store =
            fdop_data::fetch_window(&w.track_spec(bbox, 600.0, -30.0), &a.cache, &opts).unwrap();
        assert_eq!(store.meta.stats["days"], json!(["2026-09-08"]));
        assert!(
            first_published_window(
                &a,
                (hint - 60.0, hint + 60.0),
                None,
                600.0,
                now,
                &mut Vec::new(),
            )
            .is_err(),
            "an explicit clock cannot widen to the earlier cached day"
        );
        // Required midnight coverage remains strict even though the next day is not cached.
        let late = parse_iso_utc("2026-09-08T23:50:00Z").unwrap();
        let w = PublishedWindow {
            clock: (late - 60.0, late + 60.0),
            adsb_end: f64::INFINITY,
            pending: Vec::new(),
        };
        let error = fdop_data::fetch_window(&w.track_spec(bbox, 3600.0, -30.0), &a.cache, &opts)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("2026-09-09 (offline, not cached)")
        );
    }

    #[test]
    fn empty_metadata_search_fetches_the_wider_dates() {
        let a = offline_test_args("metadata-empty");
        std::fs::create_dir_all(a.media.parent().unwrap()).unwrap();
        let mux = std::process::Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=4000:cl=mono",
                "-t",
                "12",
                "-c:a",
                "pcm_s16le",
                "-metadata",
                "creation_time=2026-09-08T19:16:40Z",
            ])
            .arg(&a.media)
            .output()
            .expect("ffmpeg is required by locate");
        assert!(
            mux.status.success(),
            "{}",
            String::from_utf8_lossy(&mux.stderr)
        );
        let region = parse_region(&a.near, &RegionContext::new(a.cache.clone(), false)).unwrap();
        cache_test_day(&a, region.bbox.padded_km(30.0), (2026, 9, 8), Vec::new());
        let log = Log::new(true);
        let error = locate(&a, &log).unwrap_err();
        assert!(
            error.contains("2026-09-07 (offline, not cached)"),
            "{error}"
        );
        assert!(
            log.lines()
                .iter()
                .any(|s| s.contains("widening to the full date window"))
        );
    }

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
    fn metadata_uses_decoded_duration_and_keeps_container_end_tags() {
        let full = (0.0, 20_000.0);
        let mut info = audio::MediaInfo {
            creation_unix: Some(10_000.0),
            duration_s: 7200.0,
            ..Default::default()
        };
        // A container-end tag must still include the real start when audio is shorter.
        assert_eq!(
            media_metadata_start_window(full, &info, 1300.0, 60.0),
            Some((2740.0, 10_060.0))
        );
        // Missing, understated and nonfinite probe values cannot omit the decoded start.
        for duration in [0.0, 12.0, -1.0, f64::NAN, f64::INFINITY] {
            info.duration_s = duration;
            assert_eq!(
                media_metadata_start_window(full, &info, 1300.0, 60.0),
                Some((8640.0, 10_060.0)),
                "container duration {duration}"
            );
        }
        info.creation_unix = None;
        assert!(media_metadata_start_window(full, &info, 1300.0, 60.0).is_none());
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
    fn a_curve_match_short_of_decoys_says_why_and_what_would_judge() {
        let p = curvematch::MatchParams::default();
        let need = curvematch::MIN_DECOYS;
        let warn = |clock, n, source| too_few_decoys_warning(&p, clock, n, source, 600.0);
        assert!(warn((1000.0, 1240.0), need, ClockSource::Start).is_none());
        // +-180 s around --start: the old default
        let w = warn((1000.0, 1360.0), 34, ClockSource::Start).unwrap();
        assert!(w.contains("34 decoy clocks"), "{w}");
        assert!(w.contains("360 s clock window"), "{w}");
        assert!(w.contains("--clock-tol 150 or less with --start"), "{w}");
        // a creation_time window holds the recording's length too
        let w = warn((1000.0, 1840.0), 16, ClockSource::CreationTime).unwrap();
        assert!(w.contains("creation_time window is 840 s wide"), "{w}");
        assert!(!w.contains("--clock-tol"), "{w}");
        // a date window clipped by the archive is neither of those
        let w = warn((1000.0, 2500.0), 8, ClockSource::Dates).unwrap();
        assert!(w.contains("1500 s clock window"), "{w}");
        assert!(!w.contains("creation_time"), "{w}");
        // a window narrow enough, short only because its UTC days or the archive end
        let w = warn((1000.0, 1240.0), 30, ClockSource::Start).unwrap();
        assert!(w.contains("leave no room"), "{w}");
        // limits that are not whole seconds are rounded down, and the width up, so the advice
        // stays inside the line: 7230 / 20 - 60 = 301.5 s, a tolerance of 150.75 s
        let odd = curvematch::MatchParams {
            decoy_span_s: 7230.0,
            ..Default::default()
        };
        assert_eq!(curvematch::max_judged_window_s(&odd), 301.5);
        let w =
            too_few_decoys_warning(&odd, (1000.0, 1301.75), 38, ClockSource::Start, 600.0).unwrap();
        assert!(w.contains("302 s clock window"), "{w}");
        assert!(w.contains("one of 301 s or less"), "{w}");
        assert!(w.contains("--clock-tol 150 or less"), "{w}");
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

    #[test]
    fn metadata_weather_brackets_curve_decoys_across_the_sounding_boundary() {
        let day = unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let p = curvematch::MatchParams::default();
        let bbox = BBox::new(40.8, -73.6, 40.9, -73.5);
        let duration = 120.0;
        // A 13:05 creation time gives [13:02, 13:06]. Its 48 decoys include 11:02,
        // which needs the 00Z sounding as well as 12Z, not a clamp to the noon profile.
        let clock = metadata_start_window(
            (day, day + 86_400.0),
            day + (13.0 * 60.0 + 5.0) * 60.0,
            duration,
            60.0,
        )
        .unwrap();
        let w = PublishedWindow {
            clock,
            adsb_end: f64::INFINITY,
            pending: Vec::new(),
        };
        let catalog = curve_catalog_span(&p, clock, duration, w.adsb_end);
        assert_eq!(
            catalog,
            (
                day + (10.0 * 60.0 + 42.0) * 60.0,
                day + (15.0 * 60.0 + 28.0) * 60.0
            )
        );
        let weather = w.weather_span(bbox, duration, Some(&p));
        assert_eq!(weather, (catalog.0 - 3600.0, catalog.1 + 3600.0));
        let trace_only = w.weather_span(bbox, duration, None);
        assert_eq!(trace_only, (clock.0 - 3600.0, clock.1 + duration + 3600.0));
        assert_eq!((trace_only.0 / 43_200.0).floor() * 43_200.0, day + 43_200.0);
        assert_eq!((weather.0 / 43_200.0).floor() * 43_200.0, day);
        assert!(
            weather.0 > day && weather.1 < day + 86_400.0,
            "not the whole requested date range"
        );
        // Neither disabling curve matching nor exceeding either eligibility limit should
        // request decoy weather for a matcher that will not run.
        let wide = PublishedWindow {
            clock: (clock.0, clock.0 + 1801.0),
            adsb_end: w.adsb_end,
            pending: Vec::new(),
        };
        assert_eq!(
            wide.weather_span(bbox, duration, Some(&p)),
            wide.weather_span(bbox, duration, None)
        );
        let large = BBox::new(40.0, -74.0, 42.0, -72.0);
        assert_eq!(w.weather_span(large, duration, Some(&p)), trace_only);
    }

    #[test]
    fn curve_weather_covers_one_sided_decoys_at_archive_edges() {
        let day = unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let p = curvematch::MatchParams::default();
        let bbox = BBox::new(40.8, -73.6, 40.9, -73.5);
        // Near midnight the missing earlier decoys move later; near publication's end,
        // later decoys move earlier. Expected catalogues include the existing track padding.
        for (clock, end, expected) in [
            (
                (day + 30.0 * 60.0, day + 34.0 * 60.0),
                f64::INFINITY,
                (day, day + (4.0 * 60.0 + 31.0) * 60.0),
            ),
            (
                (
                    day + (23.0 * 60.0 + 10.0) * 60.0,
                    day + (23.0 * 60.0 + 14.0) * 60.0,
                ),
                day + 86_400.0 - 1.0,
                (day + (19.0 * 60.0 + 30.0) * 60.0, day + 86_400.0 - 1.0),
            ),
        ] {
            let w = PublishedWindow {
                clock,
                adsb_end: end,
                pending: Vec::new(),
            };
            let catalog = curve_catalog_span(&p, clock, 120.0, end);
            assert_eq!(catalog, expected);
            assert_eq!(
                w.weather_span(bbox, 120.0, Some(&p)),
                (expected.0 - 3600.0, expected.1 + 3600.0)
            );
            assert!(catalog.1 <= end);
        }
    }

    #[test]
    fn late_curve_match_uses_one_cached_day_and_keeps_all_decoys() {
        use fdop_types::store::{FlightLeg, TrackPoint, alt_src};
        let mut a = offline_test_args("late-curve-catalog");
        a.curve_match = true;
        let day = unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let midnight = day + 86_400.0;
        let clock = (day + 23.0 * 3600.0 + 600.0, day + 23.0 * 3600.0 + 840.0);
        let duration = 120.0;
        let (lat, lon) = (40.876466, -73.524951);
        let bbox = BBox::new(lat - 0.001, lon - 0.001, lat + 0.001, lon + 0.001);
        let fetch_box = bbox.padded_km(30.0);
        let leg = FlightLeg {
            key: "synthetic-evening".into(),
            icao: "000001".into(),
            reg: String::new(),
            typ: "C172".into(),
            source: "synthetic".into(),
            engine: fdop_types::engine::EngineClass::Piston,
            points: [(clock.0 + 80.0, lon - 0.01), (clock.0 + 100.0, lon + 0.01)]
                .into_iter()
                .map(|(t, lon)| TrackPoint {
                    t,
                    lat,
                    lon,
                    alt_m: 1000.0,
                    gs_mps: f32::NAN,
                    track_deg: f32::NAN,
                    vrate_mps: f32::NAN,
                    alt_src: alt_src::GEOM,
                    flags: 0,
                })
                .collect(),
        };
        cache_test_day(&a, fetch_box, (2026, 9, 8), vec![leg]);
        let (_, key) = fdop_data::fetch::bbox_key(&fetch_box);
        let cache_path = a
            .cache
            .join("adsb/2026-09-08")
            .join(format!("prod+mlatonly_{key}.fdtrk"));
        // A real partial-feed cache makes fetch_window generate its coverage warning.
        std::fs::rename(
            &cache_path,
            cache_path.with_file_name(format!("prod_{key}.fdtrk")),
        )
        .unwrap();
        // Only the spectral bank is reduced; production clock and decoy parameters stay
        // unchanged. This runs the actual cache fetch, track builder and curve matcher.
        let p = curvematch::MatchParams {
            v_grid: vec![84.0],
            d_grid: vec![1000.0],
            f0_lo: 100.0,
            f0_hi: 105.0,
            ..Default::default()
        };
        let catalog = curve_catalog_span(&p, clock, duration, f64::INFINITY);
        assert_eq!(catalog, (day + 19.5 * 3600.0, midnight.next_down()));
        assert!(catalog.0 <= clock.0 - 1200.0);
        assert!(catalog.1 >= clock.1 + duration + 1200.0);
        // The old inclusive midnight limit really requires an unavailable second store.
        let error = fdop_data::fetch_window(
            &fdop_data::IngestSpec {
                bbox: fetch_box,
                t_start: catalog.0,
                t_end: midnight,
                geoid_n_m: -30.0,
                keep_unknown: true,
            },
            &a.cache,
            &fdop_data::FetchOptions {
                offline: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("2026-09-09 (offline, not cached)")
        );
        let image = curvematch::build_image(&vec![0.0; 120 * 4000], 4000, &p).unwrap();
        let dem = fdop_types::FlatElevation(0.0);
        let atmo = fdop_types::atmo::StandardAtmosphere::new(15.0, 0.0, 0.5);
        let mut warnings = vec!["prior pipeline warning".into()];
        let result = run_curve_match(
            &a,
            &p,
            &image,
            bbox,
            fetch_box,
            clock,
            duration,
            f64::INFINITY,
            -30.0,
            &dem,
            &dem,
            &atmo,
            &Log::new(true),
            &mut warnings,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            warnings,
            [
                "prior pipeline warning",
                "curve match ADS-B 2026-09-08: no mlatonly release, so aircraft located only by multilateration (transponders without ADS-B Out) are missing"
            ]
        );
        assert_eq!(result.n_tracks, 1);
        assert_eq!(result.decoys.len(), 48);
        let mut shifts: Vec<_> = result.decoys.iter().map(|d| d.shift_s).collect();
        shifts.sort_by(f64::total_cmp);
        assert_eq!(
            shifts,
            (-40..=8)
                .filter(|&k| k != 0)
                .map(|k| k as f64 * 300.0)
                .collect::<Vec<_>>()
        );
        for shift in std::iter::once(0.0).chain(shifts) {
            // catalogue_span clips its preferred 1200 s padding at day boundaries;
            // every real/decoy recording must still retain its required 60 s guard.
            assert!(
                clock.0 + shift - catalog.0 >= 60.0,
                "start at shift {shift}"
            );
            assert!(
                catalog.1 - (clock.1 + shift + duration) >= 60.0,
                "end at shift {shift}"
            );
        }
    }

    #[test]
    fn curve_catalog_keeps_required_midnight_timestamps_and_crossings() {
        let a = offline_test_args("curve-midnight-coverage");
        let day = unix_from_utc(2026, 9, 8, 0, 0, 0.0);
        let midnight = day + 86_400.0;
        let bbox = BBox::new(40.8, -73.6, 40.9, -73.5);
        cache_test_day(&a, bbox, (2026, 9, 8), Vec::new());
        let p = curvematch::MatchParams::default();
        let duration = 120.0;
        let hi_at_midnight_padding = midnight - duration - 1200.0;
        for (hi, needs_next_day) in [
            (hi_at_midnight_padding.next_down(), false),
            (hi_at_midnight_padding, true),
            (hi_at_midnight_padding.next_up(), true),
            // The recording itself crosses midnight, independent of the extra padding.
            (midnight + 180.0, true),
        ] {
            let clock = (hi - 240.0, hi);
            let catalog = curve_catalog_span(&p, clock, duration, f64::INFINITY);
            assert!(catalog.0 <= clock.0 - 1200.0);
            assert!(catalog.1 >= clock.1 + duration + 1200.0);
            let result = fdop_data::fetch_window(
                &fdop_data::IngestSpec {
                    bbox,
                    t_start: catalog.0,
                    t_end: catalog.1,
                    geoid_n_m: -30.0,
                    keep_unknown: true,
                },
                &a.cache,
                &fdop_data::FetchOptions {
                    offline: true,
                    ..Default::default()
                },
            );
            if needs_next_day {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("2026-09-09 (offline, not cached)")
                );
            } else {
                assert_eq!(result.unwrap().meta.stats["days"], json!(["2026-09-08"]));
            }
        }
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
    /// survey image; anything else, including contrast finds and harmonic recoveries, is added.
    #[test]
    fn contrast_and_harmonic_recoveries_are_measured_past_the_old_line() {
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
        let primary_only = |rank, d: Detection| past_the_old_line(rank, &d, &d, 24, 4.5);
        assert!(!primary_only(0, det(6.0, false)));
        assert!(!primary_only(23, det(4.5, false)));
        assert!(primary_only(24, det(9.0, false)));
        assert!(primary_only(0, det(4.4, false)));
        assert!(primary_only(0, det(9.0, true)));
        let mut fast = det(9.0, false);
        fast.v = detect::PRIMARY_MAX_SPEED_MPS;
        assert!(!primary_only(0, fast.clone()));
        fast.v += 1.0;
        // Provenance belongs to the coarse seed even if refinement later lowers its speed.
        assert!(past_the_old_line(0, &fast, &det(9.0, false), 24, 4.5));

        let primary = det(9.0, false);
        let mut measured = primary.clone();
        // Refinement replaces the survey score with a different statistic. A normal primary
        // is still judged by its original score, not the refined one.
        measured.score = 1.0;
        assert!(!past_the_old_line(0, &primary, &measured, 24, 4.5));
        measured.id = 3;
        measured.harmonic_of = Some(primary.id);
        let added = if past_the_old_line(0, &primary, &measured, 24, 4.5) {
            ["e1".to_string()].into()
        } else {
            std::collections::HashSet::new()
        };
        assert!(leans_on_added(
            "medium",
            CurveCall::Silent,
            false,
            &["e1"],
            &added
        ));
        assert!(!leans_on_added(
            "medium",
            CurveCall::Agrees,
            true,
            &["e1"],
            &added
        ));
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
