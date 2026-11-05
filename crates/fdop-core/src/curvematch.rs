//! Curve matching: score logged flights directly on the spectrogram, with no event detection.
//!
//! The trace search measures a ridge first and compares the fitted numbers, so a pass whose ridge
//! cannot be traced contributes nothing. Here the question is asked the other way round: if the
//! recording was made at `x` with clock `c`, each logged flight predicts one exact Doppler curve.
//! Is there energy along it, more than along the same curve at other times?
//!
//! Scoring whitened spectrogram power along a real trajectory's curve with a shared clock, and
//! checking the answer against flights from other times, follow dopplerprint (MIT, (c) 2026 Henry
//! Alexander), reimplemented here. What differs:
//!
//! - **A bank instead of per-cell sums.** On a log-frequency axis a constant-velocity pass is a
//!   fixed path for each (v, d), and emitted tone f0 is a row shift, so one sweep over the recording
//!   per (v, d) scores every (arrival time, f0). The cost does not depend on the number of cells or
//!   flights. A cell then only looks up each flight at its (v, d, arrival time).
//! - **The null comes from the recording.** Each (v, d) path's score at all times of the recording
//!   gives its own tail, turned into a p-value per lookup, then an e-value (Vovk-Wang calibrator
//!   e = kappa p^(kappa-1), which has expectation 1 under the null for any kappa in (0, 1)).
//! - **Many decoy clocks, each searched like the real one.** Because a lookup is cheap, the whole
//!   search (vote, exact rescoring, refinement) runs again at 48 clock windows far from the prior.
//!   Their best scores are what a place reaches at a clock that cannot be right, so the answer is
//!   ranked against many decoys, not two.
//! - **A tone prior by aircraft type** (`TONE_TABLE`), divided by where this recording's best tones
//!   fall at chance, so it moves evidence between tones without adding any on average.
//! - **Both halves of the pass must light up** (`Stat::Min`, as in `detect`): a flat tone only
//!   lights one.

use crate::physics::cv_ratio_rel_t0r;
use crate::spectro::{median_f32, stft_band_db};
use crate::track::{Prop, TrackModel};
use fdop_types::atmo::Atmosphere;
use fdop_types::geo::{
    Enu, V3, ecef_to_geodetic, geodetic_to_ecef, haversine_m, m_per_deg, norm, sub,
};
use fdop_types::{BBox, Elevation, MIC_HEIGHT_M};
use rayon::prelude::*;
use serde::Serialize;

const BASE_C: f64 = 343.0;

/// How one pass's score is formed from its approach and recede halves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Stat {
    /// sqrt(2) * min(approach, recede): both halves must light up.
    Min,
    /// (approach + recede) / sqrt(2): plain matched filter.
    Sum,
}

/// An answer may be asserted only above this margin over the best decoy clock: it has to beat every
/// one of them, which under exchangeable windows happens by chance about 1 time in `decoys + 1`.
/// On 42 recordings-at-the-wrong-date-or-place the largest margin was -0.8 and none was above 0.
pub const ASSERT_MARGIN_NATS: f64 = 0.0;
/// Fewer decoys than this and the best of them says too little.
pub const MIN_DECOYS: usize = 40;
/// Places within this many nats of the best are not ruled out (about 20 to 1).
pub const TIED_NATS: f64 = 3.0;
/// Smallest radius reported; modes are at least 1 km apart, so nothing finer is resolved here.
pub const RADIUS_FLOOR_M: f64 = 1000.0;
/// An answer this uncertain is not asserted, whatever its margin.
pub const MAX_ASSERT_RADIUS_M: f64 = 5000.0;
/// A decoy window must sit this far inside the fetched catalogue.
const CATALOG_PAD_S: f64 = 60.0;

#[derive(Clone, Debug)]
pub struct MatchParams {
    pub win_s: f64,
    pub hop_s: f64,
    pub bins_per_octave: f64,
    pub band_lo: f64,
    pub band_hi: f64,
    pub f0_lo: f64,
    pub f0_hi: f64,
    /// (harmonic number, weight), summed incoherently.
    pub harmonics: Vec<(u32, f32)>,
    pub floor_window_s: f64,
    pub floor_quantile: f64,
    pub floor_stride_s: f64,
    pub clip: (f32, f32),
    /// Cost of taking a neighbouring row's value (whitened units).
    pub neighbour_cost: f32,
    pub v_grid: Vec<f64>,
    pub d_grid: Vec<f64>,
    /// Range inside which a frame carries full weight; beyond it amplitude falls as 1/r.
    pub r_ref_m: f64,
    pub half_k: f64,
    pub half_min_s: f64,
    pub half_max_s: f64,
    pub stat: Stat,
    pub cell_m: f64,
    pub reach_m: f64,
    pub chunk_s: f64,
    pub chunk_step_s: f64,
    pub sample_s: f64,
    /// Lookups under this many nats do not vote at the coarse stage.
    pub n_min: f64,
    /// Calibrator exponent and the share of passes assumed heard, at the coarse stage.
    pub kappa: f64,
    pub pi_heard: f64,
    /// Likelihood ratio of "heard" for a lookup at chance (the cost of an unheard close pass).
    pub e_floor: f64,
    /// Slant range heard with half the capped probability; spread of source and path in dB; cap.
    pub r50_m: f64,
    pub sigma_db: f64,
    pub p_cap: f64,
    /// Decoy clocks reach this far from the prior, in windows of the prior's width plus a guard.
    pub decoy_span_s: f64,
    pub decoy_guard_s: f64,
    /// Weight an emitted tone by what the aircraft's propeller can make (`TONE_TABLE`), relative to
    /// where this recording's best tones fall at chance (`tone_ln_q`).
    pub tone_prior: bool,
    /// Density of the tone prior outside the type's bands, relative to no prior at all.
    pub tone_off_q: f64,
    pub top_modes: usize,
    pub refine_modes: usize,
    pub nms_m: f64,
    pub refine_steps_m: Vec<f64>,
    pub clock_sweep_frames: i64,
    /// One pass may sit this many frames off the shared clock (ADS-B timing and path error), at a
    /// Student-t cost with 2 degrees of freedom and this scale. Measured spread on real flights at a
    /// known site is about 1 s.
    pub pass_slack_frames: i64,
    pub pass_clock_sigma_s: f64,
    /// The same allowance when voting, without the cost.
    pub coarse_slack_frames: usize,
    /// A pass with at least this share of its central frames on claimed pixels is the same event.
    pub same_event_share: f64,
    /// Diagnostic: report evidence at (lat, lon, clock). Never affects the result.
    pub probe: Option<(f64, f64, f64)>,
}

impl Default for MatchParams {
    fn default() -> Self {
        Self {
            win_s: 1.0,
            hop_s: 0.5,
            bins_per_octave: 48.0,
            band_lo: 25.0,
            band_hi: 1400.0,
            f0_lo: 40.0,
            f0_hi: 350.0,
            harmonics: vec![(1, 1.0), (2, 1.0), (3, 0.8), (4, 0.6), (5, 0.5), (6, 0.4)],
            floor_window_s: 60.0,
            floor_quantile: 0.25,
            floor_stride_s: 2.0,
            clip: (-3.0, 8.0),
            neighbour_cost: 1.0,
            v_grid: geometric(28.0, 1.2, 11),
            d_grid: geometric(120.0, 1.3, 17),
            r_ref_m: 1500.0,
            half_k: 3.5,
            half_min_s: 30.0,
            half_max_s: 120.0,
            stat: Stat::Min,
            cell_m: 200.0,
            reach_m: 8000.0,
            chunk_s: 360.0,
            chunk_step_s: 240.0,
            sample_s: 4.0,
            n_min: 4.6,
            kappa: 0.5,
            pi_heard: 0.5,
            e_floor: 0.1,
            r50_m: 2000.0,
            sigma_db: 6.0,
            p_cap: 0.9,
            decoy_span_s: 7200.0,
            decoy_guard_s: 60.0,
            tone_prior: true,
            tone_off_q: 0.25,
            top_modes: 16,
            refine_modes: 4,
            nms_m: 1000.0,
            refine_steps_m: vec![100.0, 50.0, 25.0],
            clock_sweep_frames: 4,
            pass_slack_frames: 3,
            pass_clock_sigma_s: 0.8,
            coarse_slack_frames: 3,
            same_event_share: 0.4,
            probe: None,
        }
    }
}

fn geometric(first: f64, ratio: f64, n: usize) -> Vec<f64> {
    (0..n).map(|i| first * ratio.powi(i as i32)).collect()
}

/// Harmonic comb of a whitened log-frequency spectrogram, row-major `[frame][row]`, where a row is
/// the received fundamental: `comb[t][u] = sum_h w_h S[t][u + bpo log2 h] / sqrt(sum_h w_h^2)`.
pub struct CombImage {
    pub t0: f64,
    pub dt: f64,
    pub f_lo: f64,
    pub bpo: f64,
    pub nt: usize,
    pub nu: usize,
    pub comb: Vec<f32>,
}

impl CombImage {
    fn row_of(&self, f: f64) -> f64 {
        self.bpo * (f / self.f_lo).log2()
    }
}

/// Whitening after dopplerprint's `audio/whiten.rs`: each row less a running low-quantile floor (a
/// tone may hold a bin for most of the window and survive), then a robust z-score, then each frame
/// less its median, then clipping.
fn whiten_rows(rows: &mut [f32], nt: usize, nr: usize, p: &MatchParams, dt: f64) {
    let half = ((0.5 * p.floor_window_s / dt).round() as usize).max(1);
    let stride = ((p.floor_stride_s / dt).round() as usize).max(1);
    rows.par_chunks_mut(nt)
        .for_each_init(Vec::<f32>::new, |tmp, row| {
            let knots: Vec<usize> = (0..nt).step_by(stride).collect();
            let vals: Vec<f32> = knots
                .iter()
                .map(|&k| {
                    let (a, b) = (k.saturating_sub(half), (k + half + 1).min(nt));
                    tmp.clear();
                    tmp.extend_from_slice(&row[a..b]);
                    let i = ((tmp.len() - 1) as f64 * p.floor_quantile) as usize;
                    let (_, v, _) = tmp.select_nth_unstable_by(i, f32::total_cmp);
                    *v
                })
                .collect();
            for (t, v) in row.iter_mut().enumerate() {
                let a = t / stride;
                let base = if a + 1 < vals.len() {
                    let w = (t - knots[a]) as f32 / stride as f32;
                    vals[a] * (1.0 - w) + vals[a + 1] * w
                } else {
                    vals[vals.len() - 1]
                };
                *v -= base;
            }
            tmp.clear();
            tmp.extend_from_slice(row);
            let centre = median_f32(tmp);
            for x in tmp.iter_mut() {
                *x = (*x - centre).abs();
            }
            let sigma = (1.4826 * median_f32(tmp)).max(1e-3);
            for v in row.iter_mut() {
                *v = (*v - centre) / sigma;
            }
        });
    let centres: Vec<f32> = (0..nt)
        .into_par_iter()
        .map_init(Vec::<f32>::new, |tmp, t| {
            tmp.clear();
            tmp.extend((0..nr).map(|r| rows[r * nt + t]));
            median_f32(tmp)
        })
        .collect();
    rows.par_chunks_mut(nt).for_each(|row| {
        for (v, c) in row.iter_mut().zip(&centres) {
            *v = (*v - c).clamp(p.clip.0, p.clip.1);
        }
    });
}

/// Build the comb image from audio samples.
pub fn build_image(x: &[f32], sr: u32, p: &MatchParams) -> Result<CombImage, String> {
    let band_hi = p.band_hi.min(0.45 * sr as f64);
    let lin = stft_band_db(
        x,
        sr,
        p.win_s,
        p.hop_s,
        p.band_lo * 0.95,
        band_hi * 1.02,
        1.0,
        0.0,
    )?;
    let (nt, df) = (lin.nt(), lin.df());
    let b = p.bins_per_octave;
    let nr = ((band_hi / p.band_lo).log2() * b).floor() as usize + 1;
    let freqs: Vec<f64> = (0..nr).map(|k| p.band_lo * (k as f64 / b).exp2()).collect();
    let mut rows = vec![0f32; nr * nt];
    rows.par_chunks_mut(nt).enumerate().for_each(|(k, row)| {
        let fc = freqs[k];
        let (lo, hi) = (fc * (-0.5 / b).exp2(), fc * (0.5 / b).exp2());
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
    let (t0, dt) = (lin.t[0], lin.dt());
    drop(lin);
    whiten_rows(&mut rows, nt, nr, p, dt);
    // A curve lands on the nearest row, and neither the bank's (v, d) nor f0 sit exactly on the
    // grid, so a pass may be a row away. One row either side, at a small cost, as in dopplerprint.
    let mut rows2 = rows.clone();
    rows2.par_chunks_mut(nt).enumerate().for_each(|(r, out)| {
        for (t, o) in out.iter_mut().enumerate() {
            if r > 0 {
                *o = o.max(rows[(r - 1) * nt + t] - p.neighbour_cost);
            }
            if r + 1 < nr {
                *o = o.max(rows[(r + 1) * nt + t] - p.neighbour_cost);
            }
        }
    });
    let rows = rows2;
    let nu = ((p.f0_hi * 2.2 / p.band_lo).log2() * b).floor() as usize + 1;
    let nu = nu.min(nr);
    let shifts: Vec<(usize, f32)> = p
        .harmonics
        .iter()
        .map(|&(h, w)| ((b * (h as f64).log2()).round() as usize, w))
        .collect();
    let mut comb = vec![0f32; nt * nu];
    comb.par_chunks_mut(nu).enumerate().for_each(|(t, out)| {
        for (u, o) in out.iter_mut().enumerate() {
            let (mut s, mut w2) = (0f32, 0f32);
            for &(sh, w) in &shifts {
                if u + sh < nr {
                    s += w * rows[(u + sh) * nt + t];
                    w2 += w * w;
                }
            }
            *o = if w2 > 0.0 { s / w2.sqrt() } else { 0.0 };
        }
    });
    Ok(CombImage {
        t0,
        dt,
        f_lo: p.band_lo,
        bpo: b,
        nt,
        nu,
        comb,
    })
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Null {
    pub q50: f32,
    pub beta: f32,
}

impl Null {
    /// -ln P(score >= s), from the exponential tail fitted above the median. Never negative.
    #[inline]
    pub fn nats(&self, s: f32) -> f32 {
        (std::f32::consts::LN_2 + (s - self.q50) / self.beta).max(0.0)
    }
}

/// Propellers of common types: (blades, lowest rpm in flight, highest rpm). Blade-pass tone is
/// blades x rpm / 60; on a piston single a 4-cylinder's firing rate falls on the 2-blade range and a
/// 6-cylinder's on the 3-blade range, so both sources land in these bands. Where a type flies with
/// more than one propeller, each is listed. These are typical published figures written from general
/// knowledge, not checked one by one against type certificates. A type not listed gets no prior.
/// Helicopters are left out on purpose: main and tail rotors give tones no one band describes.
/// Type designators and their (blades, lowest rpm, highest rpm) bands.
type ToneEntry = (&'static [&'static str], &'static [(f64, f64, f64)]);
const TONE_TABLE: &[ToneEntry] = &[
    // two blades
    (
        &[
            "C120", "C140", "C150", "C152", "C170", "C172", "C175", "P28A", "PA18", "PA20", "PA22",
            "AA5", "BE23", "DA20", "J3",
        ],
        &[(2.0, 2000.0, 2800.0)],
    ),
    // two or three blades, singles and twins
    (
        &[
            "C177", "C180", "C182", "C185", "C206", "C210", "P28R", "PA24", "PA46", "M20P", "M20T",
            "AC11", "RV6", "RV7", "RV8", "RV9", "BE35", "BE55", "BE76", "C310", "PA23", "PA27",
            "PA30", "PA34", "PA44",
        ],
        &[(2.0, 2000.0, 2750.0), (3.0, 2000.0, 2750.0)],
    ),
    // three blades
    (
        &[
            "SR20", "SR22", "S22T", "BE33", "BE36", "PA32", "P32R", "P32T", "DA40", "RV10", "BE58",
            "C340", "C402", "C404", "C414", "C421", "PA31", "DA42",
        ],
        &[(3.0, 2000.0, 2750.0)],
    ),
    // single-engine turboprops
    (&["C208"], &[(3.0, 1600.0, 1900.0), (4.0, 1600.0, 1900.0)]),
    (&["PC12"], &[(4.0, 1400.0, 1700.0), (5.0, 1400.0, 1700.0)]),
    (
        &["TBM7", "TBM8", "TBM9"],
        &[(4.0, 1800.0, 2000.0), (5.0, 1800.0, 2000.0)],
    ),
    (&["P46T"], &[(3.0, 1800.0, 2000.0), (4.0, 1800.0, 2000.0)]),
    (&["DHC2"], &[(3.0, 1800.0, 2300.0)]),
    // twin turboprops
    (
        &[
            "BE9L", "BE99", "BE10", "BE20", "BE30", "B350", "PAY1", "PAY2", "PAY3", "PAY4", "C425",
            "C441",
        ],
        &[(3.0, 1500.0, 2200.0), (4.0, 1500.0, 2200.0)],
    ),
    (&["DHC6"], &[(3.0, 1600.0, 2200.0)]),
    (&["SF34"], &[(4.0, 1100.0, 1400.0)]),
];
/// How far outside blades x rpm / 60 a tone still counts as in the band: the fitted curve's tone and
/// the listed rpm both have error.
const TONE_BAND_SLACK: f64 = 1.10;

/// Share of the chance distribution of tone rows that stays uniform, so that a row no peak happened
/// to fall on keeps a finite weight.
const TONE_CHANCE_FLOOR: f64 = 0.1;

/// ln q(row) for one table entry: the prior over rows divided by where this recording's best rows
/// fall, `counts` (the best row at every time and path, with no prior). That distribution, with a
/// small uniform share so no row divides by zero, is the divisor; q is then scaled so that its mean
/// over the counted rows is exactly 1. So the weighting is centred on this recording and does not
/// favour every flight at every clock. That is not a calibration of the final score, which takes
/// the best row after the weighting: the decoy windows, weighted the same way, are what that is
/// judged against. Dividing by a uniform distribution does not hold even the centring: on real audio
/// the best rows pile up at low tones, where the propeller bands are, so every flight at every clock
/// collected the bonus (`a_tone_prior_on_a_rumbling_recording_lifts_no_place_over_the_decoys`).
fn tone_ln_q(
    im: &CombImage,
    r0: usize,
    counts: &[u64],
    bands: &[(f64, f64, f64)],
    off_q: f64,
) -> Vec<f32> {
    let chance = chance_of_rows(counts);
    let nf0 = chance.len();
    let weight: Vec<f64> = (0..nf0)
        .map(|r| {
            let f = im.f_lo * ((r0 + r) as f64 / im.bpo).exp2();
            let inside = bands.iter().any(|&(b, lo, hi)| {
                f >= b * lo / 60.0 / TONE_BAND_SLACK && f <= b * hi / 60.0 * TONE_BAND_SLACK
            });
            if inside { 1.0 } else { off_q }
        })
        .collect();
    let total: f64 = weight.iter().sum();
    let q: Vec<f64> = weight
        .iter()
        .zip(&chance)
        .map(|(w, c)| w / total / c)
        .collect();
    let n: u64 = counts.iter().sum();
    let mean = if n > 0 {
        q.iter()
            .zip(counts)
            .map(|(q, &k)| q * k as f64)
            .sum::<f64>()
            / n as f64
    } else {
        q.iter().zip(&chance).map(|(q, c)| q * c).sum()
    };
    q.iter().map(|q| (q / mean).ln() as f32).collect()
}

/// Turn counts of the best row at every time and path into a distribution with a uniform floor.
fn chance_of_rows(counts: &[u64]) -> Vec<f64> {
    let total: u64 = counts.iter().sum();
    let n = counts.len().max(1) as f64;
    counts
        .iter()
        .map(|&c| {
            let seen = if total > 0 {
                c as f64 / total as f64
            } else {
                1.0 / n
            };
            (1.0 - TONE_CHANCE_FLOOR) * seen + TONE_CHANCE_FLOOR / n
        })
        .collect()
}

/// One (v, d) path over the whole recording.
struct Surface {
    null: Null,
    /// (arrival frame, value), over `n_min` nats, dilated by `coarse_slack_frames`.
    peaks: Vec<(u32, f32)>,
}

struct Bank {
    surfaces: Vec<Surface>,
    r0: usize,
    nf0: usize,
    /// `tone_ln_q` per `TONE_TABLE` entry; empty when the prior is off.
    tones: Vec<Vec<f32>>,
}

impl Bank {
    fn tone_of(&self, typ: &str) -> Option<&[f32]> {
        if self.tones.is_empty() {
            return None;
        }
        let i = TONE_TABLE
            .iter()
            .position(|(types, _)| types.contains(&typ))?;
        Some(&self.tones[i])
    }
}

struct Sample {
    dt: i32,
    dr: i32,
    w: f32,
    approach: bool,
}

fn path(p: &MatchParams, v: f64, d: f64, dt: f64, bpo: f64) -> Vec<Sample> {
    let half = (p.half_k * d / v).clamp(p.half_min_s, p.half_max_s);
    let n = (half / dt).round() as i32;
    (-n..=n)
        .map(|j| {
            let u = j as f64 * dt;
            let r = (d * d + v * v * u * u).sqrt();
            Sample {
                dt: j,
                dr: (bpo * cv_ratio_rel_t0r(u, v, d, BASE_C).log2()).round() as i32,
                w: (p.r_ref_m / r).min(1.0) as f32,
                approach: j < 0,
            }
        })
        .collect()
}

#[inline]
fn stat_of(stat: Stat, a: f32, b: f32) -> f32 {
    match stat {
        Stat::Min => std::f32::consts::SQRT_2 * a.min(b),
        Stat::Sum => (a + b) / std::f32::consts::SQRT_2,
    }
}

/// Sum one path over every emitted-tone row at arrival frame `tau` of `im`. `acc` holds two rows of
/// `nf0`. Returns the best statistic and its row (`None` if too little of either half is in the
/// recording). `tone` is the type's `ln q` per row and the path's null scale: a row's statistic is
/// raised by what its tone is worth in nats.
#[allow(clippy::too_many_arguments)]
#[inline]
fn sweep_tau(
    im: &CombImage,
    pth: &[Sample],
    tau: i64,
    r0: usize,
    nf0: usize,
    stat: Stat,
    acc_a: &mut [f32],
    acc_b: &mut [f32],
    full: (f32, f32),
    tone: Option<(&[f32], f32)>,
) -> Option<(f32, usize)> {
    acc_a.fill(0.0);
    acc_b.fill(0.0);
    let (mut wa, mut wb) = (0f32, 0f32);
    for s in pth {
        let frame = tau + s.dt as i64;
        if frame < 0 || frame >= im.nt as i64 {
            continue;
        }
        let first = r0 as i64 + s.dr as i64;
        if first < 0 || first as usize + nf0 > im.nu {
            continue;
        }
        let off = frame as usize * im.nu + first as usize;
        let src = &im.comb[off..off + nf0];
        let acc = if s.approach {
            wa += s.w * s.w;
            &mut *acc_a
        } else {
            wb += s.w * s.w;
            &mut *acc_b
        };
        for (d, v) in acc.iter_mut().zip(src) {
            *d += s.w * v;
        }
    }
    if wa < 0.25 * full.0 || wb < 0.25 * full.1 {
        return None;
    }
    let (ra, rb) = (1.0 / wa.sqrt(), 1.0 / wb.sqrt());
    let (mut best, mut at) = (f32::NEG_INFINITY, 0);
    for (r, (a, b)) in acc_a.iter().zip(acc_b.iter()).enumerate() {
        let mut s = stat_of(stat, a * ra, b * rb);
        if let Some((q, beta)) = tone {
            s += beta * q[r];
        }
        if s > best {
            best = s;
            at = r;
        }
    }
    Some((best, at))
}

/// ln of the Vovk-Wang e-value kappa p^(kappa - 1) of a lookup worth `nats` = -ln p. Kept in logs:
/// the e-value itself passes f64's range near 1,420 nats at kappa 0.5, and a bright curve on quiet
/// audio can score that.
#[inline]
fn ln_e_value(p: &MatchParams, nats: f64) -> f64 {
    p.kappa.ln() + (1.0 - p.kappa) * nats
}

/// ln(e^a + e^b) without overflow. Either side may be -inf.
#[inline]
fn ln_add(a: f64, b: f64) -> f64 {
    let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
    if hi == f64::NEG_INFINITY {
        return hi;
    }
    hi + (lo - hi).exp().ln_1p()
}

/// Coarse vote of one lookup: ln(1 - pi + pi e).
#[inline]
fn vote_value(p: &MatchParams, nats: f64) -> f32 {
    ln_add(
        (1.0 - p.pi_heard).ln(),
        p.pi_heard.ln() + ln_e_value(p, nats),
    ) as f32
}

fn build_bank(im: &CombImage, p: &MatchParams) -> Bank {
    let r0 = im.row_of(p.f0_lo).round().max(0.0) as usize;
    let nf0 = (im.row_of(p.f0_hi).round() as usize + 1 - r0).min(im.nu.saturating_sub(r0));
    let specs: Vec<(f64, f64)> = p
        .v_grid
        .iter()
        .flat_map(|&v| p.d_grid.iter().map(move |&d| (v, d)))
        .collect();
    let built = specs
        .par_iter()
        .map(|&(v, d)| {
            let pth = path(p, v, d, im.dt, im.bpo);
            let full = pth.iter().fold((0f32, 0f32), |acc, s| {
                if s.approach {
                    (acc.0 + s.w * s.w, acc.1)
                } else {
                    (acc.0, acc.1 + s.w * s.w)
                }
            });
            let (mut a, mut b) = (vec![0f32; nf0], vec![0f32; nf0]);
            let mut score = vec![f32::NEG_INFINITY; im.nt];
            let mut rows = vec![0u64; nf0];
            for (tau, sc) in score.iter_mut().enumerate() {
                if let Some((s, r)) = sweep_tau(
                    im, &pth, tau as i64, r0, nf0, p.stat, &mut a, &mut b, full, None,
                ) {
                    *sc = s;
                    rows[r] += 1;
                }
            }
            let mut vals: Vec<f32> = score.iter().copied().filter(|s| s.is_finite()).collect();
            let null = if vals.len() < 20 {
                Null {
                    q50: 0.0,
                    beta: 1.0,
                }
            } else {
                let q = |vals: &mut Vec<f32>, f: f64| {
                    let i = ((vals.len() - 1) as f64 * f) as usize;
                    *vals.select_nth_unstable_by(i, f32::total_cmp).1
                };
                let q50 = q(&mut vals, 0.5);
                let q90 = q(&mut vals, 0.9);
                Null {
                    q50,
                    beta: ((q90 - q50) / 5f32.ln()).max(0.05),
                }
            };
            let mut peaks = Vec::new();
            for tau in 0..im.nt {
                let (a0, b0) = (
                    tau.saturating_sub(p.coarse_slack_frames),
                    (tau + p.coarse_slack_frames + 1).min(im.nt),
                );
                let s = score[a0..b0]
                    .iter()
                    .copied()
                    .fold(f32::NEG_INFINITY, f32::max);
                if s.is_finite() {
                    let n = null.nats(s) as f64;
                    if n >= p.n_min {
                        peaks.push((tau as u32, vote_value(p, n)));
                    }
                }
            }
            (Surface { null, peaks }, rows)
        })
        .collect::<Vec<_>>();
    let mut counts = vec![0u64; nf0];
    for (_, rows) in &built {
        for (c, r) in counts.iter_mut().zip(rows) {
            *c += r;
        }
    }
    let surfaces = built.into_iter().map(|(sf, _)| sf).collect();
    let tones = if p.tone_prior {
        TONE_TABLE
            .iter()
            .map(|(_, bands)| tone_ln_q(im, r0, &counts, bands, p.tone_off_q))
            .collect()
    } else {
        Vec::new()
    };
    Bank {
        surfaces,
        r0,
        nf0,
        tones,
    }
}

fn nearest_log(grid: &[f64], x: f64) -> usize {
    let mut best = (f64::INFINITY, 0);
    for (i, &g) in grid.iter().enumerate() {
        let e = (x / g).ln().abs();
        if e < best.0 {
            best = (e, i);
        }
    }
    best.1
}

pub struct MatchInputs<'a> {
    pub image: &'a CombImage,
    /// Every flight that can matter, including those needed by the decoy clocks.
    pub tracks: &'a [TrackModel],
    pub region: BBox,
    /// Prior on the recording's start (UNIX s).
    pub start_lo: f64,
    pub start_hi: f64,
    pub duration_s: f64,
    /// Span the catalogue really covers: a decoy clock outside it would lose for lack of flights.
    pub catalog: (f64, f64),
    pub dem: &'a dyn Elevation,
    pub fine_dem: &'a dyn Elevation,
    pub atmo: &'a dyn Atmosphere,
}

#[derive(Clone, Debug, Serialize)]
pub struct MatchedPass {
    pub icao: String,
    pub reg: String,
    pub typ: String,
    pub key: String,
    /// Audio time of the heard closest approach (s).
    pub t_audio: f64,
    pub slant_m: f64,
    pub speed_mps: f64,
    pub f0_hz: f64,
    pub score: f64,
    pub nats: f64,
    pub value: f64,
    pub at_track_end: bool,
    /// This pass's own offset from the shared clock (s).
    pub clock_offset_s: f64,
    /// Its curve lies on pixels a stronger pass already claimed, so it earned nothing.
    pub shared: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct MatchMode {
    pub lat: f64,
    pub lon: f64,
    pub clock_unix: f64,
    /// Sum of coarse votes at the cell it grew from.
    pub coarse: f64,
    /// Exact-curve log evidence, including unheard close passes.
    pub evidence: f64,
    pub n_heard: usize,
    pub n_unheard_close: usize,
    pub passes: Vec<MatchedPass>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DecoyWindow {
    pub shift_s: f64,
    pub best_coarse: f64,
    pub best_evidence: f64,
    pub lat: f64,
    pub lon: f64,
    pub n_heard: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct MatchResult {
    pub modes: Vec<MatchMode>,
    pub decoys: Vec<DecoyWindow>,
    /// Best evidence less the best decoy's.
    pub decoy_margin: Option<f64>,
    /// (1 + decoys at least as good) / (1 + decoys).
    pub decoy_p: Option<f64>,
    /// Best evidence less the next distinct mode's.
    pub mode_margin: Option<f64>,
    pub n_cells: usize,
    pub n_tracks: usize,
    pub n_passes: usize,
    pub timings_s: Vec<(String, f64)>,
    /// See `MatchParams::probe`: at the given place, then after climbing from it.
    pub probe: Vec<MatchMode>,
}

struct Grid {
    enu: Enu,
    ncols: usize,
    nrows: usize,
    e0: f64,
    n0: f64,
    step: f64,
    /// Up coordinate of the observer and whether the cell is in the region, per cell.
    up: Vec<f32>,
    inside: Vec<bool>,
}

impl Grid {
    fn latlon(&self, ix: usize, iy: usize) -> (f64, f64) {
        let (la, lo, _) = self.enu.to_geodetic([
            self.e0 + ix as f64 * self.step,
            self.n0 + iy as f64 * self.step,
            0.0,
        ]);
        (la, lo)
    }
}

fn make_grid(p: &MatchParams, region: &BBox, dem: &dyn Elevation) -> Grid {
    let (clat, clon) = region.center();
    let enu = Enu::new(clat, clon, 0.0);
    let (mlat, mlon) = m_per_deg(clat);
    let (he, hn) = (
        0.5 * (region.east - region.west) * mlon,
        0.5 * (region.north - region.south) * mlat,
    );
    let (ncols, nrows) = (
        (2.0 * he / p.cell_m).ceil() as usize + 1,
        (2.0 * hn / p.cell_m).ceil() as usize + 1,
    );
    let mut g = Grid {
        enu,
        ncols,
        nrows,
        e0: -he,
        n0: -hn,
        step: p.cell_m,
        up: Vec::new(),
        inside: Vec::new(),
    };
    let cells: Vec<(f32, bool)> = (0..ncols * nrows)
        .into_par_iter()
        .map(|i| {
            let (ix, iy) = (i % ncols, i / ncols);
            let (la, lo) = g.latlon(ix, iy);
            let z = dem.elev_m(la, lo);
            let e = g.enu.from_geodetic(la, lo, z + MIC_HEIGHT_M);
            (e[2] as f32, region.contains(la, lo))
        })
        .collect();
    g.up = cells.iter().map(|c| c.0).collect();
    g.inside = cells.iter().map(|c| c.1).collect();
    g
}

/// One flight's closest approach to one cell.
#[derive(Clone, Copy)]
struct Pass {
    /// Heard time less `t_base` (s).
    arrive: f32,
    tpl: u16,
}

/// Closest approaches of `tr` to every cell in reach, per chunk of track, so a flight that comes
/// back counts once per visit. A minimum on a chunk's edge belongs to the neighbouring chunk unless
/// the track really ends there.
fn stamp_track(
    tr: &TrackModel,
    g: &Grid,
    p: &MatchParams,
    c_at: &(dyn Fn(f64) -> f64 + Sync),
    t_base: f64,
    span: (f64, f64),
    out: &mut Vec<(u32, Pass)>,
) {
    let (ta, tb) = (tr.t0().max(span.0), tr.t1().min(span.1));
    if tb - ta < 2.0 * p.sample_s {
        return;
    }
    let reach = p.reach_m;
    let rc = (reach / g.step).ceil() as i64;
    let mut chunk_start = ta;
    loop {
        let chunk_end = (chunk_start + p.chunk_s).min(tb);
        let n = (((chunk_end - chunk_start) / p.sample_s).ceil() as usize).max(2);
        let times: Vec<f64> = (0..=n)
            .map(|i| chunk_start + (chunk_end - chunk_start) * i as f64 / n as f64)
            .collect();
        let pts: Vec<V3> = times
            .iter()
            .map(|&t| g.enu.from_ecef(tr.pos_at(t)))
            .collect();
        let (mut x0, mut x1, mut y0, mut y1) = (i64::MAX, i64::MIN, i64::MAX, i64::MIN);
        for q in &pts {
            let (ix, iy) = (
                ((q[0] - g.e0) / g.step).round() as i64,
                ((q[1] - g.n0) / g.step).round() as i64,
            );
            x0 = x0.min(ix - rc);
            x1 = x1.max(ix + rc);
            y0 = y0.min(iy - rc);
            y1 = y1.max(iy + rc);
        }
        let (x0, x1, y0, y1) = (
            x0.max(0),
            x1.min(g.ncols as i64 - 1),
            y0.max(0),
            y1.min(g.nrows as i64 - 1),
        );
        if x0 <= x1 && y0 <= y1 {
            let (w, h) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
            let mut best = vec![(f32::INFINITY, 0u32); w * h];
            for (i, q) in pts.iter().enumerate() {
                let (ix, iy) = (
                    ((q[0] - g.e0) / g.step).round() as i64,
                    ((q[1] - g.n0) / g.step).round() as i64,
                );
                for yy in (iy - rc).max(y0)..=(iy + rc).min(y1) {
                    let dy = g.n0 + yy as f64 * g.step - q[1];
                    let row = (yy - y0) as usize * w;
                    let cell_row = yy as usize * g.ncols;
                    for xx in (ix - rc).max(x0)..=(ix + rc).min(x1) {
                        let dx = g.e0 + xx as f64 * g.step - q[0];
                        let dz = g.up[cell_row + xx as usize] as f64 - q[2];
                        let d2 = (dx * dx + dy * dy + dz * dz) as f32;
                        let b = &mut best[row + (xx - x0) as usize];
                        if d2 < b.0 {
                            *b = (d2, i as u32);
                        }
                    }
                }
            }
            let last = pts.len() - 1;
            let (ends_a, ends_b) = (chunk_start <= tr.t0() + 1.0, chunk_end >= tr.t1() - 1.0);
            // chunks overlap; a closest approach counts only in the chunk whose middle holds it
            let trim = 0.5 * (p.chunk_s - p.chunk_step_s);
            let own_lo = if chunk_start <= ta {
                f64::NEG_INFINITY
            } else {
                chunk_start + trim
            };
            let own_hi = if chunk_end >= tb {
                f64::INFINITY
            } else {
                chunk_start + trim + p.chunk_step_s
            };
            for yy in y0..=y1 {
                for xx in x0..=x1 {
                    let (d2, i) = best[(yy - y0) as usize * w + (xx - x0) as usize];
                    let i = i as usize;
                    if !d2.is_finite()
                        || d2 > (reach * reach) as f32
                        || (i == 0 && !ends_a)
                        || (i == last && !ends_b)
                    {
                        continue;
                    }
                    if times[i] < own_lo || times[i] >= own_hi {
                        continue;
                    }
                    let cell = yy as usize * g.ncols + xx as usize;
                    if !g.inside[cell] {
                        continue;
                    }
                    let x = [
                        g.e0 + xx as f64 * g.step,
                        g.n0 + yy as f64 * g.step,
                        g.up[cell] as f64,
                    ];
                    // the closest point on the two segments around the best sample
                    let (mut bd2, mut bt) = (d2 as f64, times[i]);
                    for (a, b) in [(i.saturating_sub(1), i), (i, (i + 1).min(last))] {
                        if a == b {
                            continue;
                        }
                        let s = sub(pts[b], pts[a]);
                        let ss: f64 = s.iter().map(|v| v * v).sum();
                        if ss <= 0.0 {
                            continue;
                        }
                        let r = sub(x, pts[a]);
                        let u = (r[0] * s[0] + r[1] * s[1] + r[2] * s[2]) / ss;
                        let u = u.clamp(0.0, 1.0);
                        let q = [
                            pts[a][0] + u * s[0],
                            pts[a][1] + u * s[1],
                            pts[a][2] + u * s[2],
                        ];
                        let e = sub(q, x);
                        let dd: f64 = e.iter().map(|v| v * v).sum();
                        if dd < bd2 {
                            bd2 = dd;
                            bt = times[a] + u * (times[b] - times[a]);
                        }
                    }
                    let d = bd2.sqrt();
                    let (_, vel) = tr.pv(bt);
                    let v = norm(vel);
                    if v < 15.0 || v > p.v_grid.last().copied().unwrap_or(200.0) * 1.15 {
                        continue;
                    }
                    let c = c_at(bt);
                    let scale = BASE_C / c;
                    let tv = nearest_log(&p.v_grid, v * scale);
                    let td = nearest_log(&p.d_grid, d * scale);
                    out.push((
                        cell as u32,
                        Pass {
                            arrive: (bt + d / c - t_base) as f32,
                            tpl: (tv * p.d_grid.len() + td) as u16,
                        },
                    ));
                }
            }
        }
        if chunk_end >= tb {
            break;
        }
        chunk_start += p.chunk_step_s;
    }
}

/// Probability of hearing a pass at slant `d` (logistic in dB around `r50`).
fn p_heard(p: &MatchParams, d: f64) -> f64 {
    let margin = -20.0 * (d.max(1.0) / p.r50_m).log10();
    let k = std::f64::consts::PI / (3f64.sqrt() * p.sigma_db);
    p.p_cap / (1.0 + (-margin * k).exp())
}

/// Log evidence of one pass: ln(1 - P + P e), with e at least `e_floor`. A close pass at chance costs
/// about ln(1 - P (1 - e_floor)); a far one costs nothing either way.
fn pass_value(p: &MatchParams, nats: f64, d: f64) -> f64 {
    let pd = p_heard(p, d);
    let ln_e = if nats < 1.0 {
        p.e_floor.ln()
    } else {
        ln_e_value(p, nats).max(p.e_floor.ln())
    };
    ln_add((1.0 - pd).ln(), pd.ln() + ln_e)
}

struct Exact {
    evidence: f64,
    clock: f64,
    passes: Vec<MatchedPass>,
    n_heard: usize,
    n_close: usize,
}

/// Evidence at a point with exact curves (real trajectory, emission-time solve, modelled wind and
/// sound speed), over a small clock sweep shared by all passes. `None` if no pass is in reach.
fn score_exact(
    inp: &MatchInputs<'_>,
    p: &MatchParams,
    bank: &Bank,
    idx: &[usize],
    lat: f64,
    lon: f64,
    clock0: f64,
) -> Option<Exact> {
    let im = inp.image;
    let z = inp.fine_dem.elev_m(lat, lon);
    let x = geodetic_to_ecef(lat, lon, z + MIC_HEIGHT_M);
    let enu = Enu::new(lat, lon, z + MIC_HEIGHT_M);
    let (lo, hi) = (clock0 - 150.0, clock0 + inp.duration_s + 150.0);
    let kmax = p.clock_sweep_frames;
    let slack = p.pass_slack_frames;
    let span = kmax + slack;
    struct One {
        tr: usize,
        table: Vec<(f32, u16)>,
        d: f64,
        v: f64,
        arrive: f64,
        null: Null,
        at_end: bool,
        f_arr: i64,
        /// Row offset at each frame of the middle half of the path; `i32::MIN` where unsolved.
        core: Vec<i32>,
    }
    let v_cap = p.v_grid.last().copied().unwrap_or(200.0) * 1.15;
    let mut ones: Vec<One> = Vec::new();
    let (mut a, mut b) = (vec![0f32; bank.nf0], vec![0f32; bank.nf0]);
    for &ti in idx {
        let tr = &inp.tracks[ti];
        let (ta, tb) = (tr.t0().max(lo), tr.t1().min(hi));
        if tb - ta < 2.0 * p.sample_s {
            continue;
        }
        // local minima of range along the track
        let n = ((tb - ta) / p.sample_s).ceil() as usize;
        let ds: Vec<f64> = (0..=n)
            .map(|i| norm(sub(tr.pos_at(ta + (tb - ta) * i as f64 / n as f64), x)))
            .collect();
        for i in 0..=n {
            let left = i == 0 || ds[i] <= ds[i - 1];
            let right = i == n || ds[i] < ds[i + 1];
            if !(left && right) || ds[i] > p.reach_m {
                continue;
            }
            let te = ta + (tb - ta) * i as f64 / n as f64;
            let Some(cpa) = tr.cpa_near(x, te, 2.0 * p.sample_s) else {
                continue;
            };
            if cpa.speed < 15.0 || cpa.speed > v_cap || cpa.d > p.reach_m {
                continue;
            }
            let (_, _, h) = ecef_to_geodetic(tr.pos_at(cpa.t_e));
            let m = inp.atmo.medium(lat, lon, cpa.t_e, z + MIC_HEIGHT_M, h);
            let prop = Prop::from_medium(&m, &enu);
            let arrive = cpa.t_r_p(&prop);
            let aud = arrive - clock0 - im.t0;
            if aud < -30.0 || aud > inp.duration_s + 30.0 {
                continue;
            }
            let half = (p.half_k * cpa.d / cpa.speed).clamp(p.half_min_s, p.half_max_s);
            let scale = BASE_C / m.c;
            let tv = nearest_log(&p.v_grid, cpa.speed * scale);
            let td = nearest_log(&p.d_grid, cpa.d * scale);
            let null = bank.surfaces[tv * p.d_grid.len() + td].null;
            // the curve is fixed in UNIX time; a clock step only moves it along the frames
            let nj = (half / im.dt).round() as i64;
            let f_arr = (aud / im.dt).round() as i64;
            let mut pth: Vec<Sample> = Vec::with_capacity((2 * nj + 1) as usize);
            let mut hint = 0usize;
            let mut warm = None;
            for j in -nj..=nj {
                let t_r = clock0 + im.t0 + (f_arr + j) as f64 * im.dt;
                if let Some((t_e, r, ratio)) =
                    tr.solve_emission_sweep(t_r, x, &prop, warm, &mut hint, 1e-4)
                {
                    warm = Some(t_e);
                    pth.push(Sample {
                        dt: j as i32,
                        dr: (im.bpo * ratio.log2()).round() as i32,
                        w: (p.r_ref_m / r).min(1.0) as f32,
                        approach: t_r < arrive,
                    });
                } else {
                    warm = None;
                }
            }
            let full = path(p, cpa.speed * scale, cpa.d * scale, im.dt, im.bpo)
                .iter()
                .fold((0f32, 0f32), |acc, s| {
                    if s.approach {
                        (acc.0 + s.w * s.w, acc.1)
                    } else {
                        (acc.0, acc.1 + s.w * s.w)
                    }
                });
            let tone = bank.tone_of(&tr.typ).map(|q| (q, null.beta));
            let table: Vec<(f32, u16)> = (-span..=span)
                .map(|k| {
                    sweep_tau(
                        im,
                        &pth,
                        f_arr + k,
                        bank.r0,
                        bank.nf0,
                        p.stat,
                        &mut a,
                        &mut b,
                        full,
                        tone,
                    )
                    .map_or((f32::NEG_INFINITY, 0), |(s, r)| (s, r as u16))
                })
                .collect();
            let nc = nj / 2;
            let mut core = vec![i32::MIN; (2 * nc + 1) as usize];
            for s in &pth {
                let j = s.dt as i64;
                if j.abs() <= nc {
                    core[(j + nc) as usize] = s.dr;
                }
            }
            ones.push(One {
                tr: ti,
                table,
                d: cpa.d,
                v: cpa.speed,
                arrive,
                null,
                at_end: !cpa.interior,
                f_arr,
                core,
            });
        }
    }
    if ones.is_empty() {
        return None;
    }
    // Row differences at which two passes share pixels through any pair of their harmonics.
    let shifts: Vec<i32> = p
        .harmonics
        .iter()
        .map(|&(h, _)| (im.bpo * (h as f64).log2()).round() as i32)
        .collect();
    let dmax = shifts.iter().copied().max().unwrap_or(0) + 2;
    let mut same_pixels = vec![false; (2 * dmax + 1) as usize];
    for &sa in &shifts {
        for &sb in &shifts {
            for e in -1..=1 {
                same_pixels[(sa - sb + e + dmax) as usize] = true;
            }
        }
    }
    let pens: Vec<f64> = (-slack..=slack)
        .map(|j| {
            let z = j as f64 * im.dt / p.pass_clock_sigma_s;
            1.5 * (1.0 + 0.5 * z * z).ln()
        })
        .collect();
    // one pass's value at shared clock step k, and the step it used. A pass with no usable frames,
    // or at a track end, says nothing when it is silent.
    let value_at = |o: &One, k: i64| -> (f64, i64) {
        let mut best = (f64::NEG_INFINITY, k);
        for (jj, j) in (-slack..=slack).enumerate() {
            let (s, _) = o.table[(k + j + span) as usize];
            let nats = if s.is_finite() {
                o.null.nats(s) as f64
            } else {
                0.0
            };
            let v = if !s.is_finite() || (o.at_end && nats < 1.0) {
                0.0
            } else {
                pass_value(p, nats, o.d) - pens[jj]
            };
            if v > best.0 {
                best = (v, k + j);
            }
        }
        best
    };
    // Total value at clock k, one audio event to one flight: each pass credited, best first, claims
    // its pixels, and a later pass whose curve lies mostly on claimed pixels earns nothing for them.
    // Weak passes claim too, or ten faint copies of one sound would each be credited. An unheard close
    // pass keeps its cost, because it was not heard whoever else is claiming the audio.
    let assign = |k: i64| -> (f64, Vec<(f64, i64, bool)>) {
        let mut vals: Vec<(f64, i64, bool)> = ones
            .iter()
            .map(|o| {
                let (v, at) = value_at(o, k);
                (v, at, false)
            })
            .collect();
        let mut order: Vec<usize> = (0..ones.len()).filter(|&i| vals[i].0 > 0.0).collect();
        order.sort_by(|&x, &y| vals[y].0.total_cmp(&vals[x].0));
        // (first frame, row of tone, index into ones) of each pass that has claimed pixels
        let mut claims: Vec<(i64, i64, usize)> = Vec::new();
        for &i in &order {
            let o = &ones[i];
            let at = vals[i].1;
            let nc = (o.core.len() as i64 - 1) / 2;
            let first = o.f_arr + at - nc;
            let row = o.table[(at + span) as usize].1 as i64;
            let (mut shared, mut total) = (0usize, 0usize);
            for (q, &dr) in o.core.iter().enumerate() {
                if dr == i32::MIN {
                    continue;
                }
                total += 1;
                let frame = first + q as i64;
                let here = row + dr as i64;
                let hit = claims.iter().any(|&(f2, r2, c)| {
                    let q2 = frame - f2;
                    let other = &ones[c].core;
                    if q2 < 0 || q2 >= other.len() as i64 || other[q2 as usize] == i32::MIN {
                        return false;
                    }
                    let d = here - (r2 + other[q2 as usize] as i64);
                    d.abs() <= dmax as i64 && same_pixels[(d + dmax as i64) as usize]
                });
                shared += usize::from(hit);
            }
            if total > 0 && shared as f64 >= p.same_event_share * total as f64 {
                vals[i].0 = 0.0;
                vals[i].2 = true;
            } else {
                claims.push((first, row, i));
            }
        }
        (vals.iter().map(|v| v.0).sum(), vals)
    };
    let mut best = (f64::NEG_INFINITY, 0i64);
    for k in -kmax..=kmax {
        let (sum, _) = assign(k);
        if sum > best.0 {
            best = (sum, k);
        }
    }
    let k = best.1;
    let clock = clock0 - k as f64 * im.dt;
    let mut passes = Vec::with_capacity(ones.len());
    let (mut n_heard, mut n_close) = (0, 0);
    let (_, assigned) = assign(k);
    for (o, &(value, at, shared)) in ones.iter().zip(&assigned) {
        let (s, r) = o.table[(at + span) as usize];
        let usable = s.is_finite();
        let nats = if usable { o.null.nats(s) as f64 } else { 0.0 };
        if nats >= 6.9 && !shared {
            n_heard += 1;
        } else if usable && !o.at_end && o.d <= 2000.0 && nats < 1.0 {
            n_close += 1;
        }
        let tr = &inp.tracks[o.tr];
        passes.push(MatchedPass {
            icao: tr.icao.clone(),
            reg: tr.reg.clone(),
            typ: tr.typ.clone(),
            key: tr.key.clone(),
            t_audio: o.arrive - clock,
            slant_m: o.d,
            speed_mps: o.v,
            f0_hz: im.f_lo * ((bank.r0 + r as usize) as f64 / im.bpo).exp2(),
            score: s as f64,
            nats,
            value,
            at_track_end: o.at_end,
            clock_offset_s: (k - at) as f64 * im.dt,
            shared,
        });
    }
    passes.sort_by(|x, y| y.value.total_cmp(&x.value));
    Some(Exact {
        evidence: best.0,
        clock,
        passes,
        n_heard,
        n_close,
    })
}

/// Refine a coarse cell: scan the disc it stands for, then climb on successively finer local grids,
/// moving the clock with the best point.
///
/// A start stands for every place within `nms_m` of it, because no other start is kept there. The
/// coarse vote picks that start, and the vote can favour a broad bump over a sharper peak elsewhere
/// in the disc. So every point of the disc on the first (coarsest) step is scored first
/// (`disc_offsets`). Then, at each scale, the 5x5 grid moves to its best point until none beats
/// the centre, for at most `refine_moves` moves. Before both, one 5x5 move per scale reached 350 m
/// on each axis. On osprey 11-02 that left the answer on a broad bump 0.61 km from the cam, while a
/// sharper peak in the same disc, near the cam, scored about 2 nats more. Each decoy window runs
/// this same code (`search_window`), so a better search does not flatter the real window.
fn refine_point(
    inp: &MatchInputs<'_>,
    p: &MatchParams,
    bank: &Bank,
    idx: &[usize],
    lat: f64,
    lon: f64,
    clock: f64,
) -> Option<(f64, f64, Exact)> {
    let mut cur = (lat, lon, score_exact(inp, p, bank, idx, lat, lon, clock)?);
    // The whole disc the start stands for, on the first (coarsest) step, before any climbing: the
    // coarse vote picks one cell per disc, and a sharper peak elsewhere in it can sit across a
    // valley. As in the climb, each point sweeps the clock around the start's (`clock_sweep_frames`) and
    // the best point brings its own clock.
    if let Some(&step) = p.refine_steps_m.first() {
        let (mlat, mlon) = m_per_deg(lat);
        let (dla, dlo) = (step / mlat, step / mlon);
        let best = disc_offsets(p.nms_m, step)
            .into_par_iter()
            .filter_map(|(i, j)| {
                let (la, lo) = (lat + i as f64 * dla, lon + j as f64 * dlo);
                if !inp.region.contains(la, lo) {
                    return None;
                }
                score_exact(inp, p, bank, idx, la, lo, cur.2.clock).map(|e| (la, lo, e))
            })
            .max_by(|a, b| a.2.evidence.total_cmp(&b.2.evidence));
        if let Some(b) = best
            && b.2.evidence > cur.2.evidence
        {
            cur = b;
        }
    }
    for &step in &p.refine_steps_m {
        for _ in 0..refine_moves(p.nms_m, step) {
            let (mlat, mlon) = m_per_deg(cur.0);
            let (dla, dlo) = (step / mlat, step / mlon);
            let centre = (cur.0, cur.1, cur.2.clock);
            let best = (-2i32..=2)
                .flat_map(|i| (-2i32..=2).map(move |j| (i, j)))
                .collect::<Vec<_>>()
                .into_par_iter()
                .filter_map(|(i, j)| {
                    if i == 0 && j == 0 {
                        return None;
                    }
                    let (la, lo) = (centre.0 + i as f64 * dla, centre.1 + j as f64 * dlo);
                    if !inp.region.contains(la, lo) {
                        return None;
                    }
                    score_exact(inp, p, bank, idx, la, lo, centre.2).map(|e| (la, lo, e))
                })
                .max_by(|a, b| a.2.evidence.total_cmp(&b.2.evidence));
            match best {
                Some(b) if b.2.evidence > cur.2.evidence => cur = b,
                _ => break,
            }
        }
    }
    Some(cur)
}

/// Widest disc scanned before climbing, in steps: about 1,260 points.
const DISC_MAX_STEPS: i32 = 20;

/// Whole steps in the radius of the disc scanned before climbing: `nms_m` on `step_m`, at most
/// `DISC_MAX_STEPS`. `None` when there is no disc to scan.
fn disc_steps(nms_m: f64, step_m: f64) -> Option<i32> {
    (step_m > 0.0 && nms_m.is_finite() && nms_m >= step_m)
        .then(|| ((nms_m / step_m).floor() as i32).min(DISC_MAX_STEPS))
}

/// Grid offsets, in steps, of every point of that disc but its centre (the defaults give 316). The
/// radius is `nms_m / step_m` itself, not its whole part, and at most `DISC_MAX_STEPS`.
fn disc_offsets(nms_m: f64, step_m: f64) -> Vec<(i32, i32)> {
    let Some(n) = disc_steps(nms_m, step_m) else {
        return Vec::new();
    };
    let r2 = (nms_m / step_m).min(f64::from(DISC_MAX_STEPS)).powi(2);
    (-n..=n)
        .flat_map(|i| (-n..=n).map(move |j| (i, j)))
        .filter(|&(i, j)| (i != 0 || j != 0) && f64::from(i * i + j * j) <= r2)
        .collect()
}

/// Moves allowed at one refinement scale: enough for the 5x5 grid (two steps a move) to cross
/// `nms_m`, at least one and at most 100 (the defaults give 5, 10 and 20).
fn refine_moves(nms_m: f64, step_m: f64) -> usize {
    if step_m > 0.0 && nms_m.is_finite() && nms_m > 0.0 {
        ((nms_m / (2.0 * step_m)).ceil() as usize).clamp(1, 100)
    } else {
        1
    }
}

/// A mode from the coarse stage, or a window's best cell.
#[derive(Clone, Copy)]
struct Top {
    coarse: f32,
    cell: u32,
    bin: u32,
}

/// Decoy windows before and after the real one. The count stays the same near a day's edge, or at
/// the end of the archive, by taking what one side cannot hold from the other.
fn decoy_counts(
    p: &MatchParams,
    catalog: (f64, f64),
    start_lo: f64,
    start_hi: f64,
    duration_s: f64,
) -> (i64, i64) {
    let stride = (start_hi - start_lo).max(1.0) + p.decoy_guard_s;
    let each = (p.decoy_span_s / stride).floor();
    let room_lo = ((start_lo - catalog.0 - CATALOG_PAD_S) / stride)
        .floor()
        .max(0.0);
    let room_hi = ((catalog.1 - start_hi - duration_s - CATALOG_PAD_S) / stride)
        .floor()
        .max(0.0);
    let hi = (2.0 * each - each.min(room_lo)).min(room_hi);
    let lo = (2.0 * each - hi).min(room_lo);
    (lo as i64, hi as i64)
}

/// The ADS-B span a caller must fetch so that every decoy window has flights, kept inside
/// `limits` (the days already being fetched, and the end of the archive).
pub fn catalog_span(
    p: &MatchParams,
    start_lo: f64,
    start_hi: f64,
    duration_s: f64,
    limits: (f64, f64),
) -> (f64, f64) {
    let (n_lo, n_hi) = decoy_counts(p, limits, start_lo, start_hi, duration_s);
    let stride = (start_hi - start_lo).max(1.0) + p.decoy_guard_s;
    (
        (start_lo - n_lo as f64 * stride - 1200.0).max(limits.0),
        (start_hi + n_hi as f64 * stride + duration_s + 1200.0).min(limits.1),
    )
}

/// What the curve match lets an answer say.
#[derive(Clone, Debug, Serialize)]
pub struct CurveVerdict {
    pub stands: bool,
    pub lat: f64,
    pub lon: f64,
    pub clock_unix: f64,
    pub radius_m: f64,
    pub evidence: f64,
    pub margin: f64,
    pub n_decoys: usize,
    pub p: f64,
    pub n_heard: usize,
    pub why: String,
}

/// Grade one result. The margin line comes from recordings with no right answer: see
/// `ASSERT_MARGIN_NATS`. Places the evidence cannot rule out widen the radius, and an answer whose
/// radius would be wider than `MAX_ASSERT_RADIUS_M` does not stand.
pub fn verdict(r: &MatchResult) -> Option<CurveVerdict> {
    let top = r.modes.first()?;
    let (margin, p) = (r.decoy_margin?, r.decoy_p?);
    // within TIED_NATS, inclusive; a gap that is not a number counts as tied, so it cannot hide a place
    let radius_m = r
        .modes
        .iter()
        .skip(1)
        .filter(|m| {
            let gap = top.evidence - m.evidence;
            gap <= TIED_NATS || gap.is_nan()
        })
        .map(|m| haversine_m(top.lat, top.lon, m.lat, m.lon))
        .fold(RADIUS_FLOOR_M, f64::max);
    let enough = r.decoys.len() >= MIN_DECOYS;
    let finite = top.evidence.is_finite()
        && margin.is_finite()
        && r.decoys.iter().all(|w| w.best_evidence.is_finite());
    let clears = finite && margin > ASSERT_MARGIN_NATS;
    let tight = radius_m <= MAX_ASSERT_RADIUS_M;
    let why = format!(
        "{:.1} nats at this place and clock, {margin:+.1} over the best of {} decoy clocks (line {ASSERT_MARGIN_NATS:+.1}), {} flights heard, places within {TIED_NATS:.0} nats span {radius_m:.0} m{}",
        top.evidence,
        r.decoys.len(),
        top.n_heard,
        if enough {
            ""
        } else {
            "; too few decoy clocks to judge"
        },
    );
    Some(CurveVerdict {
        stands: enough && clears && tight,
        lat: top.lat,
        lon: top.lon,
        clock_unix: top.clock_unix,
        radius_m,
        evidence: top.evidence,
        margin,
        n_decoys: r.decoys.len(),
        p,
        n_heard: top.n_heard,
        why,
    })
}

/// Run the whole match. `say` reports progress.
/// Grid step of `SoundTable` (s).
const SOUND_STEP_S: f64 = 600.0;

/// Sound speed for the coarse stage, by time, on a grid of absolute times. A pass is stamped with
/// the value at its own time, so it gets the same template and arrival whichever window is the
/// prior: one speed taken at the prior's time would fit the real window better than the decoys.
struct SoundTable {
    k0: i64,
    c: Vec<f64>,
}

impl SoundTable {
    fn new(atmo: &dyn Atmosphere, lat: f64, lon: f64, span: (f64, f64)) -> Self {
        let k0 = (span.0 / SOUND_STEP_S).floor() as i64;
        let k1 = ((span.1 / SOUND_STEP_S).ceil() as i64).max(k0);
        let c = (k0..=k1)
            .map(|k| {
                atmo.medium(lat, lon, k as f64 * SOUND_STEP_S, 10.0, 1500.0)
                    .c
            })
            .collect();
        Self { k0, c }
    }

    fn at(&self, t: f64) -> f64 {
        let last = self.c.len() - 1;
        if last == 0 {
            return self.c[0];
        }
        let x = (t / SOUND_STEP_S - self.k0 as f64).clamp(0.0, last as f64);
        let i = (x.floor() as usize).min(last - 1);
        let f = x - i as f64;
        self.c[i] * (1.0 - f) + self.c[i + 1] * f
    }
}

pub fn run(inp: &MatchInputs<'_>, p: &MatchParams, say: &(dyn Fn(&str) + Sync)) -> MatchResult {
    let mut timings = Vec::new();
    let im = inp.image;
    let t = std::time::Instant::now();
    let bank = build_bank(im, p);
    timings.push(("bank".to_string(), t.elapsed().as_secs_f64()));
    let npk: usize = bank.surfaces.iter().map(|s| s.peaks.len()).sum();
    say(&format!(
        "curve match: bank of {} paths over {} frames, {} rows of tone; {:.1} peaks per path",
        bank.surfaces.len(),
        im.nt,
        bank.nf0,
        npk as f64 / bank.surfaces.len().max(1) as f64
    ));
    let t = std::time::Instant::now();
    let g = make_grid(p, &inp.region, inp.dem);
    let (clat, clon) = inp.region.center();
    // windows of clock: 0 is the prior, the rest are decoys either side
    let width = (inp.start_hi - inp.start_lo).max(2.0 * im.dt);
    let stride = width + p.decoy_guard_s;
    let (n_lo, n_hi) = decoy_counts(p, inp.catalog, inp.start_lo, inp.start_hi, inp.duration_s);
    let clock_lo = inp.start_lo - n_lo as f64 * stride;
    let span = (
        clock_lo - 300.0,
        inp.start_hi + n_hi as f64 * stride + inp.duration_s + 300.0,
    );
    let t_base = inp.start_lo;
    let sound = SoundTable::new(inp.atmo, clat, clon, span);
    let c_at = |t: f64| sound.at(t);
    let mut stamped: Vec<(u32, Pass)> = inp
        .tracks
        .par_iter()
        .fold(Vec::new, |mut v, tr| {
            stamp_track(tr, &g, p, &c_at, t_base, span, &mut v);
            v
        })
        .reduce(Vec::new, |mut a, mut b| {
            a.append(&mut b);
            a
        });
    stamped.par_sort_unstable_by_key(|s| s.0);
    let ncells = g.ncols * g.nrows;
    let mut offs = vec![0u32; ncells + 1];
    for s in &stamped {
        offs[s.0 as usize + 1] += 1;
    }
    for i in 0..ncells {
        offs[i + 1] += offs[i];
    }
    let passes: Vec<Pass> = stamped.iter().map(|s| s.1).collect();
    drop(stamped);
    timings.push(("stamp".to_string(), t.elapsed().as_secs_f64()));
    say(&format!(
        "curve match: {} cells, {} tracks, {} closest approaches, sound speed {:.1} to {:.1} m/s, {} decoy windows",
        ncells,
        inp.tracks.len(),
        passes.len(),
        sound.c.iter().copied().fold(f64::INFINITY, f64::min),
        sound.c.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        n_lo + n_hi
    ));
    // votes: each peak of a pass implies one clock
    let t = std::time::Instant::now();
    let dt = im.dt;
    let nbins = ((span.1 - inp.duration_s - 300.0 - clock_lo) / dt).ceil() as usize + 2;
    let window_of = |clock: f64| -> Option<i64> {
        let w = ((clock - inp.start_lo) / stride).floor() as i64;
        let off = clock - (inp.start_lo + w as f64 * stride);
        (off <= width && w >= -n_lo && w <= n_hi).then_some(w)
    };
    let bin_clock = |b: usize| clock_lo + b as f64 * dt;
    let bin_window: Vec<i32> = (0..nbins)
        .map(|b| window_of(bin_clock(b)).map_or(i32::MIN, |w| w as i32))
        .collect();
    let n_win = (n_lo + n_hi + 1) as usize;
    // Each window keeps its own list of cells with their best clock, so the real window and the
    // decoys go through one code path: a margin over decoys searched less hard would mean nothing.
    let per_window: Vec<Vec<Top>> = (0..ncells)
        .into_par_iter()
        .fold(
            || {
                (
                    vec![0f32; nbins],
                    Vec::<u32>::new(),
                    vec![Vec::<Top>::new(); n_win],
                )
            },
            |(mut acc, mut touched, mut lists), cell| {
                let rng = offs[cell] as usize..offs[cell + 1] as usize;
                if rng.is_empty() {
                    return (acc, touched, lists);
                }
                for ps in &passes[rng] {
                    let base = t_base + ps.arrive as f64 - im.t0 - clock_lo;
                    for &(tau, val) in &bank.surfaces[ps.tpl as usize].peaks {
                        let b = ((base - tau as f64 * dt) / dt).round();
                        if b >= 0.0 && (b as usize) < nbins {
                            let b = b as usize;
                            if acc[b] == 0.0 {
                                touched.push(b as u32);
                            }
                            acc[b] += val;
                        }
                    }
                }
                let mut per_win: Vec<(f32, u32)> = vec![(0.0, 0); n_win];
                for &b in &touched {
                    let v = acc[b as usize];
                    acc[b as usize] = 0.0;
                    let w = bin_window[b as usize];
                    if w == i32::MIN {
                        continue;
                    }
                    let wi = (w as i64 + n_lo) as usize;
                    if v > per_win[wi].0 {
                        per_win[wi] = (v, b);
                    }
                }
                touched.clear();
                for (wi, &(v, b)) in per_win.iter().enumerate() {
                    if v > 0.0 {
                        lists[wi].push(Top {
                            coarse: v,
                            cell: cell as u32,
                            bin: b,
                        });
                    }
                }
                (acc, touched, lists)
            },
        )
        .map(|(_, _, lists)| lists)
        .reduce(
            || vec![Vec::new(); n_win],
            |mut a, b| {
                for (x, mut y) in a.iter_mut().zip(b) {
                    x.append(&mut y);
                }
                a
            },
        );
    timings.push(("votes".to_string(), t.elapsed().as_secs_f64()));
    let cell_latlon = |cell: u32| g.latlon(cell as usize % g.ncols, cell as usize / g.ncols);
    // tracks worth solving exactly
    let t = std::time::Instant::now();
    let pad = inp.region.padded_km(p.reach_m / 1000.0 + 2.0);
    let keep: Vec<usize> = (0..inp.tracks.len())
        .into_par_iter()
        .filter(|&i| {
            let tr = &inp.tracks[i];
            tr.t.iter()
                .step_by(4)
                .zip(tr.pos.iter().step_by(4))
                .any(|(&tt, &q)| {
                    if tt < span.0 || tt > span.1 {
                        return false;
                    }
                    let (la, lo, _) = ecef_to_geodetic(q);
                    pad.contains(la, lo)
                })
        })
        .collect();
    // The whole search inside one clock window. Run once for the real window and once per decoy.
    let search_window = |list: &[Top]| -> Vec<MatchMode> {
        let mut list = list.to_vec();
        list.sort_unstable_by(|a, b| b.coarse.total_cmp(&a.coarse).then(a.cell.cmp(&b.cell)));
        let mut chosen: Vec<(Top, f64, f64)> = Vec::new();
        for t in list {
            let (la, lo) = cell_latlon(t.cell);
            if chosen
                .iter()
                .all(|m| haversine_m(m.1, m.2, la, lo) >= p.nms_m)
            {
                chosen.push((t, la, lo));
                if chosen.len() >= p.top_modes {
                    break;
                }
            }
        }
        let mut scored: Vec<(usize, Exact)> = chosen
            .par_iter()
            .enumerate()
            .filter_map(|(i, m)| {
                score_exact(inp, p, &bank, &keep, m.1, m.2, bin_clock(m.0.bin as usize))
                    .map(|e| (i, e))
            })
            .collect();
        scored.sort_by(|a, b| b.1.evidence.total_cmp(&a.1.evidence).then(a.0.cmp(&b.0)));
        let mut modes: Vec<MatchMode> = scored
            .into_par_iter()
            .enumerate()
            .filter_map(|(rank, (i, e))| {
                let m = &chosen[i];
                let (la, lo, e) = if rank < p.refine_modes {
                    refine_point(inp, p, &bank, &keep, m.1, m.2, bin_clock(m.0.bin as usize))?
                } else {
                    (m.1, m.2, e)
                };
                Some(MatchMode {
                    lat: la,
                    lon: lo,
                    clock_unix: e.clock,
                    coarse: m.0.coarse as f64,
                    evidence: e.evidence,
                    n_heard: e.n_heard,
                    n_unheard_close: e.n_close,
                    passes: e.passes,
                })
            })
            .collect();
        modes.sort_by(|a, b| b.evidence.total_cmp(&a.evidence));
        // climbing can bring two starts to one place; the runner-up must be somewhere else
        let mut distinct: Vec<MatchMode> = Vec::with_capacity(modes.len());
        for m in modes {
            if distinct
                .iter()
                .all(|k| haversine_m(k.lat, k.lon, m.lat, m.lon) >= p.nms_m)
            {
                distinct.push(m);
            }
        }
        distinct
    };
    let modes = search_window(&per_window[n_lo as usize]);
    timings.push(("exact".to_string(), t.elapsed().as_secs_f64()));
    // Diagnostic only, on recordings whose place is known: evidence at a given place, best over a
    // wide range of clocks, then after refining from it (`refine_point`: the `nms_m` disc around
    // it, then the climb). Never used to choose anything.
    let probe: Vec<MatchMode> = p
        .probe
        .iter()
        .flat_map(|&(la, lo, ck)| {
            let step = (2 * p.clock_sweep_frames + 1) as f64 * im.dt;
            let best = (-15i32..=15)
                .into_par_iter()
                .filter_map(|i| score_exact(inp, p, &bank, &keep, la, lo, ck + i as f64 * step))
                .max_by(|a, b| a.evidence.total_cmp(&b.evidence));
            let mut out = Vec::new();
            if let Some(e) = best {
                let clock = e.clock;
                let up = refine_point(inp, p, &bank, &keep, la, lo, clock);
                out.push(MatchMode {
                    lat: la,
                    lon: lo,
                    clock_unix: e.clock,
                    coarse: 0.0,
                    evidence: e.evidence,
                    n_heard: e.n_heard,
                    n_unheard_close: e.n_close,
                    passes: e.passes,
                });
                if let Some((a, b, e)) = up {
                    out.push(MatchMode {
                        lat: a,
                        lon: b,
                        clock_unix: e.clock,
                        coarse: 0.0,
                        evidence: e.evidence,
                        n_heard: e.n_heard,
                        n_unheard_close: e.n_close,
                        passes: e.passes,
                    });
                }
            }
            out
        })
        .collect();
    // the same treatment for the decoy windows: exact score at their best cells, refine the best
    let t = std::time::Instant::now();
    let decoys: Vec<DecoyWindow> = (0..n_win)
        .into_par_iter()
        .filter(|&wi| wi as i64 != n_lo)
        .map(|wi| {
            let shift_s = (wi as i64 - n_lo) as f64 * stride;
            match search_window(&per_window[wi]).first() {
                Some(m) => DecoyWindow {
                    shift_s,
                    best_coarse: m.coarse,
                    best_evidence: m.evidence,
                    lat: m.lat,
                    lon: m.lon,
                    n_heard: m.n_heard,
                },
                None => DecoyWindow {
                    shift_s,
                    best_coarse: 0.0,
                    best_evidence: 0.0,
                    lat: 0.0,
                    lon: 0.0,
                    n_heard: 0,
                },
            }
        })
        .collect();
    timings.push(("decoys".to_string(), t.elapsed().as_secs_f64()));
    let top = modes.first().map(|m| m.evidence);
    let decoy_best = decoys
        .iter()
        .map(|d| d.best_evidence)
        .fold(f64::NEG_INFINITY, f64::max);
    let decoy_margin = top.filter(|_| !decoys.is_empty()).map(|t| t - decoy_best);
    let decoy_p = top.filter(|_| !decoys.is_empty()).map(|t| {
        (1 + decoys.iter().filter(|d| d.best_evidence >= t).count()) as f64
            / (1 + decoys.len()) as f64
    });
    let mode_margin = modes.get(1).map(|m| modes[0].evidence - m.evidence);
    MatchResult {
        modes,
        decoys,
        decoy_margin,
        decoy_p,
        mode_margin,
        n_cells: ncells,
        n_tracks: inp.tracks.len(),
        n_passes: passes.len(),
        timings_s: timings,
        probe,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track::straight_track;
    use fdop_types::FlatElevation;
    use fdop_types::atmo::{Medium, UniformAtmosphere};

    /// xorshift Gaussian noise, so the test needs no dependency.
    fn noise(n: usize, seed: u64, amp: f64) -> Vec<f64> {
        let mut s = seed | 1;
        let mut u = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s as f64 / u64::MAX as f64).clamp(1e-12, 1.0)
        };
        (0..n)
            .map(|_| amp * (-2.0 * u().ln()).sqrt() * (2.0 * std::f64::consts::PI * u()).cos())
            .collect()
    }

    /// Adds a harmonic tone from `tr` heard at `x`, amplitude falling as 1/r, by phase integration.
    #[allow(clippy::too_many_arguments)]
    fn add_flight(
        buf: &mut [f64],
        sr: f64,
        clock: f64,
        tr: &TrackModel,
        x: V3,
        m: &Prop,
        f0: f64,
        amp: f64,
    ) {
        let (mut phase, mut hint) = (0.0f64, 0usize);
        let (mut warm, mut rr, mut rat) = (None, 1e9, 0.0);
        for (i, b) in buf.iter_mut().enumerate() {
            if i % 400 == 0 {
                let t_r = clock + i as f64 / sr;
                match tr.solve_emission_sweep(t_r, x, m, warm, &mut hint, 1e-5) {
                    Some((te, r, ratio)) => {
                        warm = Some(te);
                        rr = r;
                        rat = ratio;
                    }
                    None => {
                        warm = None;
                        rat = 0.0;
                    }
                }
            }
            if rat <= 0.0 {
                continue;
            }
            phase += 2.0 * std::f64::consts::PI * f0 * rat / sr;
            let a = amp * 1000.0 / rr;
            for h in 1..=4 {
                *b += a / h as f64 * (h as f64 * phase).sin();
            }
        }
    }

    struct Case {
        x: Vec<f32>,
        tracks: Vec<TrackModel>,
        region: BBox,
        lat: f64,
        lon: f64,
        clock: f64,
    }

    /// Three flights heard from one point, plus flights that are not heard at all, in noise.
    fn case(amp: f64, tone: bool) -> Case {
        let (lat, lon) = (40.8765, -73.525);
        let enu = Enu::new(lat, lon, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let clock = 1_788_895_000.0;
        let (sr, dur) = (2800.0, 900.0);
        let m = Prop::still(BASE_C);
        let mk = |p0: V3, v: V3, t0: f64, name: &str| {
            let mut t =
                straight_track(&enu, p0, v, clock + t0, clock - 1500.0, clock + 3000.0, 4.0);
            t.key = name.into();
            t.icao = name.into();
            t
        };
        let heard = vec![
            mk([900.0, 0.0, 700.0], [0.0, 70.0, 0.0], 200.0, "AAA"),
            mk([0.0, -1500.0, 900.0], [65.0, 0.0, 0.0], 450.0, "BBB"),
            mk([-1200.0, 900.0, 600.0], [55.0, 55.0, 0.0], 700.0, "CCC"),
        ];
        let silent = vec![
            mk([6000.0, 2000.0, 1000.0], [0.0, 80.0, 0.0], 300.0, "DDD"),
            mk([-5000.0, -3000.0, 1200.0], [75.0, 0.0, 0.0], 600.0, "EEE"),
        ];
        let n = (sr * dur) as usize;
        let mut buf = noise(n, 42, 0.05);
        for (i, tr) in heard.iter().enumerate() {
            add_flight(&mut buf, sr, clock, tr, x, &m, 90.0 + 25.0 * i as f64, amp);
        }
        if tone {
            // a stationary tone with a slow wobble, the classic false pass
            let mut ph = 0.0;
            for (i, b) in buf.iter_mut().enumerate() {
                let f = 180.0 + 0.8 * (i as f64 / sr / 40.0).sin();
                ph += 2.0 * std::f64::consts::PI * f / sr;
                *b += 0.2 * ph.sin();
            }
        }
        let mut tracks = heard;
        tracks.extend(silent);
        Case {
            x: buf.iter().map(|&v| v as f32).collect(),
            tracks,
            region: BBox::new(lat - 0.05, lon - 0.07, lat + 0.05, lon + 0.07),
            lat,
            lon,
            clock,
        }
    }

    fn params() -> MatchParams {
        MatchParams {
            v_grid: geometric(45.0, 1.3, 4),
            d_grid: geometric(300.0, 1.5, 8),
            decoy_span_s: 900.0,
            cell_m: 400.0,
            top_modes: 6,
            refine_modes: 2,
            ..Default::default()
        }
    }

    /// The case's sky again every 1000 s, `copies` times either side, so decoy windows see flights
    /// like the real one does. Silent copies: only the real window's audio was made.
    fn with_sky(mut c: Case, copies: i32) -> Case {
        let base = c.tracks.clone();
        for k in (-copies..=copies).filter(|&k| k != 0) {
            for tr in &base {
                let mut t2 = tr.clone();
                for t in &mut t2.t {
                    *t += 1000.0 * k as f64;
                }
                t2.key = format!("{}{k}", tr.key);
                t2.icao = t2.key.clone();
                c.tracks.push(t2);
            }
        }
        c
    }

    /// `params` with the production decoy span, so a verdict could stand.
    fn null_params() -> MatchParams {
        MatchParams {
            decoy_span_s: MatchParams::default().decoy_span_s,
            ..params()
        }
    }

    /// A run with no flight in its audio must not stand, judged with enough decoys to stand.
    fn assert_refused(r: &MatchResult) {
        assert!(r.decoys.len() >= MIN_DECOYS, "{} decoys", r.decoys.len());
        let v = verdict(r);
        assert!(
            !v.as_ref().is_some_and(|v| v.stands),
            "stands: margin {:?}, {v:?}",
            r.decoy_margin
        );
    }

    fn run_case(c: &Case, p: &MatchParams, lo: f64, hi: f64) -> MatchResult {
        run_case_in(c, p, lo, hi, (f64::NEG_INFINITY, f64::INFINITY))
    }

    fn run_case_in(
        c: &Case,
        p: &MatchParams,
        lo: f64,
        hi: f64,
        catalog: (f64, f64),
    ) -> MatchResult {
        let dem = FlatElevation(0.0);
        let atmo = UniformAtmosphere(Medium::still(BASE_C));
        let im = build_image(&c.x, 2800, p).unwrap();
        let inp = MatchInputs {
            image: &im,
            tracks: &c.tracks,
            region: c.region,
            start_lo: lo,
            start_hi: hi,
            duration_s: c.x.len() as f64 / 2800.0,
            catalog,
            dem: &dem,
            fine_dem: &dem,
            atmo: &atmo,
        };
        run(&inp, p, &|_| {})
    }

    #[test]
    fn three_flights_locate_the_observer_and_beat_every_decoy() {
        let c = case(0.5, false);
        let r = run_case(&c, &params(), c.clock - 60.0, c.clock + 60.0);
        let top = r.modes.first().expect("a mode");
        let err = haversine_m(top.lat, top.lon, c.lat, c.lon);
        assert!(err < 200.0, "error {err} m: {top:?}");
        assert!(
            (top.clock_unix - c.clock).abs() < 2.0,
            "clock off by {}",
            top.clock_unix - c.clock
        );
        assert!(top.n_heard >= 3, "{top:?}");
        assert!(
            r.decoy_margin.unwrap() > 5.0,
            "{:?} {:?}",
            r.decoy_margin,
            r.decoys
        );
    }

    /// A climb that starts 600 m from the observer (420 m north, 430 m west: past the 350 m on
    /// each axis one move per scale could reach) gets at least the evidence the observer's own
    /// point has, within the 200 m this scene locates to
    /// (`three_flights_locate_the_observer_and_beat_every_decoy`). The old refinement, one move per
    /// scale and no disc (both what `nms_m` 0 gives), stops short on both counts. The disc's points
    /// are checked against Gauss's lattice counts.
    #[test]
    fn refinement_climbs_from_a_start_beyond_one_move_per_scale() {
        let c = case(0.5, false);
        let p = params();
        let dem = FlatElevation(0.0);
        let atmo = UniformAtmosphere(Medium::still(BASE_C));
        let im = build_image(&c.x, 2800, &p).unwrap();
        let inp = MatchInputs {
            image: &im,
            tracks: &c.tracks,
            region: c.region,
            start_lo: c.clock - 60.0,
            start_hi: c.clock + 60.0,
            duration_s: c.x.len() as f64 / 2800.0,
            catalog: (f64::NEG_INFINITY, f64::INFINITY),
            dem: &dem,
            fine_dem: &dem,
            atmo: &atmo,
        };
        let bank = build_bank(&im, &p);
        let idx: Vec<usize> = (0..c.tracks.len()).collect();
        let (mlat, mlon) = m_per_deg(c.lat);
        let (la0, lo0) = (c.lat + 420.0 / mlat, c.lon - 430.0 / mlon);
        let start = haversine_m(la0, lo0, c.lat, c.lon);
        assert!(start > 590.0 && start < 610.0, "start {start} m");
        let e0 = score_exact(&inp, &p, &bank, &idx, la0, lo0, c.clock).expect("start scores");
        let (la, lo, e) =
            refine_point(&inp, &p, &bank, &idx, la0, lo0, c.clock).expect("climb scores");
        let at_obs = score_exact(&inp, &p, &bank, &idx, c.lat, c.lon, c.clock)
            .expect("observer scores")
            .evidence;
        let err = haversine_m(la, lo, c.lat, c.lon);
        assert!(
            err < 200.0,
            "ended {err} m from the observer, from {start} m"
        );
        assert!(
            e.evidence >= at_obs,
            "{} -> {}, observer {at_obs}",
            e0.evidence,
            e.evidence
        );
        let one = MatchParams {
            nms_m: 0.0,
            ..p.clone()
        };
        let (lb, ob, eb) =
            refine_point(&inp, &one, &bank, &idx, la0, lo0, c.clock).expect("climb scores");
        let err_one = haversine_m(lb, ob, c.lat, c.lon);
        assert!(
            err_one > 200.0 && eb.evidence < at_obs,
            "one move per scale reached it too: {err_one} m, {} vs {at_obs}",
            eb.evidence
        );
        assert_eq!(refine_moves(1000.0, 100.0), 5);
        assert_eq!(refine_moves(1000.0, 25.0), 20);
        assert_eq!(refine_moves(0.0, 100.0), 1);
        assert_eq!(refine_moves(f64::NAN, 100.0), 1);
        assert_eq!(refine_moves(1000.0, 1e-9), 100);
        assert_eq!(disc_steps(1000.0, 100.0), Some(10));
        assert_eq!(disc_steps(1e6, 100.0), Some(20));
        assert_eq!(disc_steps(50.0, 100.0), None);
        assert_eq!(disc_steps(0.0, 100.0), None);
        assert_eq!(disc_steps(f64::INFINITY, 100.0), None);
        assert_eq!(disc_steps(1000.0, 0.0), None);
        // lattice points in a circle of radius 10 and 20 (Gauss: 317 and 1,257), less the centre
        let d = disc_offsets(1000.0, 100.0);
        assert_eq!(d.len(), 316);
        assert!(!d.contains(&(0, 0)) && d.contains(&(10, 0)) && !d.contains(&(10, 1)));
        let wide = disc_offsets(1e6, 100.0);
        assert_eq!(wide.len(), 1256);
        assert!(wide.iter().all(|&(i, j)| i * i + j * j <= 400));
        // a radius of 10.5 steps keeps the points past 10 steps that its whole part would drop
        let half = disc_offsets(1050.0, 100.0);
        assert_eq!(half.len(), 348);
        assert!(half.contains(&(10, 3)) && !half.contains(&(10, 4)));
        assert!(disc_offsets(50.0, 100.0).is_empty());
        assert!(disc_offsets(1000.0, 0.0).is_empty());
    }

    /// Metamorphic: moving the whole catalogue in time by a whole number of frames moves the clock by
    /// the same amount and nothing else.
    #[test]
    fn shifting_all_flights_in_time_moves_only_the_clock() {
        let c = case(0.5, false);
        let shift = 300.0;
        let mut d = case(0.5, false);
        for tr in &mut d.tracks {
            for t in &mut tr.t {
                *t += shift;
            }
        }
        let p = params();
        let a = run_case(&c, &p, c.clock - 60.0, c.clock + 60.0);
        let b = run_case(&d, &p, c.clock + shift - 60.0, c.clock + shift + 60.0);
        let (ta, tb) = (&a.modes[0], &b.modes[0]);
        assert!((ta.lat - tb.lat).abs() < 1e-9 && (ta.lon - tb.lon).abs() < 1e-9);
        assert!(((tb.clock_unix - ta.clock_unix) - shift).abs() < 1e-3);
        assert!((ta.evidence - tb.evidence).abs() < 1e-6);
    }

    /// One search for every window: slide the flights by whole windows and the answer lands in a
    /// decoy, which must score what the real window scored. A decoy searched less hard would fail
    /// here, and the margin over it would flatter every recording.
    #[test]
    fn a_decoy_window_gets_the_search_the_real_window_gets() {
        let c = case(0.5, false);
        let p = params();
        let (lo, hi) = (c.clock - 60.0, c.clock + 60.0);
        let a = run_case(&c, &p, lo, hi);
        let shift = 4.0 * (hi - lo + p.decoy_guard_s);
        let mut d = case(0.5, false);
        for tr in &mut d.tracks {
            for t in &mut tr.t {
                *t += shift;
            }
        }
        let b = run_case(&d, &p, lo, hi);
        let w = b
            .decoys
            .iter()
            .find(|w| (w.shift_s - shift).abs() < 1e-6)
            .expect("that decoy");
        let top = &a.modes[0];
        assert!(
            (w.best_evidence - top.evidence).abs() < 1e-6,
            "decoy {} real {}",
            w.best_evidence,
            top.evidence
        );
        assert!(
            haversine_m(w.lat, w.lon, top.lat, top.lon) < 1.0,
            "{w:?} {top:?}"
        );
        assert!(
            b.decoy_margin.is_none_or(|m| m < -5.0),
            "{:?}",
            b.decoy_margin
        );
    }

    /// Near the end of a day the decoys come from the other side, and there are as many of them.
    #[test]
    fn decoys_fill_from_the_side_the_catalogue_covers() {
        let p = MatchParams {
            decoy_span_s: 7200.0,
            ..Default::default()
        };
        let (lo, hi, dur) = (1000.0, 1240.0, 600.0);
        let open = (f64::NEG_INFINITY, f64::INFINITY);
        assert_eq!(decoy_counts(&p, open, lo, hi, dur), (24, 24));
        // no room before the window at all
        assert_eq!(decoy_counts(&p, (lo, f64::INFINITY), lo, hi, dur), (0, 48));
        // room for 3 before, none after
        let c = (lo - 3.0 * 300.0 - CATALOG_PAD_S, hi + dur);
        assert_eq!(decoy_counts(&p, c, lo, hi, dur), (3, 0));
        // room for 10 before, 100 after
        let c = (
            lo - 10.0 * 300.0 - CATALOG_PAD_S,
            hi + dur + 100.0 * 300.0 + CATALOG_PAD_S,
        );
        assert_eq!(decoy_counts(&p, c, lo, hi, dur), (10, 38));
        // what the caller fetches gives back the same windows
        let limits = (
            lo - 3.0 * 300.0 - CATALOG_PAD_S,
            hi + dur + 100.0 * 300.0 + CATALOG_PAD_S,
        );
        let (a, b) = catalog_span(&p, lo, hi, dur, limits);
        assert_eq!(decoy_counts(&p, (a, b), lo, hi, dur), (3, 45));
        assert!(a >= limits.0 && b <= limits.1);
        // a real search keeps to the catalogue
        let c = case(0.5, false);
        let q = MatchParams {
            decoy_span_s: 900.0,
            ..params()
        };
        let (l, h) = (c.clock - 60.0, c.clock + 60.0);
        let r = run_case_in(&c, &q, l, h, (l - 200.0 - CATALOG_PAD_S, f64::INFINITY));
        assert_eq!(r.decoys.len(), 10);
        assert!(r.decoys.iter().all(|d| d.shift_s >= -200.0));
    }

    fn fake_result(margin: f64, n_decoys: usize, next: Option<(f64, f64)>) -> MatchResult {
        let mode = |lat: f64, evidence: f64| MatchMode {
            lat,
            lon: -73.5,
            clock_unix: 0.0,
            coarse: 0.0,
            evidence,
            n_heard: 3,
            n_unheard_close: 0,
            passes: vec![],
        };
        let mut modes = vec![mode(40.0, 30.0)];
        if let Some((km, nats)) = next {
            modes.push(mode(40.0 + km / 111.2, 30.0 - nats));
        }
        MatchResult {
            modes,
            decoys: vec![
                DecoyWindow {
                    shift_s: 300.0,
                    best_coarse: 0.0,
                    best_evidence: 30.0 - margin,
                    lat: 0.0,
                    lon: 0.0,
                    n_heard: 0
                };
                n_decoys
            ],
            decoy_margin: Some(margin),
            decoy_p: Some(1.0 / (1 + n_decoys) as f64),
            mode_margin: next.map(|n| n.1),
            n_cells: 1,
            n_tracks: 1,
            n_passes: 1,
            timings_s: vec![],
            probe: vec![],
        }
    }

    /// Each of the three conditions can refuse on its own, and a place the evidence cannot rule
    /// out widens the radius even when it does not refuse.
    #[test]
    fn a_verdict_needs_margin_decoys_and_one_place() {
        let m = ASSERT_MARGIN_NATS;
        let v = verdict(&fake_result(m + 1.0, 48, None)).unwrap();
        assert!(v.stands, "{v:?}");
        assert_eq!(v.radius_m, RADIUS_FLOOR_M);
        assert!(!verdict(&fake_result(m - 0.1, 48, None)).unwrap().stands);
        // a decoy that ties is a decoy that was not beaten
        assert!(!verdict(&fake_result(m, 48, None)).unwrap().stands);
        assert!(
            !verdict(&fake_result(m + 1.0, MIN_DECOYS - 1, None))
                .unwrap()
                .stands
        );
        // a rival place 3 km off, tied: stands with a wider radius
        let v = verdict(&fake_result(m + 1.0, 48, Some((3.0, 1.0)))).unwrap();
        assert!(v.stands && (v.radius_m - 3000.0).abs() < 50.0, "{v:?}");
        // the same place, clearly worse: does not count
        let v = verdict(&fake_result(m + 1.0, 48, Some((3.0, TIED_NATS + 0.5)))).unwrap();
        assert_eq!(v.radius_m, RADIUS_FLOOR_M);
        // tied 20 km off: refused
        assert!(
            !verdict(&fake_result(m + 1.0, 48, Some((20.0, 1.0))))
                .unwrap()
                .stands
        );
        // exactly TIED_NATS down is within TIED_NATS
        assert!(
            !verdict(&fake_result(m + 1.0, 48, Some((20.0, TIED_NATS))))
                .unwrap()
                .stands
        );
        // evidence that is not finite proves nothing, and a gap that is not a number is a tie
        let mut r = fake_result(f64::INFINITY, 48, None);
        r.modes[0].evidence = f64::INFINITY;
        assert!(!verdict(&r).unwrap().stands);
        let mut r = fake_result(m + 1.0, 48, Some((20.0, 1.0)));
        r.modes[1].evidence = f64::NAN;
        assert!(!verdict(&r).unwrap().stands);
    }

    /// A bright curve on quiet audio can score thousands of nats; its value must stay finite and
    /// keep growing, where exp() of it would overflow to infinity and tie every such place.
    #[test]
    fn huge_lookups_stay_finite() {
        let p = MatchParams::default();
        for nats in [710.0, 1500.0, 3000.0, 1e6] {
            let (v, pv) = (vote_value(&p, nats), pass_value(&p, nats, 700.0));
            assert!(v.is_finite() && pv.is_finite(), "{nats}: {v} {pv}");
        }
        assert!(pass_value(&p, 3000.0, 700.0) > pass_value(&p, 1500.0, 700.0) + 500.0);
        // and the small ones are what they were: ln(1 - P + P e)
        for nats in [1.0, 4.0, 30.0] {
            let pd = p_heard(&p, 700.0);
            let e = (p.kappa * ((1.0 - p.kappa) * nats).exp()).max(p.e_floor);
            let old = (1.0 - pd + pd * e).ln();
            assert!((pass_value(&p, nats, 700.0) - old).abs() < 1e-9, "{nats}");
        }
    }

    /// The coarse stage's sound speed at a pass's time does not depend on which window is the prior,
    /// and it follows the atmosphere.
    #[test]
    fn a_pass_gets_the_same_sound_speed_whichever_window_is_the_prior() {
        struct Drifting;
        impl Atmosphere for Drifting {
            fn medium(&self, _: f64, _: f64, t: f64, _: f64, _: f64) -> Medium {
                Medium::still(340.0 + 4.0 * (t / 3600.0).sin())
            }
            fn describe(&self) -> String {
                "drifting".into()
            }
        }
        let a = SoundTable::new(&Drifting, 40.0, -73.5, (1_000.0, 9_000.0));
        let b = SoundTable::new(&Drifting, 40.0, -73.5, (4_100.0, 20_000.0));
        for t in [4_200.0, 5_000.0, 7_777.7, 8_999.0] {
            assert_eq!(a.at(t), b.at(t), "{t}");
            let truth = Drifting.medium(0.0, 0.0, t, 0.0, 0.0).c;
            assert!((a.at(t) - truth).abs() < 0.05, "{t}: {} {truth}", a.at(t));
        }
    }

    /// The prior moves evidence between tones and adds none on average: its mean over the counted
    /// best rows is 1 for every entry, whatever those counts look like, including counts that sit on
    /// the entry's own band (the case where smoothing alone left a mean above 1).
    #[test]
    fn the_tone_prior_has_mean_one_under_chance_and_favours_its_bands() {
        let c = case(0.0, false);
        let p = params();
        let im = build_image(&c.x, 2800, &p).unwrap();
        let r0 = im.row_of(p.f0_lo).round().max(0.0) as usize;
        let nf0 = (im.row_of(p.f0_hi).round() as usize + 1 - r0).min(im.nu.saturating_sub(r0));
        let flat = vec![1u64; nf0];
        let low: Vec<u64> = (0..nf0)
            .map(|r| 1 + (nf0 - r) as u64 * (nf0 - r) as u64)
            .collect();
        let f_of = |r: usize| im.f_lo * ((r0 + r) as f64 / im.bpo).exp2();
        let mut sparse = vec![0u64; nf0];
        sparse[nf0 / 3] = 5;
        for (types, bands) in TONE_TABLE {
            let on_band: Vec<u64> = (0..nf0)
                .map(|r| {
                    let f = f_of(r);
                    let inside = bands.iter().any(|&(b, lo, hi)| {
                        f >= b * lo / 60.0 / TONE_BAND_SLACK && f <= b * hi / 60.0 * TONE_BAND_SLACK
                    });
                    if inside { 6 } else { 2 }
                })
                .collect();
            for counts in [&flat, &low, &on_band, &sparse] {
                let n: u64 = counts.iter().sum();
                let q = tone_ln_q(&im, r0, counts, bands, p.tone_off_q);
                let mean: f64 = q
                    .iter()
                    .zip(counts.iter())
                    .map(|(&v, &k)| (v as f64).exp() * k as f64 / n as f64)
                    .sum();
                assert!((mean - 1.0).abs() < 1e-5, "{types:?} mean {mean}");
            }
        }
        // on flat counts a band row is worth more than a row far above every band
        for (types, bands) in TONE_TABLE {
            let q = tone_ln_q(&im, r0, &flat, bands, p.tone_off_q);
            let row = |f: f64| ((im.row_of(f)).round() as usize) - r0;
            for &(b, lo, hi) in *bands {
                let mid = b * 0.5 * (lo + hi) / 60.0;
                assert!(q[row(mid)] > q[row(345.0)], "{types:?} at {mid} Hz");
            }
        }
    }

    /// The failure this prior had on real audio, in miniature. Low rumble makes the best rows of a
    /// flightless recording fall at low tones, on the propeller bands. Typing every flight must then
    /// lift no place over the decoys, and the best place by under 1.5 nats: dividing by a uniform
    /// distribution instead adds about 1.7 nats to every pass at every clock and lifts a place to
    /// about 10 nats on nothing but rumble.
    #[test]
    fn a_tone_prior_on_a_rumbling_recording_lifts_no_place_over_the_decoys() {
        let mut c = with_sky(case(0.0, false), 8);
        // rumble: noise low-passed to about 110 Hz, loud against the 0.05 background
        let mut y = 0f32;
        let rumble = noise(c.x.len(), 7, 1.5);
        for (b, n) in c.x.iter_mut().zip(rumble) {
            y += 0.22 * (n as f32 - y);
            *b += y;
        }
        let p = MatchParams {
            tone_prior: true,
            ..null_params()
        };
        let plain = run_case(&c, &p, c.clock - 60.0, c.clock + 60.0);
        for tr in &mut c.tracks {
            tr.typ = "C172".into();
        }
        let typed = run_case(&c, &p, c.clock - 60.0, c.clock + 60.0);
        let best = |r: &MatchResult| r.modes.first().map_or(0.0, |m| m.evidence);
        assert!(
            best(&typed) < best(&plain) + 1.5,
            "typed {} plain {}",
            best(&typed),
            best(&plain)
        );
        assert_refused(&plain);
        assert_refused(&typed);
    }

    /// A flight whose tone is one its type can make earns more than with no type; the same flight
    /// labelled with a type that cannot make that tone earns less, and the place holds in all three.
    /// This shows the direction only. The size is small here (0.79 nats over three flights when
    /// first run), because the flights' own tones raise the chance distribution at their rows.
    #[test]
    fn a_tone_its_type_can_make_earns_more() {
        // heard tones are 90, 115 and 140 Hz
        let label = |types: [&str; 3]| {
            let mut c = case(0.5, false);
            for (tr, t) in c.tracks.iter_mut().zip(types) {
                tr.typ = t.into();
            }
            let p = MatchParams {
                tone_prior: true,
                ..params()
            };
            let r = run_case(&c, &p, c.clock - 60.0, c.clock + 60.0);
            let top = r.modes.first().expect("a mode").clone();
            assert!(
                haversine_m(top.lat, top.lon, c.lat, c.lon) < 300.0,
                "{top:?}"
            );
            top.evidence
        };
        let none = label(["", "", ""]);
        let right = label(["C172", "SR22", "SR22"]);
        // every one of these bands misses its flight's tone (C208 would not: 4 blades reach 115 Hz)
        let wrong = label(["TBM9", "SF34", "C172"]);
        assert!(right > none, "right {right} none {none}");
        assert!(wrong < none, "wrong {wrong} none {none}");
    }

    /// In noise, typing every flight must not lift the real window over the decoys: they all get it.
    #[test]
    fn typing_every_flight_does_not_lift_noise_over_the_decoys() {
        let mut c = with_sky(case(0.0, false), 8);
        for tr in &mut c.tracks {
            tr.typ = "C172".into();
        }
        let p = MatchParams {
            tone_prior: true,
            ..null_params()
        };
        let r = run_case(&c, &p, c.clock - 60.0, c.clock + 60.0);
        assert_refused(&r);
    }

    /// A recording with no flight in it: no place may beat the decoys by a clear margin.
    #[test]
    fn noise_alone_does_not_stand_out_from_the_decoys() {
        let c = with_sky(case(0.0, false), 8);
        let r = run_case(&c, &null_params(), c.clock - 60.0, c.clock + 60.0);
        assert_refused(&r);
    }

    /// A strong wobbling tone adds no real evidence: the answer must not move to explain it.
    #[test]
    fn a_stationary_tone_does_not_move_the_answer() {
        let c = case(0.5, true);
        let r = run_case(&c, &params(), c.clock - 60.0, c.clock + 60.0);
        let top = r.modes.first().expect("a mode");
        assert!(
            haversine_m(top.lat, top.lon, c.lat, c.lon) < 300.0,
            "{top:?}"
        );
    }

    /// A second logged flight almost on top of a heard one must not double its credit.
    #[test]
    fn one_audio_event_cannot_credit_two_flights() {
        let c0 = case(0.5, false);
        let mut c1 = case(0.5, false);
        let mut shadow = c1.tracks[0].clone();
        shadow.key = "SHADOW".into();
        shadow.icao = "SHADOW".into();
        let enu = Enu::new(c1.lat, c1.lon, 0.0);
        for q in &mut shadow.pos {
            let e = enu.from_ecef(*q);
            *q = enu.to_ecef([e[0] + 60.0, e[1], e[2]]);
        }
        c1.tracks.push(shadow);
        let p = params();
        let a = run_case(&c0, &p, c0.clock - 60.0, c0.clock + 60.0);
        let b = run_case(&c1, &p, c1.clock - 60.0, c1.clock + 60.0);
        let shared = b.modes[0].passes.iter().filter(|q| q.shared).count();
        assert!(shared >= 1, "{:?}", b.modes[0]);
        assert!(
            b.modes[0].evidence < a.modes[0].evidence + 1.0,
            "{} vs {}",
            b.modes[0].evidence,
            a.modes[0].evidence
        );
    }

    /// Two flights heard at the same time on different tones are two events.
    #[test]
    fn two_flights_at_once_on_different_tones_both_count() {
        let (lat, lon) = (40.8765, -73.525);
        let enu = Enu::new(lat, lon, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let clock = 1_788_895_000.0;
        let (sr, dur) = (2800.0, 900.0);
        let m = Prop::still(BASE_C);
        let mk = |p0: V3, v: V3, t0: f64, name: &str| {
            let mut t =
                straight_track(&enu, p0, v, clock + t0, clock - 1500.0, clock + 3000.0, 4.0);
            t.key = name.into();
            t.icao = name.into();
            t
        };
        let tracks = vec![
            mk([900.0, 0.0, 700.0], [0.0, 70.0, 0.0], 450.0, "AAA"),
            mk([-1100.0, 0.0, 800.0], [0.0, -65.0, 0.0], 452.0, "BBB"),
            mk([0.0, -1500.0, 900.0], [65.0, 0.0, 0.0], 150.0, "CCC"),
        ];
        let mut buf = noise((sr * dur) as usize, 42, 0.05);
        // a ratio of 1.27 lies on no pair of harmonics up to 6, even a row either way
        for (tr, f0) in tracks.iter().zip([90.0, 114.5, 150.0]) {
            add_flight(&mut buf, sr, clock, tr, x, &m, f0, 0.5);
        }
        let c = Case {
            x: buf.iter().map(|&v| v as f32).collect(),
            tracks,
            region: BBox::new(lat - 0.05, lon - 0.07, lat + 0.05, lon + 0.07),
            lat,
            lon,
            clock,
        };
        let r = run_case(&c, &params(), c.clock - 60.0, c.clock + 60.0);
        let top = &r.modes[0];
        let credited: Vec<_> = top
            .passes
            .iter()
            .filter(|q| q.value > 1.0)
            .map(|q| q.icao.clone())
            .collect();
        assert!(
            credited.contains(&"AAA".to_string()) && credited.contains(&"BBB".to_string()),
            "{top:?}"
        );
    }

    /// Real flights sit up to about 3 s off the shared clock at a known site. Two seconds either way
    /// must not cost the answer. The allowance can only help, by construction; this test does not show
    /// it is needed (here it changes nothing), so whether to keep it is a question for real recordings.
    #[test]
    fn per_flight_time_errors_keep_the_answer() {
        let c0 = case(0.5, false);
        let mut c1 = case(0.5, false);
        for (tr, off) in c1.tracks.iter_mut().zip([2.0, -2.0, 2.0]) {
            for t in &mut tr.t {
                *t += off;
            }
        }
        let p = params();
        let strict = MatchParams {
            pass_slack_frames: 0,
            coarse_slack_frames: 0,
            ..params()
        };
        let r = run_case(&c1, &p, c1.clock - 60.0, c1.clock + 60.0);
        let top = r.modes.first().expect("a mode");
        assert!(
            haversine_m(top.lat, top.lon, c1.lat, c1.lon) < 300.0,
            "{top:?}"
        );
        let s = run_case(&c1, &strict, c1.clock - 60.0, c1.clock + 60.0);
        let base = run_case(&c0, &p, c0.clock - 60.0, c0.clock + 60.0);
        let strict_top = s.modes.first().map_or(f64::NEG_INFINITY, |m| m.evidence);
        println!(
            "clean {} slack {} strict {}",
            base.modes[0].evidence, top.evidence, strict_top
        );
        assert!(
            top.evidence >= strict_top - 1e-9,
            "slack {} strict {}",
            top.evidence,
            strict_top
        );
        assert!(
            top.evidence > 0.6 * base.modes[0].evidence,
            "offset {} clean {}",
            top.evidence,
            base.modes[0].evidence
        );
    }

    #[test]
    fn nulls_turn_scores_into_nonnegative_nats() {
        let n = Null {
            q50: 2.0,
            beta: 0.5,
        };
        assert_eq!(n.nats(-10.0), 0.0);
        assert!((n.nats(2.0) - std::f32::consts::LN_2).abs() < 1e-6);
        assert!((n.nats(2.0 + 0.5 * 5f32.ln()) - 10f32.ln()).abs() < 1e-5);
    }

    #[test]
    fn close_silence_costs_and_far_silence_does_not() {
        let p = MatchParams::default();
        assert!(pass_value(&p, 0.0, 400.0) < -1.0);
        assert!(pass_value(&p, 0.0, 7000.0).abs() < 0.1);
        assert!(pass_value(&p, 30.0, 700.0) > 10.0);
    }
}
