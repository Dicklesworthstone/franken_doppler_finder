//! Automatic flyover detection: a matched-filter bank of retarded-model Doppler curves.
//!
//! A pass's frequency trajectory is f(t) = f0 * r_{v,d}(t - t0r). On a log-frequency axis the
//! shape r is a fixed path for each (v, d) and f0 is a pure row shift, so one template per
//! (v, d) scores every (t0r, f0) by summing the row-normalised spectrogram along the shifted
//! path. The statistic is min(mean over the approach half, mean over the recede half): a real
//! flyover lights up both halves, while a flat engine tone, a stepped tone or a click column can
//! only light one. Detections are then refined on the linear spectrogram (continuous f0, t0r,
//! v, d) so that ridge-trace seeds land on the ridge, and grouped into harmonic families.

use crate::physics::{cv_ratio_rel_t0r, harmonic_frequencies};
use crate::spectro::{Spec, stft_band_db};
use rayon::prelude::*;
use serde::Serialize;

const RIDGE_HALF_ROWS: i64 = 3;

#[derive(Clone, Debug)]
pub struct DetectParams {
    pub fmin: f64,
    pub fmax: f64,
    pub win_s: f64,
    pub hop_s: f64,
    pub bins_per_octave: usize,
    pub v_grid: Vec<f64>,
    pub d_grid: Vec<f64>,
    /// Spacing (s) of the path samples.
    pub path_dt: f64,
    /// Path half-lengths = clamp(k * d / v, min_half_s, max_half_s) for each k: short paths
    /// catch passes whose asymptotes fade into the noise, long ones accumulate more evidence.
    pub k_tau: Vec<f64>,
    pub min_half_s: f64,
    pub max_half_s: f64,
    /// Min-halves matched-filter score (robust-z sum / sqrt(n)) of a strong curve: strong curves
    /// are accepted first and their residual searched for weaker ones. Pure noise peaks near 3.
    pub thr_db: f64,
    /// Least score a detection keeps. Every primary from here up is a measurement candidate,
    /// strongest first, and the measurement gates decide which are flyovers; the line also
    /// decides whether a gap between two events is silent.
    pub thr_silence_db: f64,
    pub max_detections: usize,
    /// Least robust spread (1.4826 x MAD over every cell) the survey image is scored at. The two
    /// lines above were set on recordings whose image spread is 0.57 to 0.78. A recording that is
    /// loud only while a pass goes over (a short clip, a livestream near traffic) inflates each
    /// row's MAD with that swell, which shrinks noise and ridge alike (Livermore clips: 0.18 to
    /// 0.7), so a fixed line asks several times more of it. An image under this spread is
    /// multiplied up to it (at most `MAX_IMAGE_GAIN` times) and clamped again; an image at or
    /// above it is left exactly as it is.
    pub min_image_scale: f64,
    pub c: f64,
}

/// Largest factor the survey image is raised by. An image with almost no spread holds almost no
/// structure; raising it without bound would turn rounding into ridges.
pub const MAX_IMAGE_GAIN: f64 = 4.0;

impl Default for DetectParams {
    fn default() -> Self {
        let d_grid: Vec<f64> = (0..14)
            .map(|i| 250.0 * (12_000.0f64 / 250.0).powf(i as f64 / 13.0))
            .collect();
        Self {
            fmin: 35.0,
            fmax: 1100.0,
            win_s: 1.0,
            hop_s: 1.0,
            bins_per_octave: 48,
            v_grid: vec![
                40.0, 52.0, 64.0, 76.0, 88.0, 100.0, 113.0, 127.0, 142.0, 158.0, 175.0,
            ],
            d_grid,
            path_dt: 2.0,
            k_tau: vec![1.25, 2.0, 3.0],
            min_half_s: 24.0,
            max_half_s: 240.0,
            thr_db: 4.5,
            thr_silence_db: 3.5,
            max_detections: 60,
            min_image_scale: 0.5,
            c: fdop_types::C_SOUND,
        }
    }
}

/// Log-frequency survey spectrogram, row-major `[row][frame]`.
pub struct LogSpec {
    pub t: Vec<f64>,
    pub freqs: Vec<f64>,
    pub data: Vec<f32>,
    pub bins_per_octave: usize,
    pub hop_s: f64,
    /// Robust spread of the image as surveyed (1.4826 x MAD over every cell), before any gain.
    pub scale: f64,
    /// Factor the image was multiplied by to reach `DetectParams::min_image_scale` (1 = none).
    pub gain: f64,
}

/// 1.4826 x the median absolute deviation from the median, over every value.
fn robust_scale(v: &[f32]) -> f64 {
    let mut tmp = v.to_vec();
    let m = crate::spectro::median_f32(&mut tmp);
    for x in tmp.iter_mut() {
        *x = (*x - m).abs();
    }
    1.4826 * crate::spectro::median_f32(&mut tmp) as f64
}

/// Gain that brings an image of robust spread `scale` up to `floor`: 1 at or above it, and at
/// most `MAX_IMAGE_GAIN`. An image with no spread at all (silence) is left alone.
fn image_gain(scale: f64, floor: f64) -> f64 {
    if scale > 0.0 && scale < floor {
        (floor / scale).min(MAX_IMAGE_GAIN)
    } else {
        1.0
    }
}

impl LogSpec {
    pub fn nt(&self) -> usize {
        self.t.len()
    }
    pub fn nr(&self) -> usize {
        self.freqs.len()
    }
}

/// Survey: linear STFT, row-median normalisation, clip, then max-pool onto a log-f axis.
pub fn survey(x: &[f32], sr: u32, p: &DetectParams) -> Result<(LogSpec, Spec), String> {
    let mut lin = stft_band_db(
        x,
        sr,
        p.win_s,
        p.hop_s,
        p.fmin * 0.95,
        p.fmax * 1.05,
        1.0,
        0.0,
    )?;
    lin.normalize_rows(None);
    for v in lin.db.iter_mut() {
        *v = v.clamp(-6.0, 20.0);
    }
    let b = p.bins_per_octave as f64;
    let n_rows = ((p.fmax / p.fmin).log2() * b).floor() as usize + 1;
    let freqs: Vec<f64> = (0..n_rows)
        .map(|k| p.fmin * 2f64.powf(k as f64 / b))
        .collect();
    let nt = lin.nt();
    let df = lin.df();
    let mut data = vec![0f32; n_rows * nt];
    data.par_chunks_mut(nt).enumerate().for_each(|(k, row)| {
        let fc = freqs[k];
        let (lo, hi) = (fc * 2f64.powf(-0.5 / b), fc * 2f64.powf(0.5 / b));
        let i0 = ((lo - lin.f[0]) / df).ceil().max(0.0) as usize;
        let i1 = (((hi - lin.f[0]) / df).floor() as i64).min(lin.nf() as i64 - 1);
        if i1 >= i0 as i64 {
            for (ti, out) in row.iter_mut().enumerate() {
                let mut m = f32::NEG_INFINITY;
                for fi in i0..=i1 as usize {
                    m = m.max(lin.at(fi, ti));
                }
                *out = m;
            }
        } else {
            for (ti, out) in row.iter_mut().enumerate() {
                *out = lin.bilinear(lin.t[ti], fc).unwrap_or(0.0);
            }
        }
    });
    // Max-pooling k linear bins raises the noise floor by an amount that grows with k (i.e. with
    // frequency), so put every log row on a common robust z scale before matched filtering.
    data.par_chunks_mut(nt).for_each(|row| {
        let mut tmp = row.to_vec();
        let med = crate::spectro::median_f32(&mut tmp);
        for v in tmp.iter_mut() {
            *v = (*v - med).abs();
        }
        let mad = 1.4826 * crate::spectro::median_f32(&mut tmp) + 0.05;
        for v in row.iter_mut() {
            *v = ((*v - med) / mad).clamp(-3.0, 8.0);
        }
    });
    // Broadband events (traffic, wind gusts, handling noise) raise whole columns; a Doppler ridge
    // is 1-3 log-rows wide. Subtract each frame's running median across +-1/3 octave so only
    // narrowband structure survives.
    let half_w = (p.bins_per_octave / 3).max(4);
    let mut cols = vec![0f32; n_rows * nt];
    cols.par_chunks_mut(n_rows)
        .enumerate()
        .for_each(|(ti, col)| {
            let src: Vec<f32> = (0..n_rows).map(|r| data[r * nt + ti]).collect();
            let mut win = Vec::with_capacity(2 * half_w + 1);
            for (r, out) in col.iter_mut().enumerate() {
                let (a, b) = (r.saturating_sub(half_w), (r + half_w + 1).min(n_rows));
                win.clear();
                win.extend_from_slice(&src[a..b]);
                let m = crate::spectro::median_f32(&mut win);
                *out = (src[r] - m).clamp(-3.0, 8.0);
            }
        });
    data.par_chunks_mut(nt).enumerate().for_each(|(r, row)| {
        for (ti, v) in row.iter_mut().enumerate() {
            *v = cols[ti * n_rows + r];
        }
    });
    // Put a compressed image back on the spread the detection lines were set at (see
    // `DetectParams::min_image_scale`). Noise and ridge scale together, so this changes the
    // lines' meaning, not the evidence; an image at or above the floor is untouched.
    let scale = robust_scale(&data);
    let gain = image_gain(scale, p.min_image_scale);
    if gain > 1.0 {
        let g = gain as f32;
        data.par_iter_mut()
            .for_each(|v| *v = (*v * g).clamp(-3.0, 8.0));
    }
    Ok((
        LogSpec {
            t: lin.t.clone(),
            freqs,
            data,
            bins_per_octave: p.bins_per_octave,
            hop_s: lin.dt(),
            scale,
            gain,
        },
        lin,
    ))
}

struct Template {
    v: f64,
    d: f64,
    half: f64,
    /// (frame offset, row offset, is_approach)
    path: Vec<(i32, i32, bool)>,
    n_a: usize,
    n_r: usize,
}

/// Score each row independently so a worker keeps only one row's sums in cache. Coverage
/// comes from intervals of valid frames, rather than summing a full padded mask per sample.
/// The evidence additions retain path order, including when scoring the residual pass.
fn score_bank(ls: &LogSpec, tmpls: &[Template], evidence: &[f32]) -> (Vec<f32>, Vec<u16>) {
    let (nt, nr) = (ls.nt(), ls.nr());
    let mut best = vec![f32::NEG_INFINITY; nr * nt];
    let mut indices = vec![0u16; nr * nt];
    let max_count = tmpls.iter().map(|t| t.n_a.max(t.n_r)).max().unwrap_or(0);
    let roots: Vec<f32> = (0..=max_count).map(|n| (n as f32).sqrt()).collect();
    best.par_chunks_mut(nt)
        .zip(indices.par_chunks_mut(nt))
        .enumerate()
        .for_each_init(
            || {
                (
                    vec![0f32; nt],
                    vec![0f32; nt],
                    vec![0i32; nt + 1],
                    vec![0i32; nt + 1],
                )
            },
            |(sa, sr, ca, cr), (r, (bs, bi))| {
                for (ti, tm) in tmpls.iter().enumerate() {
                    sa.fill(0.0);
                    sr.fill(0.0);
                    ca.fill(0);
                    cr.fill(0);
                    for &(dt, dr, app) in &tm.path {
                        let source_row = r as i64 + dr as i64;
                        if source_row < 0 || source_row >= nr as i64 {
                            continue;
                        }
                        let a = (-(dt as i64)).clamp(0, nt as i64) as usize;
                        let b = (nt as i64 - dt as i64).clamp(0, nt as i64) as usize;
                        if a >= b {
                            continue;
                        }
                        let (s, counts) = if app {
                            (&mut *sa, &mut *ca)
                        } else {
                            (&mut *sr, &mut *cr)
                        };
                        let src = source_row as usize * nt + (a as i64 + dt as i64) as usize;
                        for (sum, &z) in s[a..b].iter_mut().zip(&evidence[src..src + b - a]) {
                            *sum += z;
                        }
                        counts[a] += 1;
                        counts[b] -= 1;
                    }
                    let (mut na, mut nrr) = (0i32, 0i32);
                    for k in 0..nt {
                        na += ca[k];
                        nrr += cr[k];
                        if (na as f32) < 0.6 * tm.n_a as f32 || (nrr as f32) < 0.6 * tm.n_r as f32 {
                            continue;
                        }
                        // Keep sum/sqrt(n), including zeroed known samples in n: removing
                        // those samples from coverage would inflate residual detections.
                        let s = (sa[k] / roots[na as usize]).min(sr[k] / roots[nrr as usize]);
                        if s > bs[k] || (s == bs[k] && ti < bi[k] as usize) {
                            bs[k] = s;
                            bi[k] = ti as u16;
                        }
                    }
                }
            },
        );
    (best, indices)
}

fn build_templates(p: &DetectParams, hop: f64) -> Vec<Template> {
    let b = p.bins_per_octave as f64;
    let mut out = Vec::new();
    for &v in &p.v_grid {
        for &d in &p.d_grid {
            let mut halves: Vec<f64> = p
                .k_tau
                .iter()
                .map(|k| (k * d / v).clamp(p.min_half_s, p.max_half_s))
                .collect();
            halves.sort_by(f64::total_cmp);
            halves.dedup_by(|a, b| (*a - *b).abs() < 0.15 * *b);
            for half in halves {
                let n = (half / p.path_dt).floor() as i64;
                let mut path = Vec::new();
                for k in -n..=n {
                    if k == 0 {
                        continue;
                    }
                    let u = k as f64 * p.path_dt;
                    let r = cv_ratio_rel_t0r(u, v, d, p.c);
                    path.push((
                        (u / hop).round() as i32,
                        (r.log2() * b).round() as i32,
                        k < 0,
                    ));
                }
                let n_a = path.iter().filter(|x| x.2).count();
                let n_r = path.len() - n_a;
                out.push(Template {
                    v,
                    d,
                    half,
                    path,
                    n_a,
                    n_r,
                });
            }
        }
    }
    out
}

#[derive(Clone, Debug, Serialize)]
pub struct Detection {
    pub id: usize,
    /// Delayed (received) CPA time, recording seconds.
    pub t0r: f64,
    pub f0: f64,
    pub v: f64,
    pub d: f64,
    /// min(approach mean, recede mean), dB above the row median.
    pub score: f64,
    pub score_approach: f64,
    pub score_recede: f64,
    /// Path half-length used (s).
    pub half_s: f64,
    /// Primary detection of the harmonic family this one belongs to (None = primary).
    pub harmonic_of: Option<usize>,
    /// For primaries: frequency ratios (to this f0) of the other family members.
    pub harmonics: Vec<f64>,
}

/// Run the template bank. Returns detections with score >= `thr_silence_db`, strongest first;
/// the primaries among them are the event candidates.
pub fn detect(ls: &LogSpec, p: &DetectParams) -> Vec<Detection> {
    detect_counted(ls, p).0
}

/// [`detect`], plus how many accepted candidates the cap `p.max_detections` cut. The count is a
/// lower bound when the scan stopped at three times the cap.
pub fn detect_counted(ls: &LogSpec, p: &DetectParams) -> (Vec<Detection>, usize) {
    let (nt, nr) = (ls.nt(), ls.nr());
    if nt < 8 || nr < 8 {
        return (Vec::new(), 0);
    }
    let tmpls = build_templates(p, ls.hop_s);
    let mut lp = ls.data.clone();
    // Greedy non-maximum suppression in (t0r, log f0) plus residual evidence: `known` marks
    // the cells (+-3 log-rows) along every accepted curve; a later candidate is re-scored with
    // those cells counted as zero, so a template that merely rides an already detected ridge
    // with a different (v, d) cannot become a second event.
    let mut known = vec![false; nr * nt];
    let bpo = p.bins_per_octave as f64;
    let mut out: Vec<Detection> = Vec::new();
    let mut remaining_candidates = 400_000usize;
    for pass in 0..2 {
        let (bs, bi) = score_bank(ls, &tmpls, &lp);
        // Weak detections excuse unheard passes; they must not erase a measurable curve
        // before its template gets a chance to win. Accept strong curves first, then search
        // their residual for both strong and weak curves. A weak-only recording keeps the
        // ordinary single scan at the silence threshold.
        let strong_pass = pass == 0 && bs.iter().any(|&s| s as f64 >= p.thr_db);
        let threshold = if strong_pass {
            p.thr_db.max(p.thr_silence_db)
        } else {
            p.thr_silence_db
        };
        let mut cand: Vec<usize> = (0..nr * nt)
            .filter(|&k| bs[k] as f64 >= threshold)
            .collect();
        cand.sort_by(|&a, &b| bs[b].total_cmp(&bs[a]));
        cand.truncate(remaining_candidates);
        remaining_candidates -= cand.len();
        for k in cand {
            if out.len() >= p.max_detections * 3 {
                break;
            }
            let (r, i) = (k / nt, k % nt);
            let tm = &tmpls[bi[k] as usize];
            let t0r = ls.t[i];
            let f0 = ls.freqs[r];
            let tau = tm.d / tm.v;
            let half = tm.half;
            let mut candidate = Detection {
                id: out.len(),
                t0r,
                f0,
                v: tm.v,
                d: tm.d,
                score: bs[k] as f64,
                score_approach: 0.0,
                score_recede: 0.0,
                half_s: half,
                harmonic_of: None,
                harmonics: Vec::new(),
            };
            let suppressed = out.iter().any(|o| {
                let dt_lim = (0.5 * tau.min(o.d / o.v)).max(10.0);
                let same_line = ((f0 / o.f0).log2() * bpo).abs() < 4.0;
                // Separate aircraft can share a tone and CPA. Suppress a neighbouring seed only
                // when its transition also agrees at the accepted-curve mask resolution.
                (t0r - o.t0r).abs() < dt_lim && same_line && harmonic_shapes(&candidate, o, p)
            });
            if suppressed {
                continue;
            }
            let (ra, rr) = residual_halves(ls, r, i, tm, &known);
            if (ra.min(rr) as f64) < threshold {
                continue;
            }
            // mark this curve
            let (j0, j1) = (
                ls.t.partition_point(|&x| x < t0r - half),
                ls.t.partition_point(|&x| x <= t0r + half),
            );
            for j in j0..j1 {
                let fr = f0 * cv_ratio_rel_t0r(ls.t[j] - t0r, tm.v, tm.d, p.c);
                let row = ((fr / ls.freqs[0]).log2() * bpo).round() as i64;
                for rr2 in
                    (row - RIDGE_HALF_ROWS).max(0)..=(row + RIDGE_HALF_ROWS).min(nr as i64 - 1)
                {
                    known[rr2 as usize * nt + j] = true;
                }
            }
            candidate.score = ra.min(rr) as f64;
            candidate.score_approach = ra as f64;
            candidate.score_recede = rr as f64;
            out.push(candidate);
        }
        if !strong_pass
            || out.is_empty()
            || out.len() >= p.max_detections * 3
            || remaining_candidates == 0
        {
            break;
        }
        // A weaker, different curve can lose every (time, tone) cell to the stronger pass.
        // Search the residual evidence once so its template can become a cell winner.
        // Keep the original coverage mask and denominator: known samples contribute zero,
        // exactly as in residual_halves, rather than making short fragments look stronger.
        for (z, &seen) in lp.iter_mut().zip(&known) {
            if seen {
                *z = 0.0;
            }
        }
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    let cut = out.len().saturating_sub(p.max_detections);
    out.truncate(p.max_detections);
    for (i, d) in out.iter_mut().enumerate() {
        d.id = i;
    }
    group_harmonics(&mut out, p);
    (out, cut)
}

/// Matched-filter halves of template `tm` anchored at (row `r`, frame `i`), with samples on
/// `known` cells contributing zero (but still counted in n).
fn residual_halves(ls: &LogSpec, r: usize, i: usize, tm: &Template, known: &[bool]) -> (f32, f32) {
    let (nt, nr) = (ls.nt() as i64, ls.nr() as i64);
    let (mut sa, mut na, mut sr, mut nrr) = (0f32, 0usize, 0f32, 0usize);
    for &(dt, dr, app) in &tm.path {
        let (ti, ri) = (i as i64 + dt as i64, r as i64 + dr as i64);
        if ti < 0 || ti >= nt || ri < 0 || ri >= nr {
            continue;
        }
        let k = ri as usize * nt as usize + ti as usize;
        let z = if known[k] { 0.0 } else { ls.data[k] };
        if app {
            sa += z;
            na += 1;
        } else {
            sr += z;
            nrr += 1;
        }
    }
    if (na as f64) < 0.6 * tm.n_a as f64 || (nrr as f64) < 0.6 * tm.n_r as f64 {
        return (f32::NEG_INFINITY, f32::NEG_INFINITY);
    }
    (sa / (na as f32).sqrt(), sr / (nrr as f32).sqrt())
}

/// Mark detections that are harmonics of a stronger one (same t0r within 4 s, same shape,
/// small-integer frequency ratio within 1.5 %).
fn group_harmonics(dets: &mut [Detection], p: &DetectParams) {
    let n = dets.len();
    for j in 0..n {
        for i in 0..j {
            if dets[i].harmonic_of.is_some() {
                continue;
            }
            let (a, b) = (&dets[i], &dets[j]);
            if (a.t0r - b.t0r).abs() > 4.0 || (a.v - b.v).abs() > 30.0 {
                continue;
            }
            if harmonic_frequencies(a.f0, b.f0, 8, 0.015, false) && harmonic_shapes(a, b, p) {
                let ratio = b.f0 / a.f0;
                dets[j].harmonic_of = Some(dets[i].id);
                dets[i].harmonics.push(ratio);
                break;
            }
        }
    }
}

fn harmonic_shapes(a: &Detection, b: &Detection, p: &DetectParams) -> bool {
    let ta = (a.t0r - a.half_s).max(b.t0r - b.half_s);
    let tb = (a.t0r + a.half_s).min(b.t0r + b.half_s);
    if tb <= ta {
        return false;
    }
    let n = ((tb - ta) / p.path_dt.max(p.hop_s)).ceil() as usize;
    // Use the combined widths of the existing +/- RIDGE_HALF_ROWS accepted-curve masks.
    // This permits neighbouring v/d grid choices for the same physical pass,
    // but not different Doppler transitions that only share a CPA time and tone ratio.
    (0..=n).all(|k| {
        let t = ta + (tb - ta) * k as f64 / n.max(1) as f64;
        let ra = cv_ratio_rel_t0r(t - a.t0r, a.v, a.d, p.c);
        let rb = cv_ratio_rel_t0r(t - b.t0r, b.v, b.d, p.c);
        ((ra / rb).log2() * p.bins_per_octave as f64).abs() <= 2.0 * RIDGE_HALF_ROWS as f64
    })
}

/// Mean of the linear spectrogram along the modelled curve (approach, recede halves) for a
/// continuous (t0r, f0, v, d) hypothesis.
#[allow(clippy::too_many_arguments)]
pub fn path_score(
    spec: &Spec,
    t0r: f64,
    f0: f64,
    v: f64,
    d: f64,
    half: f64,
    step: f64,
    c: f64,
) -> (f64, f64) {
    let n = (half / step).floor() as i64;
    let (mut sa, mut na, mut sr, mut nr) = (0.0, 0usize, 0.0, 0usize);
    for k in -n..=n {
        if k == 0 {
            continue;
        }
        let u = k as f64 * step;
        if let Some(val) = spec.bilinear(t0r + u, f0 * cv_ratio_rel_t0r(u, v, d, c)) {
            if k < 0 {
                sa += val as f64;
                na += 1;
            } else {
                sr += val as f64;
                nr += 1;
            }
        }
    }
    let ma = if na * 3 >= n as usize {
        sa / na as f64
    } else {
        f64::NEG_INFINITY
    };
    let mr = if nr * 3 >= n as usize {
        sr / nr as f64
    } else {
        f64::NEG_INFINITY
    };
    (ma, mr)
}

/// Coordinate-descent refinement of a detection on a finer linear spectrogram.
pub fn refine(spec: &Spec, det: &Detection, c: f64) -> Detection {
    let mut best = det.clone();
    let step = spec.dt().max(0.25);
    let score = |t0r: f64, f0: f64, v: f64, d: f64| {
        let half = (3.0 * d / v).clamp(24.0, 240.0);
        let (a, r) = path_score(spec, t0r, f0, v, d, half, step, c);
        (a.min(r), a, r)
    };
    let (mut s, mut sa, mut sr) = score(best.t0r, best.f0, best.v, best.d);
    for round in 0..4 {
        let scale = 1.0 / (1 << round) as f64;
        // f0
        let mut improved = false;
        for k in -12..=12 {
            let f0 = best.f0 * (1.0 + 0.0015 * k as f64 * scale);
            let r = score(best.t0r, f0, best.v, best.d);
            if r.0 > s {
                (s, sa, sr) = r;
                best.f0 = f0;
                improved = true;
            }
        }
        for k in -12..=12 {
            let t0r = best.t0r + 0.5 * k as f64 * scale;
            let r = score(t0r, best.f0, best.v, best.d);
            if r.0 > s {
                (s, sa, sr) = r;
                best.t0r = t0r;
                improved = true;
            }
        }
        for k in -8..=8 {
            let v = (best.v + 2.0 * k as f64 * scale).clamp(20.0, 0.6 * c);
            let r = score(best.t0r, best.f0, v, best.d);
            if r.0 > s {
                (s, sa, sr) = r;
                best.v = v;
                improved = true;
            }
        }
        for k in -8..=8 {
            let d = (best.d * (1.0 + 0.05 * k as f64 * scale)).clamp(100.0, 40_000.0);
            let r = score(best.t0r, best.f0, best.v, d);
            if r.0 > s {
                (s, sa, sr) = r;
                best.d = d;
                improved = true;
            }
        }
        if !improved && round > 0 {
            break;
        }
    }
    best.score = s;
    best.score_approach = sa;
    best.score_recede = sr;
    best.half_s = (3.0 * best.d / best.v).clamp(24.0, 240.0);
    best
}

/// (f0, v, d, received CPA time t0r) of the pass in [`swell_scene`].
#[cfg(test)]
pub(crate) const SWELL_PASS: (f64, f64, f64, f64) = (300.0, 90.0, 900.0, 22.0);

/// 48 s at 4 kHz of uniform noise whose level swells by `swell_db` around the CPA, as one loud
/// nearby pass makes a short clip, carrying [`SWELL_PASS`]'s tone at amplitude `amp`, which
/// swells with it (tone and roar come from the same aircraft).
#[cfg(test)]
pub(crate) fn swell_scene(amp: f64, swell_db: f64) -> Vec<f32> {
    let (f0, v, d, t0r) = SWELL_PASS;
    let (sr, dur, c) = (4000.0, 48.0, 343.0);
    let t0e = t0r - d / c;
    let (mut seed, mut phase) = (2026u64, 0.0f64);
    (0..(sr * dur) as usize)
        .map(|i| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let t = i as f64 / sr;
            let env =
                10f64.powf(swell_db / 20.0 * (-0.5 * ((t - t0r) / (0.22 * dur)).powi(2)).exp());
            phase += std::f64::consts::TAU * crate::physics::cv_curve(t, f0, v, d, t0e, c) / sr;
            (env * ((2.0 * seed as f64 / u64::MAX as f64 - 1.0) + amp * phase.sin())) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::cv_curve;

    #[test]
    fn image_scale_floor_leaves_a_spread_image_alone_and_raises_a_compressed_one_exactly() {
        // Metamorphic: the floor is a no-op on an image at or above it (bit for bit, so every
        // recording the lines were set on is surveyed as before), and below it the image is the
        // unscaled one times floor / spread, clamped again.
        let off = DetectParams {
            min_image_scale: 0.0,
            ..DetectParams::default()
        };
        let on = DetectParams::default();
        let x = swell_scene(0.16, 0.0);
        let (a, _) = survey(&x, 4000, &off).unwrap();
        let (b, _) = survey(&x, 4000, &on).unwrap();
        eprintln!("no swell: spread {:.3}, gain {}", b.scale, b.gain);
        assert!(a.scale > on.min_image_scale, "spread {}", a.scale);
        assert_eq!((a.gain, b.gain), (1.0, 1.0));
        assert!(a.data == b.data, "an image above the floor changed");

        let x = swell_scene(0.16, 25.0);
        let (a, _) = survey(&x, 4000, &off).unwrap();
        let (b, _) = survey(&x, 4000, &on).unwrap();
        eprintln!("25 dB swell: spread {:.3}, gain {:.3}", b.scale, b.gain);
        assert!(a.scale < 0.4, "the swell left spread {}", a.scale);
        assert_eq!(a.scale, b.scale);
        let g = on.min_image_scale / a.scale;
        assert!(
            g < MAX_IMAGE_GAIN && (b.gain - g).abs() < 1e-12,
            "gain {}",
            b.gain
        );
        for (&u, &w) in a.data.iter().zip(&b.data) {
            assert_eq!(w, (u * g as f32).clamp(-3.0, 8.0));
        }

        // Bounded, and silence (no spread) is left alone.
        assert_eq!(image_gain(0.01, 0.5), MAX_IMAGE_GAIN);
        assert_eq!(image_gain(0.0, 0.5), 1.0);
        assert_eq!(image_gain(0.5, 0.5), 1.0);
        assert_eq!(image_gain(0.2, 0.0), 1.0);
        assert_eq!(robust_scale(&[0.0; 9]), 0.0);
    }

    #[test]
    fn template_bank_matches_scalar_with_edges_and_residuals() {
        let p = DetectParams {
            // Duplicate templates exercise the earliest-template tie break.
            v_grid: vec![40.0, 100.0, 175.0, 100.0],
            d_grid: vec![250.0, 1000.0, 12_000.0],
            ..DetectParams::default()
        };
        for nt in [8, 61, 251] {
            let nr = 37;
            let ls = LogSpec {
                t: (0..nt).map(|i| i as f64).collect(),
                freqs: (0..nr).map(|r| 35.0 * 2f64.powf(r as f64 / 48.0)).collect(),
                data: (0..nr * nt)
                    .map(|k| (((k * 37 + 11) % 101) as f32 - 50.0) / 17.0)
                    .collect(),
                bins_per_octave: 48,
                hop_s: 1.0,
                scale: 1.0,
                gain: 1.0,
            };
            let tmpls = build_templates(&p, ls.hop_s);
            for residual in [false, true] {
                let known: Vec<bool> = (0..nr * nt)
                    .map(|k| residual && (k % 7 == 0 || (k / nt) % 5 == 0))
                    .collect();
                let evidence: Vec<f32> = ls
                    .data
                    .iter()
                    .zip(&known)
                    .map(|(&z, &seen)| if seen { 0.0 } else { z })
                    .collect();
                let mut expected = (vec![f32::NEG_INFINITY; nr * nt], vec![0u16; nr * nt]);
                for r in 0..nr {
                    for i in 0..nt {
                        let k = r * nt + i;
                        for (ti, tm) in tmpls.iter().enumerate() {
                            // Direct per-cell oracle for the original padded bank. Its
                            // coverage comparison is f32 (residual_halves uses f64, which
                            // rounds the 60% boundary differently for e.g. 25 samples).
                            let (mut a, mut b, mut na, mut nb) = (0f32, 0f32, 0f32, 0f32);
                            for &(dt, dr, approach) in &tm.path {
                                let row = r as i64 + dr as i64;
                                let frame = i as i64 + dt as i64;
                                if row < 0 || row >= nr as i64 || frame < 0 || frame >= nt as i64 {
                                    continue;
                                }
                                let z = evidence[row as usize * nt + frame as usize];
                                if approach {
                                    a += z;
                                    na += 1.0;
                                } else {
                                    b += z;
                                    nb += 1.0;
                                }
                            }
                            let score = if na < 0.6 * tm.n_a as f32 || nb < 0.6 * tm.n_r as f32 {
                                f32::NEG_INFINITY
                            } else {
                                (a / na.sqrt()).min(b / nb.sqrt())
                            };
                            if score > expected.0[k] {
                                expected.0[k] = score;
                                expected.1[k] = ti as u16;
                            }
                        }
                    }
                }
                for threads in [1, 3] {
                    let pool = rayon::ThreadPoolBuilder::new()
                        .num_threads(threads)
                        .build()
                        .unwrap();
                    let actual = pool.install(|| score_bank(&ls, &tmpls, &evidence));
                    for (k, (&a, &b)) in actual.0.iter().zip(&expected.0).enumerate() {
                        assert_eq!(
                            actual.1[k], expected.1[k],
                            "nt={nt} cell={k} residual={residual}"
                        );
                        assert_eq!(
                            a.to_bits(),
                            b.to_bits(),
                            "nt={nt} cell={k} residual={residual}"
                        );
                    }
                }
            }
        }
    }

    fn harmonic_detection(id: usize, f0: f64, d: f64) -> Detection {
        Detection {
            id,
            t0r: 300.0,
            f0,
            v: 100.0,
            d,
            score: 20.0 - id as f64,
            score_approach: 20.0 - id as f64,
            score_recede: 20.0 - id as f64,
            half_s: 120.0,
            harmonic_of: None,
            harmonics: Vec::new(),
        }
    }

    #[test]
    fn simultaneous_different_shapes_are_not_harmonics() {
        // Equal CPA times, equal speeds and a 2:1 tone ratio do not identify one plane:
        // these passes have very different Doppler transitions because their ranges differ.
        let mut dets = vec![
            harmonic_detection(0, 100.0, 1000.0),
            harmonic_detection(1, 200.0, 9000.0),
        ];
        group_harmonics(&mut dets, &DetectParams::default());
        assert!(dets.iter().all(|d| d.harmonic_of.is_none()), "{dets:?}");
    }

    #[test]
    fn harmonic_fifth_groups_in_either_strength_order() {
        for (first, second) in [(100.0, 500.0), (500.0, 100.0)] {
            let mut dets = vec![
                harmonic_detection(0, first, 3000.0),
                harmonic_detection(1, second, 3000.0),
            ];
            group_harmonics(&mut dets, &DetectParams::default());
            assert_eq!(dets[1].harmonic_of, Some(0), "{dets:?}");
            assert_eq!(dets[0].harmonics, vec![second / first]);
        }
    }

    #[test]
    fn harmonic_neighbouring_templates_share_a_family() {
        let mut dets = vec![
            harmonic_detection(0, 100.0, 3000.0),
            harmonic_detection(1, 200.0, 4000.0),
        ];
        dets[1].v = 113.0;
        dets[1].t0r += 2.0;
        group_harmonics(&mut dets, &DetectParams::default());
        assert_eq!(dets[1].harmonic_of, Some(0), "{dets:?}");
    }

    /// Synthesise a flyover (fundamental + 2nd harmonic) plus a flat engine tone in noise.
    fn synth(sr: u32, dur: f64) -> Vec<f32> {
        let n = (sr as f64 * dur) as usize;
        let mut x = vec![0f32; n];
        let mut seed = 12345u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed as f64 / u64::MAX as f64) * 2.0 - 1.0
        };
        let (f0, v, d, t0e) = (118.0, 120.0, 3000.0, 300.0);
        let mut phase = 0.0f64;
        for (i, xi) in x.iter_mut().enumerate() {
            let t = i as f64 / sr as f64;
            let f = cv_curve(t, f0, v, d, t0e, 343.0);
            phase += 2.0 * std::f64::consts::PI * f / sr as f64;
            let amp = 0.08;
            *xi = (amp * phase.sin()
                + 0.5 * amp * (2.0 * phase).sin()
                + 0.06 * (2.0 * std::f64::consts::PI * 250.0 * t).sin()
                + 0.9 * rnd()) as f32;
        }
        x
    }

    #[test]
    fn noise_only_stays_below_threshold() {
        let sr = 4000u32;
        let mut seed = 987654321u64;
        let x: Vec<f32> = (0..sr as usize * 900)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                ((seed as f64 / u64::MAX as f64) * 2.0 - 1.0) as f32
            })
            .collect();
        let p = DetectParams::default();
        let (ls, _) = survey(&x, sr, &p).unwrap();
        let dets = detect(&ls, &p);
        let mx = dets.iter().map(|d| d.score).fold(0.0, f64::max);
        eprintln!("noise max score {mx:.2} over {} candidates", dets.len());
        assert!(mx < p.thr_db, "noise reached {mx}");
    }

    #[test]
    fn finds_the_pass_not_the_flat_tone() {
        let sr = 4000;
        let x = synth(sr, 600.0);
        let p = DetectParams::default();
        let (ls, lin) = survey(&x, sr, &p).unwrap();
        let dets = detect(&ls, &p);
        let strong: Vec<&Detection> = dets
            .iter()
            .filter(|d| d.score >= p.thr_db && d.harmonic_of.is_none())
            .collect();
        assert!(!strong.is_empty(), "no detection");
        for d in dets.iter().take(8) {
            eprintln!(
                "det t0r={:.1} f0={:.1} v={:.0} d={:.0} s={:.2} a={:.2} r={:.2} h={:?}",
                d.t0r, d.f0, d.v, d.d, d.score, d.score_approach, d.score_recede, d.harmonic_of
            );
        }
        let top = strong[0];
        let t0r_true = 300.0 + 3000.0 / 343.0;
        assert!(
            (top.t0r - t0r_true).abs() < 6.0,
            "t0r {} vs {}",
            top.t0r,
            t0r_true
        );
        assert!((top.f0 / 118.0 - 1.0).abs() < 0.03, "f0 {}", top.f0);
        assert!(
            !dets
                .iter()
                .any(|d| d.score >= p.thr_db && (d.f0 / 250.0 - 1.0).abs() < 0.02),
            "flat tone detected"
        );
        let r = refine(&lin, top, 343.0);
        assert!((r.f0 / 118.0 - 1.0).abs() < 0.01, "refined f0 {}", r.f0);
    }

    #[test]
    fn detection_cap_reports_what_it_cut() {
        // a cap of one on a pass with a harmonic (plus the flat tone) must say what it cut
        let sr = 4000;
        let x = synth(sr, 600.0);
        let mut p = DetectParams::default();
        let (ls, _) = survey(&x, sr, &p).unwrap();
        let (all, cut_all) = detect_counted(&ls, &p);
        assert_eq!(cut_all, 0, "{} detections under the default cap", all.len());
        assert!(all.len() >= 2, "need two candidates, got {}", all.len());
        p.max_detections = 1;
        let (one, cut) = detect_counted(&ls, &p);
        assert_eq!(one.len(), 1);
        assert!(cut >= 1, "cut {cut}");
        assert_eq!(one[0].score, all[0].score, "the cap keeps the strongest");

        // Classify every curve as weak without changing the silence floor. Such recordings
        // still need their ordinary detections, so audible passes do not count as unheard.
        p.thr_db = f64::INFINITY;
        let (weak, weak_cut) = detect_counted(&ls, &p);
        assert_eq!(weak.len(), 1);
        assert_eq!(weak[0].score, one[0].score);
        assert!(weak[0].score >= p.thr_silence_db && weak[0].score < p.thr_db);
        assert!(weak_cut >= 1, "weak-only cap cut {weak_cut}");
    }
}
