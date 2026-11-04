//! Ridge tracing of a Doppler curve on a row-normalised spectrogram (port of the oracle's
//! `_ridge_peak` / `ridge_trace`): integrate dB along a short line of the current slope, then
//! peak-pick with a robust z-score gate; follow the ridge both ways from each seed.

use crate::spectro::{Spec, median_f64};
use serde::Serialize;

#[derive(Clone, Copy, Debug)]
pub struct TraceParams {
    /// Half-width (Hz) of the search band around the predicted frequency.
    pub band: f64,
    pub min_snr: f64,
    pub min_z: f64,
    /// Stop after this many seconds without an accepted point.
    pub stop_s: f64,
    /// Noise reference half-width (Hz); default 4 * band.
    pub noise_band: Option<f64>,
    pub slope_pts: usize,
    /// Integration half-length along the ridge (s).
    pub integ_s: f64,
}

impl Default for TraceParams {
    fn default() -> Self {
        Self {
            band: 6.0,
            min_snr: 3.0,
            min_z: 4.0,
            stop_s: 1.5,
            noise_band: None,
            slope_pts: 30,
            integ_s: 3.0,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Trace {
    pub t: Vec<f64>,
    pub f: Vec<f64>,
    pub snr: Vec<f64>,
}

impl Trace {
    pub fn len(&self) -> usize {
        self.t.len()
    }
    pub fn is_empty(&self) -> bool {
        self.t.is_empty()
    }
    pub fn span(&self) -> f64 {
        if self.t.len() < 2 {
            0.0
        } else {
            self.t[self.t.len() - 1] - self.t[0]
        }
    }
    pub fn write_csv(&self, path: &std::path::Path) -> std::io::Result<()> {
        let mut s = String::from("t_rec,f_hz,snr_db\n");
        for i in 0..self.t.len() {
            s.push_str(&format!(
                "{:.3},{:.4},{:.2}\n",
                self.t[i], self.f[i], self.snr[i]
            ));
        }
        std::fs::write(path, s)
    }
}

/// (fhat, snr_db, robust z) of the ridge near `f_pred` at frame `j`, or None.
pub(crate) fn ridge_peak(
    spec: &Spec,
    j: usize,
    f_pred: f64,
    slope: f64,
    band: f64,
    noise_band: f64,
    half: usize,
) -> Option<(f64, f64, f64)> {
    profile_peak(
        spec,
        j,
        f_pred,
        slope,
        band,
        noise_band,
        half,
        PeakChoice::Strongest,
    )
}

/// A model-guided trace follows the nearest qualifying local peak, so a louder nearby
/// aircraft does not take over the trace merely by entering the same search band.
#[allow(clippy::too_many_arguments)]
pub(crate) fn guided_ridge_peak(
    spec: &Spec,
    j: usize,
    f_pred: f64,
    slope: f64,
    band: f64,
    noise_band: f64,
    half: usize,
    min_snr: f64,
    min_z: f64,
) -> Option<(f64, f64, f64)> {
    profile_peak(
        spec,
        j,
        f_pred,
        slope,
        band,
        noise_band,
        half,
        PeakChoice::NearestQualified { min_snr, min_z },
    )
}

enum PeakChoice {
    Strongest,
    NearestQualified { min_snr: f64, min_z: f64 },
}

#[allow(clippy::too_many_arguments)]
fn profile_peak(
    spec: &Spec,
    j: usize,
    f_pred: f64,
    slope: f64,
    band: f64,
    noise_band: f64,
    half: usize,
    choice: PeakChoice,
) -> Option<(f64, f64, f64)> {
    let df = spec.df();
    let n_off = ((2.0 * noise_band + 0.5 * df) / df).floor() as usize + 1;
    let nt = spec.nt();
    let jj: Vec<usize> = (0..=2 * half)
        .map(|k| (j as i64 + k as i64 - half as i64).clamp(0, nt as i64 - 1) as usize)
        .collect();
    let need = 3usize.max(jj.len() / 3);
    let mut prof = vec![f64::NAN; n_off];
    let mut offs = vec![0.0; n_off];
    for (o, (pv, ov)) in prof.iter_mut().zip(offs.iter_mut()).enumerate() {
        let off = -noise_band + o as f64 * df;
        *ov = off;
        let (mut sum, mut cnt) = (0.0f64, 0usize);
        for &k in &jj {
            let tk = spec.t[k];
            if let Some(v) = spec.bilinear(tk, f_pred + off + slope * (tk - spec.t[j])) {
                sum += v as f64;
                cnt += 1;
            }
        }
        if cnt >= need {
            *pv = sum / cnt as f64;
        }
    }
    let good = prof.iter().filter(|v| v.is_finite()).count();
    if good < 10 {
        return None;
    }
    let mut kin: Option<usize> = None;
    let (mut n_in, mut noise) = (0usize, Vec::new());
    for o in 0..n_off {
        if !prof[o].is_finite() {
            continue;
        }
        if offs[o].abs() <= band {
            n_in += 1;
            if kin.is_none_or(|k| prof[o] > prof[k]) {
                kin = Some(o);
            }
        } else {
            noise.push(prof[o]);
        }
    }
    if n_in < 3 || noise.len() < 6 {
        return None;
    }
    let med = median_f64(&mut noise.clone());
    let mut dev: Vec<f64> = noise.iter().map(|x| (x - med).abs()).collect();
    let mad = 1.4826 * median_f64(&mut dev) + 1e-6;
    let peak_frequency = |k: usize| {
        let mut fhat = f_pred + offs[k];
        if k > 0 && k + 1 < n_off && prof[k - 1].is_finite() && prof[k + 1].is_finite() {
            let (a, b, cc) = (prof[k - 1], prof[k], prof[k + 1]);
            let den = a - 2.0 * b + cc;
            if den < 0.0 {
                fhat += 0.5 * (a - cc) / den * df;
            }
        }
        fhat
    };
    let k = match choice {
        PeakChoice::Strongest => kin?,
        PeakChoice::NearestQualified { min_snr, min_z } => (1..n_off - 1)
            .filter(|&k| {
                // Check the full profile, including the bins just outside the search band:
                // an increasing shoulder at the band edge is not an observed ridge peak.
                prof[k].is_finite()
                    && prof[k - 1].is_finite()
                    && prof[k + 1].is_finite()
                    && prof[k] >= prof[k - 1]
                    && prof[k] >= prof[k + 1]
                    && (prof[k] > prof[k - 1] || prof[k] > prof[k + 1])
                    && offs[k].abs() <= band
                    && prof[k] - med >= min_snr
                    && (prof[k] - med) / mad >= min_z
                    && (peak_frequency(k) - f_pred).abs() <= band
            })
            .min_by(|&a, &b| {
                (peak_frequency(a) - f_pred)
                    .abs()
                    .total_cmp(&(peak_frequency(b) - f_pred).abs())
                    .then_with(|| prof[b].total_cmp(&prof[a]))
                    .then(a.cmp(&b))
            })?,
    };
    let snr = prof[k] - med;
    Some((peak_frequency(k), snr, snr / mad))
}

#[allow(clippy::too_many_arguments)]
fn best_slope(
    spec: &Spec,
    j: usize,
    fc: f64,
    center: f64,
    span: f64,
    step: f64,
    p: &TraceParams,
    nb: f64,
    half: usize,
) -> Option<(f64, (f64, f64, f64))> {
    let mut best: Option<(f64, (f64, f64, f64))> = None;
    let n = ((2.0 * span + 1e-9) / step).floor() as usize + 1;
    for i in 0..n {
        let sl = center - span + i as f64 * step;
        if let Some(r) = ridge_peak(spec, j, fc, sl, p.band, nb, half)
            && best.is_none_or(|b| r.2 > b.1.2)
        {
            best = Some((sl, r));
        }
    }
    best
}

fn polyfit_slope(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len() as f64;
    let (mx, my) = (x.iter().sum::<f64>() / n, y.iter().sum::<f64>() / n);
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for (a, b) in x.iter().zip(y) {
        sxy += (a - mx) * (b - my);
        sxx += (a - mx) * (a - mx);
    }
    if sxx > 0.0 { sxy / sxx } else { 0.0 }
}

/// Follow the ridge from each seed `(t, f)` in both directions.
pub fn ridge_trace(spec: &Spec, seeds: &[(f64, f64)], p: &TraceParams) -> Trace {
    let nt = spec.nt();
    if nt < 3 {
        return Trace::default();
    }
    let nb = p.noise_band.unwrap_or(4.0 * p.band);
    let hop = spec.dt();
    let stop_frames = ((p.stop_s / hop).round() as usize).max(5);
    let half = ((p.integ_s / hop).round() as usize).max(1);
    let mut got: std::collections::BTreeMap<usize, (f64, f64)> = Default::default();
    for &(ts, fs) in seeds {
        if ts < spec.t[0] || ts > spec.t[nt - 1] {
            continue;
        }
        let j0 = spec.frame_index(ts);
        let slope0 = best_slope(spec, j0, fs, 0.0, 25.0, 0.25, p, nb, half).map_or(0.0, |b| b.0);
        for dir in [1i64, -1] {
            let (mut ht, mut hf) = (vec![spec.t[j0]], vec![fs]);
            let (mut j, mut misses) = (j0 as i64, 0usize);
            let (mut f_last, mut t_last) = (fs, spec.t[j0]);
            while j >= 0 && (j as usize) < nt {
                let ju = j as usize;
                let slope = if ht.len() >= 8 {
                    let k0 = ht.len().saturating_sub(p.slope_pts);
                    let tl = ht[ht.len() - 1];
                    let xs: Vec<f64> = ht[k0..].iter().map(|x| x - tl).collect();
                    polyfit_slope(&xs, &hf[k0..])
                } else {
                    slope0
                };
                let f_pred = f_last + slope * (spec.t[ju] - t_last);
                let res = best_slope(spec, ju, f_pred, slope, 1.0, 0.5, p, nb, half).map(|b| b.1);
                match res {
                    Some((fhat, snr, z)) if snr >= p.min_snr && z >= p.min_z => {
                        let e = got.entry(ju).or_insert((fhat, snr));
                        if e.1 < snr {
                            *e = (fhat, snr);
                        }
                        ht.push(spec.t[ju]);
                        hf.push(fhat);
                        f_last = fhat;
                        t_last = spec.t[ju];
                        misses = 0;
                    }
                    _ => {
                        misses += 1;
                        if misses >= stop_frames {
                            break;
                        }
                    }
                }
                j += dir;
            }
        }
    }
    let mut tr = Trace::default();
    for (j, (f, s)) in got {
        tr.t.push(spec.t[j]);
        tr.f.push(f);
        tr.snr.push(s);
    }
    tr
}

/// Sample a hand-drawn polyline (sorted by time) at every frame between its ends.
pub fn polyline_trace(spec: &Spec, pts: &[(f64, f64)]) -> Trace {
    let mut pts = pts.to_vec();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut tr = Trace::default();
    if pts.len() < 2 {
        return tr;
    }
    for &tt in &spec.t {
        if tt < pts[0].0 || tt > pts[pts.len() - 1].0 {
            continue;
        }
        let k = pts.partition_point(|p| p.0 <= tt).clamp(1, pts.len() - 1);
        let (a, b) = (pts[k - 1], pts[k]);
        let ff = a.1 + (b.1 - a.1) * (tt - a.0) / (b.0 - a.0).max(1e-9);
        tr.t.push(tt);
        tr.f.push(ff);
        tr.snr
            .push(spec.bilinear(tt, ff).map_or(f64::NAN, |v| v as f64));
    }
    tr
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spectro::SpecMeta;

    fn profile_spec(peaks: &[(f64, f64)]) -> Spec {
        let f: Vec<f64> = (0..=160).map(|i| 80.0 + 0.25 * i as f64).collect();
        let db = f
            .iter()
            .flat_map(|&f| {
                let profile = 0.2 * (1.7 * f).sin()
                    + peaks
                        .iter()
                        .map(|&(center, height)| {
                            height * (-0.5 * ((f - center) / 0.5).powi(2)).exp()
                        })
                        .sum::<f64>();
                [profile as f32; 3]
            })
            .collect();
        Spec {
            t: vec![0.0, 1.0, 2.0],
            f,
            db,
            meta: SpecMeta {
                sr: 1000,
                win_s: 2.0,
                hop_s: 1.0,
                nfft: 4000,
                df: 0.25,
                fmin: 80.0,
                fmax: 120.0,
                norm: "analytic profile".into(),
            },
        }
    }

    #[test]
    fn guided_peak_requires_a_qualified_local_maximum() {
        let p = TraceParams::default();
        for (near_height, expected) in [(6.0, 100.0), (2.0, 104.0)] {
            let spec = profile_spec(&[(100.0, near_height), (104.0, 12.0)]);
            let strongest = ridge_peak(&spec, 1, 100.1, 0.0, 5.0, 12.0, 1).unwrap();
            assert!((strongest.0 - 104.0).abs() < 0.1, "{strongest:?}");
            let guided =
                guided_ridge_peak(&spec, 1, 100.1, 0.0, 5.0, 12.0, 1, p.min_snr, p.min_z).unwrap();
            // A qualified nearer ridge wins despite being weaker. A nearer noise bump
            // below the unchanged SNR gate must not hide a valid, more distant ridge.
            assert!((guided.0 - expected).abs() < 0.1, "{guided:?}");
        }
        let edge = profile_spec(&[(105.5, 16.0)]);
        assert!(edge.bilinear(1.0, 105.0).unwrap() as f64 > p.min_snr);
        assert!(
            guided_ridge_peak(&edge, 1, 100.0, 0.0, 5.0, 12.0, 1, p.min_snr, p.min_z).is_none(),
            "a strong band-edge shoulder is not a local maximum"
        );
    }
}
