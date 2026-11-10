//! Event measurement: turn a survey detection into a fitted, quality-gated Doppler event.
//!
//! 1. Adaptive STFT around the detection: window from the expected maximum chirp rate
//!    (win ~ 1/sqrt(RoC), at least 0.5 s), hop 0.1 s, bins <= 0.25 Hz, band from the detection's
//!    asymptotes;
//!    rows median-normalised over a wide context (+-`context_s`) so the pass itself does not
//!    raise its own noise floor.
//! 2. Model-guided ridge trace: at every frame, choose the nearest qualifying local peak within
//!    `band` Hz of the current model curve (integrating along the model's local slope), so a
//!    louder neighbouring ridge does not win solely on strength. A frame too steep for that
//!    window is read from a shorter one instead, integrating along the curve.
//! 3. Retarded-time constant-velocity fit of the trace; the fitted curve becomes the next model
//!    and the band shrinks to a few fit-RMS. Three rounds.
//! 4. Quality gates: points, coverage of both sides of the CPA, a ridge through the fitted drop,
//!    fit RMS, plausible v and d.
//! 5. A fit the gates refuse because its trace held one side of the pass is followed afresh once:
//!    from its own trace, the ridge is traced without the model, fitted, and the guided rounds
//!    start again from that fit. The second attempt replaces the first only if every gate accepts it.
//!    If that still fails on a steep ridge fitted beyond the primary detector's speed bank, and
//!    its only missing gate is side coverage or the central drop, the original fit gets one
//!    bounded continuation. A missing side can renew its spectral band; a missing drop keeps
//!    its fitted band and original spectrogram. It completes the ordinary fitting rounds, then continues only
//!    while the retained ridge through the CPA gains side coverage. Every gate and both sides of
//!    that continuous ridge must pass; a failed continuation leaves the original refusal unchanged.

use crate::detect::{Detection, PRIMARY_MAX_SPEED_MPS};
use crate::fit::{Best, FitResult, PreparedFit, Side, prepare_constant_velocity};
use crate::physics::{cv_curve, readouts, window_for_roc};
use crate::spectro::{Spec, stft_band_db};
use crate::trace::{
    Trace, TraceParams, guided_raw_ridge_peak, guided_ridge_peak, guided_ridge_peak_with_exclusion,
    ridge_trace,
};
use serde::Serialize;

/// At most this many independent aircraft-like events enter the search, with primary detections
/// measured strongest first. Repeated seeds of one pass and non-aircraft fits do not use a place.
/// More usable events from a long recording can promote a wrong answer (CHANGELOG, Unreleased), so
/// the cap stays until the grading accounts for how many events were searched. A detection the gates
/// refuse does not use up a place either: [`measure_ranked_counted`] goes on down the list.
pub const MAX_MEASURED: usize = 24;

/// Measures ranked detections, strongest first, until `max_ok` of them pass the quality gates or
/// all `n` are measured, and returns every measured item in rank order.
///
/// `batch` measures the detections of a range of ranks, in any order internally (the pipeline
/// runs each batch in parallel), and returns them in rank order; `ok` says whether an item passed.
/// Each batch holds as many detections as passes are still missing. So the first batch is the
/// strongest `max_ok`, exactly what a fixed cap of `max_ok` measured, and no batch can take the
/// number of passing items past `max_ok`. The result does not depend on how a batch is scheduled.
pub fn measure_ranked<T>(
    n: usize,
    max_ok: usize,
    mut batch: impl FnMut(std::ops::Range<usize>) -> Vec<T>,
    ok: impl Fn(&T) -> bool,
) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    let mut n_ok = 0;
    while n_ok < max_ok && out.len() < n {
        let start = out.len();
        let end = (start + max_ok - n_ok).min(n);
        let got = batch(start..end);
        assert_eq!(got.len(), end - start, "a batch returns one item per rank");
        n_ok += got.iter().filter(|t| ok(t)).count();
        out.extend(got);
    }
    out
}

/// Measure a ranked prefix until `count` finds at least `max_ok` usable, independent events.
/// Unlike [`measure_ranked`], the count can compare every measured item to its neighbours, so
/// duplicate passes and otherwise unusable fits do not consume slots needed by later flyovers.
/// All measured items remain in their original rank order, including rejected and duplicate ones.
///
/// A batch holds only the number of places still missing. Grouping can nevertheless change when
/// a better representative arrives: its count need not be monotonic, and can jump over the cap.
/// The caller must apply `max_ok` to its final selected events, reporting any such cut separately.
pub fn measure_ranked_counted<T>(
    n: usize,
    max_ok: usize,
    mut batch: impl FnMut(std::ops::Range<usize>) -> Vec<T>,
    count: impl Fn(&[T]) -> usize,
) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    let mut n_ok = 0;
    while n_ok < max_ok && out.len() < n {
        let start = out.len();
        let end = (start + max_ok - n_ok).min(n);
        let got = batch(start..end);
        assert_eq!(got.len(), end - start, "a batch returns one item per rank");
        out.extend(got);
        n_ok = count(&out);
    }
    out
}

#[derive(Clone, Debug)]
pub struct MeasureParams {
    pub c: f64,
    pub hop_s: f64,
    pub df_max: f64,
    /// Row-normalisation context on each side of the event (s).
    pub context_s: f64,
    pub min_snr: f64,
    pub min_z: f64,
    pub min_points: usize,
    /// Minimum traced span on each side of the CPA (s).
    pub min_side_s: f64,
    /// Least `EventQuality::drop_traversed` of an event that counts.
    pub min_drop_traversed: f64,
    pub max_rmse_hz: f64,
    pub rounds: usize,
    /// Follow the ridge of a fit the gates refuse without the model, and measure again from
    /// what that finds ([`reseeded_rounds`]). If that fails, a steep fit beyond the primary speed
    /// bank whose sole refusal is side coverage or the central drop can continue from its fitted
    /// spectral geometry.
    pub free_reseed: bool,
}

impl Default for MeasureParams {
    fn default() -> Self {
        Self {
            c: fdop_types::C_SOUND,
            hop_s: 0.1,
            df_max: 0.25,
            context_s: 600.0,
            min_snr: 3.0,
            min_z: 4.0,
            min_points: 80,
            min_side_s: 8.0,
            min_drop_traversed: 0.15,
            max_rmse_hz: 1.5,
            rounds: 3,
            free_reseed: true,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct EventQuality {
    pub n_points: usize,
    pub span_approach_s: f64,
    pub span_recede_s: f64,
    /// Net fall of the trace's continuous stretches, as a share of the central 80 % of the fitted
    /// frequency drop. Falls of a wandering tone add up, so it can exceed 1.
    pub drop_traversed: f64,
    pub median_snr_db: f64,
    pub rmse_hz: f64,
    /// Ranking value: median SNR (dB) x sqrt(effective independent points).
    pub score: f64,
    pub ok: bool,
    /// The detection's curve was refused and this event was recovered from its observed ridge:
    /// by free tracing, or a bounded guided continuation of a fast fitted ridge with renewed
    /// spectral support ([`MeasureParams::free_reseed`]). This remains the same detected event.
    pub reseeded: bool,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MeasuredEvent {
    pub id: String,
    pub detection: Detection,
    pub fit: Option<FitResult>,
    pub trace: Trace,
    pub quality: EventQuality,
    /// STFT window used (s).
    pub win_s: f64,
    #[serde(skip)]
    pub spec: Option<Spec>,
}

impl MeasuredEvent {
    pub fn t_rec(&self) -> f64 {
        self.fit.as_ref().map_or(self.detection.t0r, |f| f.best.t0r)
    }

    /// The search's existing aircraft bounds, in addition to the measurement gates. A
    /// quality-passing fit to a nearby slow source must not consume an aircraft's place.
    pub fn is_aircraft_like(&self) -> bool {
        self.quality.ok
            && self
                .fit
                .as_ref()
                .is_some_and(|f| aircraft_geometry(&f.best))
    }
}

fn aircraft_geometry(b: &Best) -> bool {
    b.v >= 28.0 && b.d >= 200.0
}

fn model_fn(f0: f64, v: f64, d: f64, t0r: f64, c: f64) -> impl Fn(f64) -> f64 {
    let t0e = t0r - d / c;
    move |t| cv_curve(t, f0, v, d, t0e, c)
}

/// Shortest window (s) of an event's main spectrogram. A shorter one's main lobe would fill the
/// noise reference around the steady sides of a pass. A chirp above 1 / win^2 (4 Hz/s here) smears
/// out of this window, so a steeper pass adds a spectrogram of its own for its steep frames.
const MIN_MAIN_WIN_S: f64 = 0.5;

/// The spectral window must hold the largest received chirp, which occurs at
/// t0e, d/c before the heard CPA. Readouts deliberately reports the CPA rate;
/// using that smaller rate can omit the short spectrogram of a fast pass.
fn peak_received_chirp(f0: f64, v: f64, d: f64, c: f64) -> f64 {
    readouts(f0, v, d, c).max_roc * (1.0 - (v / c).powi(2)).powf(-1.5)
}

/// Time error (s) the guided trace allows for in a steep frame. With it a steep pass is followed
/// when its detection rates the chirp half as steep, about as far as real detections were seen to
/// be off on passes traced well. A detection four times too gentle can lose the CPA frames and be
/// refused, and at eight times most are.
const STEEP_TIME_ERROR_S: f64 = 0.25;

/// A longer silence ends a continuous observed ridge. Use the same gap for the frequency-drop
/// gate and for the retained ridge through the CPA in a bounded continuation.
const MAX_RIDGE_GAP_S: f64 = 3.0;

/// Share of a fit's frequency drop that the trace travels through. A pass's ridge moves from the
/// approach tone to the recede tone and is loudest on the way. Two steady tones joined by a fit do
/// not: the trace sits flat on each, and the fit crosses the empty space between them with points
/// on both sides of the CPA, a low RMS and a plausible speed. Within the central 80 % of the drop,
/// this adds up the net fall of each continuous stretch of the trace (`t` ascending) and divides by
/// the model's fall there. A trace hopping down from tone to tone earns nothing for its hops, but
/// tones spaced within `MAX_STEP_OFF_MODEL_HZ` of each other can still pass for a ridge, and a tone
/// that keeps falling and jumping back earns each fall.
fn drop_traversed(
    t: &[f64],
    f: &[f64],
    f_pre: f64,
    f_post: f64,
    model: impl Fn(f64) -> f64,
) -> f64 {
    /// A step this far from the model's own step is a move to another ridge, and ends a stretch.
    /// The trace takes the peak nearest the model in each frame with no limit on continuity, so
    /// neighbouring peaks can differ from the model's step by up to twice the band. That is this
    /// limit at the 1 Hz band of a clean main-window fit. A wider band can step further, as can any
    /// steep frame (0.25 s of chirp is 5 Hz at 20 Hz/s), and then a real ridge reads low; on
    /// synthetic passes this limit changed none.
    const MAX_STEP_OFF_MODEL_HZ: f64 = 2.0;
    const MIN_STRETCH_POINTS: usize = 5;
    let net_fall = |stretch: &[f64]| {
        if stretch.len() < MIN_STRETCH_POINTS {
            return 0.0;
        }
        let k = (stretch.len() / 4).clamp(2, 10);
        let start = crate::spectro::median_f64(&mut stretch[..k].to_vec());
        let end = crate::spectro::median_f64(&mut stretch[stretch.len() - k..].to_vec());
        (start - end).max(0.0)
    };
    let drop = f_pre - f_post;
    if !drop.is_finite() || drop <= 0.0 {
        return 0.0;
    }
    let (mut fell, mut stretch, mut last_t, mut last_off) =
        (0.0, Vec::new(), f64::NEG_INFINITY, 0.0);
    for (&ti, &fi) in t.iter().zip(f) {
        let fitted = model(ti);
        if !(0.1..=0.9).contains(&((f_pre - fitted) / drop)) {
            continue;
        }
        let off = fi - fitted;
        if ti - last_t > MAX_RIDGE_GAP_S || (off - last_off).abs() > MAX_STEP_OFF_MODEL_HZ {
            fell += net_fall(&stretch);
            stretch.clear();
        }
        stretch.push(fi);
        last_t = ti;
        last_off = off;
    }
    fell += net_fall(&stretch);
    fell / (0.8 * drop)
}

/// Trace the ridge within `band` Hz of `model` over frames whose time lies in [ta, tb]. A frame
/// whose chirp `spec`'s window cannot hold is read from `steep` if there is one: its shorter window
/// keeps the ridge narrow, and integrating along the curve stays on a ridge that leaves any tangent
/// within the integration length.
#[allow(clippy::too_many_arguments)]
pub fn guided_trace(
    spec: &Spec,
    steep: Option<&Spec>,
    model: &(dyn Fn(f64) -> f64 + Sync),
    ta: f64,
    tb: f64,
    band: f64,
    min_snr: f64,
    min_z: f64,
) -> Trace {
    guided_trace_with_background(
        spec,
        None,
        steep.map(|short| (short, None)),
        model,
        ta,
        tb,
        band,
        min_snr,
        min_z,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn guided_trace_with_background(
    spec: &Spec,
    background: Option<&[f32]>,
    steep: Option<(&Spec, Option<&[f32]>)>,
    model: &(dyn Fn(f64) -> f64 + Sync),
    ta: f64,
    tb: f64,
    band: f64,
    min_snr: f64,
    min_z: f64,
    raw_only: bool,
) -> Trace {
    let nt = spec.nt();
    let hop = spec.dt();
    let half = ((1.5 / hop).round() as usize).max(1);
    let noise_band = (4.0 * band).max(8.0);
    let held_chirp = 1.0 / (spec.meta.win_s * spec.meta.win_s);
    let rows: Vec<Option<(f64, f64, f64)>> = {
        use rayon::prelude::*;
        (0..nt)
            .into_par_iter()
            .map(|j| {
                let t = spec.t[j];
                if t < ta || t > tb {
                    return None;
                }
                let fp = model(t);
                let chirp = (model(t + 0.125) - model(t - 0.125)).abs() / 0.25;
                let (peak, allowed) = match steep.filter(|_| chirp > held_chirp) {
                    Some((short, short_background)) => {
                        // A fit made without these frames can be off here, by a time error that
                        // this slope turns into frequency.
                        let band = band.max(STEEP_TIME_ERROR_S * chirp);
                        // Four times the ridge's half-width (half a Hann main lobe, 2 / win, plus
                        // half the smear within a window), as the main window's reference, so most
                        // of the reference is not ridge. A real tone's skirts are wider than a pure
                        // tone's: at twice, once the band had narrowed to a few fit-RMS they filled
                        // the reference, and a ridge 8-14 dB up read a robust z under 3.
                        let win = short.meta.win_s;
                        let half_width = 2.0 / win + 0.5 * chirp * win;
                        // As in the main window, exclude the entire signal lobe
                        // from the noise reference. The final position check
                        // still uses the original timing/frequency tolerance.
                        let peak_band = band.max(half_width);
                        let reference = (4.0 * band).max(noise_band).max(4.0 * half_width);
                        let along = |tk: f64| model(tk) - fp;
                        let j = short.frame_index(t);
                        let peak = match (short_background, raw_only) {
                            (Some(background), true) => guided_raw_ridge_peak(
                                short, j, fp, along, band, peak_band, reference, half, min_snr,
                                min_z, background,
                            ),
                            _ => guided_ridge_peak_with_exclusion(
                                short,
                                j,
                                fp,
                                along,
                                band,
                                peak_band,
                                reference,
                                half,
                                min_snr,
                                min_z,
                                short_background,
                            ),
                        };
                        (peak, band)
                    }
                    None => {
                        // Curvature shifts a peak averaged along its tangent even when the
                        // local chirp fits inside the STFT window. Follow the full curve here too.
                        let along = |tk: f64| model(tk) - fp;
                        // A narrow position tolerance is not a noise exclusion band: a Hann
                        // main lobe extends 2 / win Hz on each side, plus half the chirp smear.
                        // Exclude that signal from the noise estimate while the final distance
                        // check below retains the original frequency tolerance.
                        let win = spec.meta.win_s;
                        let peak_band = band.max(2.0 / win + 0.5 * chirp * win);
                        let reference = noise_band.max(4.0 * peak_band);
                        let peak = match (background, raw_only) {
                            (Some(background), true) => guided_raw_ridge_peak(
                                spec, j, fp, along, peak_band, peak_band, reference, half, min_snr,
                                min_z, background,
                            ),
                            _ => guided_ridge_peak(
                                spec, j, fp, along, peak_band, reference, half, min_snr, min_z,
                                background,
                            ),
                        };
                        (peak, band)
                    }
                };
                peak.and_then(|(fh, snr, z)| {
                    (snr >= min_snr && z >= min_z && (fh - fp).abs() <= allowed)
                        .then_some((t, fh, snr))
                })
            })
            .collect()
    };
    let mut tr = Trace::default();
    for (t, f, s) in rows.into_iter().flatten() {
        tr.t.push(t);
        tr.f.push(f);
        tr.snr.push(s);
    }
    tr
}

/// Measure one detection. `x` is the whole recording at `sr`; times are recording seconds.
pub fn measure(x: &[f32], sr: u32, det: &Detection, id: &str, p: &MeasureParams) -> MeasuredEvent {
    measure_with_fit(x, sr, det, id, p, true, prepare_constant_velocity)
}

/// Refine and measure a detection family's primary, then try its detected harmonics only if
/// the primary cannot supply an aircraft event. A strong survey harmonic can lose its ridge
/// around the CPA while another harmonic of the same pass remains measurable.
///
/// A usable primary is returned unchanged, and so is one whose ridge was traced through the
/// CPA ([`lost_ridge`]): a sibling of the same pass shares its speed and distance, so a sibling
/// that passes where such a primary was refused (fit RMS) or fell under the aircraft bounds is a
/// different curve, not a recovery. Otherwise the first usable sibling, strongest survey score
/// first, supplies the family's one event; if none succeeds, keep the primary's refusal. The
/// selected detection retains its `harmonic_of` link, so callers can distinguish
/// a recovery from an event measured by the primary alone when grading the expanded search.
pub fn measure_family(
    x: &[f32],
    sr: u32,
    survey: &Spec,
    primary: &Detection,
    detections: &[Detection],
    id: &str,
    p: &MeasureParams,
) -> MeasuredEvent {
    let attempt = |d: &Detection| {
        let refined = crate::detect::refine(survey, d, p.c);
        measure(x, sr, &refined, id, p)
    };
    let first = attempt(primary);
    if first.is_aircraft_like() || !lost_ridge(&first, p) {
        return first;
    }
    let mut siblings: Vec<_> = detections
        .iter()
        .filter(|d| d.harmonic_of == Some(primary.id))
        .collect();
    siblings.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
    for sibling in siblings {
        let recovered = attempt(sibling);
        if recovered.is_aircraft_like() {
            return recovered;
        }
    }
    first
}

/// Whether a measurement lost its ridge around the CPA, the failure a harmonic sibling can
/// recover: no fit, too few trace points, a trace short of the CPA on either side, or too little
/// of the frequency drop followed. A NaN reading counts as lost.
fn lost_ridge(m: &MeasuredEvent, p: &MeasureParams) -> bool {
    let q = &m.quality;
    let short = |x: f64, min: f64| x.is_nan() || x < min;
    m.fit.is_none()
        || q.n_points < p.min_points
        || short(q.span_approach_s, p.min_side_s)
        || short(q.span_recede_s, p.min_side_s)
        || short(q.drop_traversed, p.min_drop_traversed)
}

/// Least number of points a guided trace needs to be fitted (and so to be followed afresh).
const MIN_GUIDED_POINTS: usize = 20;

/// Integration half-length (s) of a free trace along its own slope: the guided trace's, since a
/// straight line held longer leaves a curving ridge (about 1 Hz at 1 Hz/s^2 over this length).
const FREE_INTEG_S: f64 = 1.5;

/// What every measurement attempt of one event reads: the recording slice its spectrograms come
/// from, the main spectrogram with its row background, and the analysis window.
struct Frames<'a> {
    x: &'a [f32],
    sr: u32,
    s0: usize,
    s1: usize,
    ca: f64,
    fmin: f64,
    fmax: f64,
    spec: &'a Spec,
    background: &'a [f32],
    raw_only: bool,
    ta: f64,
    tb: f64,
}

/// One guided run keeps its audio, band and sampling fixed. Reuse its normalized short
/// spectrum when the fitted chirp selects the exact same window, commonly the 0.25 s floor.
/// Release the old allocation before a replacement, or when the main window suffices.
fn short_spectrum(
    cache: &mut Option<(Spec, Vec<f32>)>,
    win_s: f64,
    build: impl FnOnce(f64) -> Result<Spec, String>,
) -> Option<(&Spec, &[f32])> {
    if cache
        .as_ref()
        .is_some_and(|(spec, _)| spec.meta.win_s != win_s)
        || win_s >= MIN_MAIN_WIN_S
    {
        *cache = None;
    }
    if cache.is_none() && win_s < MIN_MAIN_WIN_S {
        let mut spec = build(win_s).ok()?;
        let background = spec.normalize_rows_with_offsets(None);
        *cache = Some((spec, background));
    }
    cache
        .as_ref()
        .map(|(spec, background)| (spec, background.as_slice()))
}

/// What a run of guided rounds produced: the last fitted trace (or, without a fit, the trace that
/// failed to fit), its fit, and why the rounds stopped short.
struct Guided {
    trace: Trace,
    prepared: Option<PreparedFit>,
    reasons: Vec<String>,
    /// The next round's narrowed tolerance, retained so a continuation follows the same ridge.
    band: f64,
}

/// One continuation of an already fitted ridge. Only missing side or central-drop coverage can
/// initiate it; progress measures the retained continuous stretch through the fitted CPA.
struct SideContinuation {
    det_t0r: f64,
    span: f64,
    covered_s: f64,
}

/// A coarse curve can need more than three fits to reach a fast pass's approach. Complete the
/// ordinary fitting budget before stopping on support: early parameter corrections can shorten
/// a side temporarily, and an isolated point can make an early fit look two-sided. Extra rounds
/// must then gain continuous retained support. The total is at most twice the ordinary budget,
/// with this absolute ceiling; no additional detection or independent starting curve is searched.
const MAX_CONTINUATION_ROUNDS: usize = 6;

#[derive(Default)]
struct SideSupport {
    approach_s: f64,
    recede_s: f64,
}

impl SideSupport {
    /// More receding audio cannot stand in for the missing approach, or vice versa.
    fn covered_s(&self, min_side_s: f64) -> f64 {
        self.approach_s.min(min_side_s) + self.recede_s.min(min_side_s)
    }

    fn complete(&self, min_side_s: f64) -> bool {
        self.approach_s >= min_side_s && self.recede_s >= min_side_s
    }
}

/// Retained points outside the continuous stretch containing the fitted CPA cannot establish
/// that this ridge was followed to the approach or recede side. A single distant point may be
/// retained by a fit even though the gap between it and the pass contains no observed ridge.
fn contiguous_side_support(inlier_times: &[f64], t0r: f64) -> SideSupport {
    let mut begin = 0;
    for i in 1..=inlier_times.len() {
        if i == inlier_times.len() || inlier_times[i] - inlier_times[i - 1] > MAX_RIDGE_GAP_S {
            let (first, last) = (inlier_times[begin], inlier_times[i - 1]);
            if first <= t0r && t0r <= last {
                return SideSupport {
                    approach_s: t0r - first,
                    recede_s: last - t0r,
                };
            }
            begin = i;
        }
    }
    SideSupport::default()
}

/// Guided rounds from the curve `start` = (f0, v, d, t0r) and the band `band`: trace within the
/// band, fit, make the fit the next curve and narrow the band to a few fit-RMS.
fn guided_rounds(
    fr: &Frames,
    start: (f64, f64, f64, f64),
    mut band: f64,
    roc: f64,
    p: &MeasureParams,
    mut continuation: Option<SideContinuation>,
    fit_trace: &mut impl FnMut(&[f64], &[f64], f64, Side) -> Result<PreparedFit, String>,
) -> Guided {
    let c = p.c;
    let (s0, s1) = (fr.s0, fr.s1);
    let mut model: Box<dyn Fn(f64) -> f64 + Sync> =
        Box::new(model_fn(start.0, start.1, start.2, start.3, c));
    let mut out = Guided {
        trace: Trace::default(),
        prepared: None,
        reasons: Vec::new(),
        band,
    };
    // From the latest fit, since a detection can rate a steep pass far too gently.
    let mut fitted_roc = roc;
    let rounds = if continuation.is_some() {
        p.rounds.saturating_mul(2).min(MAX_CONTINUATION_ROUNDS)
    } else {
        p.rounds
    };
    let ordinary_rounds = p.rounds.min(rounds);
    let mut short = None;
    for round in 0..rounds {
        let steep_win = window_for_roc(fitted_roc);
        let steep = short_spectrum(&mut short, steep_win, |win_s| {
            stft_band_db(
                &fr.x[s0..s1],
                fr.sr,
                win_s,
                p.hop_s,
                fr.fmin,
                fr.fmax,
                p.df_max,
                fr.ca,
            )
        });
        let tr = guided_trace_with_background(
            fr.spec,
            Some(fr.background),
            steep.map(|(spec, offsets)| (spec, Some(offsets))),
            &*model,
            fr.ta,
            fr.tb,
            band,
            p.min_snr,
            p.min_z,
            fr.raw_only,
        );
        if tr.len() < MIN_GUIDED_POINTS {
            if round == 0 || continuation.is_some() {
                out.reasons
                    .push(format!("guided trace found only {} ridge points", tr.len()));
            }
            break;
        }
        match fit_trace(&tr.t, &tr.f, c, Side::Both) {
            Ok(prepared) => {
                let b = prepared.best();
                model = Box::new(model_fn(b.f0, b.v, b.d, b.t0r, c));
                fitted_roc = peak_received_chirp(b.f0, b.v, b.d, c);
                band = (3.0 * b.rmse).clamp(1.0, band);
                out.band = band;
                let stop = continuation.as_mut().is_some_and(|progress| {
                    let g = gate(
                        &tr,
                        prepared.inlier_times(),
                        &b,
                        progress.det_t0r,
                        progress.span,
                        p,
                    );
                    let support = contiguous_side_support(prepared.inlier_times(), b.t0r);
                    let covered = support.covered_s(p.min_side_s);
                    let gained = covered > progress.covered_s + 0.5 * fr.spec.dt();
                    progress.covered_s = covered;
                    round + 1 >= ordinary_rounds
                        && ((g.reasons.is_empty() && support.complete(p.min_side_s)) || !gained)
                });
                out.trace = tr;
                out.prepared = Some(prepared);
                if stop {
                    break;
                }
            }
            Err(e) => {
                out.reasons.push(format!("fit: {e}"));
                // Keep a successful fit paired with its own trace if a later round fails.
                if out.prepared.is_none() {
                    out.trace = tr;
                }
                break;
            }
        }
    }
    out
}

/// The quality gates' reading of a fitted trace: the traced span on each side of the CPA, the
/// share of the drop traversed, and every gate it fails.
struct Gated {
    span_approach_s: f64,
    span_recede_s: f64,
    drop_traversed: f64,
    reasons: Vec<String>,
}

fn gate(
    trace: &Trace,
    inlier_times: &[f64],
    b: &Best,
    det_t0r: f64,
    span: f64,
    p: &MeasureParams,
) -> Gated {
    let c = p.c;
    let (mut amin, mut rmax) = (b.t0r, b.t0r);
    // A ridge point rejected by the robust fit cannot establish that this
    // fitted pass was observed on that side of its CPA.
    for &t in inlier_times {
        amin = amin.min(t);
        rmax = rmax.max(t);
    }
    let asymptotes = readouts(b.f0, b.v, b.d, c);
    let mut g = Gated {
        span_approach_s: b.t0r - amin,
        span_recede_s: rmax - b.t0r,
        drop_traversed: drop_traversed(
            &trace.t,
            &trace.f,
            asymptotes.f_pre,
            asymptotes.f_post,
            model_fn(b.f0, b.v, b.d, b.t0r, c),
        ),
        reasons: Vec::new(),
    };
    if trace.len() < p.min_points {
        g.reasons
            .push(format!("{} trace points < {}", trace.len(), p.min_points));
    }
    if g.span_approach_s < p.min_side_s || g.span_recede_s < p.min_side_s {
        g.reasons.push(format!(
            "one-sided trace (approach {:.0}s, recede {:.0}s): CPA not observed",
            g.span_approach_s, g.span_recede_s
        ));
    }
    if g.drop_traversed < p.min_drop_traversed {
        g.reasons.push(format!(
            "trace follows only {:.0} % of the central frequency drop: no ridge through the CPA",
            100.0 * g.drop_traversed
        ));
    }
    if b.rmse > p.max_rmse_hz {
        g.reasons
            .push(format!("fit RMS {:.2} Hz > {:.2}", b.rmse, p.max_rmse_hz));
    }
    if !(15.0..=0.8 * c).contains(&b.v) {
        g.reasons.push(format!("implausible speed {:.0} m/s", b.v));
    }
    if !(60.0..=25_000.0).contains(&b.d) {
        g.reasons
            .push(format!("implausible CPA distance {:.0} m", b.d));
    }
    if (b.t0r - det_t0r).abs() > (0.5 * span).max(20.0) {
        g.reasons.push(format!(
            "fit CPA {:.1}s drifted from detection {:.1}s",
            b.t0r, det_t0r
        ));
    }
    g
}

/// Whether guided rounds ended in a fit that every gate accepts.
fn passes(g: &Guided, det_t0r: f64, span: f64, p: &MeasureParams) -> bool {
    g.reasons.is_empty()
        && g.prepared.as_ref().is_some_and(|f| {
            gate(&g.trace, f.inlier_times(), &f.best(), det_t0r, span, p)
                .reasons
                .is_empty()
        })
}

/// A second start for an event whose guided rounds the gates refused. The detection's curve can
/// meet the pass's ridge over one stretch and leave it elsewhere by more than the band: a detection
/// at the edge of the template bank, or one that rates the speed and distance together wrongly,
/// has the wrong shape, and the band then holds only one side of the pass. The trace still lies
/// on the ridge where the curve met it. From its first, strongest and last points this follows
/// the ridge without the model, by its own local slope ([`ridge_trace`]), inside the analysis
/// window, and fits what it finds; guided rounds then start from that fit. A steady tone, a
/// rising one or noise gives a fit the gates refuse again, and the first rounds stand.
fn reseeded_rounds(
    fr: &Frames,
    trace: &Trace,
    p: &MeasureParams,
    fit_trace: &mut impl FnMut(&[f64], &[f64], f64, Side) -> Result<PreparedFit, String>,
) -> Option<Guided> {
    let c = p.c;
    let n = trace.len();
    let strongest = (0..n).max_by(|&i, &j| trace.snr[i].total_cmp(&trace.snr[j]))?;
    let mut seeds: Vec<(f64, f64)> = Vec::with_capacity(3);
    for i in [0, strongest, n - 1] {
        if !seeds.iter().any(|s| s.0 == trace.t[i]) {
            seeds.push((trace.t[i], trace.f[i]));
        }
    }
    let spec = fr.spec;
    let (a, b) = (spec.frame_index(fr.ta), spec.frame_index(fr.tb) + 1);
    let window = spec.crop(a, b, 0, spec.nf());
    let free = ridge_trace(
        &window,
        &seeds,
        &TraceParams {
            min_snr: p.min_snr,
            min_z: p.min_z,
            integ_s: FREE_INTEG_S,
            ..Default::default()
        },
    );
    if free.len() < MIN_GUIDED_POINTS {
        return None;
    }
    let b = fit_trace(&free.t, &free.f, c, Side::Both).ok()?.best();
    let plausible = b.f0.is_finite()
        && b.t0r.is_finite()
        && (15.0..=0.8 * c).contains(&b.v)
        && (60.0..=25_000.0).contains(&b.d);
    plausible.then(|| {
        guided_rounds(
            fr,
            (b.f0, b.v, b.d, b.t0r),
            (0.04 * b.f0).clamp(3.0, 12.0),
            peak_received_chirp(b.f0, b.v, b.d, c),
            p,
            None,
            fit_trace,
        )
    })
}

/// Restore spectral support once for a steep fast fit whose only refusal is side or drop coverage.
/// Its initial detection can make the analysis band too narrow for the approach and leave the
/// first few fits converging on the receding side, or following both asymptotes while still
/// missing the central drop. A missing drop keeps the original frames and narrowed band:
/// reopening the search at that point can lose its converging ridge. A missing side uses the
/// ordinary frequency/window rules to renew its spectral support. Both keep the original
/// time/context bounds and every original gate. A new spectrogram belongs to the event only
/// if the continuation succeeds.
fn continued_rounds(
    fr: &Frames,
    first: &Guided,
    det_t0r: f64,
    span: f64,
    p: &MeasureParams,
    fit_trace: &mut impl FnMut(&[f64], &[f64], f64, Side) -> Result<PreparedFit, String>,
) -> Option<(Guided, Option<Spec>)> {
    let fitted = first.prepared.as_ref()?;
    let b = fitted.best();
    let g = gate(&first.trace, fitted.inlier_times(), &b, det_t0r, span, p);
    let roc = peak_received_chirp(b.f0, b.v, b.d, p.c);
    let lacks_side = g.span_approach_s < p.min_side_s || g.span_recede_s < p.min_side_s;
    let lacks_drop = g.drop_traversed < p.min_drop_traversed;
    if !first.reasons.is_empty()
        || g.reasons.len() != 1
        || !(lacks_side || lacks_drop)
        || !b.f0.is_finite()
        // This repairs a fitted pass already beyond the primary bank's speed domain. A slower
        // one-sided fit does not establish that gap; continuing it accepted wrong geometry in
        // the late-pass control. Keep that separate measurement failure outside this recovery.
        || b.v <= PRIMARY_MAX_SPEED_MPS
        || window_for_roc(roc) >= MIN_MAIN_WIN_S
    {
        return None;
    }
    let (fmin, fmax) = measurement_band(b.f0, b.v, fr.sr, p.c);
    let (fmin, fmax) = (fmin.min(fr.fmin), fmax.max(fr.fmax));
    let win = window_for_roc(roc).clamp(MIN_MAIN_WIN_S, 2.0);
    let mut spec = if lacks_side {
        Some(
            stft_band_db(
                &fr.x[fr.s0..fr.s1],
                fr.sr,
                win,
                p.hop_s,
                fmin,
                fmax,
                p.df_max,
                fr.ca,
            )
            .ok()?,
        )
    } else {
        None
    };
    let background = spec
        .as_mut()
        .map(|spec| spec.normalize_rows_with_offsets(None));
    let frames = spec
        .as_ref()
        .zip(background.as_ref())
        .map(|(spec, background)| Frames {
            raw_only: false,
            fmin,
            fmax,
            spec,
            background,
            ..*fr
        });
    let continued = guided_rounds(
        frames.as_ref().unwrap_or(fr),
        (b.f0, b.v, b.d, b.t0r),
        if lacks_drop {
            first.band
        } else {
            (0.04 * b.f0).clamp(3.0, 12.0)
        },
        roc,
        p,
        Some(SideContinuation {
            det_t0r,
            span,
            covered_s: contiguous_side_support(fitted.inlier_times(), b.t0r)
                .covered_s(p.min_side_s),
        }),
        fit_trace,
    );
    let supported = continued.prepared.as_ref().is_some_and(|f| {
        contiguous_side_support(f.inlier_times(), f.best().t0r).complete(p.min_side_s)
    });
    (passes(&continued, det_t0r, span, p) && supported).then_some((continued, spec))
}

fn measurement_band(f0: f64, v: f64, sr: u32, c: f64) -> (f64, f64) {
    let v_hi = (v * 1.4).min(0.8 * c);
    (
        (f0 * c / (c + v_hi) * 0.93 - 3.0).max(15.0),
        (f0 * c / (c - v_hi) * 1.07 + 3.0).min(sr as f64 * 0.45),
    )
}

fn measure_with_fit(
    x: &[f32],
    sr: u32,
    det: &Detection,
    id: &str,
    p: &MeasureParams,
    allow_continuation: bool,
    mut fit_trace: impl FnMut(&[f64], &[f64], f64, Side) -> Result<PreparedFit, String>,
) -> MeasuredEvent {
    let c = p.c;
    let dur = x.len() as f64 / sr as f64;
    let tau = det.d / det.v.max(10.0);
    let span = (4.0 * tau).clamp(45.0, 300.0);
    let (ta, tb) = ((det.t0r - span).max(0.0), (det.t0r + span).min(dur));
    let (fmin, fmax) = measurement_band(det.f0, det.v, sr, c);
    let roc = peak_received_chirp(det.f0, det.v, det.d.max(50.0), c);
    let win = window_for_roc(roc).clamp(MIN_MAIN_WIN_S, 2.0);
    let (ca, cb) = ((ta - p.context_s).max(0.0), (tb + p.context_s).min(dur));
    let mut quality = EventQuality {
        n_points: 0,
        span_approach_s: 0.0,
        span_recede_s: 0.0,
        drop_traversed: 0.0,
        median_snr_db: 0.0,
        rmse_hz: f64::NAN,
        score: 0.0,
        ok: false,
        reseeded: false,
        reasons: Vec::new(),
    };
    let s0 = (ca * sr as f64) as usize;
    let s1 = ((cb * sr as f64) as usize).min(x.len());
    let mut spec = match stft_band_db(&x[s0..s1], sr, win, p.hop_s, fmin, fmax, p.df_max, ca) {
        Ok(s) => s,
        Err(e) => {
            quality.reasons.push(format!("spectrogram: {e}"));
            return MeasuredEvent {
                id: id.into(),
                detection: det.clone(),
                fit: None,
                trace: Trace::default(),
                quality,
                win_s: win,
                spec: None,
            };
        }
    };
    let background = spec.normalize_rows_with_offsets(None);
    let frames = Frames {
        raw_only: false,
        x,
        sr,
        s0,
        s1,
        ca,
        fmin,
        fmax,
        spec: &spec,
        background: &background,
        ta,
        tb,
    };
    let first = guided_rounds(
        &frames,
        (det.f0, det.v, det.d, det.t0r),
        (0.04 * det.f0).clamp(3.0, 12.0),
        roc,
        p,
        None,
        &mut fit_trace,
    );
    // Only a fit the gates refused because its trace held one side of the pass is followed afresh:
    // that is how a curve of the wrong shape fails. A trace too short to fit, or a fit that failed,
    // has nothing to follow; a two-sided trace that does not fit (a ridge that hops or bends) is not
    // helped by following it without the model.
    let one_sided = first.prepared.as_ref().is_some_and(|f| {
        let g = gate(&first.trace, f.inlier_times(), &f.best(), det.t0r, span, p);
        !g.reasons.is_empty()
            && (g.span_approach_s < p.min_side_s || g.span_recede_s < p.min_side_s)
    });
    let refused = p.free_reseed
        && first.reasons.is_empty()
        && first.trace.len() >= MIN_GUIDED_POINTS
        && one_sided;
    let mut chosen = first;
    if refused
        && let Some(second) = reseeded_rounds(&frames, &chosen.trace, p, &mut fit_trace)
        && passes(&second, det.t0r, span, p)
    {
        chosen = second;
        quality.reseeded = true;
    }
    // Discovery can retain a normalised peak whose local raw frequency is unresolved.
    // Once that path supplies an accepted aircraft curve, refine its metrology using only
    // locally supported raw vertices. The replacement must pass every gate on its own
    // retained observations and the existing aircraft bounds; a failed calibration keeps
    // the accepted measurement.
    if passes(&chosen, det.t0r, span, p)
        && let Some(b) = chosen.prepared.as_ref().map(PreparedFit::best)
        && aircraft_geometry(&b)
    {
        let calibrated = guided_rounds(
            &Frames {
                raw_only: true,
                ..frames
            },
            (b.f0, b.v, b.d, b.t0r),
            (3.0 * b.rmse).clamp(1.0, (0.04 * b.f0).clamp(3.0, 12.0)),
            peak_received_chirp(b.f0, b.v, b.d, c),
            p,
            None,
            &mut fit_trace,
        );
        if passes(&calibrated, det.t0r, span, p)
            && calibrated
                .prepared
                .as_ref()
                .is_some_and(|f| aircraft_geometry(&f.best()))
        {
            chosen = calibrated;
        }
    }
    if p.free_reseed
        && !quality.reseeded
        && allow_continuation
        && let Some((continued, wide)) =
            continued_rounds(&frames, &chosen, det.t0r, span, p, &mut fit_trace)
    {
        chosen = continued;
        if let Some(wide) = wide {
            spec = wide;
        }
        quality.reseeded = true;
    }
    let Guided {
        trace,
        prepared,
        reasons,
        ..
    } = chosen;
    quality.reasons = reasons;
    let gated = prepared
        .as_ref()
        .map(|f| gate(&trace, f.inlier_times(), &f.best(), det.t0r, span, p));
    // Only the final accepted trace needs uncertainty profiles and manoeuvre diagnostics.
    let fit = prepared.map(PreparedFit::complete);
    // Keep only the analysis window plus a scoring margin in memory.
    let keep_a = spec.frame_index((ta - 60.0).max(spec.t[0]));
    let keep_b = (spec.frame_index((tb + 60.0).min(spec.t[spec.nt() - 1])) + 1).min(spec.nt());
    let spec = spec.crop(keep_a, keep_b, 0, spec.nf());
    quality.n_points = trace.len();
    if let Some((fr, g)) = fit.as_ref().zip(gated) {
        let mut snr = trace.snr.clone();
        quality.median_snr_db = crate::spectro::median_f64(&mut snr);
        quality.rmse_hz = fr.best.rmse;
        let n_eff = fr.diagnostics.n_eff.max(1.0);
        quality.score = quality.median_snr_db.max(0.0) * n_eff.sqrt();
        quality.span_approach_s = g.span_approach_s;
        quality.span_recede_s = g.span_recede_s;
        quality.drop_traversed = g.drop_traversed;
        quality.reasons.extend(g.reasons);
    } else if quality.reasons.is_empty() {
        quality.reasons.push("no fit".into());
    }
    quality.ok = fit.is_some() && quality.reasons.is_empty();
    MeasuredEvent {
        id: id.into(),
        detection: det.clone(),
        fit,
        trace,
        quality,
        win_s: spec.meta.win_s,
        spec: Some(spec),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{DetectParams, detect, refine, survey};
    use crate::fit::fit_constant_velocity;

    #[test]
    fn repeated_short_windows_reuse_the_fresh_spectrum_exactly() {
        let sr = 1000;
        let mut phase = 0.0f64;
        let x: Vec<f32> = (0..4000)
            .map(|i| {
                let t = i as f64 / f64::from(sr);
                let f = cv_curve(t, 83.0, 245.0, 700.0, 2.0 - 700.0 / 343.0, 343.0);
                phase += std::f64::consts::TAU * f / f64::from(sr);
                (phase.sin() + 0.1 * (std::f64::consts::TAU * 41.0 * t).sin()) as f32
            })
            .collect();
        let build = |win_s| stft_band_db(&x, sr, win_s, 0.1, 30.0, 400.0, 2.0, 17.25);
        let mut cache = None;
        let mut builds = 0;
        for (win_s, expected_builds) in [
            (0.25, 1),
            (0.25, 1),
            (0.375, 2),
            (0.375, 2),
            (0.25, 3),
        ] {
            let (actual, offsets) = short_spectrum(&mut cache, win_s, |window| {
                builds += 1;
                build(window)
            })
            .unwrap();
            assert_eq!(builds, expected_builds);
            let mut fresh = build(win_s).unwrap();
            let fresh_offsets = fresh.normalize_rows_with_offsets(None);
            assert_eq!(actual.t, fresh.t);
            assert_eq!(actual.f, fresh.f);
            assert_eq!(
                serde_json::to_vec(&actual.meta).unwrap(),
                serde_json::to_vec(&fresh.meta).unwrap()
            );
            assert_eq!(actual.db.len(), fresh.db.len());
            assert!(
                actual
                    .db
                    .iter()
                    .zip(&fresh.db)
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            );
            assert_eq!(offsets.len(), fresh_offsets.len());
            assert!(
                offsets
                    .iter()
                    .zip(&fresh_offsets)
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            );
        }
        assert!(
            short_spectrum(&mut cache, MIN_MAIN_WIN_S, |_| {
                panic!("the main window needs no short transform")
            })
            .is_none()
        );
        assert!(cache.is_none(), "the previous short allocation must be released");
    }

    #[test]
    fn a_failed_short_window_never_reuses_or_caches_a_previous_spectrum() {
        // A quarter-second transform fits, but a 0.375 s transform really fails.
        let sr = 1000;
        let x: Vec<f32> = (0..300)
            .map(|i| (std::f64::consts::TAU * 83.0 * i as f64 / f64::from(sr)).sin() as f32)
            .collect();
        let mut cache = None;
        let mut builds = 0;
        for (win_s, succeeds) in [(0.25, true), (0.375, false), (0.375, false), (0.25, true)] {
            let got = short_spectrum(&mut cache, win_s, |window| {
                builds += 1;
                stft_band_db(&x, sr, window, 0.1, 30.0, 400.0, 2.0, 0.0)
            });
            assert_eq!(got.is_some(), succeeds);
            assert_eq!(cache.is_some(), succeeds);
        }
        assert_eq!(builds, 4, "a failed window must be retried, not cached");
        assert!(
            short_spectrum(&mut cache, f64::NAN, |_| {
                panic!("an invalid window needs no short transform")
            })
            .is_none()
        );
        assert!(cache.is_none());
    }

    /// A pass of `f0` in noise, its tone silent while the curve is between `silent` of its way down.
    fn measured_pass(
        f0: f64,
        v: f64,
        d: f64,
        tone_amp: f64,
        silent: Option<(f64, f64)>,
        p: &MeasureParams,
    ) -> MeasuredEvent {
        measured_pass_detected_at(f0, v, d, d, tone_amp, silent, p)
    }

    /// As `measured_pass`, with a detection that puts the CPA at `detected_d`.
    fn measured_pass_detected_at(
        f0: f64,
        v: f64,
        d: f64,
        detected_d: f64,
        tone_amp: f64,
        silent: Option<(f64, f64)>,
        p: &MeasureParams,
    ) -> MeasuredEvent {
        let sr = 4000u32;
        let c = 343.0;
        let span = (6.0 * d / v).clamp(90.0, 300.0);
        let t0r = span;
        let det = Detection {
            id: 0,
            t0r,
            f0,
            v,
            d: detected_d,
            score: 10.0,
            score_approach: 10.0,
            score_recede: 10.0,
            half_s: 60.0,
            harmonic_of: None,
            harmonics: Vec::new(),
            from_contrast: false,
        };
        let asymptotes = readouts(f0, v, d, c);
        let tone = model_fn(f0, v, d, t0r, c);
        let (mut seed, mut phase) = (789u64, 0.0);
        let x: Vec<f32> = (0..(sr as f64 * 2.0 * span) as usize)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let f = tone(i as f64 / sr as f64);
                phase += std::f64::consts::TAU * f / sr as f64;
                let way = (asymptotes.f_pre - f) / (asymptotes.f_pre - asymptotes.f_post);
                let quiet = silent.is_some_and(|(a, b)| (a..=b).contains(&way));
                let signal = if quiet { 0.0 } else { tone_amp * phase.sin() };
                (signal + 0.3 * (2.0 * seed as f64 / u64::MAX as f64 - 1.0)) as f32
            })
            .collect();
        measure(&x, sr, &det, "e1", p)
    }

    /// Two aircraft in real PCM, the first with a strong second harmonic and a weaker
    /// fundamental. The supplied survey seeds identify their already-grouped family; the
    /// tests exercise actual refinement, ridge measurement, fitting and search selection.
    fn harmonic_family_scene(
        broken_primary: bool,
        fundamental: bool,
    ) -> (Vec<f32>, Vec<Detection>) {
        let (sr, c) = (4000u32, 343.0);
        let primary = Detection {
            id: 41,
            t0r: 110.0,
            f0: 236.0,
            v: 120.0,
            d: 1500.0,
            score: 12.0,
            score_approach: 12.0,
            score_recede: 12.0,
            half_s: 60.0,
            harmonic_of: None,
            harmonics: vec![0.5],
            from_contrast: false,
        };
        let sibling = Detection {
            id: 7,
            f0: 118.0,
            score: 8.0,
            score_approach: 8.0,
            score_recede: 8.0,
            harmonic_of: Some(primary.id),
            harmonics: Vec::new(),
            ..primary.clone()
        };
        let other = Detection {
            id: 23,
            t0r: 270.0,
            f0: 165.0,
            v: 76.0,
            d: 1800.0,
            score: 10.0,
            score_approach: 10.0,
            score_recede: 10.0,
            harmonics: Vec::new(),
            ..primary.clone()
        };
        let asymptotes = readouts(primary.f0, primary.v, primary.d, c);
        let tones = [&primary, &sibling, &other];
        let (mut seed, mut phases) = (789u64, [0.0f64; 3]);
        let x = (0..sr as usize * 360)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let t = i as f64 / sr as f64;
                let mut sample = 0.3 * (2.0 * seed as f64 / u64::MAX as f64 - 1.0);
                for (k, d) in tones.iter().enumerate() {
                    let f = cv_curve(t, d.f0, d.v, d.d, d.t0r - d.d / c, c);
                    phases[k] += std::f64::consts::TAU * f / sr as f64;
                    let way = (asymptotes.f_pre - f) / (asymptotes.f_pre - asymptotes.f_post);
                    let quiet = (k == 0 && broken_primary && (0.03..=0.97).contains(&way))
                        || (k == 1 && !fundamental);
                    if !quiet {
                        sample += [0.24, 0.08, 0.10][k] * phases[k].sin();
                    }
                }
                sample as f32
            })
            .collect();
        // Family membership is by ID, not slice index or adjacency.
        (x, vec![sibling, other, primary])
    }

    #[test]
    fn harmonic_sibling_recovers_a_refused_primary_as_one_aircraft_event() {
        let (x, detections) = harmonic_family_scene(true, true);
        let dp = DetectParams::default();
        let (_, lin) = survey(&x, 4000, &dp).unwrap();
        let mp = MeasureParams::default();
        let primary = &detections[2];
        let original = measure(&x, 4000, &refine(&lin, primary, mp.c), "e1", &mp);
        assert!(!original.is_aircraft_like(), "{:?}", original.quality);

        let primaries = [primary, &detections[1]];
        let events = measure_ranked_counted(
            primaries.len(),
            2,
            |ranks| {
                ranks
                    .map(|k| {
                        measure_family(
                            &x,
                            4000,
                            &lin,
                            primaries[k],
                            &detections,
                            &format!("e{}", k + 1),
                            &mp,
                        )
                    })
                    .collect()
            },
            |events| crate::search::independent_measured_events(events, 0.0).len(),
        );
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].detection.id, detections[0].id);
        assert_eq!(events[0].detection.harmonic_of, Some(primary.id));
        assert_eq!(events[1].detection.id, detections[1].id);
        assert_eq!(
            crate::search::independent_measured_events(&events, 0.0).len(),
            2
        );
        for (event, truth) in events.iter().zip([&detections[0], &detections[1]]) {
            assert!(event.is_aircraft_like(), "{:?}", event.quality);
            let b = event.fit.as_ref().unwrap().best;
            assert!((b.f0 - truth.f0).abs() < 0.3, "{b:?}");
            assert!((b.t0r - truth.t0r).abs() < 0.3, "{b:?}");
            assert!((b.v - truth.v).abs() < 4.0, "{b:?}");
            assert!((b.d / truth.d - 1.0).abs() < 0.08, "{b:?}");
        }
    }

    #[test]
    fn harmonic_family_preserves_a_usable_primary_unchanged() {
        let (x, detections) = harmonic_family_scene(false, true);
        let dp = DetectParams::default();
        let (_, lin) = survey(&x, 4000, &dp).unwrap();
        let mp = MeasureParams::default();
        let primary = &detections[2];
        let original = measure(&x, 4000, &refine(&lin, primary, mp.c), "e1", &mp);
        assert!(original.is_aircraft_like(), "{:?}", original.quality);
        let selected = measure_family(&x, 4000, &lin, primary, &detections, "e1", &mp);
        assert_eq!(
            serde_json::to_value(&selected).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
        assert_eq!(
            selected.spec.as_ref().unwrap().db,
            original.spec.as_ref().unwrap().db
        );
    }

    #[test]
    fn harmonic_family_preserves_refusal_when_its_siblings_fail() {
        let (x, detections) = harmonic_family_scene(true, false);
        let dp = DetectParams::default();
        let (_, lin) = survey(&x, 4000, &dp).unwrap();
        let mp = MeasureParams::default();
        let primary = &detections[2];
        let original = measure(&x, 4000, &refine(&lin, primary, mp.c), "e1", &mp);
        assert!(!original.is_aircraft_like(), "{:?}", original.quality);
        let selected = measure_family(&x, 4000, &lin, primary, &detections, "e1", &mp);
        // An unrelated aircraft is audible, but it must not substitute for this family.
        assert_eq!(
            serde_json::to_value(&selected).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
        assert_eq!(selected.detection.id, primary.id);
        assert!(!selected.is_aircraft_like());
    }

    /// Only a primary whose ridge is missing around the CPA may yield to a sibling. A primary that
    /// traced both sides and the drop was measured: a sibling of the same pass shares its speed
    /// and distance, so one that passes where this primary was refused for its RMS, or measured
    /// under the aircraft bounds, is a different curve (set D's N456CS_R38 and feederwatch's slow
    /// source both took one that way).
    #[test]
    fn only_a_primary_that_lost_its_ridge_tries_its_siblings() {
        let (x, detections) = harmonic_family_scene(false, true);
        let dp = DetectParams::default();
        let (_, lin) = survey(&x, 4000, &dp).unwrap();
        let mp = MeasureParams::default();
        let primary = &detections[2];
        let measured = measure(&x, 4000, &refine(&lin, primary, mp.c), "e1", &mp);
        assert!(measured.is_aircraft_like(), "{:?}", measured.quality);
        assert!(!lost_ridge(&measured, &mp));

        let with = |f: &dyn Fn(&mut MeasuredEvent)| {
            let mut m = measured.clone();
            m.quality.ok = false;
            f(&mut m);
            lost_ridge(&m, &mp)
        };
        // refused, but traced through the CPA: keeps its own result
        assert!(!with(&|m| m.quality.rmse_hz = 2.1));
        // measured, but slower than the aircraft bound: keeps its own result
        assert!(!with(&|m| {
            m.quality.ok = true;
            m.fit.as_mut().unwrap().best.v = 26.0;
        }));
        // the ridge is missing around the CPA: the siblings are tried
        assert!(with(
            &|m| m.quality.drop_traversed = 0.5 * mp.min_drop_traversed
        ));
        assert!(with(&|m| m.quality.drop_traversed = f64::NAN));
        assert!(with(&|m| m.quality.span_approach_s = 0.5 * mp.min_side_s));
        assert!(with(&|m| m.quality.span_recede_s = 0.5 * mp.min_side_s));
        assert!(with(&|m| m.quality.n_points = mp.min_points - 1));
        assert!(with(&|m| m.fit = None));
    }

    #[test]
    fn curved_ridge_recovers_sub_bin_frequencies_without_tangent_bias() {
        use crate::spectro::SpecMeta;

        // An independently sampled, symmetric spectral peak follows the physical curve.
        // A perfect curve has no offset to calibrate away; a tangent averages different
        // frequencies on either side of the frame and biases even moderate-chirp passes.
        let (c, df, hop) = (343.0, 0.25, 0.2);
        for (base_f0, v, d) in [
            (80.0, 100.0, 800.0),
            (120.0, 130.0, 2000.0),
            (180.0, 100.0, 1500.0),
        ] {
            for phase in [0.03, 0.17] {
                let (f0, t0r) = (base_f0 + phase, 40.0 + phase);
                let truth = model_fn(f0, v, d, t0r, c);
                let t: Vec<f64> = (0..401).map(|i| i as f64 * hop).collect();
                let fmin = ((truth(80.0) - 15.0) / df).floor() * df;
                let nf = ((truth(0.0) + 15.0 - fmin) / df).ceil() as usize + 1;
                let f: Vec<f64> = (0..nf).map(|i| fmin + i as f64 * df).collect();
                let db = f
                    .iter()
                    .flat_map(|&freq| {
                        let curve = &truth;
                        t.iter().map(move |&time| {
                            (20.0 * (-0.5 * ((freq - curve(time)) / 0.55).powi(2)).exp()) as f32
                        })
                    })
                    .collect();
                let spec = Spec {
                    t,
                    f,
                    db,
                    meta: SpecMeta {
                        sr: 4000,
                        win_s: 0.5,
                        hop_s: hop,
                        nfft: 16000,
                        df,
                        fmin,
                        fmax: fmin + (nf - 1) as f64 * df,
                        norm: "synthetic curved ridge".into(),
                    },
                };
                // Deliberately offset the guide: measuring must find the actual ridge,
                // rather than simply echo its model. Vary both frequency-bin and CPA phase.
                let guide = |time: f64| truth(time) + 0.37;
                let trace = guided_trace(&spec, None, &guide, 0.0, 80.0, 3.0, 3.0, 4.0);
                assert_eq!(trace.len(), spec.nt());
                let max_error = trace
                    .t
                    .iter()
                    .zip(&trace.f)
                    .map(|(&time, &freq)| (freq - truth(time)).abs())
                    .fold(0.0, f64::max);
                eprintln!(
                    "curved ridge f0={f0} v={v} d={d}: maximum frequency error {max_error:.6} Hz"
                );
                assert!(max_error < 0.01, "f0={f0} v={v} d={d}: {max_error} Hz");
            }
        }
    }

    fn check_emission_phase_passes(sensitivity_only: bool) {
        let (sr, c, t0r, duration) = (4000u32, 343.0, 120.013, 240.0);
        let mut failures = Vec::new();
        // Both passes have a CPA chirp below 4 Hz/s, but their maximum received
        // chirps occur earlier and exceed it. Include the existing faint-tone
        // amplitude and a strong control without changing any measurement gate.
        for (f0, v, d) in [(100.0, 200.0, 3000.0), (150.0, 260.0, 10_000.0)] {
            for amplitude in [0.012, 0.05] {
                let sensitivity_control = v == 260.0 && amplitude == 0.012;
                if sensitivity_control != sensitivity_only {
                    continue;
                }
                let t0e = t0r - d / c;
                let mut seed = 90210u64;
                let x: Vec<f32> = (0..(sr as f64 * duration) as usize)
                    .map(|i| {
                        let received = i as f64 / sr as f64;
                        let u = received - t0e;
                        // Independently solve t_r = t_e + range(t_e)/c. The
                        // production Doppler/retarded-time functions are not used
                        // to synthesize either the phase or its 1/r envelope.
                        let mut emitted = u - d / c;
                        for _ in 0..10 {
                            let range = d.hypot(v * emitted);
                            emitted -=
                                (emitted + range / c - u) / (1.0 + v * v * emitted / (c * range));
                        }
                        let range = d.hypot(v * emitted);
                        if i % 10_000 == 0 {
                            assert!((emitted + range / c - u).abs() < 1e-10);
                        }
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        let noise = 0.3 * (2.0 * seed as f64 / u64::MAX as f64 - 1.0);
                        (amplitude * d / range
                            * (std::f64::consts::TAU * f0 * emitted + 0.13).sin()
                            + noise) as f32
                    })
                    .collect();
                let det = Detection {
                    id: 0,
                    t0r: t0r + 0.3,
                    f0: f0 + 0.11,
                    v: v * 1.03,
                    d: d * 1.08,
                    score: 10.0,
                    score_approach: 10.0,
                    score_recede: 10.0,
                    half_s: 60.0,
                    harmonic_of: None,
                    harmonics: Vec::new(),
                    from_contrast: false,
                };
                let event = measure(&x, sr, &det, "fast_phase", &MeasureParams::default());
                let fit = event.fit.as_ref().map(|f| f.best);
                if sensitivity_control {
                    // Even exact physical guidance cannot observe enough of this
                    // faint pass's central drop. An accurate extrapolated fit is
                    // insufficient: preserve the existing missing-ridge refusal.
                    let p = MeasureParams::default();
                    let meta = &event.spec.as_ref().unwrap().meta;
                    let mut main = stft_band_db(
                        &x,
                        sr,
                        event.win_s,
                        p.hop_s,
                        meta.fmin,
                        meta.fmax,
                        p.df_max,
                        0.0,
                    )
                    .unwrap();
                    let main_background = main.normalize_rows_with_offsets(None);
                    let mut short = stft_band_db(
                        &x,
                        sr,
                        window_for_roc(peak_received_chirp(f0, v, d, c)),
                        p.hop_s,
                        meta.fmin,
                        meta.fmax,
                        p.df_max,
                        0.0,
                    )
                    .unwrap();
                    let short_background = short.normalize_rows_with_offsets(None);
                    let truth = model_fn(f0, v, d, t0r, c);
                    let ideal = guided_trace_with_background(
                        &main,
                        Some(&main_background),
                        Some((&short, Some(&short_background))),
                        &truth,
                        0.0,
                        duration,
                        1.0,
                        p.min_snr,
                        p.min_z,
                        false,
                    );
                    let r = readouts(f0, v, d, c);
                    let ideal_drop = drop_traversed(&ideal.t, &ideal.f, r.f_pre, r.f_post, truth);
                    assert!(ideal.len() >= p.min_points);
                    assert!(ideal.t[0] <= t0r - p.min_side_s);
                    assert!(ideal.t[ideal.len() - 1] >= t0r + p.min_side_s);
                    assert!(ideal_drop < p.min_drop_traversed, "{ideal_drop}");
                    assert!(!event.quality.ok && !event.quality.reseeded);
                    assert!(event.quality.drop_traversed < p.min_drop_traversed);
                    assert_eq!(event.quality.reasons.len(), 1, "{:?}", event.quality);
                    assert!(event.quality.reasons[0].contains("frequency drop"));
                }
                let accurate = fit.is_some_and(|b| {
                    (b.f0 - f0).abs() <= 0.005 * f0
                        && (b.v - v).abs() <= 0.03 * v
                        && (b.d - d).abs() <= 0.05 * d
                        && (b.t0r - t0r).abs() <= 0.5
                });
                if !accurate || (!sensitivity_control && !event.is_aircraft_like()) {
                    failures.push((f0, v, d, amplitude, fit, event.quality));
                }
            }
        }
        assert!(failures.is_empty(), "{failures:?}");
    }

    #[test]
    fn fast_passes_recover_physical_parameters_from_emission_phase() {
        check_emission_phase_passes(false);
    }

    #[test]
    fn faint_fast_pass_from_emission_phase_preserves_ridge_refusal() {
        check_emission_phase_passes(true);
    }

    #[test]
    fn curved_integration_limits_distance_bias_across_audio_phases() {
        use crate::physics::retarded_tau;

        let (sr, c) = (4000u32, 343.0);
        let p = MeasureParams::default();
        let mut max_distance_fraction = 0.0f64;
        for (f0, v, d) in [
            (80.0, 100.0, 800.0),
            (120.0, 130.0, 2000.0),
            (180.0, 100.0, 1500.0),
        ] {
            for phase in [0.13, 1.27] {
                let t0r = 90.0 + phase * 0.1;
                let t0e = t0r - d / c;
                let mut seed = 1777u64;
                let x: Vec<f32> = (0..sr as usize * 180)
                    .map(|i| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        // Generate the received phase from emission time, rather than
                        // numerically integrating the frequency curve used by the fitter.
                        let te = retarded_tau(i as f64 / sr as f64, t0e, v, d, c);
                        // Spherical spreading gives the received tone its physical 1/r envelope.
                        let amplitude = 0.2 * d / d.hypot(v * te);
                        let signal = amplitude * (std::f64::consts::TAU * f0 * te + phase).sin();
                        (signal + 0.05 * (2.0 * seed as f64 / u64::MAX as f64 - 1.0)) as f32
                    })
                    .collect();
                let det = Detection {
                    id: 0,
                    t0r: t0r + 0.3,
                    f0: f0 + 0.11,
                    v: v * 1.03,
                    d: d * 1.08,
                    score: 10.0,
                    score_approach: 10.0,
                    score_recede: 10.0,
                    half_s: 60.0,
                    harmonic_of: None,
                    harmonics: Vec::new(),
                    from_contrast: false,
                };
                let event = measure(&x, sr, &det, "sub_bin", &p);
                eprintln!(
                    "curved audio f0={f0} v={v} d={d} phase={phase}: fit {:?}, {} points, drop {}, side spans {}/{} s",
                    event.fit.as_ref().map(|f| f.best),
                    event.trace.len(),
                    event.quality.drop_traversed,
                    event.quality.span_approach_s,
                    event.quality.span_recede_s
                );
                assert!(
                    event.quality.ok,
                    "f0={f0} v={v} d={d} phase={phase}: {:?}",
                    event.quality.reasons
                );
                let b = event.fit.as_ref().unwrap().best;
                let distance_error = (b.d - d).abs();
                eprintln!(
                    "curved audio f0={f0} v={v} d={d} phase={phase}: distance error {distance_error:.3} m, speed error {:.4} m/s, CPA error {:.5} s",
                    b.v - v,
                    b.t0r - t0r
                );
                max_distance_fraction = max_distance_fraction.max(distance_error / d);
                assert!((b.v - v).abs() < 0.5, "f0={f0} v={v}: {}", b.v);
                assert!((b.t0r - t0r).abs() < 0.1, "f0={f0} t0r={t0r}: {}", b.t0r);
            }
        }
        assert!(max_distance_fraction < 0.005, "{max_distance_fraction}");
    }

    #[test]
    fn drop_traversed_counts_what_a_ridge_travels() {
        // a fit falling linearly from 110 to 90 Hz between t = 0 and t = 100 s
        let (f_pre, f_post) = (110.0, 90.0);
        let model = |t: f64| f_pre - (f_pre - f_post) * (t / 100.0).clamp(0.0, 1.0);
        let score = |ridge: &dyn Fn(f64) -> Option<f64>| {
            let (t, f): (Vec<f64>, Vec<f64>) = (-500..1500)
                .map(|k| k as f64 * 0.1)
                .filter_map(|t| ridge(t).map(|f| (t, f)))
                .unzip();
            drop_traversed(&t, &f, f_pre, f_post, model)
        };
        // each stretch loses about 1 s of its fall to the medians at its ends
        let near = |got: f64, want: f64| assert!((got - want).abs() < 0.01, "{got} vs {want}");
        near(score(&|t| Some(model(t))), 0.99);
        // half the drop, then silence
        near(score(&|t| (t < 50.0).then(|| model(t))), 0.49);
        // two stretches, the fall across the silence between them not observed
        near(
            score(&|t| (!(40.0..60.0).contains(&t)).then(|| model(t))),
            0.73,
        );
        // a short silence does not end the stretch
        near(
            score(&|t| (!(40.0..42.0).contains(&t)).then(|| model(t))),
            0.99,
        );
        // steady tones outside the drop, then within it
        near(
            score(&|t| {
                (!(0.0..100.0).contains(&t)).then_some(if t < 0.0 { f_pre } else { f_post })
            }),
            0.0,
        );
        near(
            score(&|t| (!(40.0..60.0).contains(&t)).then_some(if t < 50.0 { 104.0 } else { 96.0 })),
            0.0,
        );
        // a ladder of steady tones down the drop: the hops between them are not a fall
        near(
            score(&|t| Some(f_pre - 6.0 * (t.clamp(0.0, 99.9) / 25.0).floor())),
            0.0,
        );
        // a ridge the trace wanders on, within what a clean fit's band allows
        let wander = score(&|t| Some(model(t) + (t * 37.0).sin()));
        assert!(wander > 0.9, "{wander}");
        // a rising stretch, and stretches too short to mean anything
        near(score(&|t| Some(2.0 * f_post - model(t))), 0.0);
        near(
            score(&|t| ((t * 10.0).round() as i64 % 40 < 4).then(|| model(t))),
            0.0,
        );
        // no drop to travel
        assert_eq!(drop_traversed(&[1.0], &[100.0], 90.0, 90.0, model), 0.0);
        assert_eq!(drop_traversed(&[1.0], &[100.0], f64::NAN, 90.0, model), 0.0);
    }

    /// Passes of every kind: an ordinary one, slow and fast, and steep ones up to 76 Hz/s at the CPA.
    const PASSES: [(f64, f64, f64); 8] = [
        (100.0, 200.0, 3000.0),
        (70.0, 40.0, 500.0),
        (118.0, 120.0, 3000.0),
        (90.0, 70.0, 8000.0),
        (100.0, 140.0, 300.0),
        (150.0, 100.0, 300.0),
        (100.0, 100.0, 200.0),
        (400.0, 140.0, 300.0),
    ];

    #[test]
    fn a_curve_with_no_ridge_through_its_drop_is_not_a_flyover() {
        let silent = Some((0.03, 0.97));
        let p = MeasureParams::default();
        let unchecked = MeasureParams {
            min_drop_traversed: 0.0,
            ..Default::default()
        };
        for (f0, v, d) in PASSES {
            for tone_amp in [0.2, 0.05] {
                let ev = measured_pass(f0, v, d, tone_amp, silent, &p);
                let label = format!("f0 {f0} v {v} d {d} amp {tone_amp}");
                assert!(
                    ev.quality.drop_traversed < p.min_drop_traversed,
                    "{label}: {}",
                    ev.quality.drop_traversed
                );
                assert!(!ev.quality.ok, "{label}");
                assert!(
                    ev.quality
                        .reasons
                        .iter()
                        .any(|r| r.contains("frequency drop")),
                    "{label}: {:?}",
                    ev.quality.reasons
                );
            }
        }
        // steeper than its window at first sight: judged only once the fit has rated it
        let ev = measured_pass(70.0, 80.0, 200.0, 0.05, silent, &p);
        assert!(
            !ev.quality.ok
                && ev
                    .quality
                    .reasons
                    .iter()
                    .any(|r| r.contains("frequency drop")),
            "{} {:?}",
            ev.quality.drop_traversed,
            ev.quality.reasons
        );
        // the only gate that notices an ordinary one
        let ev = measured_pass(118.0, 120.0, 3000.0, 0.2, silent, &unchecked);
        assert!(ev.quality.ok, "{:?}", ev.quality.reasons);
    }

    #[test]
    fn real_passes_keep_their_ridge_at_any_chirp() {
        let p = MeasureParams::default();
        for (f0, v, d) in PASSES {
            for tone_amp in [0.2, 0.05] {
                let ev = measured_pass(f0, v, d, tone_amp, None, &p);
                assert!(
                    ev.quality.ok && ev.quality.drop_traversed > 0.4,
                    "f0 {f0} v {v} d {d} amp {tone_amp}: {} {:?}",
                    ev.quality.drop_traversed,
                    ev.quality.reasons
                );
            }
        }
        // a faint tone whose CPA chirp is what the main window just holds
        let ev = measured_pass(70.0, 140.0, 1000.0, 0.012, None, &p);
        assert!(
            ev.quality.ok,
            "{} {:?}",
            ev.quality.drop_traversed, ev.quality.reasons
        );
    }

    #[test]
    fn a_steep_pass_is_followed_when_its_detection_rates_it_too_gently() {
        let p = MeasureParams::default();
        for (f0, v, d) in [
            (100.0, 140.0, 300.0),
            (150.0, 100.0, 300.0),
            (100.0, 100.0, 200.0),
            (400.0, 140.0, 300.0),
            (150.0, 80.0, 300.0),
            (150.0, 140.0, 1000.0),
        ] {
            for tone_amp in [0.2, 0.05] {
                let ev = measured_pass_detected_at(f0, v, d, 2.0 * d, tone_amp, None, &p);
                assert!(
                    ev.quality.ok && ev.quality.drop_traversed > 0.4,
                    "f0 {f0} v {v} d {d} amp {tone_amp}: {} {:?}",
                    ev.quality.drop_traversed,
                    ev.quality.reasons
                );
            }
        }
    }

    #[test]
    fn detect_then_measure_follows_a_steep_pass() {
        let sr = 4000u32;
        let (f0, v, d, t0e) = (100.0, 140.0, 300.0, 300.0);
        let (mut seed, mut phase) = (4321u64, 0.0f64);
        let x: Vec<f32> = (0..sr as usize * 600)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let t = i as f64 / sr as f64;
                phase += std::f64::consts::TAU * cv_curve(t, f0, v, d, t0e, 343.0) / sr as f64;
                (0.2 * phase.sin() + 0.3 * (2.0 * seed as f64 / u64::MAX as f64 - 1.0)) as f32
            })
            .collect();
        let dp = DetectParams::default();
        let (ls, lin) = survey(&x, sr, &dp).unwrap();
        let dets = detect(&ls, &dp);
        let det = refine(&lin, &dets[0], 343.0);
        let ev = measure(&x, sr, &det, "e1", &MeasureParams::default());
        let fr = ev.fit.as_ref().expect("fit");
        assert!(
            ev.quality.ok && ev.quality.drop_traversed > 0.4,
            "detected as d {} v {}: {} {:?}",
            det.d,
            det.v,
            ev.quality.drop_traversed,
            ev.quality.reasons
        );
        assert!((fr.best.v - v).abs() < 7.0, "v {}", fr.best.v);
        assert!((fr.best.d / d - 1.0).abs() < 0.2, "d {}", fr.best.d);
    }

    #[test]
    fn deferred_diagnostics_match_eager_refinement() {
        let sr = 4000u32;
        let c = 343.0;
        let det = Detection {
            id: 0,
            t0r: 60.0,
            f0: 118.0,
            v: 120.0,
            d: 3000.0,
            score: 10.0,
            score_approach: 10.0,
            score_recede: 10.0,
            half_s: 60.0,
            harmonic_of: None,
            harmonics: Vec::new(),
            from_contrast: false,
        };
        let mut seed = 789u64;
        let mut phase = 0.0;
        let tone = model_fn(det.f0, det.v, det.d, det.t0r, c);
        let x: Vec<f32> = (0..sr as usize * 120)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let t = i as f64 / sr as f64;
                phase += 2.0 * std::f64::consts::PI * tone(t) / sr as f64;
                (0.2 * phase.sin() + 0.3 * (2.0 * seed as f64 / u64::MAX as f64 - 1.0)) as f32
            })
            .collect();
        let p = MeasureParams {
            hop_s: 0.25,
            ..Default::default()
        };
        let event = measure(&x, sr, &det, "e1", &p);
        assert!(event.quality.ok, "{:?}", event.quality.reasons);
        // This short recording lies entirely inside the analysis window, so the saved
        // spectrogram is the same one used by every refinement round, without cropping.
        let spec = event.spec.as_ref().unwrap();
        assert!(spec.t[0] < 1.0 && spec.t[spec.nt() - 1] > 119.0);
        let mut raw = stft_band_db(
            &x,
            sr,
            event.win_s,
            p.hop_s,
            spec.meta.fmin,
            spec.meta.fmax,
            p.df_max,
            0.0,
        )
        .unwrap();
        let background = raw.normalize_rows_with_offsets(None);
        assert_eq!(raw.db, spec.db);
        let mut model: Box<dyn Fn(f64) -> f64 + Sync> = Box::new(tone);
        let mut band = (0.04 * det.f0).clamp(3.0, 12.0);
        let mut eager_trace = Trace::default();
        let mut eager_fit: Option<FitResult> = None;
        // Complete uncertainty diagnostics eagerly in every discovery and raw-metrology
        // round. The deferred path must produce the same trace and full final fit.
        for raw_only in [false, true] {
            if raw_only {
                let b = eager_fit.as_ref().unwrap().best;
                band = (3.0 * b.rmse).clamp(1.0, (0.04 * b.f0).clamp(3.0, 12.0));
            }
            for _ in 0..p.rounds {
                let trace = guided_trace_with_background(
                    spec,
                    Some(&background),
                    None,
                    &*model,
                    0.0,
                    120.0,
                    band,
                    p.min_snr,
                    p.min_z,
                    raw_only,
                );
                assert!(trace.len() >= 20);
                let fit = fit_constant_velocity(&trace.t, &trace.f, c, Side::Both).unwrap();
                let b = fit.best;
                model = Box::new(model_fn(b.f0, b.v, b.d, b.t0r, c));
                band = (3.0 * b.rmse).clamp(1.0, band);
                eager_trace = trace;
                eager_fit = Some(fit);
            }
        }
        assert_eq!(event.trace.t, eager_trace.t);
        assert_eq!(event.trace.f, eager_trace.f);
        assert_eq!(event.trace.snr, eager_trace.snr);
        assert_eq!(
            serde_json::to_value(&event.fit).unwrap(),
            serde_json::to_value(&eager_fit).unwrap()
        );
        assert_eq!(event.quality.n_points, event.trace.len());
    }

    #[test]
    fn detect_then_measure_recovers_parameters() {
        let sr = 4000u32;
        let (f0, v, d, t0e) = (118.0, 120.0, 3000.0, 300.0);
        let n = sr as usize * 600;
        let mut seed = 99u64;
        let mut phase = 0.0f64;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let t = i as f64 / sr as f64;
                phase += 2.0 * std::f64::consts::PI * cv_curve(t, f0, v, d, t0e, 343.0) / sr as f64;
                (0.08 * phase.sin() + 0.9 * ((seed as f64 / u64::MAX as f64) * 2.0 - 1.0)) as f32
            })
            .collect();
        let dp = DetectParams::default();
        let (ls, lin) = survey(&x, sr, &dp).unwrap();
        let dets = detect(&ls, &dp);
        let det = refine(&lin, &dets[0], 343.0);
        let ev = measure(&x, sr, &det, "e1", &MeasureParams::default());
        let fr = ev.fit.as_ref().expect("fit");
        assert!(ev.quality.ok, "{:?}", ev.quality.reasons);
        assert!(
            ev.quality.drop_traversed > 0.8,
            "{}",
            ev.quality.drop_traversed
        );
        let t0r = t0e + d / 343.0;
        assert!((fr.best.t0r - t0r).abs() < 0.3, "t0r {}", fr.best.t0r);
        assert!((fr.best.f0 - f0).abs() < 0.3, "f0 {}", fr.best.f0);
        assert!((fr.best.v - v).abs() < 4.0, "v {}", fr.best.v);
        assert!((fr.best.d / d - 1.0).abs() < 0.08, "d {}", fr.best.d);
    }

    /// The locate path, including family fallback and its independent-event counting, with
    /// the original coarse detection retained beside each measurement for stage provenance.
    fn survey_measure_families(
        x: &[f32],
        dp: &DetectParams,
    ) -> (Vec<Detection>, Vec<MeasuredEvent>) {
        let (ls, lin) = survey(x, 4000, dp).unwrap();
        let detections = detect(&ls, dp);
        let primary: Vec<_> = detections
            .iter()
            .filter(|d| d.harmonic_of.is_none() && d.score >= dp.thr_silence_db)
            .collect();
        let p = MeasureParams::default();
        let events = measure_ranked_counted(
            primary.len(),
            MAX_MEASURED,
            |ranks| {
                ranks
                    .map(|k| {
                        measure_family(x, 4000, &lin, primary[k], &detections, &format!("e{k}"), &p)
                    })
                    .collect()
            },
            |events| crate::search::independent_measured_events(events, 0.0).len(),
        );
        (
            primary.into_iter().take(events.len()).cloned().collect(),
            events,
        )
    }

    #[test]
    fn continuation_refuses_a_lost_trace_before_ordinary_rounds_complete() {
        // Inject loss of the replacement short-window observations after a successful main
        // trace. The main spectrum carries a physical ridge; replacement audio is silent.
        // This isolates the short-trace exit without changing fit/gate thresholds.
        let p = MeasureParams::default();
        let start = (200.0, 200.0, 3000.0, 20.0);
        let model = model_fn(start.0, start.1, start.2, start.3, p.c);
        let t: Vec<_> = (0..396).map(|i| 0.25 + 0.1 * f64::from(i)).collect();
        let f: Vec<_> = (128..=2048)
            .map(|i| f64::from(i) * 4000.0 / 16_384.0)
            .collect();
        let mut db = Vec::with_capacity(t.len() * f.len());
        for (row, &frequency) in f.iter().enumerate() {
            for (col, &time) in t.iter().enumerate() {
                let ridge = 18.0 * (-0.5 * ((frequency - model(time)) / 0.7).powi(2)).exp();
                db.push((ridge + 0.08 * ((row * 37 + col * 13) as f64).sin()) as f32);
            }
        }
        let spec = Spec {
            t,
            f,
            db,
            meta: crate::spectro::SpecMeta {
                sr: 4000,
                win_s: 2.0,
                hop_s: 0.1,
                nfft: 16_384,
                df: 4000.0 / 16_384.0,
                fmin: 31.25,
                fmax: 500.0,
                norm: "injected normalized physical ridge".into(),
            },
        };
        let x = vec![0.0; 40 * 4000];
        let background = vec![0.0; spec.nf()];
        let frames = Frames {
            raw_only: false,
            x: &x,
            sr: 4000,
            s0: 0,
            s1: x.len(),
            ca: 0.0,
            fmin: spec.meta.fmin,
            fmax: spec.meta.fmax,
            spec: &spec,
            background: &background,
            ta: 0.0,
            tb: 40.0,
        };
        let one = MeasureParams {
            rounds: 1,
            ..p.clone()
        };
        // The initial chirp estimate keeps the main spectrum for round one; the measured
        // fast fit then requests a shorter spectrum, whose injected observations are absent.
        let first = guided_rounds(
            &frames,
            start,
            4.0,
            1.0,
            &one,
            None,
            &mut prepare_constant_velocity,
        );
        assert!(passes(&first, start.3, 60.0, &p));
        let fitted = first.prepared.as_ref().unwrap();
        assert!(
            contiguous_side_support(fitted.inlier_times(), fitted.best().t0r)
                .complete(p.min_side_s)
        );
        assert!(
            window_for_roc(peak_received_chirp(
                fitted.best().f0,
                fitted.best().v,
                fitted.best().d,
                p.c
            )) < MIN_MAIN_WIN_S
        );
        let legacy = guided_rounds(
            &frames,
            start,
            4.0,
            1.0,
            &p,
            None,
            &mut prepare_constant_velocity,
        );
        assert!(
            passes(&legacy, start.3, 60.0, &p),
            "legacy behavior changed"
        );
        let failed = guided_rounds(
            &frames,
            start,
            4.0,
            1.0,
            &p,
            Some(SideContinuation {
                det_t0r: start.3,
                span: 60.0,
                covered_s: 0.0,
            }),
            &mut prepare_constant_velocity,
        );
        assert!(!passes(&failed, start.3, 60.0, &p));
        assert_eq!(failed.reasons, ["guided trace found only 0 ridge points"]);
        assert_eq!(
            serde_json::to_value(&failed.trace).unwrap(),
            serde_json::to_value(&first.trace).unwrap()
        );
        assert_eq!(
            serde_json::to_value(failed.prepared.as_ref().unwrap().best()).unwrap(),
            serde_json::to_value(fitted.best()).unwrap()
        );
    }

    #[test]
    fn short_fast_passes_recover_the_same_detected_ridge_after_refusal() {
        let pass = (118.0, 215.0, 900.0, 10.13);
        let dp = DetectParams::default();
        let p = MeasureParams::default();
        for attenuation in [0, 6, 9, 12] {
            let mut x = crate::detect::fast_pass_scene(
                pass,
                0.32 * 10.0f64.powf(-f64::from(attenuation) / 20.0),
            );
            x.truncate(30 * 4000);
            let (ls, lin) = survey(&x, 4000, &dp).unwrap();
            let detections = detect(&ls, &dp);
            let primary = detections
                .iter()
                .find(|d| d.harmonic_of.is_none() && d.score >= dp.thr_silence_db)
                .unwrap();
            let refined = refine(&lin, primary, p.c);
            let legacy = measure_with_fit(
                &x,
                4000,
                &refined,
                "e0",
                &p,
                false,
                prepare_constant_velocity,
            );
            assert!(
                !legacy.is_aircraft_like(),
                "{attenuation} dB: {:?}",
                legacy.quality
            );
            assert_eq!(
                legacy.quality.reasons.len(),
                1,
                "{attenuation} dB: {:?}",
                legacy.quality
            );
            assert!(legacy.quality.span_approach_s < p.min_side_s);

            let (_, events) = survey_measure_families(&x, &dp);
            let independent = crate::search::independent_measured_events(&events, 0.0);
            let diagnostic: Vec<_> = events
                .iter()
                .map(|e| (&e.quality, e.fit.as_ref().map(|f| f.best)))
                .collect();
            assert_eq!(independent.len(), 1, "{attenuation} dB: {diagnostic:?}");
            let event = events.iter().find(|e| e.id == independent[0].id).unwrap();
            let b = event.fit.as_ref().unwrap().best;
            eprintln!(
                "continued fast edge {attenuation} dB: {b:?}; {:?}",
                event.quality
            );
            assert!(event.quality.reseeded);
            assert_eq!(
                serde_json::to_value(&event.detection).unwrap(),
                serde_json::to_value(&refined).unwrap(),
                "same detected event, {attenuation} dB"
            );
            assert!((b.f0 - pass.0).abs() < 0.3, "{attenuation} dB: {b:?}");
            assert!((b.v - pass.1).abs() < 4.0, "{attenuation} dB: {b:?}");
            assert!((b.d / pass.2 - 1.0).abs() < 0.08, "{attenuation} dB: {b:?}");
            assert!((b.t0r - pass.3).abs() < 0.3, "{attenuation} dB: {b:?}");
            let spec = event.spec.as_ref().unwrap();
            assert!(
                event
                    .trace
                    .f
                    .iter()
                    .all(|f| *f >= spec.f[0] && *f <= spec.f[spec.nf() - 1])
            );
            if attenuation == 0 {
                let old_top = *legacy.spec.as_ref().unwrap().f.last().unwrap();
                assert!(
                    event.trace.f.iter().any(|f| *f > old_top),
                    "the previously cropped approach was not restored"
                );
            }
        }
    }

    fn assert_same_measurement(a: &MeasuredEvent, b: &MeasuredEvent) {
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap()
        );
        match (&a.spec, &b.spec) {
            (Some(a), Some(b)) => {
                assert_eq!(a.t, b.t);
                assert_eq!(a.f, b.f);
                assert_eq!(a.db, b.db);
                assert_eq!(
                    serde_json::to_value(&a.meta).unwrap(),
                    serde_json::to_value(&b.meta).unwrap()
                );
            }
            (None, None) => {}
            _ => panic!("spectrogram presence changed"),
        }
    }

    #[test]
    fn fitted_continuation_recovers_a_long_tail_with_a_missing_central_drop() {
        let pass = (83.0, 245.0, 700.0, 169.37);
        let x = crate::detect::fast_pass_scene(pass, 0.32);
        let dp = DetectParams::default();
        let p = MeasureParams::default();
        let (ls, lin) = survey(&x, 4000, &dp).unwrap();
        let detections = detect(&ls, &dp);
        let primary = detections
            .iter()
            .find(|d| d.harmonic_of.is_none() && d.score >= dp.thr_silence_db)
            .unwrap();
        let refined = refine(&lin, primary, p.c);
        let legacy = measure_with_fit(
            &x,
            4000,
            &refined,
            "tail",
            &p,
            false,
            prepare_constant_velocity,
        );
        assert!(!legacy.is_aircraft_like());
        assert!(legacy.quality.span_approach_s >= p.min_side_s);
        assert!(legacy.quality.span_recede_s >= p.min_side_s);
        assert!(legacy.quality.drop_traversed < p.min_drop_traversed);
        assert_eq!(legacy.quality.reasons.len(), 1);
        assert!(legacy.quality.reasons[0].contains("frequency drop"));

        // Capture the real solver's retained times, rather than inferring support from an
        // extrapolated fit or outliers that the solver discarded.
        let mut retained = Vec::new();
        let mut rounds = Vec::new();
        let event = measure_with_fit(&x, 4000, &refined, "tail", &p, true, |t, f, c, side| {
            let prepared = prepare_constant_velocity(t, f, c, side)?;
            retained = prepared.inlier_times().to_vec();
            rounds.push(prepared.best());
            Ok(prepared)
        });
        let b = event.fit.as_ref().unwrap().best;
        eprintln!(
            "continued long tail: {b:?}; {:?}; rounds {rounds:?}",
            event.quality
        );
        assert!(event.is_aircraft_like(), "{:?}", event.quality);
        assert!(event.quality.reseeded);
        assert!(event.quality.drop_traversed >= p.min_drop_traversed);
        assert!(contiguous_side_support(&retained, b.t0r).complete(p.min_side_s));
        assert_eq!(
            serde_json::to_value(&event.detection).unwrap(),
            serde_json::to_value(&refined).unwrap(),
            "continuation must keep the actual detected seed"
        );
        assert!((b.f0 - pass.0).abs() < 0.3, "{b:?}");
        assert!((b.v - pass.1).abs() < 4.0, "{b:?}");
        assert!((b.d / pass.2 - 1.0).abs() < 0.08, "{b:?}");
        assert!((b.t0r - pass.3).abs() < 0.3, "{b:?}");
    }

    #[test]
    fn fitted_continuation_preserves_missing_audio_and_false_continuity_refusals() {
        let pass = (83.0, 245.0, 700.0, 169.37);
        let p = MeasureParams::default();
        let noise = crate::detect::fast_pass_scene(pass, 0.0);
        let source = crate::detect::fast_pass_scene(pass, 0.32);
        let truth = model_fn(pass.0, pass.1, pass.2, pass.3, p.c);
        let asymptotes = readouts(pass.0, pass.1, pass.2, p.c);
        let det = Detection {
            id: 0,
            f0: pass.0,
            v: pass.1,
            d: pass.2,
            t0r: pass.3,
            score: 10.0,
            score_approach: 10.0,
            score_recede: 10.0,
            half_s: 10.0,
            harmonic_of: None,
            harmonics: Vec::new(),
            from_contrast: false,
        };
        for mode in ["central gap", "one-sided", "steady asymptotes"] {
            let x: Vec<f32> = source
                .iter()
                .zip(&noise)
                .enumerate()
                .map(|(i, (&signal, &noise))| {
                    let t = i as f64 / 4000.0;
                    let way =
                        (asymptotes.f_pre - truth(t)) / (asymptotes.f_pre - asymptotes.f_post);
                    match mode {
                        "central gap" if (0.03..=0.97).contains(&way) => noise,
                        "one-sided" if t >= pass.3 => noise,
                        "steady asymptotes" => {
                            let f = if t < pass.3 {
                                asymptotes.f_pre
                            } else {
                                asymptotes.f_post
                            };
                            noise + (0.32 * (std::f64::consts::TAU * f * t).sin()) as f32
                        }
                        _ => signal,
                    }
                })
                .collect();
            // Even exact physical guidance cannot invent a missing section of observed ridge.
            let old = measure_with_fit(&x, 4000, &det, mode, &p, false, prepare_constant_velocity);
            let event = measure(&x, 4000, &det, mode, &p);
            eprintln!("continued refusal {mode}: {:?}", event.quality);
            assert!(!old.is_aircraft_like(), "{mode}: {:?}", old.quality);
            assert!(!event.is_aircraft_like(), "{mode}: {:?}", event.quality);
            assert_same_measurement(&old, &event);
        }
    }

    #[test]
    fn fitted_continuation_preserves_accepted_short_events() {
        let dp = DetectParams::default();
        let p = MeasureParams::default();
        // The early 3 dB pass already succeeds through free tracing. Some accepted late-pass
        // fits still have the wrong geometry: this recovery cannot replace accepted results,
        // and must not claim to fix those distinct failures.
        for (pass, attenuation) in [
            ((118.0, 215.0, 900.0, 10.13), 3),
            ((83.0, 245.0, 700.0, 19.37), 3),
            ((83.0, 245.0, 700.0, 19.37), 6),
        ] {
            let mut x = crate::detect::fast_pass_scene(
                pass,
                0.32 * 10.0f64.powf(-f64::from(attenuation) / 20.0),
            );
            x.truncate(30 * 4000);
            let (_, events) = survey_measure_families(&x, &dp);
            assert_eq!(
                crate::search::independent_measured_events(&events, 0.0).len(),
                1
            );
            let mut preserved = 0;
            for new in events {
                let old = measure_with_fit(
                    &x,
                    4000,
                    &new.detection,
                    &new.id,
                    &p,
                    false,
                    prepare_constant_velocity,
                );
                assert_same_measurement(&old, &new);
                if old.is_aircraft_like() {
                    preserved += 1;
                    eprintln!(
                        "unchanged accepted short pass v={} {attenuation} dB: {:?}",
                        pass.1,
                        new.fit.as_ref().unwrap().best
                    );
                }
            }
            assert!(
                preserved > 0,
                "v={} {attenuation} dB: no accepted control",
                pass.1
            );
        }
    }

    #[test]
    fn fitted_continuation_preserves_refused_late_fast_passes() {
        let pass = (83.0, 245.0, 700.0, 19.37);
        let dp = DetectParams::default();
        let p = MeasureParams::default();
        // A generic steep-ridge continuation promoted the 9 dB refusal to a wrong 159 m/s fit.
        // Its original fit was inside the primary bank's domain, so it did not establish the
        // missing-fast-bank failure this continuation repairs. The incoming retained-support
        // gates also refuse the 12 dB row. Keep all three outputs unchanged; the already
        // accepted late rows are checked separately above.
        for attenuation in [0, 9, 12] {
            let mut x = crate::detect::fast_pass_scene(
                pass,
                0.32 * 10.0f64.powf(-f64::from(attenuation) / 20.0),
            );
            x.truncate(30 * 4000);
            let (_, events) = survey_measure_families(&x, &dp);
            assert!(!events.is_empty(), "{attenuation} dB: no refused control");
            assert!(crate::search::independent_measured_events(&events, 0.0).is_empty());
            for event in events {
                let old = measure_with_fit(
                    &x,
                    4000,
                    &event.detection,
                    &event.id,
                    &p,
                    false,
                    prepare_constant_velocity,
                );
                assert!(
                    !old.is_aircraft_like(),
                    "{attenuation} dB: {:?}",
                    old.quality
                );
                assert!(
                    !event.is_aircraft_like(),
                    "{attenuation} dB: {:?}",
                    event.quality
                );
                assert_same_measurement(&old, &event);
                eprintln!(
                    "unchanged refused late pass {attenuation} dB: {:?}; {:?}",
                    event.fit.as_ref().map(|f| f.best),
                    event.quality.reasons
                );
                if attenuation == 9 {
                    assert!(old.fit.as_ref().unwrap().best.v <= PRIMARY_MAX_SPEED_MPS);
                }
            }
        }
    }

    #[test]
    fn fitted_continuation_refuses_noise_tones_and_reversed_passes() {
        let early = (118.0, 215.0, 900.0, 10.13);
        let mut noise = crate::detect::fast_pass_scene(early, 0.0);
        noise.truncate(30 * 4000);
        let mut steady = noise.clone();
        for (i, sample) in steady.iter_mut().enumerate() {
            *sample += (0.32 * (std::f64::consts::TAU * 118.0 * i as f64 / 4000.0).sin()) as f32;
        }
        let mut cases = vec![("noise and hum", noise), ("steady tone", steady)];
        for (label, pass) in [
            ("reversed early", early),
            ("reversed late", (83.0, 245.0, 700.0, 19.37)),
        ] {
            let mut reversed = crate::detect::fast_pass_scene(pass, 0.32);
            reversed.truncate(30 * 4000);
            reversed.reverse();
            cases.push((label, reversed));
        }
        let dp = DetectParams::default();
        let p = MeasureParams::default();
        for (label, x) in cases {
            let (_, events) = survey_measure_families(&x, &dp);
            assert!(
                crate::search::independent_measured_events(&events, 0.0).is_empty(),
                "{label}"
            );
            for event in events {
                assert!(!event.is_aircraft_like(), "{label}: {:?}", event.quality);
                let old = measure_with_fit(
                    &x,
                    4000,
                    &event.detection,
                    &event.id,
                    &p,
                    false,
                    prepare_constant_velocity,
                );
                assert_same_measurement(&old, &event);
            }
        }
    }

    #[test]
    fn fast_passes_survive_detection_refinement_and_measurement() {
        use crate::detect::{FAST_PASSES, PRIMARY_MAX_SPEED_MPS, fast_pass_scene};
        let dp = DetectParams::default();
        let mut supplemental_recoveries = 0;
        let check = |(f0, v, d, t0r): (f64, f64, f64, f64),
                     seeds: &[Detection],
                     events: &[MeasuredEvent]| {
            let diagnostic: Vec<_> = seeds
                .iter()
                .zip(events)
                .map(|(seed, event)| {
                    (
                        seed.v,
                        seed.f0,
                        seed.score,
                        event.quality.ok,
                        event.fit.as_ref().map(|f| f.best),
                    )
                })
                .collect();
            let independent = crate::search::independent_measured_events(events, 0.0);
            eprintln!("fast {v} m/s: {diagnostic:?}");
            assert_eq!(independent.len(), 1, "{v} m/s: {diagnostic:?}");
            let rank = events
                .iter()
                .position(|event| event.id == independent[0].id)
                .unwrap();
            let event = &events[rank];
            let b = event.fit.as_ref().unwrap().best;
            assert!((b.f0 - f0).abs() < 0.3, "{v} m/s: {b:?}");
            assert!((b.t0r - t0r).abs() < 0.3, "{v} m/s: {b:?}");
            assert!((b.v - v).abs() < 4.0, "{v} m/s: {b:?}");
            assert!((b.d / d - 1.0).abs() < 0.08, "{v} m/s: {b:?}");
            let seed = &seeds[rank];
            if seed.v > PRIMARY_MAX_SPEED_MPS {
                assert!(seed.score >= dp.thr_db);
                true
            } else {
                false
            }
        };
        for pass in FAST_PASSES {
            let x = fast_pass_scene(pass, 0.16);
            let (seeds, events) = survey_measure_families(&x, &dp);
            supplemental_recoveries += usize::from(check(pass, &seeds, &events));
        }

        // Sweep a fixed, complete 3 dB attenuation ladder for both faster scenes. Loud
        // passes can already be fitted from a poorly matching old seed; a new template
        // earns a recovery only if it supplies an accurate event where the old bank
        // supplies none. Every newly detected case is measured; none is selected by fit.
        let off = DetectParams {
            fast_templates: false,
            ..dp.clone()
        };
        for pass in [FAST_PASSES[1], FAST_PASSES[2]] {
            for quieter_db in (0..=18).step_by(3) {
                let amplitude = 0.16 * 10.0f64.powf(-(quieter_db as f64) / 20.0);
                let x = fast_pass_scene(pass, amplitude);
                let (ls, _) = survey(&x, 4000, &dp).unwrap();
                let old = detect(&ls, &off);
                let new = detect(&ls, &dp);
                let added: Vec<_> = new
                    .iter()
                    .filter(|d| d.v > PRIMARY_MAX_SPEED_MPS && d.harmonic_of.is_none())
                    .collect();
                eprintln!(
                    "fast ladder v={} attenuation={quieter_db}dB: {} original seeds, added {added:?}",
                    pass.1,
                    old.len()
                );
                if added.is_empty() {
                    continue;
                }
                let (_, baseline) = survey_measure_families(&x, &off);
                assert!(
                    crate::search::independent_measured_events(&baseline, 0.0).is_empty(),
                    "{} m/s, {quieter_db} dB: the original bank already supplied an event",
                    pass.1
                );
                let (seeds, events) = survey_measure_families(&x, &dp);
                assert!(
                    check(pass, &seeds, &events),
                    "{} m/s, {quieter_db} dB: no supplemental event",
                    pass.1
                );
                supplemental_recoveries += 1;
            }
        }
        assert!(
            supplemental_recoveries >= 1,
            "no supplemental detection supplied an aircraft event"
        );
    }

    #[test]
    fn fast_bank_noise_hum_and_reversed_passes_supply_no_aircraft_event() {
        use crate::detect::{FAST_PASSES, fast_pass_scene};
        let mut cases = vec![("noise and hum", fast_pass_scene(FAST_PASSES[0], 0.0))];
        for (label, pass) in [
            ("reversed 215 m/s", FAST_PASSES[1]),
            ("reversed 245 m/s", FAST_PASSES[2]),
        ] {
            let mut reversed = fast_pass_scene(pass, 0.16);
            reversed.reverse();
            cases.push((label, reversed));
        }
        for (label, x) in cases {
            let (seeds, events) = survey_measure_families(&x, &DetectParams::default());
            let accepted: Vec<_> = seeds
                .iter()
                .zip(&events)
                .filter(|(_, event)| event.is_aircraft_like())
                .map(|(seed, event)| (seed, event.fit.as_ref().unwrap().best))
                .collect();
            assert!(accepted.is_empty(), "{label}: {accepted:?}");
        }
    }

    fn simultaneous_same_tone_audio(amplitudes: [f64; 2]) -> Vec<f32> {
        let sr = 4000u32;
        let (f0, v, t0r, c) = (118.0, 120.0, 180.0, 343.0);
        let distances = [1000.0, 7000.0];
        let mut phases = [0.0f64; 2];
        let mut seed = 99u64;
        (0..sr as usize * 360)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let t = i as f64 / sr as f64;
                let mut sample = 0.5 * (2.0 * seed as f64 / u64::MAX as f64 - 1.0);
                for k in 0..2 {
                    phases[k] += std::f64::consts::TAU
                        * cv_curve(t, f0, v, distances[k], t0r - distances[k] / c, c)
                        / sr as f64;
                    sample += amplitudes[k] * phases[k].sin();
                }
                sample as f32
            })
            .collect()
    }

    fn measure_simultaneous_same_tone_passes(amplitudes: [f64; 2]) -> Vec<MeasuredEvent> {
        let (sr, c) = (4000u32, 343.0);
        let x = simultaneous_same_tone_audio(amplitudes);
        let dp = DetectParams::default();
        let (ls, lin) = survey(&x, sr, &dp).unwrap();
        let started = std::time::Instant::now();
        let detections = detect(&ls, &dp);
        let strong = detections
            .iter()
            .filter(|d| d.harmonic_of.is_none() && d.score >= dp.thr_db)
            .count();
        eprintln!(
            "same-tone {amplitudes:?}: {} detections, {strong} strong primaries, detection {:.2}s",
            detections.len(),
            started.elapsed().as_secs_f64()
        );
        let events: Vec<_> = detections
            .into_iter()
            .filter(|d| d.harmonic_of.is_none() && d.score >= dp.thr_db)
            .take(MAX_MEASURED)
            .enumerate()
            .map(|(k, d)| {
                let refined = refine(&lin, &d, c);
                measure(
                    &x,
                    sr,
                    &refined,
                    &format!("e{k}"),
                    &MeasureParams::default(),
                )
            })
            .collect();
        eprintln!(
            "same-tone {amplitudes:?}: {} measured seeds, {} quality-passing fits",
            events.len(),
            events.iter().filter(|e| e.quality.ok).count()
        );
        events
    }

    fn independent_measured_passes(events: &[MeasuredEvent]) -> usize {
        use crate::search::{SearchEvent, SearchInputs, SearchParams};
        use fdop_types::atmo::{Medium, UniformAtmosphere};
        use fdop_types::{BBox, FlatElevation};

        let ground = FlatElevation(0.0);
        let atmo = UniformAtmosphere(Medium::still(343.0));
        let input = SearchInputs {
            events: events
                .iter()
                .filter_map(|e| SearchEvent::from_measured(e, 0.0))
                .collect(),
            tracks: &[],
            heard_times: Vec::new(),
            coarse_dem: &ground,
            fine_dem: &ground,
            atmo: &atmo,
        };
        let (lat, lon) = (40.876466, -73.524951);
        let region = BBox::new(lat - 0.01, lon - 0.01, lat + 0.01, lon + 0.01);
        let params = SearchParams::new(0.0, 0.0, 360.0, region);
        let searched = crate::search::run(&input, &params, &|_| {})
            .funnel
            .events_used;
        let selected = crate::search::independent_measured_events(events, 0.0);
        let windy = crate::search::independent_measured_events(events, 10.0);
        assert_eq!(selected.len(), searched);
        assert_eq!(
            selected.iter().map(|e| &e.id).collect::<Vec<_>>(),
            windy.iter().map(|e| &e.id).collect::<Vec<_>>()
        );
        searched
    }

    #[test]
    fn simultaneous_same_tone_passes_survive_detection_and_measurement() {
        // Similar engines can have the same emitted tone and received CPA while tracing
        // different Doppler transitions. The long, distant pass must not hide the close one.
        for near_amplitude in [0.10, 0.20] {
            let events = measure_simultaneous_same_tone_passes([near_amplitude, 0.08]);
            let fits: Vec<_> = events
                .iter()
                .filter(|e| e.quality.ok)
                .map(|e| e.fit.as_ref().unwrap().best)
                .collect();
            assert_eq!(
                independent_measured_passes(&events),
                2,
                "near amplitude {near_amplitude}: {fits:?}"
            );
            for distance in [1000.0, 7000.0] {
                let fit = fits
                    .iter()
                    .find(|b| (b.d / distance - 1.0).abs() < 0.08)
                    .unwrap_or_else(|| panic!("distance {distance} missing from {fits:?}"));
                assert!((fit.t0r - 180.0).abs() < 0.3, "{fit:?}");
                assert!((fit.f0 - 118.0).abs() < 0.3, "{fit:?}");
                assert!((fit.v - 120.0).abs() < 4.0, "{fit:?}");
            }
        }
    }

    #[test]
    fn simultaneous_same_tone_single_pass_counts_once() {
        for (amplitudes, distance) in [([0.10, 0.0], 1000.0), ([0.0, 0.08], 7000.0)] {
            let events = measure_simultaneous_same_tone_passes(amplitudes);
            let fits: Vec<_> = events
                .iter()
                .filter(|e| e.quality.ok)
                .map(|e| e.fit.as_ref().unwrap().best)
                .collect();
            // The detector can produce several seeds for one pass; its measured curves
            // must still supply only one independent constraint to the aircraft search.
            assert_eq!(
                independent_measured_passes(&events),
                1,
                "distance {distance}: {fits:?}"
            );
            assert!(
                fits.iter().all(|b| (b.d / distance - 1.0).abs() < 0.08),
                "{fits:?}"
            );
            assert!(fits.iter().all(|b| (b.t0r - 180.0).abs() < 0.3), "{fits:?}");
        }
    }

    #[test]
    fn failed_later_round_retains_successful_trace_and_fit() {
        let sr = 1000;
        let (f0, v, d, t0r) = (118.0, 120.0, 3000.0, 100.0);
        let mut phase = 0.0;
        let mut seed = 99u64;
        let x: Vec<f32> = (0..180 * sr)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let t = i as f64 / sr as f64;
                phase += std::f64::consts::TAU * cv_curve(t, f0, v, d, t0r - d / 343.0, 343.0)
                    / sr as f64;
                (0.15 * phase.sin() + 0.9 * (2.0 * seed as f64 / u64::MAX as f64 - 1.0)) as f32
            })
            .collect();
        let det = Detection {
            id: 1,
            t0r,
            f0: f0 + 1.0,
            v: v - 2.0,
            d: d + 300.0,
            score: 8.0,
            score_approach: 8.0,
            score_recede: 8.0,
            half_s: 75.0,
            harmonic_of: None,
            harmonics: Vec::new(),
            from_contrast: false,
        };
        let mut p = MeasureParams {
            hop_s: 0.25,
            rounds: 1,
            ..Default::default()
        };
        // Keep the single discovery fit as the reference. The optional metrology fit
        // deliberately fails: that must preserve the accepted trace and its quality.
        let mut fits = 0;
        let first = measure_with_fit(&x, sr, &det, "e1", &p, true, |t, f, c, side| {
            fits += 1;
            if fits == 1 {
                prepare_constant_velocity(t, f, c, side)
            } else {
                Err("injected metrology failure".into())
            }
        });
        assert_eq!(fits, 2);
        assert!(first.quality.ok, "{:?}", first.quality.reasons);
        p.rounds = 3;
        let mut rounds = 0;
        let interrupted = measure_with_fit(&x, sr, &det, "e1", &p, true, |t, f, c, side| {
            rounds += 1;
            if rounds == 1 {
                prepare_constant_velocity(t, f, c, side)
            } else {
                assert_ne!(f, first.trace.f);
                Err("injected fit failure".into())
            }
        });
        assert_eq!(rounds, 2);
        assert_eq!(
            serde_json::to_value(&interrupted.trace).unwrap(),
            serde_json::to_value(&first.trace).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&interrupted.fit).unwrap(),
            serde_json::to_value(&first.fit).unwrap()
        );
        assert_eq!(interrupted.quality.n_points, first.trace.len());
        assert!(!interrupted.quality.ok);
        assert_eq!(interrupted.quality.reasons, ["fit: injected fit failure"]);

        // If the first fit fails, keep that trace for inspection and report no fit.
        let failed = measure_with_fit(&x, sr, &det, "e1", &p, true, |_, _, _, _| {
            Err("injected fit failure".into())
        });
        assert!(failed.fit.is_none());
        assert!(!failed.quality.ok);
        assert_eq!(
            serde_json::to_value(failed.trace).unwrap(),
            serde_json::to_value(first.trace).unwrap()
        );
    }

    /// `measure_ranked` over ranks that pass where `ok[rank]`; also returns the batches asked for.
    fn ranked(ok: &[bool], cap: usize) -> (Vec<(usize, bool)>, Vec<std::ops::Range<usize>>) {
        let mut batches = Vec::new();
        let got = measure_ranked(
            ok.len(),
            cap,
            |r| {
                batches.push(r.clone());
                r.map(|k| (k, ok[k])).collect()
            },
            |t: &(usize, bool)| t.1,
        );
        (got, batches)
    }

    #[test]
    fn measure_ranked_fills_refused_places_and_never_passes_more_than_the_cap() {
        // When the strongest all pass, it measures exactly what the fixed cap measured, in one batch.
        let (got, batches) = ranked(&[true; 40], 24);
        assert_eq!(batches.len(), 1, "{batches:?}");
        assert_eq!(batches[0], 0..24);
        assert_eq!(
            got.iter().map(|t| t.0).collect::<Vec<_>>(),
            (0..24).collect::<Vec<_>>()
        );

        // Refusals among the strongest are replaced by the next ranks, in rank order: the passes kept
        // are the 24 strongest that pass, and what was measured is the prefix ending at the 24th.
        let ok: Vec<bool> = (0..60).map(|k| k >= 30 || k % 3 == 0).collect();
        let (got, batches) = ranked(&ok, 24);
        let passed: Vec<usize> = got.iter().filter(|t| t.1).map(|t| t.0).collect();
        let strongest: Vec<usize> = (0..60).filter(|&k| ok[k]).take(24).collect();
        assert_eq!(passed, strongest);
        let last = *strongest.last().unwrap();
        assert_eq!(
            got.iter().map(|t| t.0).collect::<Vec<_>>(),
            (0..=last).collect::<Vec<_>>()
        );
        assert_eq!(batches[0], 0..24);
        assert!(
            batches.windows(2).all(|w| w[0].end == w[1].start),
            "{batches:?}"
        );

        // Too few passes: every detection is measured once, and an empty list measures nothing.
        let (got, _) = ranked(&[false; 50], 24);
        assert_eq!(
            got.iter().map(|t| t.0).collect::<Vec<_>>(),
            (0..50).collect::<Vec<_>>()
        );
        assert!(ranked(&[], 24).0.is_empty());
        assert!(ranked(&[true; 10], 0).0.is_empty());
    }

    #[test]
    fn measure_ranked_counted_rechecks_groups_and_stops_after_a_count_jump() {
        let mut batches = Vec::new();
        let got = measure_ranked_counted(
            8,
            3,
            |r| {
                batches.push(r.clone());
                r.collect::<Vec<_>>()
            },
            |items| match items.len() {
                // A better representative can regroup earlier fits; the scheduler must
                // recount the complete prefix, retain it, and stop even if the count jumps.
                3 => 2,
                4 => 1,
                6 => 4,
                n => panic!("unexpected measured prefix of length {n}"),
            },
        );
        assert_eq!(batches, [0..3, 3..4, 4..6]);
        assert_eq!(got, (0..6).collect::<Vec<_>>());
    }

    #[test]
    fn ranked_measurement_reaches_a_later_pass_after_duplicate_seeds() {
        use crate::search::independent_measured_events;

        let sr = 4000u32;
        let first = Detection {
            id: 0,
            t0r: 180.0,
            f0: 118.0,
            v: 120.0,
            d: 1000.0,
            score: 12.0,
            score_approach: 12.0,
            score_recede: 12.0,
            half_s: 60.0,
            harmonic_of: None,
            harmonics: Vec::new(),
            from_contrast: false,
        };
        let duplicate = Detection {
            id: 1,
            t0r: first.t0r + 0.5,
            f0: first.f0 + 0.1,
            v: first.v + 1.0,
            d: first.d + 100.0,
            score: 11.0,
            ..first.clone()
        };
        let second = Detection {
            id: 2,
            d: 7000.0,
            score: 10.0,
            ..first.clone()
        };
        // Reuse the real PCM scene from the overlapping-pass recovery regressions. The
        // first two seeds measure its close pass; only the later-ranked third seed can
        // supply the distant pass, despite sharing the emitted tone and received CPA.
        let x = simultaneous_same_tone_audio([0.10, 0.08]);
        let detections = [&first, &duplicate, &second];
        let mut batches = Vec::new();
        let events = measure_ranked_counted(
            detections.len(),
            2,
            |ranks| {
                batches.push(ranks.clone());
                ranks
                    .map(|k| {
                        measure(
                            &x,
                            sr,
                            detections[k],
                            &format!("e{}", k + 1),
                            &MeasureParams::default(),
                        )
                    })
                    .collect()
            },
            |events| independent_measured_events(events, 0.0).len(),
        );
        assert_eq!(batches, [0..2, 2..3]);
        assert_eq!(events.len(), 3);
        assert!(
            events.iter().all(|e| e.quality.ok),
            "{:?}",
            events
                .iter()
                .map(|e| &e.quality.reasons)
                .collect::<Vec<_>>()
        );
        let old_prefix = measure_ranked(
            detections.len(),
            2,
            |ranks| ranks.map(|k| events[k].clone()).collect(),
            |e| e.quality.ok,
        );
        assert_eq!(old_prefix.len(), 2);
        assert_eq!(independent_measured_events(&old_prefix, 0.0).len(), 1);
        let selected = independent_measured_events(&events, 0.0);
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().any(|e| e.id == "e3"));
        for d in [&first, &second] {
            let fit = events
                .iter()
                .filter_map(|e| e.fit.as_ref())
                .min_by(|a, b| (a.best.d - d.d).abs().total_cmp(&(b.best.d - d.d).abs()))
                .unwrap();
            assert!(
                (fit.best.t0r - d.t0r).abs() < 0.5,
                "CPA: {:?} against {d:?}",
                fit.best
            );
            assert!((fit.best.f0 - d.f0).abs() < 0.5, "{:?}", fit.best);
            assert!((fit.best.v - d.v).abs() < 5.0, "{:?}", fit.best);
            assert!((fit.best.d / d.d - 1.0).abs() < 0.1, "{:?}", fit.best);
        }
        let windy = independent_measured_events(&events, 10.0);
        assert_eq!(
            selected.iter().map(|e| &e.id).collect::<Vec<_>>(),
            windy.iter().map(|e| &e.id).collect::<Vec<_>>()
        );

        // Eligibility uses exactly the pre-existing aircraft bounds and quality gate.
        // Mutate only those fields of an actual fit to check each boundary independently.
        for (speed, distance, quality, expected) in [
            (27.99, 1800.0, true, 0),
            (90.0, 199.99, true, 0),
            (90.0, 1800.0, false, 0),
            (28.0, 200.0, true, 1),
        ] {
            let mut event = events[0].clone();
            let fit = event.fit.as_mut().unwrap();
            fit.best.v = speed;
            fit.best.d = distance;
            event.quality.ok = quality;
            assert_eq!(independent_measured_events(&[event], 10.0).len(), expected);
        }
        let mut missing_fit = events[0].clone();
        missing_fit.fit = None;
        assert!(independent_measured_events(&[missing_fit], 0.0).is_empty());
    }

    /// (f0, v, d, emission time of the CPA) of the weak pass in `weak_scene`.
    const WEAK_PASS: (f64, f64, f64, f64) = (118.0, 70.0, 1500.0, 300.0);

    /// Ten minutes of noise with a flat 250 Hz tone and the weak pass at amplitude `amp`.
    fn weak_scene(amp: f64) -> Vec<f32> {
        let (f0, v, d, t0e) = WEAK_PASS;
        let sr = 4000.0;
        let (mut seed, mut phase) = (4242u64, 0.0f64);
        (0..(sr * 600.0) as usize)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let t = i as f64 / sr;
                phase += std::f64::consts::TAU * cv_curve(t, f0, v, d, t0e, 343.0) / sr;
                (amp * phase.sin()
                    + 0.06 * (std::f64::consts::TAU * 250.0 * t).sin()
                    + 0.9 * (2.0 * seed as f64 / u64::MAX as f64 - 1.0)) as f32
            })
            .collect()
    }

    /// Detection, then measurement of every primary at or above the silence line, as `locate` does:
    /// (detection score, measured event) in rank order.
    fn measured_down_to_the_silence_line(x: &[f32]) -> Vec<(f64, MeasuredEvent)> {
        let dp = DetectParams {
            c: 343.0,
            ..Default::default()
        };
        let (ls, lin) = survey(x, 4000, &dp).unwrap();
        let dets = detect(&ls, &dp);
        let cands: Vec<&Detection> = dets
            .iter()
            .filter(|d| d.harmonic_of.is_none() && d.score >= dp.thr_silence_db)
            .collect();
        let p = MeasureParams {
            c: 343.0,
            ..Default::default()
        };
        measure_ranked(
            cands.len(),
            MAX_MEASURED,
            |r| {
                r.map(|k| {
                    let e = measure(
                        x,
                        4000,
                        &refine(&lin, cands[k], 343.0),
                        &format!("e{k}"),
                        &p,
                    );
                    (cands[k].score, e)
                })
                .collect()
            },
            |t| t.1.quality.ok,
        )
    }

    #[test]
    fn a_pass_inside_a_loud_swell_is_found_through_the_scale_floor_and_its_reversal_and_noise_are_refused()
     {
        // One loud pass over a short clip: its tone and its broadband roar rise together by 25 dB.
        // The swell inflates every row's MAD, so the survey image's spread falls near 0.25 and the
        // pass scores under the silence line: without the scale floor it is never measured. With
        // the floor it is detected, measured, and passes the gates with the right speed and
        // distance. The same audio reversed, and the swell without the pass, give nothing that
        // passes the gates.
        use crate::detect::{SWELL_PASS, swell_scene};
        let (f0, v, d, t0r) = SWELL_PASS;
        let on_pass = |t: f64, f: f64| (t - t0r).abs() < 6.0 && (f / f0 - 1.0).abs() < 0.03;
        let x = swell_scene(0.16, 25.0);

        let off = DetectParams {
            c: 343.0,
            min_image_scale: 0.0,
            ..Default::default()
        };
        let (ls, _) = survey(&x, 4000, &off).unwrap();
        let unscaled = detect(&ls, &off)
            .iter()
            .filter(|d| on_pass(d.t0r, d.f0))
            .map(|d| d.score)
            .fold(f64::NEG_INFINITY, f64::max);
        eprintln!(
            "spread {:.3}: pass scores {unscaled:.2} without the floor",
            ls.scale
        );
        assert!(
            unscaled < off.thr_silence_db,
            "the pass already clears the silence line without the floor ({unscaled:.2})"
        );

        let evs = measured_down_to_the_silence_line(&x);
        eprintln!("with the floor: {} measured", evs.len());
        let (score, pass) = evs
            .iter()
            .find(|(_, e)| on_pass(e.detection.t0r, e.detection.f0))
            .expect("the pass was not detected with the floor");
        eprintln!(
            "pass: detection {score:.2}, gates {:?}",
            pass.quality.reasons
        );
        assert!(pass.quality.ok, "{:?}", pass.quality.reasons);
        let b = pass.fit.as_ref().unwrap().best;
        assert!((b.v / v - 1.0).abs() < 0.1, "v {} against {v}", b.v);
        assert!((b.d / d - 1.0).abs() < 0.2, "d {} against {d}", b.d);
        assert!((b.t0r - t0r).abs() < 1.0, "t0r {} against {t0r}", b.t0r);
        for (s, e) in evs
            .iter()
            .filter(|(_, e)| !on_pass(e.detection.t0r, e.detection.f0))
        {
            assert!(
                !e.quality.ok,
                "score {s:.2} at {:.0} s, {:.0} Hz passed",
                e.detection.t0r, e.detection.f0
            );
        }

        let mut rev = x.clone();
        rev.reverse();
        for (name, y) in [("reversed", rev), ("swell alone", swell_scene(0.0, 25.0))] {
            let got = measured_down_to_the_silence_line(&y);
            eprintln!("{name}: {} measured", got.len());
            for (s, e) in &got {
                assert!(
                    !e.quality.ok,
                    "{name}: score {s:.2} at {:.0} s, {:.0} Hz passed",
                    e.detection.t0r, e.detection.f0
                );
            }
        }
    }

    #[test]
    fn a_pass_a_loud_swell_hides_is_measured_from_its_contrast_detection_and_swell_noise_is_refused()
     {
        // A faint pass in a 20 dB swell is on no detection of the survey image; the contrast stage
        // finds it (detect.rs). Measured from that detection, it passes the gates with the right
        // speed, distance and CPA, and nothing else passes. The same audio reversed, and swells
        // of 30 to 40 dB with no pass (where the contrast image adds strong curves of its own),
        // give nothing that passes.
        use crate::detect::{SWELL_PASS, swell_scene};
        let (f0, v, d, t0r) = SWELL_PASS;
        let on_pass = |t: f64, f: f64| (t - t0r).abs() < 6.0 && (f / f0 - 1.0).abs() < 0.03;
        let x = swell_scene(0.08, 20.0);
        let evs = measured_down_to_the_silence_line(&x);
        for (s, e) in &evs {
            let b = e.fit.as_ref().map(|f| f.best);
            eprintln!(
                "score {s:.2} contrast {} t0r {:.1} f0 {:.1}: {:?} {:?}",
                e.detection.from_contrast,
                e.detection.t0r,
                e.detection.f0,
                b.map(|b| (b.v, b.d, b.t0r)),
                e.quality.reasons
            );
        }
        let (_, pass) = evs
            .iter()
            .find(|(_, e)| on_pass(e.detection.t0r, e.detection.f0))
            .expect("the pass was not detected");
        assert!(pass.detection.from_contrast);
        assert!(pass.quality.ok, "{:?}", pass.quality.reasons);
        let b = pass.fit.as_ref().unwrap().best;
        assert!((b.v / v - 1.0).abs() < 0.1, "v {} against {v}", b.v);
        assert!((b.d / d - 1.0).abs() < 0.2, "d {} against {d}", b.d);
        assert!((b.t0r - t0r).abs() < 1.0, "t0r {} against {t0r}", b.t0r);
        for (s, e) in evs
            .iter()
            .filter(|(_, e)| !on_pass(e.detection.t0r, e.detection.f0))
        {
            assert!(
                !e.quality.ok,
                "score {s:.2} at {:.0} s, {:.0} Hz passed",
                e.detection.t0r, e.detection.f0
            );
        }
        let mut rev = x.clone();
        rev.reverse();
        let mut cases = vec![("reversed".to_string(), rev)];
        for db in [30.0, 35.0, 40.0] {
            cases.push((format!("{db} dB swell alone"), swell_scene(0.0, db)));
        }
        for (name, y) in cases {
            let got = measured_down_to_the_silence_line(&y);
            let n_contrast = got
                .iter()
                .filter(|(_, e)| e.detection.from_contrast)
                .count();
            eprintln!(
                "{name}: {} measured, {n_contrast} from the contrast stage",
                got.len()
            );
            for (s, e) in &got {
                assert!(
                    !e.quality.ok,
                    "{name}: score {s:.2} at {:.0} s, {:.0} Hz passed",
                    e.detection.t0r, e.detection.f0
                );
            }
        }
    }

    #[test]
    fn a_short_pass_in_a_clip_too_short_for_the_bank_is_measured_from_its_edge_detection() {
        // In a 30 s clip the 24 s templates cannot score any cell; an edge template finds a short
        // pass heard 9 s in (detect.rs). Measured from that detection, it passes the gates with
        // the right speed, distance and CPA. The same audio reversed (every curve rises) and the
        // noise alone give nothing that passes.
        use crate::detect::{EDGE_PASS, edge_clip};
        let (f0, v, d, t0r) = EDGE_PASS;
        let on_pass = |t: f64, f: f64| (t - t0r).abs() < 6.0 && (f / f0 - 1.0).abs() < 0.06;
        let x = edge_clip(30.0, EDGE_PASS, 0.3, (0.0, 0.0, 0.0));
        let evs = measured_down_to_the_silence_line(&x);
        for (s, e) in &evs {
            let b = e.fit.as_ref().map(|f| f.best);
            eprintln!(
                "detection {s:.2} at {:.1} s, {:.0} Hz, half {}: ok {} {:?} fit {:?}",
                e.detection.t0r,
                e.detection.f0,
                e.detection.half_s,
                e.quality.ok,
                e.quality.reasons,
                b.map(|b| (b.f0, b.v, b.d, b.t0r))
            );
        }
        let (_, pass) = evs
            .iter()
            .find(|(_, e)| on_pass(e.detection.t0r, e.detection.f0))
            .expect("the edge pass was not measured");
        assert!(pass.quality.ok, "{:?}", pass.quality.reasons);
        let b = pass.fit.as_ref().unwrap().best;
        assert!((b.v / v - 1.0).abs() < 0.1, "v {} against {v}", b.v);
        assert!((b.d / d - 1.0).abs() < 0.2, "d {} against {d}", b.d);
        assert!((b.t0r - t0r).abs() < 1.0, "t0r {} against {t0r}", b.t0r);
        assert!((b.f0 / f0 - 1.0).abs() < 0.01, "f0 {} against {f0}", b.f0);
        for (s, e) in evs
            .iter()
            .filter(|(_, e)| !on_pass(e.detection.t0r, e.detection.f0))
        {
            assert!(
                !e.quality.ok,
                "score {s:.2} at {:.0} s, {:.0} Hz passed",
                e.detection.t0r, e.detection.f0
            );
        }
        let mut rev = x.clone();
        rev.reverse();
        let noise = edge_clip(30.0, EDGE_PASS, 0.0, (0.0, 0.0, 0.0));
        for (name, y) in [("reversed", rev), ("noise", noise)] {
            let got = measured_down_to_the_silence_line(&y);
            eprintln!("{name}: {} measured", got.len());
            for (s, e) in &got {
                assert!(
                    !e.quality.ok,
                    "{name}: score {s:.2} at {:.0} s, {:.0} Hz passed",
                    e.detection.t0r, e.detection.f0
                );
            }
        }
    }

    #[test]
    fn a_pass_under_the_old_measure_line_is_measured_and_noise_tones_and_reversed_audio_are_refused()
     {
        // Detection keeps curves down to the silence line (3.5; pure noise peaks near 3), but only
        // those from 4.5 up used to be measured. A pass scoring between the two lines must come out
        // with the right speed and distance, and the measurement gates, not the detection score,
        // must refuse what is not a pass: the noise, a flat tone, and the same audio reversed in
        // time, where every curve rises, which no flyover makes.
        let dp = DetectParams::default();
        let (f0, v, d, t0e) = WEAK_PASS;
        let t0r = t0e + d / 343.0;
        let is_pass = |e: &MeasuredEvent| {
            (e.detection.t0r - t0r).abs() < 15.0 && (e.detection.f0 / f0 - 1.0).abs() < 0.03
        };
        let mut found = None;
        for amp in [0.05, 0.04, 0.032, 0.026, 0.021, 0.017, 0.014, 0.011] {
            let x = weak_scene(amp);
            let evs = measured_down_to_the_silence_line(&x);
            let score = evs.iter().find(|(_, e)| is_pass(e)).map(|t| t.0);
            eprintln!(
                "amplitude {amp}: {} measured, pass scores {score:?}",
                evs.len()
            );
            if score.is_some_and(|s| (dp.thr_silence_db..dp.thr_db).contains(&s)) {
                found = Some((amp, x, evs));
                break;
            }
        }
        let (amp, mut x, evs) = found.expect("no amplitude put the pass between the two lines");
        let (_, pass) = evs.iter().find(|(_, e)| is_pass(e)).unwrap();
        assert!(
            pass.quality.ok,
            "amplitude {amp}: {:?}",
            pass.quality.reasons
        );
        let b = pass.fit.as_ref().unwrap().best;
        assert!((b.v / v - 1.0).abs() < 0.1, "v {} against {v}", b.v);
        assert!((b.d / d - 1.0).abs() < 0.2, "d {} against {d}", b.d);
        for (s, e) in evs.iter().filter(|(_, e)| !is_pass(e)) {
            assert!(
                !e.quality.ok,
                "score {s:.2} at {:.0} s, {:.0} Hz passed",
                e.detection.t0r, e.detection.f0
            );
        }

        x.reverse();
        let reversed = measured_down_to_the_silence_line(&x);
        eprintln!("reversed: {} measured", reversed.len());
        for (s, e) in &reversed {
            assert!(
                !e.quality.ok,
                "reversed: score {s:.2} at {:.0} s, {:.0} Hz passed",
                e.detection.t0r, e.detection.f0
            );
        }
    }

    /// `dur` s at 4 kHz: uniform noise of amplitude 0.3 and, unless `pass` is None, the tone of a
    /// constant-velocity pass (f0, v, d) heard at its CPA at `t0r`, or a steady tone at `steady`.
    fn wrong_shape_audio(
        pass: Option<(f64, f64, f64)>,
        steady: Option<f64>,
        t0r: f64,
        dur: f64,
        tone_amp: f64,
    ) -> Vec<f32> {
        let sr = 4000.0;
        let (mut seed, mut phase) = (4242u64, 0.0f64);
        let tone = pass.map(|(f0, v, d)| model_fn(f0, v, d, t0r, 343.0));
        (0..(sr * dur) as usize)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let t = i as f64 / sr;
                let f = match (&tone, steady) {
                    (Some(tone), _) => tone(t),
                    (None, Some(f)) => f,
                    (None, None) => 0.0,
                };
                phase += std::f64::consts::TAU * f / sr;
                let signal = if f > 0.0 { tone_amp * phase.sin() } else { 0.0 };
                (signal + 0.3 * (2.0 * seed as f64 / u64::MAX as f64 - 1.0)) as f32
            })
            .collect()
    }

    #[test]
    fn a_one_sided_refusal_is_measured_from_a_free_trace_of_its_ridge_and_tones_are_not() {
        // Set D's N394FX_R20: the bank's best template sat at its top speed, 175-181 m/s at about
        // 3650 m, for a pass of 135 m/s at 1758 m. That curve meets the ridge only on the recede side,
        // 31-46 Hz off it around the CPA where the band holds 12, so the guided trace holds only that
        // side and its fit drifts to an implausible pass. Correct spectral resolution can also
        // recover these seeds directly; an accepted first result must then remain unchanged.
        // Each case is (audio, pass f0 v d, detection f0 v d as multiples, CPA offset s).
        let (dur, t0r) = (240.0, 120.0);
        let cases = [
            (
                "clean",
                0.15,
                (367.0, 135.0, 1758.0),
                (0.934, 1.34, 2.07),
                7.0,
            ),
            (
                "scintillating",
                0.15,
                (250.0, 110.0, 900.0),
                (0.934, 1.34, 2.07),
                7.0,
            ),
            (
                "scintillating",
                0.08,
                (300.0, 90.0, 1200.0),
                (0.96, 1.25, 1.8),
                14.0,
            ),
        ];
        let off = MeasureParams {
            free_reseed: false,
            ..Default::default()
        };
        let on = MeasureParams::default();
        let det_of = |(f0, v, d): (f64, f64, f64), t: f64| Detection {
            id: 0,
            t0r: t,
            f0,
            v: v.min(175.0),
            d,
            score: 8.0,
            score_approach: 8.0,
            score_recede: 8.0,
            half_s: 60.0,
            harmonic_of: None,
            harmonics: Vec::new(),
            from_contrast: false,
        };
        let audio = |kind: &str, amp: f64, (f0, v, d): (f64, f64, f64)| {
            if kind == "clean" {
                wrong_shape_audio(Some((f0, v, d)), None, t0r, dur, amp)
            } else {
                scintillating_pass(f0, v, d, t0r, dur, amp)
            }
        };
        let json = |e: &MeasuredEvent| serde_json::to_vec(&(&e.fit, &e.trace, &e.quality)).unwrap();
        let check_route = |first: &MeasuredEvent, recovered: &MeasuredEvent, label: &str| {
            let q = &first.quality;
            assert!(!q.reseeded, "{label}: guided-only result {q:?}");
            assert!(recovered.quality.ok, "{label}: {:?}", recovered.quality);
            if q.ok {
                assert!(
                    !recovered.quality.reseeded,
                    "{label}: {:?}",
                    recovered.quality
                );
                assert_eq!(
                    json(first),
                    json(recovered),
                    "{label}: accepted first result changed"
                );
            } else {
                assert!(
                    q.span_approach_s < off.min_side_s || q.span_recede_s < off.min_side_s,
                    "{label}: first fit {:?}; quality {q:?}",
                    first.fit.as_ref().map(|f| f.best)
                );
                assert!(
                    recovered.quality.reseeded,
                    "{label}: {:?}",
                    recovered.quality
                );
            }
        };
        for (kind, amp, (f0, v, d), (kf, kv, kd), dt) in cases {
            let label = format!(
                "{kind} {amp}: pass {f0} Hz {v} m/s {d} m, detection x({kf}, {kv}, {kd}) {dt:+} s"
            );
            let x = audio(kind, amp, (f0, v, d));
            let det = det_of((f0 * kf, v * kv, d * kd), t0r + dt);
            let first = measure(&x, 4000, &det, "e1", &off);
            let ev = measure(&x, 4000, &det, "e1", &on);
            check_route(&first, &ev, &label);
            let b = ev.fit.as_ref().unwrap().best;
            eprintln!(
                "{label}: measured {:.2} Hz {:.2} m/s {:.1} m CPA {:.2} s",
                b.f0, b.v, b.d, b.t0r
            );
            assert!((b.f0 - f0).abs() <= 0.005 * f0, "{label}: f0 {}", b.f0);
            assert!((b.v - v).abs() <= 0.03 * v, "{label}: v {}", b.v);
            assert!((b.d - d).abs() <= 0.05 * d, "{label}: d {}", b.d);
            assert!((b.t0r - t0r).abs() <= 0.5, "{label}: t0r {}", b.t0r);
            // The event keeps the detection it was measured from.
            assert_eq!(ev.detection.v, det.v);

            // A detection of the right shape is measured exactly as without a second start.
            let right = det_of((f0, v, d), t0r);
            let (a, b) = (
                measure(&x, 4000, &right, "e1", &on),
                measure(&x, 4000, &right, "e1", &off),
            );
            assert!(
                a.quality.ok && !a.quality.reseeded,
                "{label}: {:?}",
                a.quality.reasons
            );
            assert_eq!(json(&a), json(&b), "{label}");

            // Its reversal (a rising tone), a steady tone where the pass's curve is 15 s after its
            // CPA, and noise give nothing the gates accept, from either start.
            let mut reversed = x.clone();
            reversed.reverse();
            let mirrored = det_of((det.f0, det.v, det.d), dur - det.t0r);
            let crossing = model_fn(f0, v, d, t0r, 343.0)(t0r + 15.0);
            let steady = wrong_shape_audio(None, Some(crossing), t0r, dur, amp);
            let noise = wrong_shape_audio(None, None, t0r, dur, amp);
            for (what, y, dd) in [
                ("reversed", &reversed, &mirrored),
                ("steady tone", &steady, &det),
                ("noise", &noise, &det),
            ] {
                let ev = measure(y, 4000, dd, "e1", &on);
                assert!(
                    !ev.quality.ok && !ev.quality.reseeded,
                    "{label} {what}: {:?}",
                    ev.quality.reasons
                );
            }
        }
        // This same PCM and wrong seed formerly gave a two-sided 6 m/s misfit.
        // It must recover the physical pass within the same accuracy bounds,
        // preserving an accepted first result or retrying a one-sided refusal.
        let x = wrong_shape_audio(Some((250.0, 110.0, 900.0)), None, t0r, dur, 0.15);
        let det = det_of((250.0 * 0.96, 110.0 * 1.25, 900.0 * 1.8), t0r + 14.0);
        let (a, b) = (
            measure(&x, 4000, &det, "e1", &on),
            measure(&x, 4000, &det, "e1", &off),
        );
        check_route(&b, &a, "receding seed");
        let fit = a.fit.as_ref().unwrap().best;
        eprintln!("recovered receding seed: {fit:?}");
        assert!((fit.f0 - 250.0).abs() <= 0.005 * 250.0, "{fit:?}");
        assert!((fit.v - 110.0).abs() <= 0.03 * 110.0, "{fit:?}");
        assert!((fit.d - 900.0).abs() <= 0.05 * 900.0, "{fit:?}");
        assert!((fit.t0r - t0r).abs() <= 0.5, "{fit:?}");
        assert_eq!(a.detection.v, det.v);
    }

    #[test]
    fn quality_support_uses_only_times_retained_by_the_actual_solver() {
        let p = MeasureParams::default();
        let (f0, v, d, t0r) = (250.0, 110.0, 900.0, 120.0);
        let truth = model_fn(f0, v, d, t0r, p.c);
        let times: Vec<f64> = (0..=210).map(|i| 80.0 + 0.5 * i as f64).collect();
        let frequencies = times
            .iter()
            .enumerate()
            .map(|(i, &t)| truth(t) + 0.01 * (1.731 * i as f64).sin())
            .collect();
        let trace = Trace {
            snr: vec![10.0; times.len()],
            t: times,
            f: frequencies,
        };
        let prepared = prepare_constant_velocity(&trace.t, &trace.f, p.c, Side::Both).unwrap();
        let best = prepared.best();
        let quality = gate(&trace, prepared.inlier_times(), &best, t0r, 90.0, &p);
        assert!((best.f0 - f0).abs() <= 0.005 * f0, "{best:?}");
        assert!((best.v - v).abs() <= 0.03 * v, "{best:?}");
        assert!((best.d - d).abs() <= 0.05 * d, "{best:?}");
        assert!((best.t0r - t0r).abs() < 0.1, "{best:?}");
        assert!(quality.reasons.is_empty(), "{:?}", quality.reasons);
        assert_eq!(quality.span_approach_s, best.t0r - trace.t[0]);
        assert_eq!(quality.span_recede_s, trace.t[trace.len() - 1] - best.t0r);

        // Use the unchanged receding-seed recovery's real PCM and trace. Copy
        // each actual PreparedFit's retained times before completion consumes
        // them; do not reconstruct a mask from residuals or re-fit the trace.
        let x = wrong_shape_audio(Some((f0, v, d)), None, t0r, 240.0, 0.15);
        let det = Detection {
            id: 0,
            t0r: t0r + 14.0,
            f0: f0 * 0.96,
            v: v * 1.25,
            d: d * 1.8,
            score: 8.0,
            score_approach: 8.0,
            score_recede: 8.0,
            half_s: 60.0,
            harmonic_of: None,
            harmonics: Vec::new(),
            from_contrast: false,
        };
        let mut retained = Vec::new();
        let first = measure_with_fit(
            &x,
            4000,
            &det,
            "e1",
            &MeasureParams {
                free_reseed: false,
                ..p.clone()
            },
            true,
            |t, f, c, side| {
                let prepared = prepare_constant_velocity(t, f, c, side)?;
                retained = prepared.inlier_times().to_vec();
                Ok(prepared)
            },
        );
        let best = first.fit.as_ref().unwrap().best;
        let (lo, hi) = retained
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &t| {
                (lo.min(t), hi.max(t))
            });
        assert!(retained.len() < first.trace.len());
        assert!(best.t0r - first.trace.t[0] >= p.min_side_s);
        assert!(first.trace.t[first.trace.len() - 1] - best.t0r >= p.min_side_s);
        assert!(best.t0r - lo < p.min_side_s);
        assert_eq!(first.quality.span_approach_s, (best.t0r - lo).max(0.0));
        assert_eq!(first.quality.span_recede_s, (hi - best.t0r).max(0.0));
        assert!(!first.quality.ok && !first.quality.reseeded);
        assert!(
            first
                .quality
                .reasons
                .iter()
                .any(|r| r.starts_with("one-sided trace"))
        );
    }

    #[test]
    fn a_two_sided_slow_pass_stays_refused_without_a_free_retry() {
        // Use an actual slow source for the two-sided refusal policy, independent
        // of how an aircraft pass happens to fit from an incorrect survey seed.
        let (dur, t0r) = (240.0, 120.0);
        let (f0, v, d) = (250.0, 6.0, 200.0);
        let x = wrong_shape_audio(Some((f0, v, d)), None, t0r, dur, 0.15);
        let det = Detection {
            id: 0,
            t0r,
            f0,
            v,
            d,
            score: 8.0,
            score_approach: 8.0,
            score_recede: 8.0,
            half_s: 60.0,
            harmonic_of: None,
            harmonics: Vec::new(),
            from_contrast: false,
        };
        let on = MeasureParams::default();
        let off = MeasureParams {
            free_reseed: false,
            ..on.clone()
        };
        let (a, b) = (
            measure(&x, 4000, &det, "e1", &on),
            measure(&x, 4000, &det, "e1", &off),
        );
        let q = &b.quality;
        assert!(
            !q.ok && q.span_approach_s >= off.min_side_s && q.span_recede_s >= off.min_side_s,
            "{q:?}"
        );
        assert!(!a.quality.reseeded, "{:?}", a.quality);
        assert_eq!(q.reasons.len(), 1, "{q:?}");
        assert!(q.reasons[0].starts_with("implausible speed"), "{q:?}");
        let fit = b.fit.as_ref().unwrap().best;
        eprintln!("two-sided slow pass: {fit:?}");
        assert!((fit.f0 - f0).abs() <= 0.005 * f0, "{fit:?}");
        assert!((fit.v - v).abs() <= 0.03 * v, "{fit:?}");
        assert!((fit.d - d).abs() <= 0.05 * d, "{fit:?}");
        assert!((fit.t0r - t0r).abs() <= 0.5, "{fit:?}");
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::to_value(&b).unwrap()
        );
    }

    /// `dur` s at 4 kHz of a pass's tone with the skirts of a real one, in uniform noise of
    /// amplitude 0.3: its amplitude scintillates (noise low-passed at about 4 Hz, 60 % deep) and its
    /// frequency wanders (1 Hz RMS, 0.1 s correlation time).
    fn scintillating_pass(f0: f64, v: f64, d: f64, t0r: f64, dur: f64, amp: f64) -> Vec<f32> {
        let sr = 4000.0;
        let tone = model_fn(f0, v, d, t0r, 343.0);
        let mut seed = 77u64;
        let mut uniform = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            2.0 * seed as f64 / u64::MAX as f64 - 1.0
        };
        let (a_am, a_fm) = (
            (-std::f64::consts::TAU * 4.0 / sr).exp(),
            (-1.0 / (0.1 * sr)).exp(),
        );
        // Uniform noise has variance 1/3: these gains give the AR(1) processes unit variance.
        let unit = |a: f64| (3.0 * (1.0 - a * a)).sqrt();
        let (mut am, mut fm, mut phase) = (0.0f64, 0.0f64, 0.0f64);
        (0..(sr * dur) as usize)
            .map(|i| {
                am = a_am * am + unit(a_am) * uniform();
                fm = a_fm * fm + unit(a_fm) * uniform();
                phase += std::f64::consts::TAU * (tone(i as f64 / sr) + fm) / sr;
                let signal = amp * (1.0 + 0.6 * am).max(0.0) * phase.sin();
                (signal + 0.3 * uniform()) as f32
            })
            .collect()
    }

    #[test]
    fn a_steep_ridge_with_real_skirts_stays_traced_once_the_band_narrows() {
        // Set D's N394FX_R20 (135 m/s at 1758 m, about 10 Hz/s at the CPA): from a fit within 5 %
        // of the pass, the first round traced both sides; the next, at 3 x its 0.83 Hz RMS, lost
        // every steep approach frame although their peaks stood 8-14 dB up within 2 Hz of the
        // curve, because the ridge's skirts filled a noise reference of twice its half-width.
        let (sr, c, hop, df) = (4000u32, 343.0, 0.1, 0.25);
        let (dur, t0r) = (120.0, 60.0);
        for (f0, v, d) in [(355.0, 130.0, 1700.0), (250.0, 110.0, 900.0)] {
            let x = scintillating_pass(f0, v, d, t0r, dur, 0.15);
            let model = model_fn(f0, v, d, t0r, c);
            let r = readouts(f0, v, d, c);
            let (fmin, fmax) = (r.f_post * 0.9, r.f_pre * 1.1);
            let mut main = stft_band_db(&x, sr, MIN_MAIN_WIN_S, hop, fmin, fmax, df, 0.0).unwrap();
            main.normalize_rows_with_offsets(None);
            let steep_win = window_for_roc(r.max_roc);
            assert!(steep_win < MIN_MAIN_WIN_S);
            let mut short = stft_band_db(&x, sr, steep_win, hop, fmin, fmax, df, 0.0).unwrap();
            short.normalize_rows_with_offsets(None);
            let held = 1.0 / (MIN_MAIN_WIN_S * MIN_MAIN_WIN_S);
            let steep_frames: Vec<f64> = main
                .t
                .iter()
                .copied()
                .filter(|&t| (model(t + 0.125) - model(t - 0.125)).abs() / 0.25 > held)
                .collect();
            assert!(steep_frames.len() > 50, "{}", steep_frames.len());
            let p = MeasureParams::default();
            for band in [12.0, 2.5, 1.0] {
                let tr = guided_trace(
                    &main,
                    Some(&short),
                    &model,
                    0.0,
                    dur,
                    band,
                    p.min_snr,
                    p.min_z,
                );
                let traced = steep_frames
                    .iter()
                    .filter(|&&t| tr.t.iter().any(|&u| (u - t).abs() < 0.5 * hop))
                    .count();
                let share = traced as f64 / steep_frames.len() as f64;
                eprintln!(
                    "pass {f0} Hz {v} m/s {d} m, band {band} Hz: {traced} of {} steep frames traced ({share:.2})",
                    steep_frames.len()
                );
                assert!(share >= 0.9, "pass {f0} {v} {d} band {band}: {share:.2}");
            }
        }
    }
}
