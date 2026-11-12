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
const PRIMARY_SPEED_GRID: [f64; 11] = [
    40.0, 52.0, 64.0, 76.0, 88.0, 100.0, 113.0, 127.0, 142.0, 158.0, 175.0,
];

/// Upper speed of the original survey bank (m/s). Faster coarse seeds come from the
/// supplemental bank and count as measurements past the old line in the locate pipeline.
pub const PRIMARY_MAX_SPEED_MPS: f64 = PRIMARY_SPEED_GRID[PRIMARY_SPEED_GRID.len() - 1];

#[derive(Clone, Debug)]
pub struct DetectParams {
    pub fmin: f64,
    pub fmax: f64,
    pub win_s: f64,
    pub hop_s: f64,
    pub bins_per_octave: usize,
    pub v_grid: Vec<f64>,
    /// After the existing banks, search their residual with faster subsonic templates. Their
    /// spacing continues the primary bank's last ratio, up to the measurement bound, 0.8 c.
    /// Full-length templates run before their edge templates. Each only fills unused detection
    /// slots and must clear the strong detection line.
    pub fast_templates: bool,
    pub d_grid: Vec<f64>,
    /// Spacing (s) of the path samples.
    pub path_dt: f64,
    /// Path half-lengths = clamp(k * d / v, min_half_s, max_half_s) for each k: short paths
    /// catch passes whose asymptotes fade into the noise, long ones accumulate more evidence.
    pub k_tau: Vec<f64>,
    pub min_half_s: f64,
    pub max_half_s: f64,
    /// Half-length (s) of the edge templates; 0 (or anything from `min_half_s` up) turns them
    /// off. A short transition (shortest k * d / v at or under `min_half_s`) has a `min_half_s`
    /// template, which needs 60 % of each half on the recording, so a CPA nearer than that to
    /// the start or end cannot be scored however loud it is. After both passes, an edge
    /// template of this half scores exactly those cells, on the residual of every accepted
    /// curve: it explains only what the full-length bank left unexplained.
    pub edge_half_s: f64,
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
            v_grid: PRIMARY_SPEED_GRID.to_vec(),
            fast_templates: true,
            d_grid,
            path_dt: 2.0,
            k_tau: vec![1.25, 2.0, 3.0],
            min_half_s: 24.0,
            max_half_s: 240.0,
            edge_half_s: 10.0,
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
    /// Only for an image under `DetectParams::min_image_scale`: the survey image built contrast
    /// first (each frame's pooled level minus its median over +-1/3 octave, then each row's robust
    /// z), raised to the same floor. A loud pass that raises every row together inflates each
    /// row's MAD before the frame contrast can remove the swell, which shrinks its own ridge;
    /// taking the frame contrast first keeps the swell out of the row statistics.
    /// [`detect_counted`] scores it after the primary and edge banks, on what `data` left unexplained.
    /// A final edge scan recovers short-clip coverage after every existing stage has had its turn.
    pub contrast: Option<Vec<f32>>,
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
    let pooled = data.clone();
    // Max-pooling k linear bins raises the noise floor by an amount that grows with k (i.e. with
    // frequency), so put every log row on a common robust z scale before matched filtering.
    data.par_chunks_mut(nt).for_each(robust_z_row);
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
    raise(&mut data, gain);
    let contrast = (gain > 1.0).then(|| {
        let mut c = contrast_first(&pooled, n_rows, nt, half_w);
        let g = image_gain(robust_scale(&c), p.min_image_scale);
        raise(&mut c, g);
        c
    });
    Ok((
        LogSpec {
            t: lin.t.clone(),
            freqs,
            data,
            bins_per_octave: p.bins_per_octave,
            hop_s: lin.dt(),
            scale,
            gain,
            contrast,
        },
        lin,
    ))
}

/// A row on its robust z scale (MAD + 0.05), clamped to [-3, 8].
fn robust_z_row(row: &mut [f32]) {
    let mut tmp = row.to_vec();
    let med = crate::spectro::median_f32(&mut tmp);
    for v in tmp.iter_mut() {
        *v = (*v - med).abs();
    }
    let mad = 1.4826 * crate::spectro::median_f32(&mut tmp) + 0.05;
    for v in row.iter_mut() {
        *v = ((*v - med) / mad).clamp(-3.0, 8.0);
    }
}

/// Multiply an image by `gain` (from [`image_gain`]) and clamp it again; a gain of 1 leaves it
/// exactly as it is.
fn raise(data: &mut [f32], gain: f64) {
    if gain > 1.0 {
        let g = gain as f32;
        data.par_iter_mut()
            .for_each(|v| *v = (*v * g).clamp(-3.0, 8.0));
    }
}

/// [`LogSpec::contrast`] from the max-pooled image `pooled` (`n_rows` x `nt`, row-major): each
/// frame's level minus its median over +-`half_w` rows, then each row on its robust z scale.
fn contrast_first(pooled: &[f32], n_rows: usize, nt: usize, half_w: usize) -> Vec<f32> {
    let mut cols = vec![0f32; n_rows * nt];
    cols.par_chunks_mut(n_rows)
        .enumerate()
        .for_each(|(ti, col)| {
            let src: Vec<f32> = (0..n_rows).map(|r| pooled[r * nt + ti]).collect();
            let mut win = Vec::with_capacity(2 * half_w + 1);
            for (r, out) in col.iter_mut().enumerate() {
                let (a, b) = (r.saturating_sub(half_w), (r + half_w + 1).min(n_rows));
                win.clear();
                win.extend_from_slice(&src[a..b]);
                *out = src[r] - crate::spectro::median_f32(&mut win);
            }
        });
    let mut data = vec![0f32; n_rows * nt];
    data.par_chunks_mut(nt).enumerate().for_each(|(r, row)| {
        for (ti, v) in row.iter_mut().enumerate() {
            *v = cols[ti * n_rows + r];
        }
        robust_z_row(row);
    });
    data
}

/// A template's part in scoring ([`score_bank`]).
#[derive(Clone, Copy, Debug, PartialEq)]
enum EdgeRole {
    /// An ordinary bank template.
    Plain,
    /// Scores nothing itself: records, per cell, whether it is covered, for the edge template
    /// that follows it.
    Base,
    /// Scores a cell only where the base template before it is not covered and the cell lies
    /// within `base_half` of the recording's first or last frame.
    Edge { base_half: f64 },
}

struct Template {
    v: f64,
    d: f64,
    half: f64,
    /// (frame offset, row offset, is_approach)
    path: Vec<(i32, i32, bool)>,
    n_a: usize,
    n_r: usize,
    edge: EdgeRole,
}

/// Score each row independently so a worker keeps only one row's sums in cache. Coverage
/// comes from intervals of valid frames, rather than summing a full padded mask per sample.
/// The evidence additions retain path order, including when scoring the residual pass. Only
/// the cells (frames) in `windows` are scored, from the whole evidence image; every other cell
/// keeps -inf. A full window scores exactly as one without windows.
fn score_bank(
    ls: &LogSpec,
    tmpls: &[Template],
    evidence: &[f32],
    windows: &[(usize, usize)],
) -> (Vec<f32>, Vec<u16>) {
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
                    vec![false; nt],
                )
            },
            |(sa, sr, ca, cr, base_covered), (r, (bs, bi))| {
                for &(c0, c1) in windows {
                    let (c0, c1) = (c0.min(nt), c1.min(nt));
                    if c0 >= c1 {
                        continue;
                    }
                    for (ti, tm) in tmpls.iter().enumerate() {
                        sa[c0..c1].fill(0.0);
                        sr[c0..c1].fill(0.0);
                        ca[c0..=c1].fill(0);
                        cr[c0..=c1].fill(0);
                        for &(dt, dr, app) in &tm.path {
                            let source_row = r as i64 + dr as i64;
                            if source_row < 0 || source_row >= nr as i64 {
                                continue;
                            }
                            let a = (-(dt as i64)).clamp(c0 as i64, c1 as i64) as usize;
                            let b = (nt as i64 - dt as i64).clamp(c0 as i64, c1 as i64) as usize;
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
                        for k in c0..c1 {
                            na += ca[k];
                            nrr += cr[k];
                            let uncovered = (na as f32) < 0.6 * tm.n_a as f32
                                || (nrr as f32) < 0.6 * tm.n_r as f32;
                            match tm.edge {
                                EdgeRole::Plain => {}
                                EdgeRole::Base => {
                                    base_covered[k] = !uncovered;
                                    continue;
                                }
                                EdgeRole::Edge { base_half } => {
                                    let near_edge = ls.t[k] - ls.t[0] < base_half
                                        || ls.t[nt - 1] - ls.t[k] < base_half;
                                    if base_covered[k] || !near_edge {
                                        continue;
                                    }
                                }
                            }
                            if uncovered {
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
                }
            },
        );
    (best, indices)
}

fn template(p: &DetectParams, hop: f64, v: f64, d: f64, half: f64, edge: EdgeRole) -> Template {
    let b = p.bins_per_octave as f64;
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
    Template {
        v,
        d,
        half,
        path,
        n_a,
        n_r,
        edge,
    }
}

fn build_templates(p: &DetectParams, hop: f64) -> Vec<Template> {
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
                out.push(template(p, hop, v, d, half, EdgeRole::Plain));
            }
        }
    }
    out
}

/// Continue the established speed spacing without changing any primary template. Only the
/// existing subsonic measurement domain is added; no detection or measurement line is relaxed.
fn fast_template_params(p: &DetectParams) -> Option<DetectParams> {
    if !p.fast_templates || !p.c.is_finite() || 0.8 * p.c <= PRIMARY_MAX_SPEED_MPS {
        return None;
    }
    let ratio = PRIMARY_MAX_SPEED_MPS / PRIMARY_SPEED_GRID[PRIMARY_SPEED_GRID.len() - 2];
    let mut speeds = Vec::new();
    let mut speed = PRIMARY_MAX_SPEED_MPS * ratio;
    while speed <= 0.8 * p.c {
        speeds.push(speed);
        speed *= ratio;
    }
    Some(DetectParams {
        v_grid: speeds,
        ..p.clone()
    })
}

fn build_fast_templates(p: &DetectParams, hop: f64) -> Vec<Template> {
    fast_template_params(p).map_or_else(Vec::new, |p| build_templates(&p, hop))
}

fn build_fast_edge_templates(p: &DetectParams, hop: f64) -> Vec<Template> {
    fast_template_params(p).map_or_else(Vec::new, |p| build_edge_templates(&p, hop))
}

/// The edge bank: for each (v, d) whose shortest half is the `min_half_s` floor, that template
/// as a [`EdgeRole::Base`] followed by its `edge_half_s` edge template. Empty when
/// `edge_half_s` is not in (0, `min_half_s`).
fn build_edge_templates(p: &DetectParams, hop: f64) -> Vec<Template> {
    let mut out = Vec::new();
    if !(p.edge_half_s > 0.0 && p.edge_half_s < p.min_half_s) {
        return out;
    }
    let k_min = p.k_tau.iter().copied().fold(f64::INFINITY, f64::min);
    for &v in &p.v_grid {
        for &d in &p.d_grid {
            if k_min * d / v > p.min_half_s {
                continue;
            }
            let base = p.min_half_s.min(p.max_half_s);
            out.push(template(p, hop, v, d, base, EdgeRole::Base));
            out.push(template(
                p,
                hop,
                v,
                d,
                p.edge_half_s,
                EdgeRole::Edge { base_half: base },
            ));
        }
    }
    out
}

/// Only frames near a recording end can need the shorter coverage of an edge template.
fn edge_windows(ls: &LogSpec, min_half_s: f64) -> Vec<(usize, usize)> {
    let nt = ls.nt();
    let head = ls.t.partition_point(|&x| x - ls.t[0] < min_half_s);
    let tail = ls.t.partition_point(|&x| ls.t[nt - 1] - x >= min_half_s);
    if tail <= head {
        vec![(0, nt)]
    } else {
        vec![(0, head), (tail, nt)]
    }
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
    /// Found by the contrast stage ([`LogSpec::contrast`]): on no image the detection lines were
    /// set on, so no null run has tested what it adds. Written only when true.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub from_contrast: bool,
}

/// Run the template banks. Primary/edge detections come first, then the contrast stage's, then
/// the fast full-length and fast edge stages', then contrast edge recovery. Each group is
/// strongest first. All clear `thr_silence_db`; supplemental groups also clear `thr_db`.
/// Family primaries are the measurement candidates.
pub fn detect(ls: &LogSpec, p: &DetectParams) -> Vec<Detection> {
    detect_counted(ls, p).0
}

/// [`detect`], plus how many accepted candidates the cap `p.max_detections` cut. The count is a
/// lower bound when the scan stopped at three times the cap.
pub fn detect_counted(ls: &LogSpec, p: &DetectParams) -> (Vec<Detection>, usize) {
    let (detections, cut, _) = detect_counted_with_budget(ls, p);
    (detections, cut)
}

/// [`detect_counted`], also returning the unused part of the shared 400,000 candidate-cell
/// allowance. A later recovery scan must use this allowance rather than start another scan
/// budget. This does not change the ordinary stage order or any accepted detection.
pub fn detect_counted_with_budget(
    ls: &LogSpec,
    p: &DetectParams,
) -> (Vec<Detection>, usize, usize) {
    detect_with_budget_impl(ls, p, true)
}

// Keep the complete original stage sequence available for output-equivalence tests.
#[cfg(test)]
fn detect_counted_impl(
    ls: &LogSpec,
    p: &DetectParams,
    recover_contrast_edges: bool,
) -> (Vec<Detection>, usize) {
    let (detections, cut, _) = detect_with_budget_impl(ls, p, recover_contrast_edges);
    (detections, cut)
}

fn detect_with_budget_impl(
    ls: &LogSpec,
    p: &DetectParams,
    recover_contrast_edges: bool,
) -> (Vec<Detection>, usize, usize) {
    let (nt, nr) = (ls.nt(), ls.nr());
    if nt < 8 || nr < 8 {
        return (Vec::new(), 0, 400_000);
    }
    let tmpls = build_templates(p, ls.hop_s);
    let mut lp = ls.data.clone();
    // Greedy non-maximum suppression in (t0r, log f0) plus residual evidence: `known` marks
    // the cells (+-3 log-rows) along every accepted curve; a later candidate is re-scored with
    // those cells counted as zero, so a template that merely rides an already detected ridge
    // with a different (v, d) cannot become a second event.
    let mut known = vec![false; nr * nt];
    let mut out: Vec<Detection> = Vec::new();
    let mut remaining_candidates = 400_000usize;
    for pass in 0..2 {
        let (bs, bi) = score_bank(ls, &tmpls, &lp, &[(0, nt)]);
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
        accept_candidates(
            ls,
            p,
            &tmpls,
            (&bs, &bi),
            &cand,
            threshold,
            &mut known,
            &mut out,
        );
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
        zero_known(&mut lp, &known);
    }
    // Edge stage (`DetectParams::edge_half_s`): after both passes, so every detection they
    // accepted is the one they accept without it, on the residual of every accepted curve and
    // from the silence line, with the passes' suppression, residual and marking rules.
    let edge = build_edge_templates(p, ls.hop_s);
    if !edge.is_empty() && out.len() < p.max_detections * 3 && remaining_candidates > 0 {
        // Edge templates score only cells within the base half of the first or last frame.
        let windows = edge_windows(ls, p.min_half_s);
        zero_known(&mut lp, &known);
        let (bs, bi) = score_bank(ls, &edge, &lp, &windows);
        let threshold = p.thr_silence_db;
        let mut cand: Vec<usize> = (0..nr * nt)
            .filter(|&k| bs[k] as f64 >= threshold)
            .collect();
        cand.sort_by(|&a, &b| bs[b].total_cmp(&bs[a]));
        cand.truncate(remaining_candidates);
        remaining_candidates -= cand.len();
        accept_candidates(
            ls,
            p,
            &edge,
            (&bs, &bi),
            &cand,
            threshold,
            &mut known,
            &mut out,
        );
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut cut = out.len().saturating_sub(p.max_detections);
    out.truncate(p.max_detections);
    // Contrast stage (`LogSpec::contrast`, only for an image under the spread floor): the full
    // bank on the contrast-first image, with every cell the curves above explain counted as zero,
    // with the same suppression and marking. The lines were not set on this image, so it adds
    // strong curves only (`thr_db`), not weak ones down to the silence line. Its detections follow
    // every detection above, strongest first among themselves, in the room the cap leaves: so each
    // detection above keeps its place, its harmonic grouping and its measurement order.
    if let Some(contrast) = ls.contrast.as_deref() {
        let room = p.max_detections - out.len();
        if room > 0 && remaining_candidates > 0 {
            let view = LogSpec {
                t: ls.t.clone(),
                freqs: ls.freqs.clone(),
                data: contrast.to_vec(),
                bins_per_octave: ls.bins_per_octave,
                hop_s: ls.hop_s,
                scale: ls.scale,
                gain: ls.gain,
                contrast: None,
            };
            let mut lc = view.data.clone();
            zero_known(&mut lc, &known);
            let (bs, bi) = score_bank(&view, &tmpls, &lc, &[(0, nt)]);
            let threshold = p.thr_db.max(p.thr_silence_db);
            let mut cand: Vec<usize> = (0..nr * nt)
                .filter(|&k| bs[k] as f64 >= threshold)
                .collect();
            cand.sort_by(|&a, &b| bs[b].total_cmp(&bs[a]));
            cand.truncate(remaining_candidates);
            remaining_candidates -= cand.len();
            let before = out.len();
            accept_candidates(
                &view,
                p,
                &tmpls,
                (&bs, &bi),
                &cand,
                threshold,
                &mut known,
                &mut out,
            );
            let mut added = out.split_off(before);
            for d in added.iter_mut() {
                d.from_contrast = true;
            }
            added.sort_by(|a, b| b.score.total_cmp(&a.score));
            cut += added.len().saturating_sub(room);
            added.truncate(room);
            out.extend(added);
        }
    }
    // Faster passes were outside the original bank. Search only evidence its detections and
    // the contrast stage left unexplained, and append rather than displacing their candidates.
    if p.fast_templates {
        // Full-length seeds keep priority. Their edge scan uses only the remaining slots and
        // evidence, and the ordinary edge bank's exact same base-coverage guard.
        for edges_only in [false, true] {
            let room = p.max_detections - out.len();
            if room == 0 || remaining_candidates == 0 {
                break;
            }
            let fast = if edges_only {
                build_fast_edge_templates(p, ls.hop_s)
            } else {
                build_fast_templates(p, ls.hop_s)
            };
            if fast.is_empty() {
                continue;
            }
            let windows = if edges_only {
                edge_windows(ls, p.min_half_s)
            } else {
                vec![(0, nt)]
            };
            zero_known(&mut lp, &known);
            let (bs, bi) = score_bank(ls, &fast, &lp, &windows);
            let threshold = p.thr_db.max(p.thr_silence_db);
            let mut cand: Vec<usize> = (0..nr * nt)
                .filter(|&k| bs[k] as f64 >= threshold)
                .collect();
            cand.sort_by(|&a, &b| bs[b].total_cmp(&bs[a]));
            cand.truncate(remaining_candidates);
            remaining_candidates -= cand.len();
            let before = out.len();
            accept_candidates(
                ls,
                p,
                &fast,
                (&bs, &bi),
                &cand,
                threshold,
                &mut known,
                &mut out,
            );
            let mut added = out.split_off(before);
            added.sort_by(|a, b| b.score.total_cmp(&a.score));
            cut += added.len().saturating_sub(room);
            added.truncate(room);
            out.extend(added);
        }
    }
    // Contrast-first evidence can reveal a pass in a clip too short for any full template.
    // Run its edge templates last, so no original stage loses its candidates or their order.
    if recover_contrast_edges
        && !edge.is_empty()
        && out.len() < p.max_detections
        && remaining_candidates > 0
        && let Some(contrast) = ls.contrast.as_deref()
    {
        let view = LogSpec {
            t: ls.t.clone(),
            freqs: ls.freqs.clone(),
            data: contrast.to_vec(),
            bins_per_octave: ls.bins_per_octave,
            hop_s: ls.hop_s,
            scale: ls.scale,
            gain: ls.gain,
            contrast: None,
        };
        // The primary residual is no longer needed; reuse its allocation for this image.
        lp.copy_from_slice(contrast);
        zero_known(&mut lp, &known);
        let windows = edge_windows(&view, p.min_half_s);
        let (bs, bi) = score_bank(&view, &edge, &lp, &windows);
        let threshold = p.thr_db.max(p.thr_silence_db);
        let mut cand: Vec<usize> = (0..nr * nt)
            .filter(|&k| bs[k] as f64 >= threshold)
            .collect();
        cand.sort_by(|&a, &b| bs[b].total_cmp(&bs[a]));
        // This last stage can consume only what the shared candidate budget left.
        cand.truncate(remaining_candidates);
        remaining_candidates -= cand.len();
        let before = out.len();
        let room = p.max_detections - before;
        // A short sparse path can join unrelated peaks while the intervening frames hold
        // no coherent ridge. Check native frames inside the seed's survey cell, at the same
        // effective sample count and strong line. Companion templates retain the bank indices.
        let dense = build_edge_templates(
            &DetectParams {
                path_dt: ls.hop_s,
                ..p.clone()
            },
            ls.hop_s,
        );
        for &k in &cand {
            if out.len() >= p.max_detections * 3 {
                break;
            }
            // Recheck the live mask after each acceptance: explained intermediate frames
            // count as zero, with their coverage and denominator intact, just like the bank.
            let ti = bi[k] as usize;
            if !corroborate_contrast_edge(
                &view,
                p,
                (k / nt, k % nt),
                (&edge[ti], &dense[ti]),
                &known,
                threshold,
            ) {
                continue;
            }
            accept_candidates(
                &view,
                p,
                &edge,
                (&bs, &bi),
                &[k],
                threshold,
                &mut known,
                &mut out,
            );
        }
        let mut added = out.split_off(before);
        for d in added.iter_mut() {
            d.from_contrast = true;
        }
        added.sort_by(|a, b| b.score.total_cmp(&a.score));
        cut += added.len().saturating_sub(room);
        added.truncate(room);
        out.extend(added);
    }
    for (i, d) in out.iter_mut().enumerate() {
        d.id = i;
    }
    group_harmonics(&mut out, p);
    (out, cut, remaining_candidates)
}

/// An already measured ridge and its detected harmonic family. Ownership follows the observed
/// frequencies, not the fitted speed and distance: an accepted partial curve can have inaccurate
/// geometry and still own the sound it traced. Ratios are relative to this trace's tone.
pub struct RecoveryOwner<'a> {
    pub trace: &'a crate::trace::Trace,
    pub harmonics: Vec<f64>,
}

/// Try a half-second survey for fast passes clipped by a recording end when every ordinary
/// measurement was refused. With accepted aircraft, use [`recover_fast_edges_with_owners`]
/// to retain their observed ridges and harmonic families as occupied evidence.
///
/// At most 48 seconds from each real recording end are surveyed. Only fast edge templates
/// score, at the existing strong line, and crop seams cannot become artificial recording
/// ends. `slots` is the room left by the ordinary detections; `remaining_candidates` must
/// come from [`detect_counted_with_budget`]. The returned IDs and harmonic references start
/// at zero and must be rebased when appending. The ordinary linear survey remains the input
/// to refinement and measurement, with all their existing gates.
pub fn recover_fast_edges(
    x: &[f32],
    sr: u32,
    p: &DetectParams,
    slots: usize,
    remaining_candidates: usize,
) -> Result<(Vec<Detection>, usize), String> {
    recover_fast_edges_with_owners(x, sr, p, slots, remaining_candidates, &[])
}

/// The same bounded edge recovery on the residual of already measured aircraft. Known samples
/// contribute zero without shortening a template's denominator, exactly as in the ordinary
/// detector. The caller preserves those measurements and counts them toward its event limit.
pub fn recover_fast_edges_with_owners(
    x: &[f32],
    sr: u32,
    p: &DetectParams,
    slots: usize,
    remaining_candidates: usize,
    owners: &[RecoveryOwner<'_>],
) -> Result<(Vec<Detection>, usize), String> {
    recover_edges_impl(x, sr, p, slots, remaining_candidates, owners, false)
        .map(|(detections, cut, _)| (detections, cut))
}

fn recover_edges_impl(
    x: &[f32],
    sr: u32,
    p: &DetectParams,
    slots: usize,
    mut remaining_candidates: usize,
    owners: &[RecoveryOwner<'_>],
    ordinary: bool,
) -> Result<(Vec<Detection>, usize, usize), String> {
    let slots = slots.min(p.max_detections);
    if slots == 0 || remaining_candidates == 0 || (!ordinary && !p.fast_templates) {
        return Ok((Vec::new(), 0, remaining_candidates));
    }
    let short = DetectParams {
        win_s: 0.5,
        hop_s: 0.5,
        ..p.clone()
    };
    let templates = if ordinary {
        // Use the same compact primary-speed domain as the short interior bank, but retain
        // each base/edge pair's coverage guard. A crop seam must never become a true edge.
        let k_min = short.k_tau.iter().copied().fold(f64::INFINITY, f64::min);
        build_edge_templates(&short, short.hop_s)
            .into_iter()
            .filter(|t| {
                t.v > 0.0
                    && t.v <= PRIMARY_MAX_SPEED_MPS
                    && k_min * t.d / t.v <= short.edge_half_s
            })
            .collect()
    } else {
        build_fast_edge_templates(&short, short.hop_s)
    };
    if templates.is_empty() {
        return Ok((Vec::new(), 0, remaining_candidates));
    }
    let base_half = p.min_half_s.min(p.max_half_s);
    let edge_samples = ((2.0 * base_half).min(48.0) * f64::from(sr)) as usize;
    if edge_samples == 0 || x.len() < (short.win_s * f64::from(sr)) as usize {
        return Ok((Vec::new(), 0, remaining_candidates));
    }
    let chunks = if x.len() <= 2 * edge_samples {
        vec![(0, x.len())]
    } else {
        vec![(0, edge_samples), (x.len() - edge_samples, x.len())]
    };
    // With spare detection slots the ordinary stages never truncated accepted candidates,
    // so max_detections - slots is also their accepted count. Preserve the combined 3x stop.
    let accepted_limit = p.max_detections * 3 - (p.max_detections - slots);
    let threshold = p.thr_db.max(p.thr_silence_db);
    let mut out = Vec::new();
    for (start, end) in chunks {
        if remaining_candidates == 0 || out.len() >= accepted_limit {
            break;
        }
        let (mut ls, _) = survey(&x[start..end], sr, &short)?;
        for t in &mut ls.t {
            *t += start as f64 / f64::from(sr);
        }
        let (nt, nr) = (ls.nt(), ls.nr());
        if nt < 8 || nr < 8 {
            continue;
        }
        let windows = recovery_edge_windows(&ls, start == 0, end == x.len(), base_half);
        let mut known = vec![false; nr * nt];
        mark_observed_ridges(&ls, owners, &mut known);
        let mut residual = ls.data.clone();
        zero_known(&mut residual, &known);
        let (bs, bi) = score_bank(&ls, &templates, &residual, &windows);
        let mut cand: Vec<_> = (0..nr * nt)
            .filter(|&k| bs[k] as f64 >= threshold)
            .collect();
        cand.sort_by(|&a, &b| bs[b].total_cmp(&bs[a]));
        cand.truncate(remaining_candidates);
        remaining_candidates -= cand.len();
        for k in cand {
            if out.len() >= accepted_limit {
                break;
            }
            accept_candidates(
                &ls,
                &short,
                &templates,
                (&bs, &bi),
                &[k],
                threshold,
                &mut known,
                &mut out,
            );
        }
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    let cut = out.len().saturating_sub(slots);
    out.truncate(slots);
    for (id, detection) in out.iter_mut().enumerate() {
        detection.id = id;
    }
    group_harmonics(&mut out, p);
    Ok((out, cut, remaining_candidates))
}

/// Preserve true-edge recovery, then revisit the CPA neighbourhoods of refused detections
/// with full fast templates. An unmeasurable slow seed must not permanently own the audio
/// that a faster pass needs. Accepted measurements own only their observed ridges, as above.
/// Finally, short fast templates scan with complete support in bounded interior tiles, so
/// a brief pass need not illuminate the full bank's 24 s halves. All three stages share
/// the original candidate allowance and detection slots. Use the counted variant to report
/// an incomplete interior scan when those limits stop it.
#[allow(clippy::too_many_arguments)]
pub fn recover_fast_passes_with_owners(
    x: &[f32],
    sr: u32,
    p: &DetectParams,
    slots: usize,
    remaining_candidates: usize,
    owners: &[RecoveryOwner<'_>],
    refused: &[&Detection],
) -> Result<(Vec<Detection>, usize), String> {
    recover_fast_passes_counted_with_owners(x, sr, p, slots, remaining_candidates, owners, refused)
        .map(|(detections, cut, _)| (detections, cut))
}

/// [`recover_fast_passes_with_owners`], plus whether any short-bank interior centres were left
/// unsearched because the original proposal allowance or available detection slots ran out.
/// The separate cut count still counts only accepted detections discarded by the slot limit.
#[allow(clippy::too_many_arguments)]
pub fn recover_fast_passes_counted_with_owners(
    x: &[f32],
    sr: u32,
    p: &DetectParams,
    slots: usize,
    remaining_candidates: usize,
    owners: &[RecoveryOwner<'_>],
    refused: &[&Detection],
) -> Result<(Vec<Detection>, usize, bool), String> {
    recover_fast_passes_impl(x, sr, p, slots, remaining_candidates, owners, refused, true)
}

/// Short-window recovery stages separated by ordinary measurement at the caller. A proposal
/// that later fails physical measurement must not own the evidence needed by the shorter bank.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FastRecoveryStage {
    EdgeAndRefused,
    ShortInterior,
    /// Compact bank at the primary survey's speeds, after both fast stages were measured.
    OrdinaryShortInterior,
    /// Final compact primary-speed bank at true recording ends, after interior measurement.
    OrdinaryEdges,
}

/// One recovery stage's detections and its use of the original shared limits.
#[derive(Debug)]
pub struct FastRecoveryBatch {
    pub detections: Vec<Detection>,
    /// Accepted detections discarded by the available detection-slot limit.
    pub cut: usize,
    /// Unused proposal allowance to pass unchanged to the next stage.
    pub remaining_candidates: usize,
    /// Short-window recovery whose coverage cannot be completed within the original limits.
    /// Includes the ordinary edge stage; always false for `EdgeAndRefused`.
    pub interior_incomplete: bool,
}

/// Run just one recovery stage, retaining its unused proposal allowance for the next.
///
/// `EdgeAndRefused` preserves the true-edge and refused-seed searches. The two short interior
/// stages ignore `refused` and do not rerun an earlier search. `OrdinaryShortInterior` uses the
/// primary speed grid: a close ordinary-speed pass can also fade before a full template ends.
/// `OrdinaryEdges` runs last, for the same compact passes too close to a real recording end
/// to have complete short-template support. Measure the interior stage before calling it.
/// Only the supplied accepted measurements and detections made within this stage own its
/// evidence. After measuring the earlier stage,
/// callers pass its accepted observed ridges as `owners`; its refused detections still consume
/// detection slots and proposal allowance, but cannot hide a new short-template candidate.
#[allow(clippy::too_many_arguments)]
pub fn recover_fast_passes_stage_with_owners(
    x: &[f32],
    sr: u32,
    p: &DetectParams,
    slots: usize,
    remaining_candidates: usize,
    owners: &[RecoveryOwner<'_>],
    refused: &[&Detection],
    stage: FastRecoveryStage,
) -> Result<FastRecoveryBatch, String> {
    recover_fast_stages_impl(
        x,
        sr,
        p,
        slots,
        remaining_candidates,
        owners,
        refused,
        Some(stage),
    )
}

// Keep the established edge/refused-seed path available for native recovery comparisons.
#[allow(clippy::too_many_arguments)]
fn recover_fast_passes_impl(
    x: &[f32],
    sr: u32,
    p: &DetectParams,
    slots: usize,
    remaining_candidates: usize,
    owners: &[RecoveryOwner<'_>],
    refused: &[&Detection],
    scan_short: bool,
) -> Result<(Vec<Detection>, usize, bool), String> {
    let batch = recover_fast_stages_impl(
        x,
        sr,
        p,
        slots,
        remaining_candidates,
        owners,
        refused,
        (!scan_short).then_some(FastRecoveryStage::EdgeAndRefused),
    )?;
    Ok((batch.detections, batch.cut, batch.interior_incomplete))
}

// None retains the combined API's original stage ownership and output order. The pipeline
// selects each stage separately so only measurements accepted between them become owners.
#[allow(clippy::too_many_arguments)]
fn recover_fast_stages_impl(
    x: &[f32],
    sr: u32,
    p: &DetectParams,
    slots: usize,
    remaining_candidates: usize,
    owners: &[RecoveryOwner<'_>],
    refused: &[&Detection],
    stage: Option<FastRecoveryStage>,
) -> Result<FastRecoveryBatch, String> {
    if stage == Some(FastRecoveryStage::OrdinaryEdges) {
        let (detections, cut, remaining_candidates) =
            recover_edges_impl(x, sr, p, slots, remaining_candidates, owners, true)?;
        return Ok(FastRecoveryBatch {
            detections,
            cut,
            remaining_candidates,
            // Exhausting the original allowance cannot certify complete edge coverage.
            interior_incomplete: slots.min(p.max_detections) == 0 || remaining_candidates == 0,
        });
    }
    let short_only = matches!(
        stage,
        Some(FastRecoveryStage::ShortInterior | FastRecoveryStage::OrdinaryShortInterior)
    );
    let ordinary_short = stage == Some(FastRecoveryStage::OrdinaryShortInterior);
    let (mut out, mut cut, mut remaining_candidates) = if short_only {
        (Vec::new(), 0, remaining_candidates)
    } else {
        recover_edges_impl(x, sr, p, slots, remaining_candidates, owners, false)?
    };
    let scan_short = stage != Some(FastRecoveryStage::EdgeAndRefused);
    let slots = slots.min(p.max_detections);
    let short = DetectParams {
        win_s: 0.5,
        hop_s: 0.5,
        ..p.clone()
    };
    let templates = if ordinary_short {
        Vec::new()
    } else {
        build_fast_templates(&short, short.hop_s)
    };
    let duration = x.len() as f64 / f64::from(sr);
    let base_half = p.min_half_s.min(p.max_half_s);
    // Reuse edge paths only for transitions whose natural shortest support fits
    // inside their short half. Here both complete sides must fit inside the recording.
    let k_min = p.k_tau.iter().copied().fold(f64::INFINITY, f64::min);
    let compact_bank = if ordinary_short {
        build_edge_templates(&short, short.hop_s)
    } else {
        build_fast_edge_templates(&short, short.hop_s)
    };
    let compact: Vec<_> = compact_bank
        .into_iter()
        .filter_map(|mut t| {
            if matches!(t.edge, EdgeRole::Edge { .. })
                && k_min * t.d / t.v <= t.half
                && (!ordinary_short || (t.v > 0.0 && t.v <= PRIMARY_MAX_SPEED_MPS))
            {
                t.edge = EdgeRole::Plain;
                Some(t)
            } else {
                None
            }
        })
        .collect();
    if (templates.is_empty() && compact.is_empty()) || !duration.is_finite() {
        return Ok(FastRecoveryBatch {
            detections: out,
            cut,
            remaining_candidates,
            interior_incomplete: false,
        });
    }
    let interior = if compact.is_empty() {
        Vec::new()
    } else {
        short_recovery_windows(p.edge_half_s, duration)
    };
    if out.len() >= slots || remaining_candidates == 0 {
        return Ok(FastRecoveryBatch {
            detections: out,
            cut,
            remaining_candidates,
            interior_incomplete: scan_short && !interior.is_empty(),
        });
    }
    let mut windows: Vec<_> = refused
        .iter()
        .filter(|d| d.t0r.is_finite() && d.v.is_finite() && d.v > 0.0 && d.d.is_finite())
        .filter_map(|d| {
            // Use the refused seed's time scale and the detector's ten-second suppression
            // floor. Only this bounded CPA neighbourhood is revisited.
            let half = (0.5 * d.d / d.v).max(10.0);
            let lo = (d.t0r - half).max(base_half);
            let hi = (d.t0r + half).min(duration - base_half);
            (lo <= hi).then_some((lo, hi))
        })
        .collect();
    windows.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (lo, hi) in windows {
        if let Some(last) = merged.last_mut()
            && lo <= last.1
        {
            last.1 = last.1.max(hi);
        } else {
            merged.push((lo, hi));
        }
    }
    let accepted_limit = p.max_detections * 3 - (p.max_detections - slots);
    let threshold = p.thr_db.max(p.thr_silence_db);
    let mut incomplete = false;
    for (short_stage, windows, templates) in
        [(false, &merged, &templates), (true, &interior, &compact)]
    {
        if templates.is_empty() || (short_stage && !scan_short) || (!short_stage && short_only) {
            continue;
        }
        let first = out.len();
        let support = templates.iter().map(|t| t.half).fold(0.0, f64::max);
        for &(lo, hi) in windows {
            if remaining_candidates == 0 || out.len() >= accepted_limit || first >= slots {
                incomplete |= short_stage;
                break;
            }
            // Include complete path and STFT support on both sides. Tile seams cannot
            // become recording edges. Keep the original refused-window sampling phase;
            // short tiles share an absolute half-second grid.
            let mut from = (lo - support - short.win_s).max(0.0);
            if short_stage {
                from = (from / short.hop_s).floor() * short.hop_s;
            }
            let start = (from * f64::from(sr)) as usize;
            let end = (((hi + support + short.win_s) * f64::from(sr)).ceil() as usize).min(x.len());
            let (mut ls, _) = survey(&x[start..end], sr, &short)?;
            for t in &mut ls.t {
                *t += start as f64 / f64::from(sr);
            }
            let (nt, nr) = (ls.nt(), ls.nr());
            if nt < 8 || nr < 8 {
                continue;
            }
            let centres = [(
                ls.t.partition_point(|&t| t < lo || (short_stage && t < ls.t[0] + support)),
                ls.t.partition_point(|&t| {
                    if short_stage {
                        t < hi && t <= ls.t[nt - 1] - support
                    } else {
                        t <= hi
                    }
                }),
            )];
            let mut known = vec![false; nr * nt];
            mark_observed_ridges(&ls, owners, &mut known);
            for detection in &out {
                mark_detection_ridge(&ls, &short, detection, &mut known);
            }
            let mut residual = ls.data.clone();
            zero_known(&mut residual, &known);
            let (bs, bi) = score_bank(&ls, templates, &residual, &centres);
            let mut candidates: Vec<_> = (0..nr * nt)
                .filter(|&k| bs[k] as f64 >= threshold)
                .collect();
            candidates.sort_by(|&a, &b| bs[b].total_cmp(&bs[a]));
            incomplete |= short_stage && candidates.len() > remaining_candidates;
            candidates.truncate(remaining_candidates);
            remaining_candidates -= candidates.len();
            for k in candidates {
                if out.len() >= accepted_limit {
                    incomplete |= short_stage;
                    break;
                }
                accept_candidates(
                    &ls,
                    &short,
                    templates,
                    (&bs, &bi),
                    &[k],
                    threshold,
                    &mut known,
                    &mut out,
                );
            }
        }
        // Preserve edge -> refused-seed -> short-bank priority, even when a later stage's
        // score is larger. A new family may name an earlier primary, never replace it.
        let mut added = out.split_off(first);
        added.sort_by(|a, b| b.score.total_cmp(&a.score));
        cut += added.len().saturating_sub(slots - first);
        added.truncate(slots - first);
        out.extend(added);
        for (id, detection) in out.iter_mut().enumerate() {
            detection.id = id;
        }
        group_harmonics_after(&mut out, p, first);
    }
    Ok(FastRecoveryBatch {
        detections: out,
        cut,
        remaining_candidates,
        interior_incomplete: incomplete,
    })
}

/// Complete short-template support, tiled into at most two minutes of centres. Each default
/// survey includes another 10.5 s on either side, so its working image stays within 141 s.
/// Earlier full-bank centres remain eligible: their 24 s halves tested different evidence.
/// Accepted detections retain their masks and priority.
fn short_recovery_windows(half: f64, duration: f64) -> Vec<(f64, f64)> {
    // The domain's last centre is eligible, including a fractional recording duration
    // that puts it exactly on the absolute STFT grid. The tile intervals are half-open.
    let end = (duration - half).next_up();
    let mut windows = Vec::new();
    let mut next = half;
    while next < end {
        let until = (next + 120.0).min(end);
        windows.push((next, until));
        next = until;
    }
    windows
}

/// Interpolate only through observed stretches. A larger gap is not owned evidence, matching
/// the measurement's three-second ridge-continuity limit. Never extrapolate to untraced audio.
fn mark_observed_ridges(ls: &LogSpec, owners: &[RecoveryOwner<'_>], known: &mut [bool]) {
    let (nt, nr) = (ls.nt(), ls.nr());
    for owner in owners {
        for (times, frequencies) in owner.trace.t.windows(2).zip(owner.trace.f.windows(2)) {
            let dt = times[1] - times[0];
            if !(dt > 0.0 && dt <= 3.0 && times[0].is_finite() && times[1].is_finite()) {
                continue;
            }
            let lo = ls.t.partition_point(|&t| t < times[0]);
            let hi = ls.t.partition_point(|&t| t <= times[1]);
            for j in lo..hi {
                let u = (ls.t[j] - times[0]) / dt;
                let f = frequencies[0] + u * (frequencies[1] - frequencies[0]);
                for ratio in std::iter::once(1.0).chain(owner.harmonics.iter().copied()) {
                    let frequency = f * ratio;
                    if !(frequency.is_finite() && frequency > 0.0) {
                        continue;
                    }
                    let row =
                        ((frequency / ls.freqs[0]).log2() * ls.bins_per_octave as f64).round();
                    if !row.is_finite()
                        || row < -RIDGE_HALF_ROWS as f64
                        || row > (nr - 1) as f64 + RIDGE_HALF_ROWS as f64
                    {
                        continue;
                    }
                    let row = row as i64;
                    for r in
                        (row - RIDGE_HALF_ROWS).max(0)..=(row + RIDGE_HALF_ROWS).min(nr as i64 - 1)
                    {
                        known[r as usize * nt + j] = true;
                    }
                }
            }
        }
    }
}

/// A cropped edge keeps the complete base-template context toward the recording interior.
/// Its other boundary is a crop seam, so no candidate there is eligible for edge recovery.
fn recovery_edge_windows(
    ls: &LogSpec,
    at_start: bool,
    at_end: bool,
    base_half: f64,
) -> Vec<(usize, usize)> {
    let nt = ls.nt();
    let first = ls.t[0];
    let last = ls.t[nt - 1];
    let head = if at_start {
        ls.t.partition_point(|&t| t - first < base_half && (at_end || t + base_half <= last))
    } else {
        0
    };
    let tail = if at_end {
        ls.t.partition_point(|&t| last - t >= base_half || (!at_start && t - base_half < first))
    } else {
        nt
    };
    if tail <= head {
        vec![(0, nt)]
    } else {
        [(0, head), (tail, nt)]
            .into_iter()
            .filter(|(a, b)| a < b)
            .collect()
    }
}

/// Require one coherent curve between a short contrast seed's sparse samples. Keep its
/// speed/range and observed-frame coverage fixed; only the half-row tone cell and half-hop
/// clock cell are searched. Native-frame means use the original sparse sample counts so a
/// finer cadence cannot increase the score merely by adding correlated observations.
fn corroborate_contrast_edge(
    ls: &LogSpec,
    p: &DetectParams,
    (r, i): (usize, usize),
    (sparse, dense): (&Template, &Template),
    known: &[bool],
    threshold: f64,
) -> bool {
    if p.path_dt <= ls.hop_s {
        return true;
    }
    let (nt, nr) = (ls.nt() as i64, ls.nr() as i64);
    let index = |dt: i32, dr: i32| {
        let (t, row) = (i as i64 + dt as i64, r as i64 + dr as i64);
        (t >= 0 && t < nt && row >= 0 && row < nr).then_some(t as usize)
    };
    let mut counts = [0usize; 2];
    for &(dt, dr, approach) in &sparse.path {
        if index(dt, dr).is_some() {
            counts[usize::from(!approach)] += 1;
        }
    }
    let frames: Vec<_> = dense
        .path
        .iter()
        .filter_map(|&(dt, dr, approach)| index(dt, dr).map(|t| (t, usize::from(!approach))))
        .collect();
    let mut native_counts = [0usize; 2];
    for &(_, side) in &frames {
        native_counts[side] += 1;
    }
    if counts.contains(&0)
        || native_counts.contains(&0)
        || (native_counts[0] as f64) < 0.6 * dense.n_a as f64
        || (native_counts[1] as f64) < 0.6 * dense.n_r as f64
    {
        return false;
    }
    let scale = [
        (counts[0] as f64).sqrt() / native_counts[0] as f64,
        (counts[1] as f64).sqrt() / native_counts[1] as f64,
    ];
    let clears = |sums: [f64; 2]| (sums[0] * scale[0]).min(sums[1] * scale[1]) >= threshold;
    let sample = |t: usize, row: i64| {
        if row < 0 || row >= nr {
            return 0.0;
        }
        let k = row as usize * nt as usize + t;
        if known[k] { 0.0 } else { ls.data[k] as f64 }
    };
    let mut crossings = Vec::with_capacity(frames.len());
    // As in refine, four binary scales resolve the clock cell. The score line is unchanged.
    for phase in -8..=8 {
        let clock = phase as f64 * ls.hop_s / 16.0;
        let mut sums = [0.0; 2];
        crossings.clear();
        for &(t, side) in &frames {
            let u = ls.t[t] - ls.t[i] - clock;
            let row = r as f64
                + cv_ratio_rel_t0r(u, sparse.v, sparse.d, p.c).log2() * p.bins_per_octave as f64;
            if !row.is_finite() {
                return false;
            }
            // Inside a one-row-wide f0 cell each frame crosses at most one rounding
            // boundary. Start just inside its lower edge, then sweep every open interval.
            let lower = row.floor() as i64;
            let old = sample(t, lower);
            sums[side] += old;
            let shift = lower as f64 + 0.5 - row;
            if shift < 0.5 {
                crossings.push((shift, side, sample(t, lower + 1) - old));
            }
        }
        if clears(sums) {
            return true;
        }
        crossings.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let mut j = 0;
        while j < crossings.len() {
            let boundary = crossings[j].0;
            // Equal boundaries move together: mixing their old/new rows would not describe
            // any single physical frequency. Masked or out-of-band rows contribute zero.
            while j < crossings.len() && crossings[j].0 == boundary {
                let (_, side, delta) = crossings[j];
                sums[side] += delta;
                j += 1;
            }
            if clears(sums) {
                return true;
            }
        }
    }
    false
}

/// Known samples contribute zero to a later bank scan.
fn zero_known(lp: &mut [f32], known: &[bool]) {
    for (z, &seen) in lp.iter_mut().zip(known) {
        if seen {
            *z = 0.0;
        }
    }
}

/// Accept `cand` (cells, strongest first) in order: a candidate is kept unless an accepted
/// detection on its line, near its CPA and of its shape suppresses it, or its residual halves
/// (known cells counted as zero) fall under `threshold`; a kept curve marks its cells known.
#[allow(clippy::too_many_arguments)]
fn accept_candidates(
    ls: &LogSpec,
    p: &DetectParams,
    tmpls: &[Template],
    (bs, bi): (&[f32], &[u16]),
    cand: &[usize],
    threshold: f64,
    known: &mut [bool],
    out: &mut Vec<Detection>,
) {
    let nt = ls.nt();
    let bpo = p.bins_per_octave as f64;
    for &k in cand {
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
            from_contrast: false,
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
        let (ra, rr) = residual_halves(ls, r, i, tm, known);
        if (ra.min(rr) as f64) < threshold {
            continue;
        }
        mark_detection_ridge(ls, p, &candidate, known);
        candidate.score = ra.min(rr) as f64;
        candidate.score_approach = ra as f64;
        candidate.score_recede = rr as f64;
        out.push(candidate);
    }
}

fn mark_detection_ridge(ls: &LogSpec, p: &DetectParams, d: &Detection, known: &mut [bool]) {
    let (nt, nr) = (ls.nt(), ls.nr());
    let (j0, j1) = (
        ls.t.partition_point(|&t| t < d.t0r - d.half_s),
        ls.t.partition_point(|&t| t <= d.t0r + d.half_s),
    );
    for j in j0..j1 {
        let fr = d.f0 * cv_ratio_rel_t0r(ls.t[j] - d.t0r, d.v, d.d, p.c);
        let row = ((fr / ls.freqs[0]).log2() * p.bins_per_octave as f64).round() as i64;
        for r in (row - RIDGE_HALF_ROWS).max(0)..=(row + RIDGE_HALF_ROWS).min(nr as i64 - 1) {
            known[r as usize * nt + j] = true;
        }
    }
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
    group_harmonics_after(dets, p, 0);
}

fn group_harmonics_after(dets: &mut [Detection], p: &DetectParams, first: usize) {
    let n = dets.len();
    for j in first..n {
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

/// Coordinate-descent refinement of a detection on a finer linear spectrogram, over the path
/// half 3 d / v (24 to 240 s). A detection whose half is under 24 s (an edge template's, whose
/// CPA lies too near the recording's start or end for a 24 s path) keeps its own half.
pub fn refine(spec: &Spec, det: &Detection, c: f64) -> Detection {
    let mut best = det.clone();
    // Keep the original refinement path for every primary-bank seed. A faster seed must
    // be allowed to refine inside the same 0.8 c domain that measurement already accepts.
    let v_max = if det.v > PRIMARY_MAX_SPEED_MPS {
        0.8 * c
    } else {
        0.6 * c
    };
    let step = spec.dt().max(0.25);
    let fixed_half = (det.half_s < 24.0).then_some(det.half_s);
    let score = |t0r: f64, f0: f64, v: f64, d: f64| {
        let half = fixed_half.unwrap_or_else(|| (3.0 * d / v).clamp(24.0, 240.0));
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
            let v = (best.v + 2.0 * k as f64 * scale).clamp(20.0, v_max);
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
    best.half_s = fixed_half.unwrap_or_else(|| (3.0 * best.d / best.v).clamp(24.0, 240.0));
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

/// (f0, v, d, received CPA time t0r) of a short pass heard 9 s into [`edge_clip`]'s 30 s clip.
#[cfg(test)]
pub(crate) const EDGE_PASS: (f64, f64, f64, f64) = (600.0, 64.0, 700.0, 9.0);

/// `dur` s at 4 kHz of uniform noise carrying a pass (f0, v, d) at amplitude `amp` whose received
/// CPA is at `t0r`, and a flat tone (Hz, amplitude) sounding until `tone.2` s.
#[cfg(test)]
pub(crate) fn edge_clip(
    dur: f64,
    (f0, v, d, t0r): (f64, f64, f64, f64),
    amp: f64,
    tone: (f64, f64, f64),
) -> Vec<f32> {
    let (sr, c) = (4000.0, 343.0);
    let t0e = t0r - d / c;
    let (mut seed, mut phase) = (4242u64, 0.0f64);
    (0..(sr * dur) as usize)
        .map(|i| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let t = i as f64 / sr;
            phase += std::f64::consts::TAU * crate::physics::cv_curve(t, f0, v, d, t0e, c) / sr;
            let on = if t < tone.2 { tone.1 } else { 0.0 };
            let flat = on * (std::f64::consts::TAU * tone.0 * t).sin();
            ((2.0 * seed as f64 / u64::MAX as f64 - 1.0) + amp * phase.sin() + flat) as f32
        })
        .collect()
}

/// A subsonic source's exact retarded phase and inverse-range amplitude in stationary noise,
/// with a separate steady 250 Hz tone. The clock phases and speeds are not on the survey grid.
#[cfg(test)]
pub(crate) fn fast_pass_scene((f0, v, d, t0r): (f64, f64, f64, f64), amp: f64) -> Vec<f32> {
    let (sr, c) = (4000.0, 343.0);
    let t0e = t0r - d / c;
    let mut seed = 4242u64;
    (0..(180.0 * sr) as usize)
        .map(|i| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let t = i as f64 / sr;
            let tau = crate::physics::retarded_tau(t, t0e, v, d, c);
            let range = crate::physics::cv_radial(tau, v, d).0;
            let source = amp * d / range * (std::f64::consts::TAU * f0 * tau).sin();
            let noise = 0.3 * (2.0 * seed as f64 / u64::MAX as f64 - 1.0);
            (source + noise + 0.03 * (std::f64::consts::TAU * 250.0 * t).sin()) as f32
        })
        .collect()
}

#[cfg(test)]
pub(crate) const FAST_PASSES: [(f64, f64, f64, f64); 3] = [
    (95.0, 190.0, 4500.0, 75.13),
    (118.0, 215.0, 5000.0, 80.37),
    (83.0, 245.0, 3600.0, 85.61),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::cv_curve;

    #[test]
    fn short_recovery_tiles_preserve_interior_centres() {
        let windows = short_recovery_windows(10.0, 700.0);
        assert!(windows.iter().all(|&(lo, hi)| {
            lo >= 10.0 && hi <= 690.0f64.next_up() && hi > lo && hi <= lo + 120.0
        }));
        assert!(windows.windows(2).all(|w| w[0].1 <= w[1].0));
        // Every eligible half-second frame belongs to one tile. Refused full-bank
        // windows are not holes in this shorter bank, which tests different evidence.
        for k in 0..1400 {
            let t = 0.25 + 0.5 * f64::from(k);
            let expected = (10.0..=690.0).contains(&t);
            let count = windows
                .iter()
                .filter(|&&(lo, hi)| lo <= t && t < hi)
                .count();
            assert_eq!(count, usize::from(expected), "t={t}");
        }
        assert!(short_recovery_windows(10.0, 19.0).is_empty());
        assert_eq!(windows.first(), Some(&(10.0, 130.0)));
        assert_eq!(windows.last().unwrap().1, 690.0f64.next_up());
        let fractional = short_recovery_windows(10.0, 700.25);
        assert!(
            fractional
                .iter()
                .any(|&(lo, hi)| lo <= 690.25 && 690.25 < hi)
        );
    }

    #[test]
    fn short_recovery_reports_exhausted_original_limits() {
        let p = DetectParams::default();
        let x = vec![0.0; 180 * 4000];
        for (slots, allowance) in [(0, 400_000), (p.max_detections, 0)] {
            let (added, cut, incomplete) =
                recover_fast_passes_counted_with_owners(&x, 4000, &p, slots, allowance, &[], &[])
                    .unwrap();
            assert!(added.is_empty() && cut == 0);
            assert!(incomplete, "unvisited interior must be reported");
            for stage in [
                FastRecoveryStage::EdgeAndRefused,
                FastRecoveryStage::ShortInterior,
                FastRecoveryStage::OrdinaryShortInterior,
                FastRecoveryStage::OrdinaryEdges,
            ] {
                let batch = recover_fast_passes_stage_with_owners(
                    &x,
                    4000,
                    &p,
                    slots,
                    allowance,
                    &[],
                    &[],
                    stage,
                )
                .unwrap();
                assert!(batch.detections.is_empty() && batch.cut == 0);
                assert_eq!(batch.remaining_candidates, allowance);
                assert_eq!(
                    batch.interior_incomplete,
                    stage != FastRecoveryStage::EdgeAndRefused
                );
            }
        }
        let disabled = DetectParams {
            fast_templates: false,
            ..p
        };
        let (added, cut, incomplete) =
            recover_fast_passes_counted_with_owners(&x, 4000, &disabled, 0, 0, &[], &[]).unwrap();
        assert!(added.is_empty() && cut == 0 && !incomplete);
    }

    #[test]
    fn short_recovery_after_refused_legacy_measurement_finds_physical_pass() {
        let pass = (350.0, 215.0, 900.0, 80.13);
        let x = fast_pass_scene(pass, 0.32 * 10.0f64.powf(-3.0 / 20.0));
        let p = DetectParams::default();
        let (ls, lin) = survey(&x, 4000, &p).unwrap();
        let (ordinary, cut, budget) = detect_counted_with_budget(&ls, &p);
        assert_eq!(cut, 0);
        let events = measure_edge_families(&x, &lin, &ordinary);
        assert!(events.iter().all(|e| !e.is_aircraft_like()));
        let refused: Vec<_> = events.iter().map(|e| &ordinary[e.detection.id]).collect();
        let slots = p.max_detections - ordinary.len();

        // The prior combined path lets its full-length candidate own the short bank before
        // knowing that ordinary physical measurement will reject that candidate.
        let (combined, cut, incomplete) =
            recover_fast_passes_counted_with_owners(&x, 4000, &p, slots, budget, &[], &refused)
                .unwrap();
        assert_eq!(cut, 0);
        assert!(!incomplete);
        assert!(!combined.is_empty());
        let legacy_events = measure_edge_families(&x, &lin, &combined);
        assert!(legacy_events.iter().all(|e| !e.is_aircraft_like()));

        let legacy = recover_fast_passes_stage_with_owners(
            &x,
            4000,
            &p,
            slots,
            budget,
            &[],
            &refused,
            FastRecoveryStage::EdgeAndRefused,
        )
        .unwrap();
        assert_eq!(legacy.cut, 0);
        assert!(!legacy.interior_incomplete);
        assert!(legacy.remaining_candidates < budget);
        assert_eq!(
            serde_json::to_value(&legacy.detections).unwrap(),
            serde_json::to_value(&combined).unwrap(),
            "the old short stage supplied no alternative behind its refused owner"
        );
        // Derive ownership from the actual measurement results, not from which seeds were
        // accepted by the detector. Failed seeds still spend their original slots and budget.
        let owners: Vec<_> = events
            .iter()
            .chain(&legacy_events)
            .filter(|e| e.is_aircraft_like())
            .map(|e| RecoveryOwner {
                trace: &e.trace,
                harmonics: Vec::new(),
            })
            .collect();
        let short = recover_fast_passes_stage_with_owners(
            &x,
            4000,
            &p,
            slots - legacy.detections.len(),
            legacy.remaining_candidates,
            &owners,
            &refused,
            FastRecoveryStage::ShortInterior,
        )
        .unwrap();
        assert_eq!(short.cut, 0);
        assert!(!short.interior_incomplete);
        assert!(short.remaining_candidates < legacy.remaining_candidates);
        assert!(
            ordinary.len() + legacy.detections.len() + short.detections.len() <= p.max_detections
        );
        assert!(
            short.detections.iter().all(|d| {
                d.half_s == p.edge_half_s && d.score >= p.thr_db.max(p.thr_silence_db)
            })
        );
        let measured = measure_edge_families(&x, &lin, &short.detections);
        let accepted: Vec<_> = measured.iter().filter(|e| e.is_aircraft_like()).collect();
        assert_eq!(accepted.len(), 1);
        assert_eq!(
            crate::search::independent_measured_events(&measured, 0.0).len(),
            1
        );
        let b = accepted[0].fit.as_ref().unwrap().best;
        eprintln!("short recovery after refused legacy measurement: {b:?}");
        assert!((b.f0 - pass.0).abs() < 0.3, "{b:?}");
        assert!((b.v - pass.1).abs() < 4.0, "{b:?}");
        assert!((b.d / pass.2 - 1.0).abs() < 0.08, "{b:?}");
        assert!((b.t0r - pass.3).abs() < 0.3, "{b:?}");

        let owned = recover_fast_passes_stage_with_owners(
            &x,
            4000,
            &p,
            slots - legacy.detections.len(),
            legacy.remaining_candidates,
            &[RecoveryOwner {
                trace: &accepted[0].trace,
                harmonics: Vec::new(),
            }],
            &refused,
            FastRecoveryStage::ShortInterior,
        )
        .unwrap();
        assert_eq!(owned.cut, 0);
        assert!(!owned.interior_incomplete);
        assert!(
            owned.detections.is_empty(),
            "accepted observed ridge must retain ownership"
        );
    }

    #[test]
    fn short_interior_fast_recovery_finds_physical_passes() {
        let p = DetectParams::default();
        let pass = (83.0, 245.0, 700.0, 79.37);
        let x = fast_pass_scene(pass, 0.32);
        let (ls, lin) = survey(&x, 4000, &p).unwrap();
        let (ordinary, cut, budget) = detect_counted_with_budget(&ls, &p);
        assert_eq!(cut, 0);
        let events = measure_edge_families(&x, &lin, &ordinary);
        assert!(
            events.iter().all(|e| !e.is_aircraft_like()),
            "the original survey must miss this pass"
        );
        let refused: Vec<_> = events.iter().map(|e| &ordinary[e.detection.id]).collect();
        let slots = p.max_detections - ordinary.len();
        let (old, _, _) =
            recover_fast_passes_impl(&x, 4000, &p, slots, budget, &[], &refused, false).unwrap();
        let old_measured = measure_edge_families(&x, &lin, &old);
        assert!(
            old_measured.iter().all(|e| !e.is_aircraft_like()),
            "edge and full-length recovery must still miss this pass"
        );
        let (added, cut, incomplete) =
            recover_fast_passes_counted_with_owners(&x, 4000, &p, slots, budget, &[], &refused)
                .unwrap();
        assert_eq!(cut, 0);
        assert!(!incomplete);
        assert!(added.len() > old.len());
        for seed in &added[old.len()..] {
            assert_eq!(seed.half_s, p.edge_half_s);
            assert!(seed.v > PRIMARY_MAX_SPEED_MPS && seed.score >= p.thr_db);
        }
        let measured = measure_edge_families(&x, &lin, &added);
        let accepted: Vec<_> = measured.iter().filter(|e| e.is_aircraft_like()).collect();
        eprintln!(
            "short interior recovery {pass:?}: {:?}",
            measured
                .iter()
                .map(|e| (&e.quality, e.fit.as_ref().map(|f| f.best)))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            crate::search::independent_measured_events(&measured, 0.0).len(),
            1
        );
        assert_eq!(accepted.len(), 1);
        for event in accepted {
            let b = event.fit.as_ref().unwrap().best;
            assert!((b.f0 - pass.0).abs() < 0.3, "{b:?}");
            assert!((b.v - pass.1).abs() < 4.0, "{b:?}");
            assert!((b.d / pass.2 - 1.0).abs() < 0.08, "{b:?}");
            assert!((b.t0r - pass.3).abs() < 0.3, "{b:?}");
        }
    }

    /// Exercise the production recovery order with measurement between stages, as the
    /// recording pipeline does. Only accepted observed ridges own later evidence.
    fn measured_recovery_stage(
        x: &[f32],
        lin: &Spec,
        p: &DetectParams,
        detections: &mut Vec<Detection>,
        events: &mut Vec<crate::measure::MeasuredEvent>,
        budget: usize,
        stage: FastRecoveryStage,
    ) -> usize {
        let owners: Vec<_> = events
            .iter()
            .filter(|event| event.is_aircraft_like())
            .map(|event| {
                let measured = &detections[event.detection.id];
                let family = measured.harmonic_of.unwrap_or(measured.id);
                RecoveryOwner {
                    trace: &event.trace,
                    harmonics: detections
                        .iter()
                        .filter(|d| {
                            d.id != measured.id && (d.id == family || d.harmonic_of == Some(family))
                        })
                        .map(|d| d.f0 / measured.f0)
                        .collect(),
                }
            })
            .collect();
        let refused: Vec<_> = events
            .iter()
            .filter(|event| !event.is_aircraft_like())
            .map(|event| &detections[event.detection.id])
            .collect();
        let mut batch = recover_fast_passes_stage_with_owners(
            x,
            4000,
            p,
            p.max_detections - detections.len(),
            budget,
            &owners,
            &refused,
            stage,
        )
        .unwrap();
        assert_eq!(batch.cut, 0);
        assert!(!batch.interior_incomplete);
        assert!(batch.remaining_candidates <= budget);
        let first = detections.len();
        for detection in &mut batch.detections {
            detection.id += first;
            detection.harmonic_of = detection.harmonic_of.map(|id| id + first);
        }
        events.extend(measure_edge_families(x, lin, &batch.detections));
        detections.extend(batch.detections);
        assert!(detections.len() <= p.max_detections);
        batch.remaining_candidates
    }

    #[test]
    fn ordinary_speed_short_interior_pass_reaches_measurement() {
        // A close ordinary-speed aircraft can have a briefer, steeper transition
        // than a distant fast one. Synthesize its actual emission phase and 1/r
        // amplitude; a short survey must still earn the unchanged physical gates.
        let pass = (450.0, 88.0, 250.0, 60.13);
        let mut x = fast_pass_scene(pass, 0.08);
        x.truncate(120 * 4000);
        let p = DetectParams::default();
        let (ls, lin) = survey(&x, 4000, &p).unwrap();
        let (mut detections, cut, mut budget) = detect_counted_with_budget(&ls, &p);
        assert_eq!(cut, 0);
        let mut events = measure_edge_families(&x, &lin, &detections);
        for stage in [
            FastRecoveryStage::EdgeAndRefused,
            FastRecoveryStage::ShortInterior,
        ] {
            budget =
                measured_recovery_stage(&x, &lin, &p, &mut detections, &mut events, budget, stage);
        }
        assert!(crate::search::independent_measured_events(&events, 0.0).is_empty());
        budget = measured_recovery_stage(
            &x,
            &lin,
            &p,
            &mut detections,
            &mut events,
            budget,
            FastRecoveryStage::OrdinaryShortInterior,
        );
        eprintln!("ordinary-speed short detections: {detections:?}");
        let diagnostic: Vec<_> = events
            .iter()
            .map(|event| (&event.quality, event.fit.as_ref().map(|fit| fit.best)))
            .collect();
        let independent = crate::search::independent_measured_events(&events, 0.0);
        assert_eq!(independent.len(), 1, "{diagnostic:?}");
        let event = events
            .iter()
            .find(|event| event.id == independent[0].id)
            .unwrap();
        let best = event.fit.as_ref().unwrap().best;
        assert!((best.f0 - pass.0).abs() < 0.3, "{best:?}");
        assert!((best.v - pass.1).abs() < 4.0, "{best:?}");
        assert!((best.d / pass.2 - 1.0).abs() < 0.08, "{best:?}");
        assert!((best.t0r - pass.3).abs() < 0.3, "{best:?}");

        // An accepted observed ridge owns its sound. Repeating this final stage cannot
        // turn one pass into two, or change the earlier detections and their family IDs.
        let count = detections.len();
        let earlier = serde_json::to_value(&detections).unwrap();
        measured_recovery_stage(
            &x,
            &lin,
            &p,
            &mut detections,
            &mut events,
            budget,
            FastRecoveryStage::OrdinaryShortInterior,
        );
        assert_eq!(serde_json::to_value(&detections[..count]).unwrap(), earlier);
        assert_eq!(
            crate::search::independent_measured_events(&events, 0.0).len(),
            1
        );
    }

    #[test]
    fn ordinary_short_recovery_supplies_a_missing_second_aircraft() {
        let pass = (450.0, 88.0, 250.0, 60.13);
        let original = (118.0, 90.0, 900.0, 35.13);
        let mut x = fast_pass_scene(pass, 0.08);
        x.truncate(120 * 4000);
        for (i, sample) in x.iter_mut().enumerate() {
            let t = i as f64 / 4000.0;
            let tau = crate::physics::retarded_tau(
                t,
                original.3 - original.2 / 343.0,
                original.1,
                original.2,
                343.0,
            );
            let range = crate::physics::cv_radial(tau, original.1, original.2).0;
            *sample += (0.16 * original.2 / range
                * (std::f64::consts::TAU * original.0 * tau).sin()) as f32;
        }
        let p = DetectParams::default();
        let (ls, lin) = survey(&x, 4000, &p).unwrap();
        let (mut detections, cut, mut budget) = detect_counted_with_budget(&ls, &p);
        assert_eq!(cut, 0);
        let mut events = measure_edge_families(&x, &lin, &detections);
        for stage in [
            FastRecoveryStage::EdgeAndRefused,
            FastRecoveryStage::ShortInterior,
        ] {
            budget =
                measured_recovery_stage(&x, &lin, &p, &mut detections, &mut events, budget, stage);
        }
        let before = crate::search::independent_measured_events(&events, 0.0);
        assert_eq!(before.len(), 1, "{events:?}");
        let existing = events.iter().find(|e| e.id == before[0].id).unwrap();
        assert!((existing.fit.as_ref().unwrap().best.f0 - original.0).abs() < 0.3);
        let (detection_count, event_count) = (detections.len(), events.len());
        let original_detections = serde_json::to_value(&detections).unwrap();
        let original_events = serde_json::to_value(&events).unwrap();
        measured_recovery_stage(
            &x,
            &lin,
            &p,
            &mut detections,
            &mut events,
            budget,
            FastRecoveryStage::OrdinaryShortInterior,
        );
        let independent = crate::search::independent_measured_events(&events, 0.0);
        assert_eq!(independent.len(), 2, "{events:?}");
        assert_eq!(
            serde_json::to_value(&detections[..detection_count]).unwrap(),
            original_detections
        );
        assert_eq!(
            serde_json::to_value(&events[..event_count]).unwrap(),
            original_events
        );
        let recovered = events[event_count..]
            .iter()
            .find(|event| independent.iter().any(|e| e.id == event.id))
            .unwrap();
        let best = recovered.fit.as_ref().unwrap().best;
        assert!((best.f0 - pass.0).abs() < 0.3, "{best:?}");
        assert!((best.v - pass.1).abs() < 4.0, "{best:?}");
        assert!((best.d / pass.2 - 1.0).abs() < 0.08, "{best:?}");
        assert!((best.t0r - pass.3).abs() < 0.3, "{best:?}");
    }

    #[test]
    fn ordinary_short_edges_recover_previously_unmeasured_physical_passes() {
        // These CPAs leave enough sound for the unchanged eight-second measurement gate,
        // but not the complete ten-second halves required by the short interior bank.
        let p = DetectParams::default();
        for t0r in [9.13, 110.87] {
            let pass = (450.0, 88.0, 250.0, t0r);
            let mut recovered = 0;
            for quieter_db in (0..=12).step_by(3) {
                let amplitude = 0.32 * 10.0f64.powf(-(quieter_db as f64) / 20.0);
                let mut x = fast_pass_scene(pass, amplitude);
                x.truncate(120 * 4000);
                let (ls, lin) = survey(&x, 4000, &p).unwrap();
                let (mut detections, cut, mut budget) = detect_counted_with_budget(&ls, &p);
                assert_eq!(cut, 0);
                let mut events = measure_edge_families(&x, &lin, &detections);
                for stage in [
                    FastRecoveryStage::EdgeAndRefused,
                    FastRecoveryStage::ShortInterior,
                    FastRecoveryStage::OrdinaryShortInterior,
                ] {
                    if !crate::search::independent_measured_events(&events, 0.0).is_empty() {
                        break;
                    }
                    budget = measured_recovery_stage(
                        &x, &lin, &p, &mut detections, &mut events, budget, stage,
                    );
                }
                // Only a previously unmeasurable pass demonstrates this additional capability.
                if !crate::search::independent_measured_events(&events, 0.0).is_empty() {
                    continue;
                }
                let first_detection = detections.len();
                let first_event = events.len();
                let earlier_detections = serde_json::to_value(&detections).unwrap();
                let earlier_events = serde_json::to_value(&events).unwrap();
                budget = measured_recovery_stage(
                    &x,
                    &lin,
                    &p,
                    &mut detections,
                    &mut events,
                    budget,
                    FastRecoveryStage::OrdinaryEdges,
                );
                assert_eq!(
                    serde_json::to_value(&detections[..first_detection]).unwrap(),
                    earlier_detections
                );
                assert_eq!(
                    serde_json::to_value(&events[..first_event]).unwrap(),
                    earlier_events
                );
                assert!(detections[first_detection..].iter().all(|d| {
                    d.v <= PRIMARY_MAX_SPEED_MPS
                        && d.half_s == p.edge_half_s
                        && d.score >= p.thr_db.max(p.thr_silence_db)
                }));
                let independent = crate::search::independent_measured_events(&events, 0.0);
                let diagnostics: Vec<_> = events[first_event..]
                    .iter()
                    .map(|e| (&e.quality, e.fit.as_ref().map(|f| f.best)))
                    .collect();
                assert!(independent.len() <= 1, "{t0r}/{quieter_db}: {diagnostics:?}");
                for event in events[first_event..]
                    .iter()
                    .filter(|event| event.is_aircraft_like())
                {
                    let best = event.fit.as_ref().unwrap().best;
                    assert!(
                        (best.f0 - pass.0).abs() < 0.3,
                        "{t0r}/{quieter_db}: {best:?}"
                    );
                    assert!(
                        (best.v - pass.1).abs() < 4.0,
                        "{t0r}/{quieter_db}: {best:?}"
                    );
                    assert!(
                        (best.d / pass.2 - 1.0).abs() < 0.08,
                        "{t0r}/{quieter_db}: {best:?}"
                    );
                    assert!(
                        (best.t0r - pass.3).abs() < 0.3,
                        "{t0r}/{quieter_db}: {best:?}"
                    );
                }
                if independent.is_empty() {
                    continue;
                }
                recovered += 1;
                // A second scan cannot turn this accepted ridge into another aircraft.
                if recovered == 1 {
                    measured_recovery_stage(
                        &x,
                        &lin,
                        &p,
                        &mut detections,
                        &mut events,
                        budget,
                        FastRecoveryStage::OrdinaryEdges,
                    );
                    assert_eq!(
                        crate::search::independent_measured_events(&events, 0.0).len(),
                        1,
                        "an accepted edge ridge cannot become another aircraft"
                    );
                }
            }
            assert!(
                recovered > 0,
                "no newly measured ordinary-speed edge pass at {t0r}"
            );
        }
    }

    #[test]
    fn ordinary_short_edges_refuse_noise_tones_and_one_sided_sound() {
        let p = DetectParams {
            fast_templates: false,
            ..Default::default()
        };
        let pass = (450.0, 88.0, 250.0, 9.13);
        let mut noise = fast_pass_scene(pass, 0.0);
        noise.truncate(120 * 4000);
        let mut physical = fast_pass_scene(pass, 0.32);
        physical.truncate(noise.len());
        let mut steady = noise.clone();
        for (i, sample) in steady.iter_mut().enumerate() {
            *sample +=
                (0.32 * (std::f64::consts::TAU * pass.0 * i as f64 / 4000.0).sin()) as f32;
        }
        let split = (pass.3 * 4000.0) as usize;
        let mut no_approach = physical.clone();
        no_approach[..split].copy_from_slice(&noise[..split]);
        let mut no_recede = physical.clone();
        no_recede[split..].copy_from_slice(&noise[split..]);
        physical.reverse();
        for (label, x) in [
            ("noise and hum", noise),
            ("steady tone", steady),
            ("no approach", no_approach),
            ("no recede", no_recede),
            ("reversed pass", physical),
        ] {
            let (_, lin) = survey(&x, 4000, &p).unwrap();
            let batch = recover_fast_passes_stage_with_owners(
                &x,
                4000,
                &p,
                p.max_detections,
                400_000,
                &[],
                &[],
                FastRecoveryStage::OrdinaryEdges,
            )
            .unwrap();
            assert_eq!(batch.cut, 0);
            assert!(!batch.interior_incomplete);
            let events = measure_edge_families(&x, &lin, &batch.detections);
            assert!(
                events.iter().all(|event| !event.is_aircraft_like()),
                "{label}: {events:?}"
            );
            assert!(
                crate::search::independent_measured_events(&events, 0.0).is_empty(),
                "{label}"
            );
        }
    }

    #[test]
    fn ordinary_short_recovery_requires_observed_physical_motion() {
        let pass = (450.0, 88.0, 250.0, 60.13);
        let p = DetectParams::default();
        let mut noise = fast_pass_scene(pass, 0.0);
        noise.truncate(120 * 4000);
        let mut physical = fast_pass_scene(pass, 0.08);
        physical.truncate(noise.len());
        let mut steady = noise.clone();
        for (i, sample) in steady.iter_mut().enumerate() {
            *sample += (0.08 * (std::f64::consts::TAU * pass.0 * i as f64 / 4000.0).sin()) as f32;
        }
        let mut missing_drop = physical.clone();
        let a = ((pass.3 - 10.0) * 4000.0) as usize;
        let b = ((pass.3 + 10.0) * 4000.0) as usize;
        missing_drop[a..b].copy_from_slice(&noise[a..b]);
        physical.reverse();
        for (label, x) in [
            ("noise and hum", noise),
            ("steady tone", steady),
            ("missing transition", missing_drop),
            ("reversed pass", physical),
        ] {
            let (_, lin) = survey(&x, 4000, &p).unwrap();
            let batch = recover_fast_passes_stage_with_owners(
                &x,
                4000,
                &p,
                p.max_detections,
                400_000,
                &[],
                &[],
                FastRecoveryStage::OrdinaryShortInterior,
            )
            .unwrap();
            assert_eq!(batch.cut, 0);
            assert!(!batch.interior_incomplete);
            let events = measure_edge_families(&x, &lin, &batch.detections);
            assert!(
                events.iter().all(|event| !event.is_aircraft_like()),
                "{label}"
            );
            assert!(
                crate::search::independent_measured_events(&events, 0.0).is_empty(),
                "{label}"
            );
        }
    }

    #[test]
    fn short_interior_fast_recovery_keeps_harmonics_as_one_aircraft() {
        let pass = (83.0, 245.0, 700.0, 79.37);
        let mut x = fast_pass_scene(pass, 0.32);
        for (i, sample) in x.iter_mut().enumerate() {
            let t = i as f64 / 4000.0;
            let tau =
                crate::physics::retarded_tau(t, pass.3 - pass.2 / 343.0, pass.1, pass.2, 343.0);
            let range = crate::physics::cv_radial(tau, pass.1, pass.2).0;
            *sample += (0.16 * pass.2 / range
                * (std::f64::consts::TAU * (2.0 * pass.0) * tau).sin())
                as f32;
        }
        let p = DetectParams::default();
        let (ls, lin) = survey(&x, 4000, &p).unwrap();
        let (detections, cut, budget) = detect_counted_with_budget(&ls, &p);
        assert_eq!(cut, 0);
        let mut events = measure_edge_families(&x, &lin, &detections);
        let owners: Vec<_> = events
            .iter()
            .filter(|e| e.is_aircraft_like())
            .map(|e| {
                let measured = &detections[e.detection.id];
                let family = measured.harmonic_of.unwrap_or(measured.id);
                RecoveryOwner {
                    trace: &e.trace,
                    harmonics: detections
                        .iter()
                        .filter(|d| {
                            d.id != measured.id && (d.id == family || d.harmonic_of == Some(family))
                        })
                        .map(|d| d.f0 / measured.f0)
                        .collect(),
                }
            })
            .collect();
        let refused: Vec<_> = events
            .iter()
            .filter(|e| !e.is_aircraft_like())
            .map(|e| &detections[e.detection.id])
            .collect();
        let (added, cut, incomplete) = recover_fast_passes_counted_with_owners(
            &x,
            4000,
            &p,
            p.max_detections - detections.len(),
            budget,
            &owners,
            &refused,
        )
        .unwrap();
        assert_eq!(cut, 0);
        assert!(!incomplete);
        let mut measured = measure_edge_families(&x, &lin, &added);
        for (rank, event) in measured.iter_mut().enumerate() {
            event.id = format!("recovered-{rank}");
        }
        events.extend(measured);
        let independent = crate::search::independent_measured_events(&events, 0.0);
        assert_eq!(
            independent.len(),
            1,
            "one physical aircraft supplied {} independent events: {events:?}",
            independent.len()
        );
    }

    #[test]
    fn recovery_ownership_uses_observations_and_preserves_unobserved_gaps() {
        let ls = LogSpec {
            t: (0..21).map(|i| i as f64 * 0.5).collect(),
            freqs: (0..240)
                .map(|i| 35.0 * 2.0f64.powf(i as f64 / 48.0))
                .collect(),
            data: vec![1.0; 240 * 21],
            bins_per_octave: 48,
            hop_s: 0.5,
            scale: 1.0,
            gain: 1.0,
            contrast: None,
        };
        let trace = crate::trace::Trace {
            t: vec![2.0, 2.5, 3.0, 7.0, 7.5],
            f: vec![100.0; 5],
            snr: vec![10.0; 5],
        };
        let mut known = vec![false; ls.data.len()];
        mark_observed_ridges(
            &ls,
            &[RecoveryOwner {
                trace: &trace,
                harmonics: vec![2.0],
            }],
            &mut known,
        );
        let owned = |time: usize, frequency: f64| {
            let row =
                ((frequency / ls.freqs[0]).log2() * ls.bins_per_octave as f64).round() as usize;
            known[row * ls.nt() + time]
        };
        for frame in 0..ls.nt() {
            let in_stretch = (4..=6).contains(&frame) || (14..=15).contains(&frame);
            assert_eq!(owned(frame, 100.0), in_stretch);
            assert_eq!(owned(frame, 200.0), in_stretch);
            assert!(!owned(frame, 150.0));
            assert!(!owned(frame, 300.0));
        }
        let mut residual = ls.data.clone();
        zero_known(&mut residual, &known);
        for (value, owned) in residual.iter().zip(known) {
            assert_eq!(*value, if owned { 0.0 } else { 1.0 });
        }
    }

    #[test]
    fn fast_templates_respect_the_existing_subsonic_measurement_bound() {
        let off = DetectParams {
            fast_templates: false,
            ..Default::default()
        };
        assert!(build_fast_templates(&off, 1.0).is_empty());
        assert!(build_fast_edge_templates(&off, 1.0).is_empty());
        for (c, empty) in [
            (200.0, true),
            (300.0, false),
            (343.0, false),
            (370.0, false),
        ] {
            let p = DetectParams {
                c,
                ..Default::default()
            };
            let fast = build_fast_templates(&p, 1.0);
            let edge = build_fast_edge_templates(&p, 1.0);
            assert_eq!(p.v_grid, PRIMARY_SPEED_GRID);
            assert!(
                fast.iter()
                    .all(|t| t.v > PRIMARY_MAX_SPEED_MPS && t.v <= 0.8 * c)
            );
            assert_eq!(fast.is_empty(), empty);
            assert_eq!(edge.is_empty(), empty);
            assert!(
                edge.iter()
                    .all(|t| t.v > PRIMARY_MAX_SPEED_MPS && t.v <= 0.8 * c)
            );
            for half in [0.0, p.min_half_s] {
                assert!(
                    build_fast_edge_templates(
                        &DetectParams {
                            edge_half_s: half,
                            ..p.clone()
                        },
                        1.0
                    )
                    .is_empty()
                );
            }
        }
    }

    fn measure_edge_families(
        x: &[f32],
        survey: &Spec,
        detections: &[Detection],
    ) -> Vec<crate::measure::MeasuredEvent> {
        let p = crate::measure::MeasureParams::default();
        detections
            .iter()
            .filter(|d| d.harmonic_of.is_none())
            .map(|d| {
                crate::measure::measure_family(
                    x,
                    4000,
                    survey,
                    d,
                    detections,
                    &format!("edge-{}", d.id),
                    &p,
                )
            })
            .collect()
    }

    #[test]
    fn fast_edge_recovery_windows_exclude_crop_seams() {
        let scene = |offset, frames| LogSpec {
            t: (0..frames)
                .map(|i| offset + 0.25 + 0.5 * i as f64)
                .collect(),
            freqs: Vec::new(),
            data: Vec::new(),
            bins_per_octave: 48,
            hop_s: 0.5,
            scale: 1.0,
            gain: 1.0,
            contrast: None,
        };
        let head = scene(0.0, 96);
        let tail = scene(132.0, 96);
        assert_eq!(
            recovery_edge_windows(&head, true, false, 24.0),
            vec![(0, 48)]
        );
        assert_eq!(
            recovery_edge_windows(&tail, false, true, 24.0),
            vec![(48, 96)]
        );
        assert!(recovery_edge_windows(&head, false, false, 24.0).is_empty());
        assert_eq!(
            recovery_edge_windows(&scene(0.0, 60), true, true, 24.0),
            vec![(0, 60)]
        );
        // Overlapping edge crops use one survey, with the middle still ineligible.
        assert_eq!(
            recovery_edge_windows(&scene(0.0, 140), true, true, 24.0),
            vec![(0, 48), (92, 140)]
        );
    }

    #[test]
    fn fast_edge_recovery_restores_refused_passes_with_the_original_survey() {
        let p = DetectParams::default();
        for t0r in [19.37, 19.75] {
            let pass = (83.0, 245.0, 700.0, t0r);
            let mut x = fast_pass_scene(pass, 0.32);
            x.truncate(30 * 4000);
            let (ls, lin) = survey(&x, 4000, &p).unwrap();
            let (ordinary, cut, budget) = detect_counted_with_budget(&ls, &p);
            assert_eq!(cut, 0);
            assert!(budget > 0 && budget < 400_000);
            assert_eq!(
                serde_json::to_value((&ordinary, cut)).unwrap(),
                serde_json::to_value(detect_counted(&ls, &p)).unwrap()
            );
            let old = measure_edge_families(&x, &lin, &ordinary);
            assert!(old.iter().all(|event| !event.is_aircraft_like()));
            let slots = p.max_detections - ordinary.len();
            assert!(
                recover_fast_edges(&x, 4000, &p, 0, budget)
                    .unwrap()
                    .0
                    .is_empty()
            );
            assert!(
                recover_fast_edges(&x, 4000, &p, slots, 0)
                    .unwrap()
                    .0
                    .is_empty()
            );
            let (added, cut) = recover_fast_edges(&x, 4000, &p, slots, budget).unwrap();
            assert_eq!(cut, 0);
            assert!(!added.is_empty() && added.len() <= slots);
            for (id, seed) in added.iter().enumerate() {
                assert_eq!(seed.id, id);
                assert!(seed.harmonic_of.is_none_or(|parent| parent < id));
                assert!(seed.v > PRIMARY_MAX_SPEED_MPS && seed.v <= 0.8 * p.c);
                assert!(seed.score >= p.thr_db.max(p.thr_silence_db));
                assert_eq!(seed.half_s, p.edge_half_s);
                assert!(!seed.from_contrast);
            }
            // Refinement must use the ORIGINAL one-second linear image, exactly as locate
            // does; the new survey supplies coarse seeds, never different measurement gates.
            let mut events = old;
            events.extend(measure_edge_families(&x, &lin, &added));
            let independent = crate::search::independent_measured_events(&events, 0.0);
            assert_eq!(independent.len(), 1, "{t0r}: {added:?}");
            let event = events
                .iter()
                .find(|event| event.is_aircraft_like())
                .unwrap();
            let b = event.fit.as_ref().unwrap().best;
            eprintln!("half-second fast edge {t0r}: {b:?}; {:?}", event.quality);
            assert!((b.f0 - pass.0).abs() < 0.3, "{b:?}");
            assert!((b.v - pass.1).abs() < 4.0, "{b:?}");
            assert!((b.d / pass.2 - 1.0).abs() < 0.08, "{b:?}");
            assert!((b.t0r - pass.3).abs() < 0.3, "{b:?}");
            let limited = recover_fast_edges(&x, 4000, &p, 1, 1).unwrap();
            assert!(limited.0.len() <= 1 && limited.1 == 0);
        }
    }

    #[test]
    fn interior_fast_recovery_revisits_refused_detections() {
        let p = DetectParams::default();
        let mut recovered = 0;
        for pass in [(83.0, 245.0, 700.0, 79.37), (118.0, 215.0, 900.0, 80.13)] {
            for quieter_db in (0..=12).step_by(3) {
                let x = fast_pass_scene(pass, 0.32 * 10.0f64.powf(-f64::from(quieter_db) / 20.0));
                let (ls, lin) = survey(&x, 4000, &p).unwrap();
                let (ordinary, cut, budget) = detect_counted_with_budget(&ls, &p);
                assert_eq!(cut, 0);
                let events = measure_edge_families(&x, &lin, &ordinary);
                let accepted = crate::search::independent_measured_events(&events, 0.0).len();
                let refused: Vec<_> = events
                    .iter()
                    .filter(|event| !event.is_aircraft_like())
                    .map(|event| &ordinary[event.detection.id])
                    .collect();
                let owners: Vec<_> = events
                    .iter()
                    .filter(|event| event.is_aircraft_like())
                    .map(|event| RecoveryOwner {
                        trace: &event.trace,
                        harmonics: Vec::new(),
                    })
                    .collect();
                let slots = p.max_detections - ordinary.len();
                let (new, recovery_cut) =
                    recover_fast_passes_with_owners(&x, 4000, &p, slots, budget, &owners, &refused)
                        .unwrap();
                assert_eq!(recovery_cut, 0);
                let mut added = measure_edge_families(&x, &lin, &new);
                for (rank, event) in added.iter_mut().enumerate() {
                    event.id = format!("interior-{rank}");
                }
                eprintln!(
                    "interior fast {pass:?}, {quieter_db} dB: ordinary {} accepted {accepted}, refused {}, new {new:?}; measurements {:?}",
                    ordinary.len(),
                    refused.len(),
                    added
                        .iter()
                        .map(|e| (&e.quality, e.fit.as_ref().map(|f| f.best)))
                        .collect::<Vec<_>>()
                );
                for event in added.iter().filter(|e| e.is_aircraft_like()) {
                    let b = event.fit.as_ref().unwrap().best;
                    assert!((b.f0 - pass.0).abs() < 0.3, "{b:?}");
                    assert!((b.v - pass.1).abs() < 4.0, "{b:?}");
                    assert!((b.d / pass.2 - 1.0).abs() < 0.08, "{b:?}");
                    assert!((b.t0r - pass.3).abs() < 0.3, "{b:?}");
                }
                let mut all = events;
                all.extend(added);
                let independent = crate::search::independent_measured_events(&all, 0.0).len();
                assert!(
                    independent <= 1,
                    "one physical pass supplied {independent} events"
                );
                if accepted == 0 && independent == 1 {
                    recovered += 1;
                }
            }
        }
        assert!(
            recovered > 0,
            "no previously refused interior pass recovered"
        );
    }

    #[test]
    fn interior_fast_recovery_supplies_a_second_independent_aircraft() {
        check_interior_second_aircraft((118.0, 215.0, 900.0, 80.13), false);
    }

    #[test]
    fn short_interior_fast_recovery_supplies_a_second_independent_aircraft() {
        check_interior_second_aircraft((83.0, 245.0, 700.0, 79.37), true);
    }

    fn check_interior_second_aircraft(fast: (f64, f64, f64, f64), needs_short_bank: bool) {
        let ordinary = (450.0, 90.0, 900.0, 40.13);
        let ordinary_amplitude = if needs_short_bank { 0.04 } else { 0.08 };
        let mut x = fast_pass_scene(fast, 0.32);
        for (i, sample) in x.iter_mut().enumerate() {
            let t = i as f64 / 4000.0;
            let tau = crate::physics::retarded_tau(
                t,
                ordinary.3 - ordinary.2 / 343.0,
                ordinary.1,
                ordinary.2,
                343.0,
            );
            let range = crate::physics::cv_radial(tau, ordinary.1, ordinary.2).0;
            *sample += (ordinary_amplitude * ordinary.2 / range
                * (std::f64::consts::TAU * ordinary.0 * tau).sin()) as f32;
        }
        let p = DetectParams::default();
        let (ls, lin) = survey(&x, 4000, &p).unwrap();
        let (detections, cut, budget) = detect_counted_with_budget(&ls, &p);
        assert_eq!(cut, 0);
        let mut events = measure_edge_families(&x, &lin, &detections);
        assert_eq!(
            crate::search::independent_measured_events(&events, 0.0).len(),
            1,
            "ordinary-only measured fits: {:?}",
            events
                .iter()
                .map(|e| (&e.quality, e.fit.as_ref().map(|f| f.best)))
                .collect::<Vec<_>>()
        );
        let owners: Vec<_> = events
            .iter()
            .filter(|event| event.is_aircraft_like())
            .map(|event| {
                assert!((event.fit.as_ref().unwrap().best.f0 - ordinary.0).abs() < 0.3);
                RecoveryOwner {
                    trace: &event.trace,
                    harmonics: Vec::new(),
                }
            })
            .collect();
        let refused: Vec<_> = events
            .iter()
            .filter(|event| !event.is_aircraft_like())
            .map(|event| &detections[event.detection.id])
            .collect();
        if needs_short_bank {
            let (old, _, _) = recover_fast_passes_impl(
                &x,
                4000,
                &p,
                p.max_detections - detections.len(),
                budget,
                &owners,
                &refused,
                false,
            )
            .unwrap();
            let old_events = measure_edge_families(&x, &lin, &old);
            assert!(
                old_events.iter().all(|e| !e.is_aircraft_like()),
                "legacy recovery already supplied the missing aircraft: {old_events:?}"
            );
        }
        let (added, cut, incomplete) = recover_fast_passes_counted_with_owners(
            &x,
            4000,
            &p,
            p.max_detections - detections.len(),
            budget,
            &owners,
            &refused,
        )
        .unwrap();
        assert_eq!(cut, 0);
        assert!(!incomplete);
        let mut recovered = measure_edge_families(&x, &lin, &added);
        for (rank, event) in recovered.iter_mut().enumerate() {
            event.id = format!("recovered-{rank}");
            if event.is_aircraft_like() {
                let b = event.fit.as_ref().unwrap().best;
                eprintln!("interior second aircraft: {b:?}");
                assert!((b.f0 - fast.0).abs() < 0.3, "{b:?}");
                assert!((b.v - fast.1).abs() < 4.0, "{b:?}");
                assert!((b.d / fast.2 - 1.0).abs() < 0.08, "{b:?}");
                assert!((b.t0r - fast.3).abs() < 0.3, "{b:?}");
            }
        }
        events.extend(recovered);
        let independent = crate::search::independent_measured_events(&events, 0.0);
        assert_eq!(independent.len(), 2, "{added:?}");

        let (lat, lon, start) = (40.876466, -73.524951, 1_788_895_000.0);
        let enu = fdop_types::geo::Enu::new(lat, lon, fdop_types::MIC_HEIGHT_M);
        let mut tracks = Vec::new();
        for (key, pass, northbound) in [("ordinary", ordinary, false), ("recovered", fast, true)] {
            let lateral = (pass.2 * pass.2 - 500.0f64.powi(2)).sqrt();
            let (position, velocity) = if northbound {
                ([lateral, 0.0, 500.0], [0.0, pass.1, 0.0])
            } else {
                ([0.0, lateral, 500.0], [pass.1, 0.0, 0.0])
            };
            let mut track = crate::track::straight_track(
                &enu,
                position,
                velocity,
                start + pass.3 - pass.2 / 343.0,
                start - 100.0,
                start + 280.0,
                2.0,
            );
            track.key = key.into();
            track.icao = key.into();
            tracks.push(track);
        }
        let dem = fdop_types::FlatElevation(0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(fdop_types::atmo::Medium::still(343.0));
        let located = crate::search::run(
            &crate::search::SearchInputs {
                events: independent,
                tracks: &tracks,
                heard_times: detections.iter().chain(&added).map(|d| d.t0r).collect(),
                coarse_dem: &dem,
                fine_dem: &dem,
                atmo: &atmo,
            },
            &crate::search::SearchParams::new(
                start,
                start,
                180.0,
                fdop_types::BBox::new(lat - 0.01, lon - 0.01, lat + 0.01, lon + 0.01),
            ),
            &|message| eprintln!("interior recovered search: {message}"),
        );
        let best = located
            .candidates
            .first()
            .expect("two measured passes must locate");
        for flight in ["ordinary", "recovered"] {
            assert!(
                best.events.iter().any(|e| e.flight == flight && e.core),
                "{best:?}"
            );
        }
        let refined = located
            .refine
            .as_ref()
            .expect("two-pass location must refine");
        let error = fdop_types::geo::haversine_m(lat, lon, refined.lat, refined.lon);
        eprintln!(
            "interior two-pass location: {error}m, clock={}s",
            refined.t_start_utc - start
        );
        assert!(error < 75.0, "{refined:?}");
        assert!((refined.t_start_utc - start).abs() < 0.2, "{refined:?}");
        assert!(located.funnel.truncated.is_empty());
    }

    #[test]
    fn interior_fast_recovery_respects_owners_limits_and_refusals() {
        let pass = (118.0, 215.0, 900.0, 80.13);
        let p = DetectParams::default();
        let noise = fast_pass_scene(pass, 0.0);
        let physical = fast_pass_scene(pass, 0.32);
        let (ls, original_survey) = survey(&physical, 4000, &p).unwrap();
        let (detections, _, budget) = detect_counted_with_budget(&ls, &p);
        let events = measure_edge_families(&physical, &original_survey, &detections);
        assert!(events.iter().all(|event| !event.is_aircraft_like()));
        let refused: Vec<_> = events
            .iter()
            .map(|event| &detections[event.detection.id])
            .collect();
        assert!(!refused.is_empty());
        let slots = p.max_detections - detections.len();
        for (room, allowance) in [(0, budget), (slots, 0)] {
            assert!(
                recover_fast_passes_with_owners(
                    &physical,
                    4000,
                    &p,
                    room,
                    allowance,
                    &[],
                    &refused,
                )
                .unwrap()
                .0
                .is_empty()
            );
        }
        let edge = recover_fast_edges_with_owners(&physical, 4000, &p, slots, budget, &[]).unwrap();
        let no_refusal =
            recover_fast_passes_with_owners(&physical, 4000, &p, slots, budget, &[], &[]).unwrap();
        assert_eq!(
            serde_json::to_value(&edge.0).unwrap(),
            serde_json::to_value(&no_refusal.0[..edge.0.len()]).unwrap(),
            "the seedless interior stage must preserve existing edge detections"
        );
        // Hold the physically detected, refused seeds fixed while removing or reversing
        // their sound. Even this prior guidance must not manufacture an aircraft from the
        // changed observations. Each changed recording supplies its own measurement spectrum.
        for mode in ["physical", "missing drop", "reversed", "noise and hum"] {
            let mut x = if mode == "noise and hum" {
                noise.clone()
            } else {
                physical.clone()
            };
            if mode == "missing drop" {
                let a = ((pass.3 - 12.0) * 4000.0) as usize;
                let b = ((pass.3 + 12.0) * 4000.0) as usize;
                x[a..b].copy_from_slice(&noise[a..b]);
            } else if mode == "reversed" {
                x.reverse();
            }
            let (_, lin) = survey(&x, 4000, &p).unwrap();
            let (added, _) =
                recover_fast_passes_with_owners(&x, 4000, &p, slots, budget, &[], &refused)
                    .unwrap();
            eprintln!(
                "interior control {mode}: measuring {} retry candidates",
                added.len()
            );
            let measured = measure_edge_families(&x, &lin, &added);
            eprintln!(
                "interior control {mode}: {} refused seeds, {} recovered candidates; {:?}",
                refused.len(),
                added.len(),
                measured.iter().map(|e| &e.quality).collect::<Vec<_>>()
            );
            if mode != "physical" {
                assert!(
                    measured.iter().all(|event| !event.is_aircraft_like()),
                    "{mode}"
                );
                continue;
            }
            assert!(!refused.is_empty());
            let accepted: Vec<_> = measured
                .iter()
                .filter(|event| event.is_aircraft_like())
                .collect();
            assert_eq!(accepted.len(), 1);
            let owners = [RecoveryOwner {
                trace: &accepted[0].trace,
                harmonics: Vec::new(),
            }];
            let (again, _) =
                recover_fast_passes_with_owners(&x, 4000, &p, slots, budget, &owners, &refused)
                    .unwrap();
            eprintln!(
                "interior owner control: measuring {} retry candidates",
                again.len()
            );
            let measured_again = measure_edge_families(&x, &lin, &again);
            assert!(
                measured_again.iter().all(|event| !event.is_aircraft_like()),
                "the already measured ridge manufactured another aircraft"
            );
        }
    }

    #[test]
    fn fast_edge_recovery_keeps_absolute_tail_coordinates() {
        let p = DetectParams::default();
        let pass = (83.0, 245.0, 700.0, 169.37);
        let x = fast_pass_scene(pass, 0.32);
        let (_, lin) = survey(&x, 4000, &p).unwrap();
        let (added, cut) = recover_fast_edges(&x, 4000, &p, p.max_detections, 400_000).unwrap();
        assert_eq!(cut, 0);
        assert!(!added.is_empty());
        assert!(added.iter().all(|seed| seed.t0r > 156.0));
        // The last 48 seconds have exactly the same image in local and recording time.
        // Compare every coarse field, including scores, IDs and harmonic references.
        let (mut local, local_cut) =
            recover_fast_edges(&x[132 * 4000..], 4000, &p, p.max_detections, 400_000).unwrap();
        for seed in &mut local {
            seed.t0r += 132.0;
        }
        assert_eq!(
            serde_json::to_value((&added, cut)).unwrap(),
            serde_json::to_value((&local, local_cut)).unwrap()
        );
        // The long recording's row background used to leave the measured trace on both
        // asymptotes without the central drop. Continue that fitted ridge through the unchanged
        // gates, while keeping the survey's absolute recording coordinates.
        let events = measure_edge_families(&x, &lin, &added);
        let independent = crate::search::independent_measured_events(&events, 0.0);
        assert_eq!(independent.len(), 1);
        let event = events
            .iter()
            .find(|event| event.is_aircraft_like())
            .unwrap();
        let b = event.fit.as_ref().unwrap().best;
        eprintln!("recovered absolute tail: {b:?}; {:?}", event.quality);
        assert!(event.quality.reseeded);
        assert!((b.f0 - pass.0).abs() < 0.3, "{b:?}");
        assert!((b.v - pass.1).abs() < 4.0, "{b:?}");
        assert!((b.d / pass.2 - 1.0).abs() < 0.08, "{b:?}");
        assert!((b.t0r - pass.3).abs() < 0.3, "{b:?}");
    }

    #[test]
    fn fast_edge_recovery_refuses_noise_tones_and_reversed_passes() {
        let pass = (83.0, 245.0, 700.0, 19.37);
        let mut noise = fast_pass_scene(pass, 0.0);
        noise.truncate(30 * 4000);
        let mut steady = noise.clone();
        for (i, sample) in steady.iter_mut().enumerate() {
            *sample += (0.32 * (std::f64::consts::TAU * 83.0 * i as f64 / 4000.0).sin()) as f32;
        }
        let mut reversed = fast_pass_scene(pass, 0.32);
        reversed.truncate(30 * 4000);
        reversed.reverse();
        let p = DetectParams::default();
        for (label, x) in [
            ("noise and hum", noise),
            ("steady tone", steady),
            ("reversed", reversed),
        ] {
            let (_, lin) = survey(&x, 4000, &p).unwrap();
            let (added, _) = recover_fast_edges(&x, 4000, &p, p.max_detections, 400_000).unwrap();
            let events = measure_edge_families(&x, &lin, &added);
            assert!(
                events.iter().all(|event| !event.is_aircraft_like()),
                "{label}: {added:?}"
            );
            assert!(
                crate::search::independent_measured_events(&events, 0.0).is_empty(),
                "{label}"
            );
        }
    }

    #[test]
    fn contrast_edges_require_support_between_the_bank_samples() {
        let (v, d) = (90.0, 900.0);
        for hop_s in [0.5, 1.0] {
            let p = DetectParams {
                v_grid: vec![v],
                d_grid: vec![d],
                fast_templates: false,
                hop_s,
                ..Default::default()
            };
            // Twenty seconds between the first and last frame covers the entire short
            // template, but cannot give a full template 60 % coverage on both sides.
            let nt = (20.0 / hop_s) as usize + 1;
            let nr = 240;
            let t: Vec<_> = (0..nt).map(|i| 0.5 + i as f64 * hop_s).collect();
            let freqs: Vec<_> = (0..nr)
                .map(|r| p.fmin * 2.0f64.powf(r as f64 / p.bins_per_octave as f64))
                .collect();
            let row = 150;
            let (f0, t0r) = (freqs[row], t[nt / 2]);
            let stride = (p.path_dt / hop_s).round() as usize;
            let scene = |only_bank_samples| {
                let mut contrast = vec![0.0; nr * nt];
                for (i, &time) in t.iter().enumerate() {
                    if only_bank_samples && !i.abs_diff(nt / 2).is_multiple_of(stride) {
                        continue;
                    }
                    let f = cv_curve(time, f0, v, d, t0r - d / p.c, p.c);
                    let r = ((f / freqs[0]).log2() * p.bins_per_octave as f64).round() as usize;
                    contrast[r * nt + i] = 2.1;
                }
                LogSpec {
                    t: t.clone(),
                    freqs: freqs.clone(),
                    data: vec![0.0; nr * nt],
                    bins_per_octave: p.bins_per_octave,
                    hop_s,
                    scale: 0.25,
                    gain: 2.0,
                    contrast: Some(contrast),
                }
            };
            let continuous_image = scene(false);
            let (continuous, _, remaining) = detect_counted_with_budget(&continuous_image, &p);
            assert_eq!(continuous.len(), 1, "hop {hop_s}: {continuous:?}");
            assert!(continuous[0].from_contrast);
            assert_eq!(continuous[0].half_s, p.edge_half_s);
            let (_, _, before_edge) = detect_with_budget_impl(&continuous_image, &p, false);
            assert!(
                remaining < before_edge,
                "contrast edges did not consume the shared budget"
            );

            // The two images agree at every sample of the winning sparse template. Its
            // score still clears the strong line, but the second image has no ridge between
            // those samples and must not become a short contrast detection.
            let sparse = scene(true);
            let (coarse, _) = score_bank(
                &sparse,
                &build_edge_templates(&p, hop_s),
                sparse.contrast.as_ref().unwrap(),
                &[(0, nt)],
            );
            assert!(coarse[row * nt + nt / 2] as f64 >= p.thr_db);
            let detections = detect(&sparse, &p);
            assert!(detections.is_empty(), "hop {hop_s}: {detections:?}");

            // A real curve inside the anchor's tone/clock cell must survive quantization.
            // Independent neighbouring peaks that alternate around the curve must not.
            let mut shifted = scene(true);
            let mut alternating = scene(true);
            for (i, &time) in t.iter().enumerate() {
                let f = cv_curve(
                    time,
                    f0 * 2.0f64.powf(0.25 / p.bins_per_octave as f64),
                    v,
                    d,
                    t0r + 0.25 * hop_s - d / p.c,
                    p.c,
                );
                let r = ((f / freqs[0]).log2() * p.bins_per_octave as f64).round() as usize;
                shifted.contrast.as_mut().unwrap()[r * nt + i] = 2.1;
                if !i.abs_diff(nt / 2).is_multiple_of(stride) {
                    let f = cv_curve(time, f0, v, d, t0r - d / p.c, p.c);
                    let r = ((f / freqs[0]).log2() * p.bins_per_octave as f64).round() as usize;
                    let r = if (i / stride).is_multiple_of(2) {
                        r + 1
                    } else {
                        r - 1
                    };
                    alternating.contrast.as_mut().unwrap()[r * nt + i] = 2.1;
                }
            }
            let recovered = detect(&shifted, &p);
            assert!(
                !recovered.is_empty(),
                "hop {hop_s}: lost the coherent shift"
            );
            assert!(
                recovered
                    .iter()
                    .all(|d| d.from_contrast && d.half_s == p.edge_half_s)
            );
            let spurious = detect(&alternating, &p);
            assert!(spurious.is_empty(), "hop {hop_s}: {spurious:?}");
        }
    }

    #[test]
    fn contrast_edge_passes_recover_after_every_existing_stage() {
        let p = DetectParams::default();
        let (f0, v, d, t0r) = SWELL_PASS;
        // These 30 s crops have no cell with the full bank's required coverage on both
        // sides. The faint pass is visible only after contrast-first normalization.
        for start in [3, 13] {
            for swell in [20.0, 25.0] {
                let source = swell_scene(0.08, swell);
                let x = &source[start * 4000..(start + 30) * 4000];
                let (ls, lin) = survey(x, 4000, &p).unwrap();
                assert!(ls.contrast.is_some());
                let (old, old_cut) = detect_counted_impl(&ls, &p, false);
                assert!(old.is_empty(), "{start}/{swell}: {old:?}");
                let (new, cut) = detect_counted(&ls, &p);
                assert_eq!(old_cut, 0);
                assert_eq!(cut, 0);
                let events = measure_edge_families(x, &lin, &new);
                let independent = crate::search::independent_measured_events(&events, 0.0);
                assert_eq!(independent.len(), 1, "{start}/{swell}: {new:?}");
                for event in events.iter().filter(|e| e.is_aircraft_like()) {
                    assert!(event.detection.from_contrast);
                    assert_eq!(event.detection.half_s, p.edge_half_s);
                    assert!(new[event.detection.id].score >= p.thr_db);
                    let best = event.fit.as_ref().unwrap().best;
                    assert!((best.f0 - f0).abs() < 0.3, "{start}/{swell}: {best:?}");
                    assert!((best.v - v).abs() < 4.0, "{start}/{swell}: {best:?}");
                    assert!((best.d / d - 1.0).abs() < 0.08, "{start}/{swell}: {best:?}");
                    assert!(
                        (best.t0r - (t0r - start as f64)).abs() < 0.3,
                        "{start}/{swell}: {best:?}"
                    );
                    eprintln!("contrast edge start={start} swell={swell}: {best:?}");
                }
            }
        }
    }

    #[test]
    fn contrast_edge_recovery_preserves_existing_stages_and_shared_cap() {
        let p = DetectParams::default();
        let mut scenes = vec![
            fast_pass_scene((118.0, 120.0, 2400.0, 90.0), 0.16),
            fast_pass_scene(FAST_PASSES[2], 0.16),
            edge_clip(30.0, EDGE_PASS, 0.3, (0.0, 0.0, 0.0)),
            swell_scene(0.08, 25.0),
        ];
        for start in [3, 13] {
            for swell in [20.0, 25.0] {
                for amplitude in [0.04, 0.08, 0.16] {
                    let source = swell_scene(amplitude, swell);
                    scenes.push(source[start * 4000..(start + 30) * 4000].to_vec());
                }
            }
        }
        for (scene, x) in scenes.iter().enumerate() {
            let (ls, _) = survey(x, 4000, &p).unwrap();
            for cap in [1, p.max_detections] {
                let params = DetectParams {
                    max_detections: cap,
                    ..p.clone()
                };
                let (old, old_cut) = detect_counted_impl(&ls, &params, false);
                let (new, cut) = detect_counted(&ls, &params);
                assert!(new.len() >= old.len() && new.len() <= cap, "scene {scene}");
                assert!(cut >= old_cut, "scene {scene}");
                for (before, after) in old.iter().zip(&new) {
                    // A later harmonic may extend a family's members; every existing field
                    // and every pre-existing harmonic remains in its original order.
                    assert!(after.harmonics.starts_with(&before.harmonics));
                    let mut prefix = after.clone();
                    prefix.harmonics.clone_from(&before.harmonics);
                    assert_eq!(
                        serde_json::to_vec(before).unwrap(),
                        serde_json::to_vec(&prefix).unwrap(),
                        "scene {scene}"
                    );
                }
                let added = &new[old.len()..];
                assert!(
                    added.iter().all(|d| d.from_contrast
                        && d.half_s == p.edge_half_s
                        && d.score >= p.thr_db)
                );
                assert!(added.windows(2).all(|w| w[0].score >= w[1].score));
                if old.len() == cap || ls.contrast.is_none() {
                    assert!(added.is_empty(), "scene {scene}");
                }
            }
        }
    }

    #[test]
    fn contrast_edges_keep_swell_noise_flat_tones_and_reversed_passes_out() {
        let p = DetectParams::default();
        for start in [3, 13] {
            let mut cases = Vec::new();
            for swell in [20.0, 25.0] {
                let source = swell_scene(0.08, swell);
                let mut reversed = source[start * 4000..(start + 30) * 4000].to_vec();
                reversed.reverse();
                cases.push((format!("reversed {swell}"), reversed));
            }
            for swell in [20.0, 25.0, 35.0, 40.0] {
                let source = swell_scene(0.0, swell);
                cases.push((
                    format!("noise {swell}"),
                    source[start * 4000..(start + 30) * 4000].to_vec(),
                ));
            }
            let source = swell_scene(0.0, 25.0);
            let tone = source[start * 4000..(start + 30) * 4000]
                .iter()
                .enumerate()
                .map(|(i, &x)| {
                    x + (0.4 * (std::f64::consts::TAU * 300.0 * i as f64 / 4000.0).sin()) as f32
                })
                .collect();
            cases.push(("steady tone".into(), tone));
            for (name, x) in cases {
                let (ls, lin) = survey(&x, 4000, &p).unwrap();
                let detections = detect(&ls, &p);
                let events = measure_edge_families(&x, &lin, &detections);
                let diagnostics: Vec<_> = events
                    .iter()
                    .map(|e| (&e.quality, e.fit.as_ref().map(|f| f.best)))
                    .collect();
                assert!(
                    events.iter().all(|e| !e.is_aircraft_like()),
                    "start {start}, {name}: {diagnostics:?}"
                );
                assert!(
                    crate::search::independent_measured_events(&events, 0.0).is_empty(),
                    "start {start}, {name}"
                );
            }
        }
    }

    #[test]
    fn fast_edge_passes_reach_measurement_at_both_recording_ends() {
        let p = DetectParams::default();
        let off = DetectParams {
            fast_templates: false,
            ..p.clone()
        };
        // Thirty seconds cannot cover both halves of any full-length template. Keep
        // enough audio on either side for the unchanged eight-second measurement gate.
        for pass @ (f0, v, d, t0r) in [(118.0, 215.0, 900.0, 10.13), (83.0, 245.0, 700.0, 19.37)] {
            let mut recovered = 0;
            // A complete attenuation ladder: measure every new fast seed, not a case
            // chosen after looking at its fit. At least one real recovery per end is required.
            for quieter_db in (0..=12).step_by(3) {
                let amplitude = 0.32 * 10.0f64.powf(-(quieter_db as f64) / 20.0);
                let mut x = fast_pass_scene(pass, amplitude);
                x.truncate(30 * 4000);
                let (ls, lin) = survey(&x, 4000, &p).unwrap();
                let old = detect(&ls, &off);
                let new = detect(&ls, &p);
                let added: Vec<_> = new
                    .iter()
                    .filter(|seed| {
                        seed.v > PRIMARY_MAX_SPEED_MPS
                            && seed.half_s == p.edge_half_s
                            && seed.harmonic_of.is_none()
                    })
                    .collect();
                eprintln!(
                    "fast edge v={v} t={t0r} attenuation={quieter_db} dB: {} old seeds, new {added:?}",
                    old.len()
                );
                if added.is_empty() {
                    continue;
                }
                let baseline = measure_edge_families(&x, &lin, &old);
                assert!(
                    crate::search::independent_measured_events(&baseline, 0.0).is_empty(),
                    "v={v} t={t0r} attenuation={quieter_db}: the original bank already supplied an event"
                );
                let events = measure_edge_families(&x, &lin, &new);
                let independent = crate::search::independent_measured_events(&events, 0.0);
                let diagnostics: Vec<_> = events
                    .iter()
                    .map(|e| (&e.id, &e.quality, e.fit.as_ref().map(|f| f.best)))
                    .collect();
                assert_eq!(independent.len(), 1, "v={v} t={t0r}: {diagnostics:?}");
                let event = events.iter().find(|e| e.id == independent[0].id).unwrap();
                let seed = &new[event.detection.id];
                assert!(seed.v > PRIMARY_MAX_SPEED_MPS && seed.half_s == p.edge_half_s);
                assert!(seed.score >= p.thr_db);
                let b = event.fit.as_ref().unwrap().best;
                eprintln!("fast edge v={v} t={t0r} attenuation={quieter_db} dB: {b:?}");
                assert!((b.f0 - f0).abs() < 0.3, "v={v} t={t0r}: {b:?}");
                assert!((b.t0r - t0r).abs() < 0.3, "v={v} t={t0r}: {b:?}");
                assert!((b.v - v).abs() < 4.0, "v={v} t={t0r}: {b:?}");
                assert!((b.d / d - 1.0).abs() < 0.08, "v={v} t={t0r}: {b:?}");
                recovered += 1;
            }
            assert!(
                recovered > 0,
                "no fast edge pass recovered at v={v}, t={t0r}"
            );
        }
    }

    #[test]
    fn fast_edge_noise_tones_and_reversed_passes_supply_no_aircraft_event() {
        let p = DetectParams::default();
        let early = (118.0, 215.0, 900.0, 10.13);
        let late = (83.0, 245.0, 700.0, 19.37);
        let mut noise = fast_pass_scene(early, 0.0);
        noise.truncate(30 * 4000);
        let tone: Vec<_> = noise
            .iter()
            .enumerate()
            .map(|(i, &x)| {
                x + (0.4 * (std::f64::consts::TAU * 118.0 * i as f64 / 4000.0).sin()) as f32
            })
            .collect();
        let mut cases = vec![("noise and hum", noise), ("steady tone", tone)];
        for (label, pass) in [("early reversed", early), ("late reversed", late)] {
            let mut x = fast_pass_scene(pass, 0.32);
            x.truncate(30 * 4000);
            x.reverse();
            cases.push((label, x));
        }
        for (label, x) in cases {
            let (ls, lin) = survey(&x, 4000, &p).unwrap();
            let detections = detect(&ls, &p);
            let events = measure_edge_families(&x, &lin, &detections);
            let diagnostics: Vec<_> = events
                .iter()
                .map(|e| (&e.id, &e.quality, e.fit.as_ref().map(|f| f.best)))
                .collect();
            assert!(
                events.iter().all(|event| !event.is_aircraft_like()),
                "{label}: {diagnostics:?}"
            );
            assert!(crate::search::independent_measured_events(&events, 0.0).is_empty());
        }
    }

    #[test]
    fn fast_stage_preserves_the_existing_stages_and_detection_cap() {
        let p = DetectParams::default();
        let off = DetectParams {
            fast_templates: false,
            ..p.clone()
        };
        let scenes = [
            (
                "ordinary",
                fast_pass_scene((118.0, 120.0, 2400.0, 90.0), 0.16),
            ),
            ("fast", fast_pass_scene(FAST_PASSES[2], 0.16)),
            ("edge", edge_clip(30.0, EDGE_PASS, 0.3, (0.0, 0.0, 0.0))),
            ("contrast", swell_scene(0.08, 25.0)),
        ];
        let key = |d: &Detection| {
            (
                d.id,
                [
                    d.t0r,
                    d.f0,
                    d.v,
                    d.d,
                    d.score,
                    d.score_approach,
                    d.score_recede,
                    d.half_s,
                ]
                .map(f64::to_bits),
                d.harmonic_of,
                d.from_contrast,
            )
        };
        for (name, x) in scenes {
            let (ls, _) = survey(&x, 4000, &p).unwrap();
            let (without, old_cut) = detect_counted(&ls, &off);
            let (with, cut) = detect_counted(&ls, &p);
            assert!(with.len() >= without.len(), "{name}");
            assert!(with.len() <= p.max_detections, "{name}");
            assert!(cut >= old_cut, "{name}");
            for (a, b) in without.iter().zip(&with) {
                assert_eq!(key(a), key(b), "{name}");
                assert!(b.harmonics.starts_with(&a.harmonics), "{name}");
            }
            let added = &with[without.len()..];
            for d in added {
                assert!(
                    d.v > PRIMARY_MAX_SPEED_MPS && d.v <= 0.8 * p.c,
                    "{name}: {d:?}"
                );
                assert!(d.score >= p.thr_db && !d.from_contrast, "{name}: {d:?}");
            }
            let full = added
                .iter()
                .take_while(|d| d.half_s >= p.min_half_s)
                .count();
            assert!(added[full..].iter().all(|d| d.half_s == p.edge_half_s));
            for stage in [&added[..full], &added[full..]] {
                assert!(stage.windows(2).all(|w| w[0].score >= w[1].score), "{name}");
            }
            if !without.is_empty() {
                let capped = DetectParams {
                    max_detections: 1,
                    ..p.clone()
                };
                let capped_off = DetectParams {
                    fast_templates: false,
                    ..capped.clone()
                };
                assert_eq!(
                    serde_json::to_value(detect_counted(&ls, &capped).0).unwrap(),
                    serde_json::to_value(detect_counted(&ls, &capped_off).0).unwrap(),
                    "{name}: the fast stage displaced a kept detection"
                );
            }
        }
    }

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
                contrast: None,
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
                    let actual = pool.install(|| score_bank(&ls, &tmpls, &evidence, &[(0, nt)]));
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
                    // Windows score their cells exactly as the full window does and leave every
                    // other cell at -inf.
                    for windows in [vec![(0, nt / 3), (nt - nt / 4, nt)], vec![(nt / 2, nt / 2)]] {
                        let part = pool.install(|| score_bank(&ls, &tmpls, &evidence, &windows));
                        for k in 0..nr * nt {
                            let i = k % nt;
                            if windows.iter().any(|&(c0, c1)| (c0..c1).contains(&i)) {
                                assert_eq!(part.1[k], expected.1[k], "nt={nt} cell={k}");
                                assert_eq!(part.0[k].to_bits(), expected.0[k].to_bits());
                            } else {
                                assert_eq!(part.0[k], f32::NEG_INFINITY, "nt={nt} cell={k}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn edge_bank_matches_scalar() {
        // Each edge template scores a cell only where its base (the template before it) lacks
        // coverage and the cell is within the base half of the first or last frame; bases score
        // nothing. Checked cell by cell against a direct oracle, bit for bit.
        let p = DetectParams {
            v_grid: vec![40.0, 100.0, 175.0, 100.0],
            d_grid: vec![250.0, 1000.0, 12_000.0],
            ..DetectParams::default()
        };
        let off = DetectParams {
            edge_half_s: 0.0,
            ..p.clone()
        };
        assert!(build_edge_templates(&off, 1.0).is_empty());
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
                contrast: None,
            };
            let edge = build_edge_templates(&p, ls.hop_s);
            // 1.25 d / v is at most 24 s for (40, 250), (100, 250), (100, 1000), (175, 250),
            // (175, 1000) and the duplicate 100s; (40, 1000) and every 12 km template are longer.
            assert_eq!(edge.len(), 2 * 7);
            for pair in edge.chunks(2) {
                assert_eq!(pair[0].edge, EdgeRole::Base);
                assert_eq!(pair[0].half, p.min_half_s);
                assert_eq!(
                    pair[1].edge,
                    EdgeRole::Edge {
                        base_half: p.min_half_s
                    }
                );
                assert_eq!(
                    (pair[1].half, pair[1].v, pair[1].d),
                    (p.edge_half_s, pair[0].v, pair[0].d)
                );
            }
            let cell = |tm: &Template, r: usize, i: usize| {
                let (mut a, mut b, mut na, mut nb) = (0f32, 0f32, 0f32, 0f32);
                for &(dt, dr, approach) in &tm.path {
                    let row = r as i64 + dr as i64;
                    let frame = i as i64 + dt as i64;
                    if row < 0 || row >= nr as i64 || frame < 0 || frame >= nt as i64 {
                        continue;
                    }
                    let z = ls.data[row as usize * nt + frame as usize];
                    if approach {
                        a += z;
                        na += 1.0;
                    } else {
                        b += z;
                        nb += 1.0;
                    }
                }
                let covered = !(na < 0.6 * tm.n_a as f32 || nb < 0.6 * tm.n_r as f32);
                (covered, (a / na.sqrt()).min(b / nb.sqrt()))
            };
            let mut expected = (vec![f32::NEG_INFINITY; nr * nt], vec![0u16; nr * nt]);
            let mut scored = 0usize;
            for r in 0..nr {
                for i in 0..nt {
                    let k = r * nt + i;
                    for (ti, tm) in edge.iter().enumerate() {
                        let EdgeRole::Edge { base_half } = tm.edge else {
                            continue;
                        };
                        let near = (i as f64) < base_half || ((nt - 1 - i) as f64) < base_half;
                        let (base_covered, _) = cell(&edge[ti - 1], r, i);
                        let (covered, s) = cell(tm, r, i);
                        if !near || base_covered || !covered {
                            continue;
                        }
                        if s > expected.0[k] {
                            expected.0[k] = s;
                            expected.1[k] = ti as u16;
                        }
                    }
                    scored += expected.0[k].is_finite() as usize;
                }
            }
            eprintln!(
                "nt={nt}: {scored} of {} cells scored by an edge template",
                nr * nt
            );
            if nt >= 61 {
                assert!(scored > 0);
            }
            let head = (p.min_half_s as usize).min(nt);
            let tail = nt.saturating_sub(p.min_half_s as usize).max(head);
            for windows in [vec![(0, nt)], vec![(0, head), (tail, nt)]] {
                let actual = score_bank(&ls, &edge, &ls.data, &windows);
                for k in 0..nr * nt {
                    assert_eq!(actual.1[k], expected.1[k], "nt={nt} cell={k} {windows:?}");
                    assert_eq!(
                        actual.0[k].to_bits(),
                        expected.0[k].to_bits(),
                        "nt={nt} cell={k}"
                    );
                }
            }
        }
    }

    /// Without the edge stage, as `p` with `edge_half_s` 0.
    fn no_edge(p: &DetectParams) -> DetectParams {
        DetectParams {
            edge_half_s: 0.0,
            ..p.clone()
        }
    }

    #[test]
    fn edge_stage_finds_a_short_pass_in_a_clip_too_short_for_the_bank() {
        // A 24 s template needs 8 of its 12 path samples on each side, 16 s before and after
        // the CPA, so a 30 s clip leaves the passes no cell to score at all. A short transition
        // (d / v = 11 s) heard 9 s into it is found by an edge template, on its curve, and the
        // refined seed stays on the curve (measure.rs measures the event from it).
        let (f0, v, d, t0r) = EDGE_PASS;
        let x = edge_clip(30.0, EDGE_PASS, 0.3, (0.0, 0.0, 0.0));
        let on_curve = |t: f64| cv_curve(t, f0, v, d, t0r - d / 343.0, 343.0);
        let p = DetectParams::default();
        let (ls, lin) = survey(&x, 4000, &p).unwrap();
        let without = detect(&ls, &no_edge(&p));
        assert!(without.is_empty(), "{without:?}");
        let with = detect(&ls, &p);
        for d in with.iter().take(6) {
            eprintln!(
                "det t0r={:.1} f0={:.1} (curve {:.1}) v={:.0} d={:.0} s={:.2} half={} h={:?}",
                d.t0r,
                d.f0,
                on_curve(d.t0r),
                d.v,
                d.d,
                d.score,
                d.half_s,
                d.harmonic_of
            );
        }
        let found = with
            .iter()
            .find(|d| {
                d.harmonic_of.is_none()
                    && (d.t0r - t0r).abs() <= 4.0
                    && (d.f0 / on_curve(d.t0r) - 1.0).abs() < 0.02
            })
            .expect("the edge stage found no detection on the pass");
        assert_eq!(found.half_s, p.edge_half_s);
        assert!(found.score >= p.thr_silence_db, "score {}", found.score);
        let r = refine(&lin, found, 343.0);
        eprintln!(
            "refined t0r={:.2} f0={:.1} (curve {:.1}) v={:.1} d={:.0} s={:.2}",
            r.t0r,
            r.f0,
            on_curve(r.t0r),
            r.v,
            r.d,
            r.score
        );
        assert!(r.score.is_finite(), "refined score {}", r.score);
        assert_eq!(r.half_s, p.edge_half_s);
        assert!(
            (r.f0 / on_curve(r.t0r) - 1.0).abs() < 0.01,
            "refined f0 {}",
            r.f0
        );
        assert!((r.t0r - t0r).abs() <= 4.0, "refined t0r {}", r.t0r);
    }

    #[test]
    fn edge_stage_keeps_every_detection_the_passes_accept() {
        // Metamorphic: every detection without the edge stage is in the result with it, bit for
        // bit; each added one is an edge template's, within the base half of an edge. Noise and
        // a flat tone at the edges add nothing strong.
        let p = DetectParams::default();
        let near_edge = |ls: &LogSpec, d: &Detection| {
            d.t0r - ls.t[0] < p.min_half_s || ls.t[ls.nt() - 1] - d.t0r < p.min_half_s
        };
        let cases: Vec<(&str, Vec<f32>)> = vec![
            (
                "pass at 9 s",
                edge_clip(40.0, EDGE_PASS, 0.25, (0.0, 0.0, 0.0)),
            ),
            (
                "pass at 31 s",
                edge_clip(40.0, (420.0, 90.0, 900.0, 31.0), 0.25, (0.0, 0.0, 0.0)),
            ),
            ("interior pass and flat tone", synth(4000, 600.0)),
            ("noise", edge_clip(40.0, EDGE_PASS, 0.0, (0.0, 0.0, 0.0))),
            (
                "noise, 30 s",
                edge_clip(30.0, EDGE_PASS, 0.0, (0.0, 0.0, 0.0)),
            ),
            (
                "flat tone",
                edge_clip(40.0, EDGE_PASS, 0.0, (250.0, 0.1, 40.0)),
            ),
            (
                "tone for 12 s",
                edge_clip(40.0, EDGE_PASS, 0.0, (250.0, 0.1, 12.0)),
            ),
        ];
        for (name, x) in cases {
            let (ls, _) = survey(&x, 4000, &p).unwrap();
            let (without, cut_without) = detect_counted(&ls, &no_edge(&p));
            let (with, cut_with) = detect_counted(&ls, &p);
            let key = |d: &Detection| {
                [
                    d.t0r,
                    d.f0,
                    d.v,
                    d.d,
                    d.score,
                    d.score_approach,
                    d.score_recede,
                    d.half_s,
                ]
                .map(f64::to_bits)
            };
            let kept: Vec<_> = with.iter().map(key).collect();
            for d in &without {
                assert!(kept.contains(&key(d)), "{name}: lost {d:?}");
            }
            assert!(cut_with >= cut_without);
            let added: Vec<&Detection> = with
                .iter()
                .filter(|d| !without.iter().any(|o| key(o) == key(d)))
                .collect();
            for d in &added {
                assert_eq!(d.half_s, p.edge_half_s, "{name}: {d:?}");
                assert!(near_edge(&ls, d), "{name}: {d:?}");
            }
            let top = added
                .iter()
                .map(|d| d.score)
                .fold(f64::NEG_INFINITY, f64::max);
            eprintln!(
                "{name}: {} detections, {} added, strongest added {top:.2}",
                without.len(),
                added.len()
            );
            if !name.starts_with("pass at") {
                assert!(
                    added.iter().all(|d| d.score < p.thr_db),
                    "{name}: {added:?}"
                );
            }
        }
    }

    /// `ls` without its contrast image: what [`detect_counted`] scores without the contrast stage.
    fn without_contrast(ls: &LogSpec) -> LogSpec {
        LogSpec {
            t: ls.t.clone(),
            freqs: ls.freqs.clone(),
            data: ls.data.clone(),
            bins_per_octave: ls.bins_per_octave,
            hop_s: ls.hop_s,
            scale: ls.scale,
            gain: ls.gain,
            contrast: None,
        }
    }

    /// Whether `d` is a primary on [`SWELL_PASS`]'s curve near its CPA.
    fn on_swell_pass(d: &Detection) -> bool {
        let (f0, v, dist, t0r) = SWELL_PASS;
        let f = cv_curve(d.t0r, f0, v, dist, t0r - dist / 343.0, 343.0);
        d.harmonic_of.is_none() && (d.t0r - t0r).abs() <= 4.0 && (d.f0 / f - 1.0).abs() < 0.02
    }

    #[test]
    fn contrast_stage_keeps_every_detection_of_the_survey_image() {
        // Metamorphic: an image at or above the spread floor has no contrast image, so detection
        // is exactly as before. Under it, every detection without the contrast stage comes first
        // with it, field for field (a primary may gain harmonic ratios); each added one follows,
        // flagged, from the full bank, at the strong line, strongest first. A swell of noise
        // without a pass adds nothing up to 30 dB.
        let p = DetectParams::default();
        let cases: Vec<(&str, Vec<f32>)> = vec![
            ("no swell", swell_scene(0.16, 0.0)),
            ("interior pass and flat tone", synth(4000, 600.0)),
            (
                "noise, 30 s",
                edge_clip(30.0, EDGE_PASS, 0.0, (0.0, 0.0, 0.0)),
            ),
            ("faint pass in a 20 dB swell", swell_scene(0.08, 20.0)),
            ("pass in a 25 dB swell", swell_scene(0.16, 25.0)),
            ("pass in a 35 dB swell", swell_scene(0.12, 35.0)),
            ("20 dB swell", swell_scene(0.0, 20.0)),
            ("25 dB swell", swell_scene(0.0, 25.0)),
            ("30 dB swell", swell_scene(0.0, 30.0)),
            ("35 dB swell", swell_scene(0.0, 35.0)),
            ("40 dB swell", swell_scene(0.0, 40.0)),
        ];
        let key = |d: &Detection| {
            (
                d.id,
                [
                    d.t0r,
                    d.f0,
                    d.v,
                    d.d,
                    d.score,
                    d.score_approach,
                    d.score_recede,
                    d.half_s,
                ]
                .map(f64::to_bits),
                d.harmonic_of,
                d.from_contrast,
            )
        };
        for (name, x) in cases {
            let (ls, _) = survey(&x, 4000, &p).unwrap();
            assert_eq!(ls.contrast.is_some(), ls.gain > 1.0, "{name}");
            if let Some(c) = &ls.contrast {
                assert_eq!(c.len(), ls.data.len(), "{name}");
            }
            let (without, cut_without) = detect_counted(&without_contrast(&ls), &p);
            let (with, cut_with) = detect_counted(&ls, &p);
            assert!(with.len() >= without.len(), "{name}");
            for (a, b) in without.iter().zip(&with) {
                assert_eq!(key(a), key(b), "{name}");
                assert!(b.harmonics.starts_with(&a.harmonics), "{name}: {b:?}");
                assert!(!a.from_contrast, "{name}: {a:?}");
            }
            assert!(cut_with >= cut_without, "{name}");
            let added = &with[without.len()..];
            if ls.contrast.is_none() {
                assert!(added.is_empty(), "{name}: {added:?}");
            }
            for d in added {
                assert!(d.from_contrast, "{name}: {d:?}");
                assert!(d.half_s >= p.min_half_s, "{name}: {d:?}");
                assert!(d.score >= p.thr_db, "{name}: {d:?}");
            }
            assert!(
                added.windows(2).all(|w| w[0].score >= w[1].score),
                "{name}: {added:?}"
            );
            eprintln!(
                "{name}: spread {:.3}, gain {:.2}: {} detections, {} added {:?}",
                ls.scale,
                ls.gain,
                without.len(),
                added.len(),
                added
                    .iter()
                    .map(|d| (d.t0r, d.f0.round(), (d.score * 100.0).round() / 100.0))
                    .collect::<Vec<_>>()
            );
            if ["20 dB swell", "25 dB swell", "30 dB swell"].contains(&name) {
                assert!(added.is_empty(), "{name}: {added:?}");
            }
        }
    }

    #[test]
    fn contrast_stage_finds_a_pass_a_loud_swell_hides_from_the_survey_image() {
        // A faint pass whose tone and roar swell together by 20 or 25 dB: the swell inflates each
        // row's MAD before the frame contrast removes it, so even raised to the spread floor the
        // survey image holds no detection on the pass. Taking the frame contrast first, the
        // contrast stage finds it on its curve, and the refined seed (what measure.rs measures)
        // stays on the curve.
        let (f0, v, d, t0r) = SWELL_PASS;
        let on_curve = |t: f64| cv_curve(t, f0, v, d, t0r - d / 343.0, 343.0);
        let p = DetectParams::default();
        for swell_db in [20.0, 25.0] {
            let x = swell_scene(0.08, swell_db);
            let (ls, lin) = survey(&x, 4000, &p).unwrap();
            assert!(ls.gain > 1.0, "{swell_db} dB: spread {}", ls.scale);
            let without = detect(&without_contrast(&ls), &p);
            assert!(
                !without.iter().any(on_swell_pass),
                "{swell_db} dB: the survey image already holds the pass: {without:?}"
            );
            let with = detect(&ls, &p);
            let found = with
                .iter()
                .find(|d| on_swell_pass(d))
                .unwrap_or_else(|| panic!("{swell_db} dB: no detection on the pass: {with:?}"));
            eprintln!("{swell_db} dB: {found:?}");
            assert!(found.from_contrast);
            assert!(found.score >= p.thr_db, "score {}", found.score);
            let r = refine(&lin, found, 343.0);
            eprintln!(
                "refined t0r={:.2} f0={:.1} (curve {:.1}) v={:.1} d={:.0} s={:.2}",
                r.t0r,
                r.f0,
                on_curve(r.t0r),
                r.v,
                r.d,
                r.score
            );
            assert!(r.from_contrast);
            assert!(
                (r.f0 / on_curve(r.t0r) - 1.0).abs() < 0.01,
                "refined f0 {}",
                r.f0
            );
            assert!((r.t0r - t0r).abs() <= 4.0, "refined t0r {}", r.t0r);
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
            from_contrast: false,
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
