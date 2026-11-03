//! Automatic flyover detection: a matched-filter bank of retarded-model Doppler curves.
//!
//! A pass's frequency trajectory is f(t) = f0 * r_{v,d}(t - t0r). On a log-frequency axis the
//! shape r is a fixed path for each (v, d) and f0 is a pure row shift, so one template per
//! (v, d) scores every (t0r, f0) by summing the row-normalised spectrogram along the shifted
//! path. The statistic is min(mean over the approach half, mean over the recede half): a real
//! flyover lights up both halves, while a flat engine tone, a stepped tone or a click column can
//! only light one. Detections are then refined on the linear spectrogram (continuous f0, t0r,
//! v, d) so that ridge-trace seeds land on the ridge, and grouped into harmonic families.

use crate::physics::cv_ratio_rel_t0r;
use crate::spectro::{Spec, stft_band_db};
use rayon::prelude::*;
use serde::Serialize;

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
    /// Minimum min-halves matched-filter score (robust-z sum / sqrt(n)) for an event
    /// candidate. Pure noise peaks near 3.
    pub thr_db: f64,
    /// Lower threshold used only to decide whether a gap between two events is silent.
    pub thr_silence_db: f64,
    pub max_detections: usize,
    pub c: f64,
}

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
    Ok((
        LogSpec {
            t: lin.t.clone(),
            freqs,
            data,
            bins_per_octave: p.bins_per_octave,
            hop_s: lin.dt(),
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
/// those with score >= `thr_db` are the event candidates.
pub fn detect(ls: &LogSpec, p: &DetectParams) -> Vec<Detection> {
    let (nt, nr) = (ls.nt(), ls.nr());
    if nt < 8 || nr < 8 {
        return Vec::new();
    }
    let tmpls = build_templates(p, ls.hop_s);
    // Padded copies: margin rows/frames so every shifted read stays in bounds; a parallel mask
    // counts how many path samples actually fell on data.
    let (mr_up, mr_dn) = tmpls
        .iter()
        .flat_map(|t| t.path.iter())
        .fold((0i32, 0i32), |a, x| (a.0.max(x.1), a.1.min(x.1)));
    let mt = tmpls
        .iter()
        .flat_map(|t| t.path.iter())
        .map(|x| x.0.abs())
        .max()
        .unwrap_or(0);
    let (pad_r0, pad_t) = ((-mr_dn) as usize, mt as usize);
    let pr = nr + pad_r0 + mr_up as usize;
    let pt = nt + 2 * pad_t;
    let mut lp = vec![0f32; pr * pt];
    let mut mp = vec![0f32; pr * pt];
    for r in 0..nr {
        let dst = (r + pad_r0) * pt + pad_t;
        lp[dst..dst + nt].copy_from_slice(&ls.data[r * nt..(r + 1) * nt]);
        for v in &mut mp[dst..dst + nt] {
            *v = 1.0;
        }
    }
    // Per template: min-halves score map; keep the best template per (row, frame).
    let best = tmpls
        .par_iter()
        .enumerate()
        .fold(
            || {
                (
                    vec![f32::NEG_INFINITY; nr * nt],
                    vec![0u16; nr * nt],
                    vec![0f32; nr * nt],
                    vec![0f32; nr * nt],
                )
            },
            |(mut bs, mut bi, mut ba, mut bb), (ti, tm)| {
                let mut sa = vec![0f32; nr * nt];
                let mut sr = vec![0f32; nr * nt];
                let mut ca = vec![0f32; nr * nt];
                let mut cr = vec![0f32; nr * nt];
                for &(dt, dr, app) in &tm.path {
                    let (s, c) = if app {
                        (&mut sa, &mut ca)
                    } else {
                        (&mut sr, &mut cr)
                    };
                    for r in 0..nr {
                        let src = (r as i64 + pad_r0 as i64 + dr as i64) as usize * pt
                            + (pad_t as i64 + dt as i64) as usize;
                        let srow = &lp[src..src + nt];
                        let mrow = &mp[src..src + nt];
                        let drow = &mut s[r * nt..(r + 1) * nt];
                        let crow = &mut c[r * nt..(r + 1) * nt];
                        for i in 0..nt {
                            drow[i] += srow[i];
                            crow[i] += mrow[i];
                        }
                    }
                }
                let (na, nrr) = (tm.n_a as f32, tm.n_r as f32);
                for k in 0..nr * nt {
                    if ca[k] < 0.6 * na || cr[k] < 0.6 * nrr {
                        continue;
                    }
                    // matched-filter statistic: sum / sqrt(n) is ~N(0,1) per half under noise
                    // and grows as mean_z * sqrt(n) along a real ridge
                    let a = sa[k] / ca[k].sqrt();
                    let b = sr[k] / cr[k].sqrt();
                    let s = a.min(b);
                    if s > bs[k] {
                        bs[k] = s;
                        bi[k] = ti as u16;
                        ba[k] = a;
                        bb[k] = b;
                    }
                }
                (bs, bi, ba, bb)
            },
        )
        .reduce(
            || {
                (
                    vec![f32::NEG_INFINITY; nr * nt],
                    vec![0u16; nr * nt],
                    vec![0f32; nr * nt],
                    vec![0f32; nr * nt],
                )
            },
            |mut x, y| {
                for k in 0..nr * nt {
                    if y.0[k] > x.0[k] {
                        x.0[k] = y.0[k];
                        x.1[k] = y.1[k];
                        x.2[k] = y.2[k];
                        x.3[k] = y.3[k];
                    }
                }
                x
            },
        );
    let (bs, bi, ba, bb) = best;
    // Greedy non-maximum suppression in (t0r, log f0) plus residual evidence: `known` marks
    // the cells (+-3 log-rows) along every accepted curve; a later candidate is re-scored with
    // those cells counted as zero, so a template that merely rides an already detected ridge
    // with a different (v, d) cannot become a second event.
    let mut cand: Vec<usize> = (0..nr * nt)
        .filter(|&k| bs[k] as f64 >= p.thr_silence_db)
        .collect();
    cand.sort_by(|&a, &b| bs[b].total_cmp(&bs[a]));
    cand.truncate(400_000);
    let mut known = vec![false; nr * nt];
    let bpo = p.bins_per_octave as f64;
    let mut out: Vec<Detection> = Vec::new();
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
        let suppressed = out.iter().any(|o| {
            let dt_lim = (0.5 * tau.min(o.d / o.v)).max(10.0);
            let same_line = ((f0 / o.f0).log2() * bpo).abs() < 4.0;
            (t0r - o.t0r).abs() < dt_lim && same_line
        });
        if suppressed {
            continue;
        }
        let (ra, rr) = if out.is_empty() {
            (ba[k], bb[k])
        } else {
            residual_halves(ls, r, i, tm, &known)
        };
        if (ra.min(rr) as f64) < p.thr_silence_db {
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
            for rr2 in (row - 3).max(0)..=(row + 3).min(nr as i64 - 1) {
                known[rr2 as usize * nt + j] = true;
            }
        }
        out.push(Detection {
            id: out.len(),
            t0r,
            f0,
            v: tm.v,
            d: tm.d,
            score: ra.min(rr) as f64,
            score_approach: ra as f64,
            score_recede: rr as f64,
            half_s: half,
            harmonic_of: None,
            harmonics: Vec::new(),
        });
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out.truncate(p.max_detections);
    for (i, d) in out.iter_mut().enumerate() {
        d.id = i;
    }
    group_harmonics(&mut out, p.bins_per_octave);
    out
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
fn group_harmonics(dets: &mut [Detection], _bpo: usize) {
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
            let ratio = b.f0 / a.f0;
            let mut hit = false;
            'outer: for m in 1..=8u32 {
                for q in 1..=4u32 {
                    if m == q {
                        continue;
                    }
                    let r = m as f64 / q as f64;
                    if (ratio / r - 1.0).abs() < 0.015 {
                        hit = true;
                        break 'outer;
                    }
                }
            }
            if hit {
                dets[j].harmonic_of = Some(dets[i].id);
                dets[i].harmonics.push(ratio);
                break;
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::cv_curve;

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
}
