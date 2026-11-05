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

use crate::detect::Detection;
use crate::fit::{FitResult, PreparedFit, Side, prepare_constant_velocity};
use crate::physics::{cv_curve, readouts, window_for_roc};
use crate::spectro::{Spec, stft_band_db};
use crate::trace::{Trace, guided_ridge_peak};
use serde::Serialize;

/// At most this many measured events pass the quality gates, strongest primary detections first.
/// More usable events from a long recording can promote a wrong answer (CHANGELOG, Unreleased), so
/// the cap stays until the grading accounts for how many events were searched. A detection the gates
/// refuse does not use up a place: [`measure_ranked`] goes on down the list.
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
}

fn model_fn(f0: f64, v: f64, d: f64, t0r: f64, c: f64) -> impl Fn(f64) -> f64 {
    let t0e = t0r - d / c;
    move |t| cv_curve(t, f0, v, d, t0e, c)
}

/// Shortest window (s) of an event's main spectrogram. A shorter one's main lobe would fill the
/// noise reference around the steady sides of a pass. A chirp above 1 / win^2 (4 Hz/s here) smears
/// out of this window, so a steeper pass adds a spectrogram of its own for its steep frames.
const MIN_MAIN_WIN_S: f64 = 0.5;

/// Time error (s) the guided trace allows for in a steep frame. With it a steep pass is followed
/// when its detection rates the chirp half as steep, about as far as real detections were seen to
/// be off on passes traced well. A detection four times too gentle can lose the CPA frames and be
/// refused, and at eight times most are.
const STEEP_TIME_ERROR_S: f64 = 0.25;

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
    /// A longer silence ends a stretch: the fall across it was not observed.
    const MAX_GAP_S: f64 = 3.0;
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
        if ti - last_t > MAX_GAP_S || (off - last_off).abs() > MAX_STEP_OFF_MODEL_HZ {
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
                        // Twice the ridge's half-width, so most of the reference is not ridge: half
                        // a Hann main lobe (2 / win) plus half the smear within a window.
                        let win = short.meta.win_s;
                        let reference = (4.0 * band).max(noise_band).max(4.0 / win + chirp * win);
                        let along = |tk: f64| model(tk) - fp;
                        let j = short.frame_index(t);
                        let peak = guided_ridge_peak(
                            short,
                            j,
                            fp,
                            along,
                            band,
                            reference,
                            half,
                            min_snr,
                            min_z,
                            short_background,
                        );
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
                        let peak = guided_ridge_peak(
                            spec, j, fp, along, peak_band, reference, half, min_snr, min_z,
                            background,
                        );
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
    measure_with_fit(x, sr, det, id, p, prepare_constant_velocity)
}

fn measure_with_fit(
    x: &[f32],
    sr: u32,
    det: &Detection,
    id: &str,
    p: &MeasureParams,
    mut fit_trace: impl FnMut(&[f64], &[f64], f64, Side) -> Result<PreparedFit, String>,
) -> MeasuredEvent {
    let c = p.c;
    let dur = x.len() as f64 / sr as f64;
    let tau = det.d / det.v.max(10.0);
    let span = (4.0 * tau).clamp(45.0, 300.0);
    let (ta, tb) = ((det.t0r - span).max(0.0), (det.t0r + span).min(dur));
    let v_hi = (det.v * 1.4).min(0.8 * c);
    let fmin = (det.f0 * c / (c + v_hi) * 0.93 - 3.0).max(15.0);
    let fmax = (det.f0 * c / (c - v_hi) * 1.07 + 3.0).min(sr as f64 * 0.45);
    let roc = det.f0 * det.v * det.v / (c * det.d.max(50.0));
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
    let mut model: Box<dyn Fn(f64) -> f64 + Sync> =
        Box::new(model_fn(det.f0, det.v, det.d, det.t0r, c));
    let mut band = (0.04 * det.f0).clamp(3.0, 12.0);
    let mut prepared: Option<PreparedFit> = None;
    let mut trace = Trace::default();
    // From the latest fit, since a detection can rate a steep pass far too gently.
    let mut fitted_roc = roc;
    for round in 0..p.rounds {
        let steep_win = window_for_roc(fitted_roc);
        let steep = (steep_win < MIN_MAIN_WIN_S)
            .then(|| {
                stft_band_db(&x[s0..s1], sr, steep_win, p.hop_s, fmin, fmax, p.df_max, ca).ok()
            })
            .flatten()
            .map(|mut short| {
                let background = short.normalize_rows_with_offsets(None);
                (short, background)
            });
        let tr = guided_trace_with_background(
            &spec,
            Some(&background),
            steep
                .as_ref()
                .map(|(short, offsets)| (short, Some(offsets.as_slice()))),
            &*model,
            ta,
            tb,
            band,
            p.min_snr,
            p.min_z,
        );
        if tr.len() < 20 {
            if round == 0 {
                quality
                    .reasons
                    .push(format!("guided trace found only {} ridge points", tr.len()));
            }
            break;
        }
        match fit_trace(&tr.t, &tr.f, c, Side::Both) {
            Ok(fr) => {
                let b = fr.best();
                model = Box::new(model_fn(b.f0, b.v, b.d, b.t0r, c));
                fitted_roc = readouts(b.f0, b.v, b.d, c).max_roc;
                band = (3.0 * b.rmse).clamp(1.0, band);
                trace = tr;
                prepared = Some(fr);
            }
            Err(e) => {
                quality.reasons.push(format!("fit: {e}"));
                // Keep a successful fit paired with its own trace if a later round fails.
                if prepared.is_none() {
                    trace = tr;
                }
                break;
            }
        }
    }
    // Only the final accepted trace needs uncertainty profiles and manoeuvre diagnostics.
    let fit = prepared.map(PreparedFit::complete);
    // Keep only the analysis window plus a scoring margin in memory.
    let keep_a = spec.frame_index((ta - 60.0).max(spec.t[0]));
    let keep_b = (spec.frame_index((tb + 60.0).min(spec.t[spec.nt() - 1])) + 1).min(spec.nt());
    let spec = spec.crop(keep_a, keep_b, 0, spec.nf());
    quality.n_points = trace.len();
    if let Some(fr) = &fit {
        let t0r = fr.best.t0r;
        let (mut amin, mut rmax) = (t0r, t0r);
        for &t in &trace.t {
            amin = amin.min(t);
            rmax = rmax.max(t);
        }
        quality.span_approach_s = t0r - amin;
        quality.span_recede_s = rmax - t0r;
        let mut snr = trace.snr.clone();
        quality.median_snr_db = crate::spectro::median_f64(&mut snr);
        quality.rmse_hz = fr.best.rmse;
        let n_eff = fr.diagnostics.n_eff.max(1.0);
        quality.score = quality.median_snr_db.max(0.0) * n_eff.sqrt();
        let b = fr.best;
        let asymptotes = readouts(b.f0, b.v, b.d, c);
        quality.drop_traversed = drop_traversed(
            &trace.t,
            &trace.f,
            asymptotes.f_pre,
            asymptotes.f_post,
            model_fn(b.f0, b.v, b.d, b.t0r, c),
        );
        if trace.len() < p.min_points {
            quality
                .reasons
                .push(format!("{} trace points < {}", trace.len(), p.min_points));
        }
        if quality.span_approach_s < p.min_side_s || quality.span_recede_s < p.min_side_s {
            quality.reasons.push(format!(
                "one-sided trace (approach {:.0}s, recede {:.0}s): CPA not observed",
                quality.span_approach_s, quality.span_recede_s
            ));
        }
        if quality.drop_traversed < p.min_drop_traversed {
            quality.reasons.push(format!(
                "trace follows only {:.0} % of the central frequency drop: no ridge through the CPA",
                100.0 * quality.drop_traversed
            ));
        }
        if b.rmse > p.max_rmse_hz {
            quality
                .reasons
                .push(format!("fit RMS {:.2} Hz > {:.2}", b.rmse, p.max_rmse_hz));
        }
        if !(15.0..=0.8 * c).contains(&b.v) {
            quality
                .reasons
                .push(format!("implausible speed {:.0} m/s", b.v));
        }
        if !(60.0..=25_000.0).contains(&b.d) {
            quality
                .reasons
                .push(format!("implausible CPA distance {:.0} m", b.d));
        }
        if (b.t0r - det.t0r).abs() > (0.5 * span).max(20.0) {
            quality.reasons.push(format!(
                "fit CPA {:.1}s drifted from detection {:.1}s",
                b.t0r, det.t0r
            ));
        }
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
        win_s: win,
        spec: Some(spec),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{DetectParams, detect, refine, survey};
    use crate::fit::fit_constant_velocity;

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
        let mut eager_fit = None;
        // The previous measurement path completed full uncertainty diagnostics in each
        // round. Use that public API as the oracle for all three model/band updates.
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
            );
            assert!(trace.len() >= 20);
            let fit = fit_constant_velocity(&trace.t, &trace.f, c, Side::Both).unwrap();
            let b = fit.best;
            model = Box::new(model_fn(b.f0, b.v, b.d, b.t0r, c));
            band = (3.0 * b.rmse).clamp(1.0, band);
            eager_trace = trace;
            eager_fit = Some(fit);
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

    fn measure_simultaneous_same_tone_passes(amplitudes: [f64; 2]) -> Vec<MeasuredEvent> {
        let sr = 4000u32;
        let (f0, v, t0r, c) = (118.0, 120.0, 180.0, 343.0);
        let distances = [1000.0, 7000.0];
        let mut phases = [0.0f64; 2];
        let mut seed = 99u64;
        let x: Vec<f32> = (0..sr as usize * 360)
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
            .collect();
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
        crate::search::run(&input, &params, &|_| {})
            .funnel
            .events_used
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
        };
        let mut p = MeasureParams {
            hop_s: 0.25,
            rounds: 1,
            ..Default::default()
        };
        let first = measure(&x, sr, &det, "e1", &p);
        assert!(first.quality.ok, "{:?}", first.quality.reasons);
        p.rounds = 3;
        let mut rounds = 0;
        let interrupted = measure_with_fit(&x, sr, &det, "e1", &p, |t, f, c, side| {
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
        let failed = measure_with_fit(&x, sr, &det, "e1", &p, |_, _, _, _| {
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
}
