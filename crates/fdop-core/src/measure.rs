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

/// At most this many events are measured, strongest primary detections first. Measuring more
/// of a long recording's flyovers can promote a wrong answer (CHANGELOG, Unreleased), so the cap
/// stays until the grading accounts for how many events were searched.
pub const MAX_MEASURED: usize = 24;

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
    /// Least share of the fitted frequency drop the trace must travel through (`drop_traversed`).
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
    /// Share of the fitted frequency drop the trace travels through.
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
/// be off on passes traced well. Four times too gentle usually loses the CPA frames, and the event
/// is refused.
const STEEP_TIME_ERROR_S: f64 = 0.25;

/// Share of a fit's frequency drop that the trace travels through. A pass's ridge moves from the
/// approach tone to the recede tone and is loudest on the way. Two steady tones joined by a fit do
/// not: the trace sits flat on each, and the fit crosses the empty space between them with points
/// on both sides of the CPA, a low RMS and a plausible speed. Within the central 80 % of the drop,
/// this adds up the net fall of each continuous stretch of the trace (`t` ascending) and divides by
/// the model's fall there. A trace hopping down from tone to tone earns nothing for its hops, but
/// tones spaced within `MAX_STEP_OFF_MODEL_HZ` of each other can still pass for a ridge.
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
    /// The trace takes the peak nearest the model in each frame with no limit on continuity, and
    /// `band` is at least 1 Hz, so a clean fit's trace cannot step further than this.
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
                    Some(short) => {
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
                            short, j, fp, along, band, reference, half, min_snr, min_z,
                        );
                        (peak, band)
                    }
                    None => {
                        let slope = (model(t + 0.5) - model(t - 0.5)) / 1.0;
                        let along = |tk: f64| slope * (tk - t);
                        let peak = guided_ridge_peak(
                            spec, j, fp, along, band, noise_band, half, min_snr, min_z,
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
    spec.normalize_rows(None);
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
                short.normalize_rows(None);
                short
            });
        let tr = guided_trace(
            &spec,
            steep.as_ref(),
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
                "trace follows only {:.0} % of the fitted frequency drop: no ridge through the CPA",
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
        assert!(!ev.quality.ok, "{}", ev.quality.drop_traversed);
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
        // a faint tone at the steepest the main window holds
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
        let mut model: Box<dyn Fn(f64) -> f64 + Sync> = Box::new(tone);
        let mut band = (0.04 * det.f0).clamp(3.0, 12.0);
        let mut eager_trace = Trace::default();
        let mut eager_fit = None;
        // The previous measurement path completed full uncertainty diagnostics in each
        // round. Use that public API as the oracle for all three model/band updates.
        for _ in 0..p.rounds {
            let trace = guided_trace(spec, None, &*model, 0.0, 120.0, band, p.min_snr, p.min_z);
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
}
