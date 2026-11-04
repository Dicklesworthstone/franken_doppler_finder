//! Constant-velocity retarded-time fit of a traced Doppler curve, with profile-likelihood
//! ranges (F-test, 95 %), model-adequacy checks and generous search ranges.
//! Port of the reference oracle's `fit_constant_velocity` onto frankenscipy's bounded TRF
//! least squares (`fsci_opt::least_squares_bounded`, soft-L1 loss, `x_scale = jac`).

use crate::physics::{cv_curve, readouts};
use fsci_opt::{LeastSquaresMethod, LeastSquaresOptions, LossKind, least_squares_bounded};
use fsci_stats::{ContinuousDistribution, FDistribution};
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Best {
    pub f0: f64,
    pub v: f64,
    pub d: f64,
    pub t0e: f64,
    pub t0r: f64,
    pub rmse: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Ranges {
    pub v: [f64; 2],
    pub d: [f64; 2],
    pub f0: [f64; 2],
    pub t0r: [f64; 2],
}

#[derive(Clone, Debug, Serialize)]
pub struct SideFit {
    pub v: f64,
    pub d: f64,
    pub n: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct Diagnostics {
    pub n: usize,
    pub n_inliers: usize,
    pub span_s: f64,
    /// Observed temporal support in correlation-length units, never exceeding the number
    /// of distinct samples. Empty gaps and repeated samples provide no extra information.
    pub n_eff: f64,
    pub delta: f64,
    pub rms_approach: f64,
    pub rms_recede: f64,
    pub asymmetry: bool,
    pub split_rmse: Option<f64>,
    pub split_approach: Option<SideFit>,
    pub split_recede: Option<SideFit>,
    pub sides_disagree: bool,
    pub resid_autocorr: f64,
    /// Robust point-to-point scatter of the trace (Hz) and RMS / that scatter.
    pub local_sigma: f64,
    pub misfit: f64,
    pub c: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct FitResult {
    pub best: Best,
    pub ranges: Ranges,
    pub ranges_generous: Ranges,
    pub f_pre: f64,
    pub f_post: f64,
    pub max_roc: f64,
    pub diagnostics: Diagnostics,
    pub warnings: Vec<String>,
}

fn bounds(tmin: f64, tmax: f64, c: f64) -> ([f64; 4], [f64; 4]) {
    (
        [1.0, 1.0, 5.0, tmin - 900.0],
        [20_000.0, 0.95 * c, 2.0e5, tmax + 900.0],
    )
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    let pos = q / 100.0 * (n - 1) as f64;
    let i = pos.floor() as usize;
    let fr = pos - i as f64;
    if i + 1 < n {
        sorted[i] * (1.0 - fr) + sorted[i + 1] * fr
    } else {
        sorted[n - 1]
    }
}

/// numpy.gradient for non-uniform spacing (second-order interior, first-order edges).
fn gradient(y: &[f64], x: &[f64]) -> Vec<f64> {
    let n = y.len();
    let mut g = vec![0.0; n];
    if n < 2 {
        return g;
    }
    g[0] = (y[1] - y[0]) / (x[1] - x[0]);
    g[n - 1] = (y[n - 1] - y[n - 2]) / (x[n - 1] - x[n - 2]);
    for i in 1..n - 1 {
        let (h0, h1) = (x[i] - x[i - 1], x[i + 1] - x[i]);
        g[i] = (h0 * h0 * y[i + 1] - h1 * h1 * y[i - 1] + (h1 * h1 - h0 * h0) * y[i])
            / (h0 * h1 * (h0 + h1));
    }
    g
}

fn initial_guess(t: &[f64], f: &[f64], c: f64) -> [f64; 4] {
    let mut fs: Vec<f64> = f.to_vec();
    fs.sort_by(f64::total_cmp);
    let (fhi, flo) = (percentile(&fs, 97.0), percentile(&fs, 3.0));
    let f0 = 2.0 / (1.0 / fhi + 1.0 / flo);
    let v = (c * (fhi - flo) / (fhi + flo)).max(5.0);
    let mut idx: Vec<usize> = (0..t.len()).collect();
    idx.sort_by(|&a, &b| t[a].total_cmp(&t[b]));
    let ts: Vec<f64> = idx.iter().map(|&i| t[i]).collect();
    let fsr: Vec<f64> = idx.iter().map(|&i| f[i]).collect();
    let n = ts.len();
    let k = (n / 40).max(3);
    // np.convolve(fs, ones(k)/k, mode="same") (zero padded)
    let lead = (k - 1) / 2;
    let fsm: Vec<f64> = (0..n)
        .map(|i| {
            let mut s = 0.0;
            for j in 0..k {
                let src = i as i64 + lead as i64 - j as i64;
                if src >= 0 && (src as usize) < n {
                    s += fsr[src as usize];
                }
            }
            s / k as f64
        })
        .collect();
    let mut g: Vec<f64> = gradient(&fsm, &ts).into_iter().map(f64::abs).collect();
    for v in g.iter_mut().take(k.min(n)) {
        *v = 0.0;
    }
    for v in g.iter_mut().skip(n.saturating_sub(k)) {
        *v = 0.0;
    }
    let (mut i_best, mut g_best) = (n / 2, 0.0);
    for (i, &v) in g.iter().enumerate() {
        if v > g_best {
            g_best = v;
            i_best = i;
        }
    }
    let slope = g_best.max(1e-3);
    let d = (f0 * v * v / (c * slope)).clamp(20.0, 60_000.0);
    let t0r = ts[i_best.min(n - 1)];
    [f0, v, d, t0r - d / c]
}

/// One least-squares solve over the `free` parameters (others fixed at `x0`/`fixed`).
/// Returns (full params, sum of squared LINEAR residuals, residuals).
fn ls(
    t: &[f64],
    f: &[f64],
    c: f64,
    x0: [f64; 4],
    free: &[usize],
    fixed: &[(usize, f64)],
    loss: LossKind,
) -> ([f64; 4], f64, Vec<f64>) {
    let (tmin, tmax) = t.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, &v| {
        (a.0.min(v), a.1.max(v))
    });
    let (lo, hi) = bounds(tmin, tmax, c);
    let mut base = x0;
    for k in 0..4 {
        base[k] = base[k].clamp(lo[k] + 1e-9, hi[k] - 1e-9);
    }
    for &(k, v) in fixed {
        base[k] = v;
    }
    let full = |xf: &[f64]| -> [f64; 4] {
        let mut p = base;
        for (j, &k) in free.iter().enumerate() {
            p[k] = xf[j];
        }
        p
    };
    let resid = |xf: &[f64]| -> Vec<f64> {
        let p = full(xf);
        t.iter()
            .zip(f)
            .map(|(&tt, &ff)| cv_curve(tt, p[0], p[1], p[2], p[3], c) - ff)
            .collect()
    };
    let xf0: Vec<f64> = free.iter().map(|&k| base[k]).collect();
    let lof: Vec<f64> = free.iter().map(|&k| lo[k]).collect();
    let hif: Vec<f64> = free.iter().map(|&k| hi[k]).collect();
    let opts = LeastSquaresOptions {
        method: Some(LeastSquaresMethod::Trf),
        loss,
        f_scale: 0.5,
        x_scale_jac: true,
        max_nfev: Some(4000),
        ..Default::default()
    };
    let x = match least_squares_bounded(resid, &xf0, &lof, &hif, opts) {
        Ok(r) => r.x,
        Err(_) => xf0,
    };
    let p = full(&x);
    let res: Vec<f64> = t
        .iter()
        .zip(f)
        .map(|(&tt, &ff)| cv_curve(tt, p[0], p[1], p[2], p[3], c) - ff)
        .collect();
    let cost = res.iter().map(|r| r * r).sum();
    (p, cost, res)
}

fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    crate::spectro::median_f64(&mut s)
}

/// Multistart soft-L1 fit, 4*MAD outlier trim, linear refit.
/// Returns (params, inlier mask, cost, inlier residuals).
fn robust_point_fit(
    t: &[f64],
    f: &[f64],
    c: f64,
    anchors: &[[f64; 4]],
) -> ([f64; 4], Vec<bool>, f64, Vec<f64>) {
    let free = [0usize, 1, 2, 3];
    let mut inits = vec![initial_guess(t, f, c)];
    inits.extend_from_slice(anchors);
    let mut best: Option<([f64; 4], f64, Vec<f64>)> = None;
    for init in &inits {
        for dmul in [0.35, 1.0, 2.8] {
            for vmul in [0.8, 1.0, 1.25] {
                let mut x0 = *init;
                x0[2] *= dmul;
                x0[1] = (x0[1] * vmul).min(0.9 * c);
                x0[3] = (init[3] + init[2] / c) - x0[2] / c;
                let (p, cost, res) = ls(t, f, c, x0, &free, &[], LossKind::SoftL1);
                if best.as_ref().is_none_or(|b| cost < b.1) {
                    best = Some((p, cost, res));
                }
            }
        }
    }
    let (p_rob, _, res) = best.expect("at least one start");
    let med = median(&res);
    let dev: Vec<f64> = res.iter().map(|r| (r - med).abs()).collect();
    let mad = 1.4826 * median(&dev) + 1e-9;
    let cut = (4.0 * mad).max(0.3);
    let inl: Vec<bool> = res.iter().map(|r| r.abs() <= cut).collect();
    let (ti, fi): (Vec<f64>, Vec<f64>) = t
        .iter()
        .zip(f)
        .zip(&inl)
        .filter(|(_, k)| **k)
        .map(|((a, b), _)| (*a, *b))
        .unzip();
    let (p_best, cmin, res_i) = ls(&ti, &fi, c, p_rob, &free, &[], LossKind::Linear);
    (p_best, inl, cmin, res_i)
}

fn split_curve(t: f64, x: &[f64], c: f64) -> f64 {
    let (f0, t0r, v1, d1, v2, d2) = (x[0], x[1], x[2], x[3], x[4], x[5]);
    if t < t0r {
        cv_curve(t, f0, v1, d1, t0r - d1 / c, c)
    } else {
        cv_curve(t, f0, v2, d2, t0r - d2 / c, c)
    }
}

/// Approach/recede split fit: shared f0 and t0r, own (v, d) per side.
fn fit_split_sides(
    t: &[f64],
    f: &[f64],
    c: f64,
    p: [f64; 4],
) -> Option<(f64, SideFit, SideFit, f64)> {
    let (f0, v, d, t0e) = (p[0], p[1], p[2], p[3]);
    let t0r = t0e + d / c;
    let (tmin, tmax) = t.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, &v| {
        (a.0.min(v), a.1.max(v))
    });
    let lo = [1.0, tmin - 60.0, 1.0, 5.0, 1.0, 5.0];
    let hi = [20_000.0, tmax + 60.0, 0.95 * c, 2e5, 0.95 * c, 2e5];
    let resid = |x: &[f64]| -> Vec<f64> {
        t.iter()
            .zip(f)
            .map(|(&tt, &ff)| split_curve(tt, x, c) - ff)
            .collect()
    };
    let mut best: Option<(Vec<f64>, f64)> = None;
    for (m1, m2) in [(1.0, 1.0), (0.7, 1.4), (1.4, 0.7), (0.5, 2.0), (2.0, 0.5)] {
        for (vm1, vm2) in [(1.0, 1.0), (0.8, 1.2), (1.2, 0.8)] {
            let mut x0 = vec![f0, t0r, v * vm1, d * m1, v * vm2, d * m2];
            for k in 0..6 {
                x0[k] = x0[k].clamp(lo[k] + 1e-9, hi[k] - 1e-9);
            }
            let opts = LeastSquaresOptions {
                method: Some(LeastSquaresMethod::Trf),
                loss: LossKind::SoftL1,
                f_scale: 0.5,
                x_scale_jac: true,
                max_nfev: Some(3000),
                ..Default::default()
            };
            if let Ok(r) = least_squares_bounded(resid, &x0, &lo, &hi, opts)
                && best.as_ref().is_none_or(|b| r.cost < b.1)
            {
                best = Some((r.x, r.cost));
            }
        }
    }
    let (x0, _) = best?;
    let opts = LeastSquaresOptions {
        method: Some(LeastSquaresMethod::Trf),
        x_scale_jac: true,
        max_nfev: Some(3000),
        ..Default::default()
    };
    let x = least_squares_bounded(resid, &x0, &lo, &hi, opts)
        .map(|r| r.x)
        .unwrap_or(x0);
    let res = resid(&x);
    let rmse = (res.iter().map(|r| r * r).sum::<f64>() / res.len() as f64).sqrt();
    let n_app = t.iter().filter(|&&tt| tt < x[1]).count();
    Some((
        rmse,
        SideFit {
            v: x[2],
            d: x[3],
            n: n_app,
        },
        SideFit {
            v: x[4],
            d: x[5],
            n: t.len() - n_app,
        },
        x[1],
    ))
}

/// Which part of a trace to fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Both,
    Approach,
    Recede,
}

/// A point fit and the trace it was fitted to. Iterative ridge refinement only needs `best`;
/// the expensive profiles and side fits are deferred until the last accepted trace is known.
pub(crate) struct PreparedFit {
    t: Vec<f64>,
    f: Vec<f64>,
    c: f64,
    p_best: [f64; 4],
    inl: Vec<bool>,
    cmin: f64,
    res_i: Vec<f64>,
    t_i: Vec<f64>,
    f_i: Vec<f64>,
    side_check: bool,
}

impl PreparedFit {
    pub(crate) fn best(&self) -> Best {
        Best {
            f0: self.p_best[0],
            v: self.p_best[1],
            d: self.p_best[2],
            t0e: self.p_best[3],
            t0r: self.p_best[3] + self.p_best[2] / self.c,
            rmse: (self.cmin / self.t_i.len() as f64).sqrt(),
        }
    }

    pub(crate) fn complete(self) -> FitResult {
        finish_fit(self)
    }
}

/// Fit a constant-velocity pass to trace points (t in recording seconds, f in Hz).
pub fn fit_constant_velocity(
    t: &[f64],
    f: &[f64],
    c: f64,
    side: Side,
) -> Result<FitResult, String> {
    prepare_constant_velocity(t, f, c, side).map(PreparedFit::complete)
}

pub(crate) fn prepare_constant_velocity(
    t: &[f64],
    f: &[f64],
    c: f64,
    side: Side,
) -> Result<PreparedFit, String> {
    if t.len() < 12 || t.len() != f.len() {
        return Err(format!("need >= 12 trace points to fit (got {})", t.len()));
    }
    let (tt, ff, anchors): (Vec<f64>, Vec<f64>, Vec<[f64; 4]>) = match side {
        Side::Both => (t.to_vec(), f.to_vec(), vec![]),
        _ => {
            let (pj, _, _, _) = robust_point_fit(t, f, c, &[]);
            let t0r = pj[3] + pj[2] / c;
            let keep: Vec<bool> = t
                .iter()
                .map(|&x| {
                    if side == Side::Approach {
                        x < t0r
                    } else {
                        x >= t0r
                    }
                })
                .collect();
            let (a, b): (Vec<f64>, Vec<f64>) = t
                .iter()
                .zip(f)
                .zip(&keep)
                .filter(|(_, k)| **k)
                .map(|((a, b), _)| (*a, *b))
                .unzip();
            if a.len() < 12 {
                return Err(format!("{side:?} side has only {} points", a.len()));
            }
            (a, b, vec![pj])
        }
    };
    prepare_fit(tt, ff, c, &anchors, side == Side::Both)
}

fn prepare_fit(
    t: Vec<f64>,
    f: Vec<f64>,
    c: f64,
    anchors: &[[f64; 4]],
    side_check: bool,
) -> Result<PreparedFit, String> {
    let (p_best, inl, cmin, res_i) = robust_point_fit(&t, &f, c, anchors);
    let (t_i, f_i): (Vec<f64>, Vec<f64>) = t
        .iter()
        .zip(&f)
        .zip(&inl)
        .filter(|(_, k)| **k)
        .map(|((a, b), _)| (*a, *b))
        .unzip();
    let n = t_i.len();
    if n < 8 {
        return Err("too few inliers after outlier trim".into());
    }
    Ok(PreparedFit {
        t,
        f,
        c,
        p_best,
        inl,
        cmin,
        res_i,
        t_i,
        f_i,
        side_check,
    })
}

/// Union length of correlation-width intervals centered on the sorted observation times,
/// divided by that width. Overlapping samples share information; an isolated sample can
/// contribute at most one. Unlike elapsed span, this cannot count silence as observations.
fn effective_trace_points(sorted_t: &[f64], correlation_s: f64) -> f64 {
    if sorted_t.is_empty() {
        return 0.0;
    }
    1.0 + sorted_t
        .windows(2)
        .map(|w| ((w[1] - w[0]) / correlation_s).clamp(0.0, 1.0))
        .sum::<f64>()
}

fn finish_fit(prepared: PreparedFit) -> FitResult {
    const CORR_S: f64 = 6.0;
    const ASYM_FLOOR_HZ: f64 = 0.25;
    let PreparedFit {
        t,
        f,
        c,
        p_best,
        inl,
        cmin,
        res_i,
        t_i,
        f_i,
        side_check,
    } = prepared;
    let n = t_i.len();
    let (tmin, tmax) = t_i
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |a, &v| {
            (a.0.min(v), a.1.max(v))
        });
    let span = tmax - tmin;
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| t_i[a].total_cmp(&t_i[b]));
    let ts_sorted: Vec<f64> = order.iter().map(|&i| t_i[i]).collect();
    let n_eff = effective_trace_points(&ts_sorted, CORR_S);
    // The distribution needs positive degrees of freedom. Keep that numerical guard
    // separate from the observed effective count, which also weights spatial refinement.
    let dof = (n_eff - 4.0).max(2.0);
    let delta = FDistribution::new(1.0, dof).ppf(0.95) / dof;
    let thr = cmin * (1.0 + delta);
    let (lo_b, hi_b) = bounds(tmin, tmax, c);
    let mut accepted: Vec<[f64; 4]> = vec![p_best];
    let mut warnings = Vec::new();

    let prof_at = |k: usize, g: f64, warm: [f64; 4]| -> (f64, [f64; 4]) {
        let free: Vec<usize> = (0..4).filter(|&i| i != k).collect();
        let mut x0 = warm;
        x0[k] = g;
        if k == 2 {
            // keep the received CPA time while d moves
            x0[3] = (p_best[3] + p_best[2] / c) - g / c;
        }
        let (p, cost, _) = ls(&t_i, &f_i, c, x0, &free, &[(k, g)], LossKind::Linear);
        (cost, p)
    };
    let bound = |k: usize, sign: f64, max_rel: f64, acc: &mut Vec<[f64; 4]>| -> (f64, bool) {
        let base = p_best[k];
        let mut warm = p_best;
        let (mut ok_rel, mut rel) = (0.0f64, 0.002f64);
        let fail_rel;
        loop {
            let g = base * (1.0 + sign * rel);
            if g <= lo_b[k] || g >= hi_b[k] || rel > max_rel {
                return (base * (1.0 + sign * ok_rel), true);
            }
            let (cost, p) = prof_at(k, g, warm);
            if cost > thr {
                fail_rel = rel;
                break;
            }
            acc.push(p);
            ok_rel = rel;
            warm = p;
            rel *= 1.6;
        }
        let mut fail_rel = fail_rel;
        for _ in 0..8 {
            let mid = 0.5 * (ok_rel + fail_rel);
            let (cost, p) = prof_at(k, base * (1.0 + sign * mid), warm);
            if cost <= thr {
                ok_rel = mid;
                warm = p;
                acc.push(p);
            } else {
                fail_rel = mid;
            }
        }
        (base * (1.0 + sign * ok_rel), false)
    };
    let mut edge_notes = Vec::new();
    let mut rng = [[0.0; 2]; 2];
    for (j, (name, k)) in [("v", 1usize), ("d", 2usize)].into_iter().enumerate() {
        let (lo_v, lo_open) = bound(k, -1.0, 0.9, &mut accepted);
        let (hi_v, hi_open) = bound(k, 1.0, 4.0, &mut accepted);
        if lo_open {
            edge_notes.push(format!(
                "{name}_lo unbounded-within-search: trace does not constrain this side"
            ));
        }
        if hi_open {
            edge_notes.push(format!(
                "{name}_hi unbounded-within-search: trace does not constrain this side"
            ));
        }
        rng[j] = [lo_v, hi_v];
    }
    let f0r = accepted
        .iter()
        .fold([f64::INFINITY, f64::NEG_INFINITY], |a, p| {
            [a[0].min(p[0]), a[1].max(p[0])]
        });
    let t0rr = accepted
        .iter()
        .fold([f64::INFINITY, f64::NEG_INFINITY], |a, p| {
            let x = p[3] + p[2] / c;
            [a[0].min(x), a[1].max(x)]
        });
    let ranges = Ranges {
        v: rng[0],
        d: rng[1],
        f0: f0r,
        t0r: t0rr,
    };

    let rmse = (cmin / n as f64).sqrt();
    let t0r = p_best[3] + p_best[2] / c;
    let res_all: Vec<f64> = t
        .iter()
        .zip(&f)
        .map(|(&tt, &ff)| cv_curve(tt, p_best[0], p_best[1], p_best[2], p_best[3], c) - ff)
        .collect();
    let rms = |sel: &dyn Fn(f64) -> bool| -> f64 {
        let v: Vec<f64> = t
            .iter()
            .zip(&res_all)
            .zip(&inl)
            .filter(|((tt, _), k)| **k && sel(**tt))
            .map(|((_, r), _)| *r)
            .collect();
        if v.is_empty() {
            f64::NAN
        } else {
            (v.iter().map(|r| r * r).sum::<f64>() / v.len() as f64).sqrt()
        }
    };
    let rms_pre = rms(&|x| x < t0r);
    let rms_post = rms(&|x| x >= t0r);
    // residual autocorrelation at ~2 correlation lengths
    let mut dts: Vec<f64> = ts_sorted.windows(2).map(|w| w[1] - w[0]).collect();
    let hop = if dts.is_empty() {
        1.0
    } else {
        crate::spectro::median_f64(&mut dts)
    };
    let lag = ((2.0 * CORR_S / hop.max(1e-3)).round() as usize).max(1);
    let r_s: Vec<f64> = order.iter().map(|&i| res_i[i]).collect();
    let ac = if r_s.len() > lag + 5 {
        corrcoef(&r_s[..r_s.len() - lag], &r_s[lag..])
    } else {
        f64::NAN
    };
    let ratio = if rms_post > 0.0 {
        rms_pre / rms_post
    } else {
        f64::INFINITY
    };
    let asym =
        ratio.is_finite() && (ratio > 2.0 || ratio < 0.5) && rms_pre.max(rms_post) > ASYM_FLOOR_HZ;
    if asym {
        warnings.push(format!(
            "asymmetric residual (approach/recede RMS = {rms_pre:.2}/{rms_post:.2} Hz): manoeuvre likely; ranges widened"
        ));
    }
    let n_pre = t_i.iter().filter(|&&x| x < t0r).count();
    let mut sides_disagree = false;
    let (mut split_rmse, mut sa, mut sr) = (None, None, None);
    if side_check
        && n_pre >= 30
        && n - n_pre >= 30
        && let Some((srm, a, r, _)) = fit_split_sides(&t_i, &f_i, c, p_best)
    {
        if rmse > 2.0 * srm && rmse > ASYM_FLOOR_HZ {
            sides_disagree = true;
            warnings.push(format!(
                    "approach/recede sides disagree (joint RMS {rmse:.2} Hz vs split-sides {srm:.2}; side v={:.0}/{:.0} m/s d={:.0}/{:.0} m): one constant-velocity pass does not explain this curve; generous floors x3",
                    a.v, r.v, a.d, r.d
                ));
        }
        split_rmse = Some(srm);
        sa = Some(a);
        sr = Some(r);
    }
    if ac.is_finite() && ac > 0.5 {
        warnings.push(format!("structured residual (lag-{lag} autocorr {ac:.2}): straight-line model incomplete; ranges optimistic"));
    }
    // Misfit relative to the trace's own local scatter: a curve the straight-line model cannot
    // follow (power/RPM changes, turns, climbs) leaves an RMS far above the point-to-point noise
    // even when neither side test nor the autocorrelation test fires.
    let local_sigma = {
        let (mut res_loc, mut j0) = (Vec::with_capacity(n), 0usize);
        let mut win = Vec::new();
        for i in 0..ts_sorted.len() {
            while ts_sorted[j0] < ts_sorted[i] - 1.0 {
                j0 += 1;
            }
            win.clear();
            let mut j = j0;
            while j < ts_sorted.len() && ts_sorted[j] <= ts_sorted[i] + 1.0 {
                win.push(f_i[order[j]]);
                j += 1;
            }
            if win.len() >= 5 {
                let m = crate::spectro::median_f64(&mut win);
                res_loc.push((f_i[order[i]] - m).abs());
            }
        }
        if res_loc.len() >= 10 {
            1.4826 * crate::spectro::median_f64(&mut res_loc)
        } else {
            f64::NAN
        }
    };
    let misfit = rmse / local_sigma.max(0.03);
    if local_sigma.is_finite() && misfit > 10.0 && rmse > 0.3 {
        warnings.push(format!(
            "model misfit: RMS {rmse:.2} Hz is {misfit:.1}x the trace's local scatter ({local_sigma:.2} Hz); the source frequency or the flight path changed during the pass; generous floors x3"
        ));
    }
    warnings.extend(edge_notes);
    let severe = sides_disagree || (local_sigma.is_finite() && misfit > 10.0 && rmse > 0.3);
    let kf = if severe {
        3.0
    } else if warnings.is_empty() {
        1.0
    } else {
        2.0
    };
    let pad = |r: [f64; 2], b: f64, rel: Option<f64>, abs: f64| -> [f64; 2] {
        let p = match rel {
            Some(x) => b * x * kf,
            None => abs * kf,
        };
        [r[0].min(b - p), r[1].max(b + p)]
    };
    // d is a scale parameter: pad it multiplicatively so wide floors never cross zero
    let dk = 1.0 + 0.25 * kf;
    let ranges_generous = Ranges {
        v: pad(ranges.v, p_best[1], Some(0.05), 0.0),
        d: [
            ranges.d[0].min(p_best[2] / dk),
            ranges.d[1].max(p_best[2] * dk),
        ],
        f0: pad(ranges.f0, p_best[0], Some(0.01), 0.0),
        t0r: pad(ranges.t0r, t0r, None, 3.0),
    };
    let ro = readouts(p_best[0], p_best[1], p_best[2], c);
    FitResult {
        best: Best {
            f0: p_best[0],
            v: p_best[1],
            d: p_best[2],
            t0e: p_best[3],
            t0r,
            rmse,
        },
        ranges,
        ranges_generous,
        f_pre: ro.f_pre,
        f_post: ro.f_post,
        max_roc: ro.max_roc,
        diagnostics: Diagnostics {
            n: t.len(),
            n_inliers: n,
            span_s: span,
            n_eff,
            delta,
            rms_approach: rms_pre,
            rms_recede: rms_post,
            asymmetry: asym,
            split_rmse,
            split_approach: sa,
            split_recede: sr,
            sides_disagree,
            resid_autocorr: ac,
            local_sigma,
            misfit,
            c,
        },
        warnings,
    }
}

fn corrcoef(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma) * (x - ma);
        sbb += (y - mb) * (y - mb);
    }
    sab / (saa * sbb).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_information_depends_on_observed_support_not_gaps_or_density() {
        let dense: Vec<f64> = (0..=600).map(|k| k as f64 * 0.1).collect();
        let oversampled: Vec<f64> = (0..=3000).map(|k| k as f64 * 0.02).collect();
        let duplicated: Vec<f64> = dense.iter().flat_map(|&t| [t, t, t]).collect();
        for times in [&dense, &oversampled, &duplicated] {
            assert!((effective_trace_points(times, 6.0) - 11.0).abs() < 1e-10);
        }
        for gap in [60.0, 600.0, 60_000.0] {
            let times: Vec<f64> = (0..=96)
                .map(|k| k as f64 * 0.25)
                .chain((0..=96).map(|k| gap + k as f64 * 0.25))
                .collect();
            // Two 24 s observed stretches: each covers five independent supports.
            assert!((effective_trace_points(&times, 6.0) - 10.0).abs() < 1e-10);
        }
        let sparse: Vec<f64> = (0..12).map(|k| k as f64 * 60.0).collect();
        assert_eq!(effective_trace_points(&sparse, 6.0), 12.0);
        assert_eq!(effective_trace_points(&[10.0; 100], 6.0), 1.0);
        assert_eq!(effective_trace_points(&[], 6.0), 0.0);
    }

    #[test]
    fn gapped_fit_reports_only_observed_independent_information() {
        let c = 343.0;
        let t: Vec<f64> = (0..=96)
            .map(|k| 900.0 + k as f64 * 0.25)
            .chain((0..=96).map(|k| 1080.0 + k as f64 * 0.25))
            .collect();
        let f: Vec<f64> = t
            .iter()
            .enumerate()
            .map(|(k, &tt)| {
                cv_curve(tt, 112.7, 140.0, 6100.0, 1000.0, c) + 0.03 * (k as f64 * 1.731).sin()
            })
            .collect();
        let fit = fit_constant_velocity(&t, &f, c, Side::Both).unwrap();
        let n_eff = fit.diagnostics.n_eff;
        assert!(
            n_eff <= 10.0 + 1e-10,
            "unobserved gap earned information: {n_eff}"
        );
        assert!(n_eff > 8.0, "fixture must retain both observed stretches");
        assert!(fit.diagnostics.span_s / 6.0 > 3.0 * n_eff);
        assert!(fit.ranges_generous.v[0] <= 140.0 && fit.ranges_generous.v[1] >= 140.0);
        assert!(fit.ranges_generous.d[0] <= 6100.0 && fit.ranges_generous.d[1] >= 6100.0);
        let expected_delta = FDistribution::new(1.0, n_eff - 4.0).ppf(0.95) / (n_eff - 4.0);
        assert_eq!(fit.diagnostics.delta, expected_delta);
    }

    #[test]
    fn prepared_fit_keeps_its_trace_and_matches_eager_diagnostics() {
        let c = 343.0;
        let mut t: Vec<f64> = (0..240).map(|i| 900.0 + i as f64).collect();
        let mut f: Vec<f64> = t
            .iter()
            .enumerate()
            .map(|(i, &tt)| {
                cv_curve(tt, 112.7, 140.0, 6100.0, 1000.0, c)
                    + 0.12 * (i as f64 * 1.731).sin()
                    + if i % 37 == 0 { 8.0 } else { 0.0 }
            })
            .collect();
        let eager = fit_constant_velocity(&t, &f, c, Side::Both).unwrap();
        let prepared = prepare_constant_velocity(&t, &f, c, Side::Both).unwrap();
        assert_eq!(
            serde_json::to_value(prepared.best()).unwrap(),
            serde_json::to_value(eager.best).unwrap()
        );
        // A later refinement owns a different trace. Deferred diagnostics must use the
        // retained point fit's original samples, including its original outlier mask.
        t.clear();
        f.clear();
        let deferred = prepared.complete();
        assert!(deferred.diagnostics.n_inliers < deferred.diagnostics.n);
        assert_eq!(
            serde_json::to_value(deferred).unwrap(),
            serde_json::to_value(eager).unwrap()
        );
    }

    #[test]
    fn recovers_synthetic_pass() {
        let c = 343.0;
        let (f0, v, d, t0e) = (112.7, 140.0, 6100.0, 1000.0);
        let t: Vec<f64> = (0..1200).map(|i| 860.0 + i as f64 * 0.25).collect();
        // deterministic pseudo-noise, 0.15 Hz rms
        let f: Vec<f64> = t
            .iter()
            .enumerate()
            .map(|(i, &tt)| {
                cv_curve(tt, f0, v, d, t0e, c)
                    + 0.15
                        * ((i as f64 * 12.9898).sin() * 43_758.545)
                            .fract()
                            .mul_add(2.0, -1.0)
            })
            .collect();
        let r = fit_constant_velocity(&t, &f, c, Side::Both).unwrap();
        assert!((r.best.f0 - f0).abs() < 0.2, "f0 {}", r.best.f0);
        assert!((r.best.v - v).abs() / v < 0.03, "v {}", r.best.v);
        assert!((r.best.d - d).abs() / d < 0.08, "d {}", r.best.d);
        assert!(
            (r.best.t0r - (t0e + d / c)).abs() < 1.0,
            "t0r {}",
            r.best.t0r
        );
        assert!(r.ranges_generous.v[0] <= v && r.ranges_generous.v[1] >= v);
        assert!(r.ranges_generous.d[0] <= d && r.ranges_generous.d[1] >= d);
    }

    /// A pass heard 110 s before its CPA but only 23 s after (the recording or the detector cut
    /// it), with a slow power drift: v, d and f0 trade off along a ridge, and the generous
    /// ranges must still hold the truth (C172-like: 64 m/s at 3.7 km).
    #[test]
    fn short_side_generous_ranges_hold_truth() {
        let c = 343.0;
        let (f0, v, d) = (78.5, 64.0, 3734.0);
        let t0r = 1000.0;
        let t0e = t0r - d / c;
        let t: Vec<f64> = (0..535).map(|i| t0r - 110.0 + i as f64 * 0.25).collect();
        let f: Vec<f64> = t
            .iter()
            .enumerate()
            .map(|(i, &tt)| {
                // slow power drift (0.4 % of f0 over the pass) plus 0.2 Hz pseudo-noise
                cv_curve(tt, f0, v, d, t0e, c) * (1.0 + 0.004 * (tt - t0r) / 133.0)
                    + 0.2
                        * ((i as f64 * 12.9898).sin() * 43_758.545)
                            .fract()
                            .mul_add(2.0, -1.0)
            })
            .collect();
        let r = fit_constant_velocity(&t, &f, c, Side::Both).unwrap();
        let g = &r.ranges_generous;
        assert!(
            g.v[0] <= v && g.v[1] >= v && g.d[0] <= d && g.d[1] >= d,
            "best v {:.1} d {:.0}; generous v {:.1?} d {:.0?}; warnings {:?}",
            r.best.v,
            r.best.d,
            g.v,
            g.d,
            r.warnings
        );
    }
}
