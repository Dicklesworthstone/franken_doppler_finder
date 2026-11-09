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

use crate::physics::{cv_radial, cv_ratio_rel_t0r, retarded_tau};
use crate::spectro::{median_f32, stft_band_db};
use crate::track::{Prop, TrackBounds, TrackModel};
use fdop_types::atmo::Atmosphere;
use fdop_types::geo::{
    Enu, V3, WGS84_A, WGS84_E2, ecef_to_geodetic, geodetic_to_ecef, haversine_m, m_per_deg, norm,
    sub,
};
use fdop_types::{BBox, Elevation, MIC_HEIGHT_M};
use rayon::prelude::*;
use serde::Serialize;
use std::sync::atomic::{AtomicUsize, Ordering};

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
/// A pass is heard at this many nats or more (about 1000 to 1).
const HEARD_NATS: f64 = 6.9;
/// `along_track_aliases` looks this far apart along each heard flight's track, out to
/// `ALIAS_REACH_M` either way. One tie past `MAX_ASSERT_RADIUS_M` is enough to stop an answer
/// standing, so the reach goes one step further.
const ALIAS_STEP_M: f64 = RADIUS_FLOOR_M;
const ALIAS_REACH_M: f64 = MAX_ASSERT_RADIUS_M + ALIAS_STEP_M;
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
    /// Slant ranges heard with half the capped probability. How far a recording hears depends on its
    /// microphone, wind and noise, and is not known before it is matched (the osprey cam's 10-31
    /// recording has 9- to 12-nat curves from passes 4 to 6 km out). So every place is scored at each
    /// range and keeps its best, in steps of 6 dB (the spread
    /// `sigma_db`); each decoy window profiles the same way. The longest is half of `reach_m`, so a
    /// pass the reach leaves out would be heard at most about one time in eight; a longer range
    /// would expect to hear passes it never looks at. Then the spread of source and path in dB,
    /// and the cap.
    pub r50_m: Vec<f64>,
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
            // Keep the original bank and add two 20% steps for faster subsonic passes.
            // The existing 15% trajectory allowance then includes the detector's fast seeds.
            v_grid: geometric(28.0, 1.2, 13),
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
            r50_m: vec![1000.0, 2000.0, 4000.0],
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
    /// Whitened, neighbour-expanded samples before harmonic summation, `[row][frame]`.
    /// Retaining the existing allocation costs one additional `nt * nu` f32 image. Manual
    /// images may omit it; without these samples, collided tones cannot gain independent credit.
    pub whitened: Option<Vec<f32>>,
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
    // These are received frequencies. Fast approach tones can exceed the old 2.2*f0_hi
    // crop; all these rows were already whitened. Emitted-tone hypotheses stay bounded
    // by f0_lo/f0_hi, and the sweep checks each tone's visible coverage independently.
    let nu = nr;
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
        whitened: Some(rows),
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
    /// Interpolate the fitted median and upper-tail quantile on the log(v, d) grid.
    /// Since q90 = q50 + beta * ln(5), the same convex weights on q50 and beta
    /// interpolate both fitted quantiles and keep the tail scale positive. Choosing
    /// the nearest null instead creates evidence jumps at artificial cell boundaries.
    fn null_at(&self, p: &MatchParams, v: f64, d: f64) -> Null {
        let (v0, v1, vw) = log_bracket(&p.v_grid, v);
        let (d0, d1, dw) = log_bracket(&p.d_grid, d);
        let at = |vi: usize, di: usize| self.surfaces[vi * p.d_grid.len() + di].null;
        if v0 == v1 && d0 == d1 {
            return at(v0, d0);
        }
        let (mut q50, mut beta) = (0.0, 0.0);
        for (vi, wv) in [(v0, 1.0 - vw), (v1, vw)] {
            for (di, wd) in [(d0, 1.0 - dw), (d1, dw)] {
                let weight = wv * wd;
                if weight > 0.0 {
                    let null = at(vi, di);
                    q50 += weight * f64::from(null.q50);
                    beta += weight * f64::from(null.beta);
                }
            }
        }
        Null {
            q50: q50 as f32,
            beta: beta as f32,
        }
    }

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

/// Bracket a positive coordinate on an ascending positive grid. Outside coordinates
/// clamp to an endpoint; an exact node (including a singleton grid) remains exact.
fn log_bracket(grid: &[f64], x: f64) -> (usize, usize, f64) {
    let hi = grid.partition_point(|&g| g < x);
    if hi == 0 {
        return (0, 0, 0.0);
    }
    if hi == grid.len() {
        return (hi - 1, hi - 1, 0.0);
    }
    if x == grid[hi] {
        return (hi, hi, 0.0);
    }
    let lo = hi - 1;
    let weight = ((x / grid[lo]).ln() / (grid[hi] / grid[lo]).ln()).clamp(0.0, 1.0);
    (lo, hi, weight)
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
            // u is reception time relative to the delayed CPA. Range, like the
            // Doppler ratio, must come from emission time so the bank null and
            // exact trajectory score use the same physical amplitude weights.
            let emitted = retarded_tau(u + d / BASE_C, 0.0, v, d, BASE_C);
            let (r, _) = cv_radial(emitted, v, d);
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

/// Reused sums and range differences for visible weight at each emitted tone.
/// Difference arrays add a sample's squared weight to a clipped row interval in O(1).
struct SweepBuffers {
    approach: Vec<f32>,
    recede: Vec<f32>,
    approach_w2_diff: Vec<f32>,
    recede_w2_diff: Vec<f32>,
    /// Empty on the ordinary path; allocated only when an owned peak needs another tone.
    profile: Vec<f32>,
}

impl SweepBuffers {
    fn new(nf0: usize) -> Self {
        Self {
            approach: vec![0.0; nf0],
            recede: vec![0.0; nf0],
            approach_w2_diff: vec![0.0; nf0 + 1],
            recede_w2_diff: vec![0.0; nf0 + 1],
            profile: Vec::new(),
        }
    }

    fn clear(&mut self) {
        self.approach.fill(0.0);
        self.recede.fill(0.0);
        self.approach_w2_diff.fill(0.0);
        self.recede_w2_diff.fill(0.0);
        self.profile.fill(f32::NEG_INFINITY);
    }
}

/// Sum one path over every emitted-tone row at arrival frame tau of im.
/// Each tone must retain 25% of the full squared weight on BOTH halves in the image.
/// A neighbouring tone falling outside the frequency band cannot discard its pixels.
/// The tone argument is the type's log prior per row and the path's null scale.
#[allow(clippy::too_many_arguments)]
#[inline]
fn sweep_tau(
    im: &CombImage,
    pth: &[Sample],
    tau: i64,
    r0: usize,
    nf0: usize,
    stat: Stat,
    scratch: &mut SweepBuffers,
    full: (f32, f32),
    tone: Option<(&[f32], f32)>,
) -> Option<(f32, usize)> {
    scratch.clear();
    let (mut wa, mut wb) = (0f32, 0f32);
    let mut clipped = false;
    for s in pth {
        let frame = tau + i64::from(s.dt);
        if frame < 0 || frame >= im.nt as i64 {
            continue;
        }
        let first = r0 as i64 + i64::from(s.dr);
        let lo = (-first).max(0) as usize;
        let hi = ((im.nu as i64 - first).max(0) as usize).min(nf0);
        if lo >= hi {
            continue;
        }
        let (acc, diff, weight) = if s.approach {
            (&mut scratch.approach, &mut scratch.approach_w2_diff, &mut wa)
        } else {
            (&mut scratch.recede, &mut scratch.recede_w2_diff, &mut wb)
        };
        let w2 = s.w * s.w;
        if lo == 0 && hi == nf0 {
            *weight += w2;
        } else {
            clipped = true;
            diff[lo] += w2;
            diff[hi] -= w2;
        }
        let off = frame as usize * im.nu + (first + lo as i64) as usize;
        let src = &im.comb[off..off + hi - lo];
        for (d, v) in acc[lo..hi].iter_mut().zip(src) {
            *d += s.w * v;
        }
    }
    // Most bank paths fit the whole tone range. Keep their original scalar arithmetic
    // and two square roots instead of normalizing every emitted row independently.
    let common = if clipped {
        None
    } else {
        if wa < 0.25 * full.0 || wb < 0.25 * full.1 {
            return None;
        }
        Some((1.0 / wa.sqrt(), 1.0 / wb.sqrt()))
    };
    let (mut extra_a, mut extra_b) = (0f32, 0f32);
    let (mut best, mut at, mut any) = (f32::NEG_INFINITY, 0, false);
    for (row, (a, b)) in scratch.approach.iter().zip(&scratch.recede).enumerate() {
        let (ra, rb) = if let Some(gains) = common {
            gains
        } else {
            extra_a += scratch.approach_w2_diff[row];
            extra_b += scratch.recede_w2_diff[row];
            let row_wa = (wa + extra_a).max(0.0);
            let row_wb = (wb + extra_b).max(0.0);
            if row_wa < 0.25 * full.0 || row_wb < 0.25 * full.1 {
                continue;
            }
            (1.0 / row_wa.sqrt(), 1.0 / row_wb.sqrt())
        };
        any = true;
        let mut s = stat_of(stat, a * ra, b * rb);
        if let Some((q, beta)) = tone {
            s += beta * q[row];
        }
        if !scratch.profile.is_empty() {
            scratch.profile[row] = s;
        }
        if s > best {
            best = s;
            at = row;
        }
    }
    any.then_some((best, at))
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
            let mut scratch = SweepBuffers::new(nf0);
            let mut score = vec![f32::NEG_INFINITY; im.nt];
            let mut rows = vec![0u64; nf0];
            for (tau, sc) in score.iter_mut().enumerate() {
                if let Some((s, r)) = sweep_tau(
                    im, &pth, tau as i64, r0, nf0, p.stat, &mut scratch, full, None,
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
    /// The heard range (`MatchParams::r50_m`) that evidence was best at.
    pub r50_m: f64,
    pub n_heard: usize,
    pub n_unheard_close: usize,
    pub passes: Vec<MatchedPass>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DecoyWindow {
    pub shift_s: f64,
    pub best_coarse: f64,
    pub best_evidence: f64,
    /// The heard range its best place took; 0 where the window found no place.
    pub r50_m: f64,
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
    /// The real window's best place moved along the tracks of the flights it heard, with the clock
    /// moved to match (`along_track_aliases`). They only widen the radius (`verdict`).
    pub aliases: Vec<MatchMode>,
}

struct Grid {
    enu: Enu,
    region: BBox,
    ncols: usize,
    nrows: usize,
    e0: f64,
    n0: f64,
    step: f64,
    /// Actual ENU observer positions, including terrain and microphone height. Removing the
    /// tangent plane's geodetic height changes east/north as well as up away from its origin.
    observers: Vec<V3>,
    /// Largest horizontal displacement from a lattice node to its physical observer. Expand
    /// the local index lookup by this measured bound before applying exact range checks.
    horizontal_shift_m: f64,
    /// Interior nodes and a one-cell boundary fringe projected into the requested region.
    inside: Vec<bool>,
}

impl Grid {
    fn projected_latlon(&self, ix: usize, iy: usize) -> (f64, f64) {
        let (la, lo, _) = self.enu.to_geodetic([
            self.e0 + ix as f64 * self.step,
            self.n0 + iy as f64 * self.step,
            0.0,
        ]);
        (la, lo)
    }

    fn latlon(&self, ix: usize, iy: usize) -> (f64, f64) {
        let (la, lo) = self.projected_latlon(ix, iy);
        (
            la.clamp(self.region.south, self.region.north),
            lo.clamp(self.region.west, self.region.east),
        )
    }
}

struct GridShape {
    ncols: usize,
    nrows: usize,
    e0: f64,
    n0: f64,
}

/// Conservative bounds of the whole geographic box on the existing tangent plane. A surface
/// point P and its geodetic normal n project to P + h*n where the local up coordinate is zero.
/// With D = n dot n0 and X = N(latitude)*sin(latitude), N+h = (R0 + e2*X*sin(lat0))/D.
/// Bound those factors over the full box rather than replacing curved edges by metre/degree
/// chords. The angular triangle inequality bounds D below without sampling its boundary.
fn projected_region_bounds(region: &BBox) -> Option<(f64, f64, f64, f64)> {
    if ![region.south, region.west, region.north, region.east]
        .into_iter()
        .all(f64::is_finite)
        || !(-90.0..=90.0).contains(&region.south)
        || !(-90.0..=90.0).contains(&region.north)
        || !(-180.0..=180.0).contains(&region.west)
        || !(-180.0..=180.0).contains(&region.east)
        || region.south > region.north
        || region.west > region.east
    {
        return None;
    }
    let (south, north) = (region.south.to_radians(), region.north.to_radians());
    let lat0 = 0.5 * (south + north);
    let half_lon = 0.5 * (region.east - region.west).to_radians();
    let angle = 0.5 * (north - south) + half_lon;
    if angle >= std::f64::consts::FRAC_PI_2 {
        return None;
    }
    let d_min = angle.cos();
    let (s0, c0) = lat0.sin_cos();
    let prime_radius = |s: f64| WGS84_A / (1.0 - WGS84_E2 * s * s).sqrt();
    let (sin_lo, sin_hi) = (south.sin(), north.sin());
    let (x_lo, x_hi) = (prime_radius(sin_lo) * sin_lo, prime_radius(sin_hi) * sin_hi);
    let x0 = prime_radius(s0) * s0;
    let r0 = prime_radius(s0) * (1.0 - WGS84_E2 * s0 * s0);
    let product = |a: (f64, f64), b: (f64, f64)| {
        let xs = [a.0 * b.0, a.0 * b.1, a.1 * b.0, a.1 * b.1];
        (
            xs.into_iter().fold(f64::INFINITY, f64::min),
            xs.into_iter().fold(f64::NEG_INFINITY, f64::max),
        )
    };
    let extra = product((WGS84_E2 * s0, WGS84_E2 * s0), (x_lo, x_hi));
    let radial = (r0 + extra.0, (r0 + extra.1) / d_min);
    let cos_lo = south.cos().min(north.cos());
    let cos_hi = if south <= 0.0 && north >= 0.0 {
        1.0
    } else {
        south.cos().max(north.cos())
    };
    let east = (radial.1 * cos_hi * half_lon.sin()).next_up();
    let normal_north = product((-s0, -s0), (cos_lo * half_lon.cos(), cos_hi));
    let northward = product(
        radial,
        (c0 * sin_lo + normal_north.0, c0 * sin_hi + normal_north.1),
    );
    let offset = product((-WGS84_E2 * c0, -WGS84_E2 * c0), (x_lo - x0, x_hi - x0));
    Some((
        -east,
        east,
        (northward.0 + offset.0).next_down(),
        (northward.1 + offset.1).next_up(),
    ))
}

fn grid_shape(p: &MatchParams, region: &BBox) -> Option<GridShape> {
    if !p.cell_m.is_finite() || p.cell_m <= 0.0 {
        return None;
    }
    let (e_lo, e_hi, n_lo, n_hi) = projected_region_bounds(region)?;
    let (clat, _) = region.center();
    let (mlat, mlon) = m_per_deg(clat);
    let (he, hn) = (
        0.5 * (region.east - region.west) * mlon,
        0.5 * (region.north - region.south) * mlat,
    );
    let axis = |lo: f64, hi: f64, half: f64| {
        // Keep the established lattice for ordinary regions. A narrow axis needs its centre:
        // geographic corner roundoff can otherwise leave even a one-cell-wide box empty.
        let origin = if half <= p.cell_m { 0.0 } else { -half };
        let first = ((lo - origin) / p.cell_m).floor();
        let last = ((hi - origin) / p.cell_m).ceil();
        let count = last - first + 1.0;
        (count.is_finite() && (1.0..=u32::MAX as f64).contains(&count))
            .then_some((origin + first * p.cell_m, count as usize))
    };
    let (e0, ncols) = axis(e_lo, e_hi, he)?;
    let (n0, nrows) = axis(n_lo, n_hi, hn)?;
    let cells = ncols.checked_mul(nrows)?;
    (cells <= u32::MAX as usize).then_some(GridShape {
        ncols,
        nrows,
        e0,
        n0,
    })
}

/// Exact rectangular allocation of the curve grid, including its geographic boundary fringe.
/// `None` means invalid dimensions, overflow, or a box too wide for one local tangent plane.
pub fn grid_cell_count(p: &MatchParams, region: &BBox) -> Option<usize> {
    grid_shape(p, region).map(|s| s.ncols * s.nrows)
}

fn make_grid(p: &MatchParams, region: &BBox, dem: &dyn Elevation) -> Grid {
    let (clat, clon) = region.center();
    let GridShape {
        ncols,
        nrows,
        e0,
        n0,
    } = grid_shape(p, region).expect("validated local grid");
    let mut g = Grid {
        enu: Enu::new(clat, clon, 0.0),
        region: *region,
        ncols,
        nrows,
        e0,
        n0,
        step: p.cell_m,
        observers: Vec::new(),
        horizontal_shift_m: 0.0,
        inside: Vec::new(),
    };
    let cells: Vec<(V3, bool, f64)> = (0..ncols * nrows)
        .into_par_iter()
        .map(|i| {
            let (ix, iy) = (i % ncols, i / ncols);
            let (raw_la, raw_lo) = g.projected_latlon(ix, iy);
            let (la, lo) = g.latlon(ix, iy);
            // A clipped geographic edge may cross between every row of a narrow box. Keep
            // its one-cell fringe on the boundary, rather than silently leaving that strip
            // unsearched. Far corners of the enclosing projected rectangle remain inactive.
            let inside = region.contains(raw_la, raw_lo)
                || haversine_m(raw_la, raw_lo, la, lo) <= p.cell_m * std::f64::consts::SQRT_2;
            if !inside {
                return ([0.0; 3], false, 0.0);
            }
            let z = dem.elev_m(la, lo);
            let e = g.enu.from_geodetic(la, lo, z + MIC_HEIGHT_M);
            let shift =
                (e[0] - (g.e0 + ix as f64 * g.step)).hypot(e[1] - (g.n0 + iy as f64 * g.step));
            (e, true, shift)
        })
        .collect();
    g.observers = cells.iter().map(|c| c.0).collect();
    g.inside = cells.iter().map(|c| c.1).collect();
    g.horizontal_shift_m = cells.iter().map(|c| c.2).fold(0.0, f64::max);
    g
}

/// One flight's closest approach to one cell.
#[derive(Clone, Copy)]
struct Pass {
    /// Heard time less `t_base` (s).
    arrive: f32,
    tpl: u16,
}

/// Local closest approaches of `tr` to every cell in reach, so a flight that comes back counts
/// once per visit even when several visits fit in one chunk. All chunks use the same sampling
/// lattice and read one extra sample at either edge; a minimum belongs to the chunk that owns its
/// central sample. This also keeps a shortened final chunk from moving a boundary pass between
/// chunks. Scratch is bounded by the chunk's positions, not by its cells or number of visits.
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
    let rc = ((reach + g.horizontal_shift_m) / g.step).ceil() as i64;
    let last_sample = ((tb - ta) / p.sample_s).ceil() as usize;
    let (ends_a, ends_b) = (ta <= tr.t0() + 1.0, tb >= tr.t1() - 1.0);
    let trim = 0.5 * (p.chunk_s - p.chunk_step_s);
    let mut chunk = 0usize;
    loop {
        let chunk_start = ta + chunk as f64 * p.chunk_step_s;
        let chunk_end = (chunk_start + p.chunk_s).min(tb);
        // The same global sample and its two neighbours must be compared in either chunk.
        // Re-spacing the last chunk's samples could give a pass no owner, or two owners.
        let first = (((chunk_start - ta) / p.sample_s).floor() as usize).saturating_sub(1);
        let last = (((chunk_end - ta) / p.sample_s).ceil() as usize)
            .saturating_add(1)
            .min(last_sample);
        let times: Vec<f64> = (first..=last)
            .map(|i| (ta + i as f64 * p.sample_s).min(tb))
            .collect();
        let pts: Vec<V3> = times
            .iter()
            .map(|&t| g.enu.from_ecef(tr.pos_at(t)))
            .collect();
        let own_lo = if chunk == 0 {
            f64::NEG_INFINITY
        } else {
            ta + trim + chunk as f64 * p.chunk_step_s
        };
        let own_hi = if chunk_end >= tb {
            f64::INFINITY
        } else {
            ta + trim + (chunk + 1) as f64 * p.chunk_step_s
        };
        for (i, q) in pts.iter().enumerate() {
            let sample = first + i;
            if times[i] < own_lo
                || times[i] >= own_hi
                || (sample == 0 && !ends_a)
                || (sample == last_sample && !ends_b)
            {
                continue;
            }
            let (ix, iy) = (
                ((q[0] - g.e0) / g.step).round() as i64,
                ((q[1] - g.n0) / g.step).round() as i64,
            );
            for yy in (iy - rc).max(0)..=(iy + rc).min(g.nrows as i64 - 1) {
                for xx in (ix - rc).max(0)..=(ix + rc).min(g.ncols as i64 - 1) {
                    let cell = yy as usize * g.ncols + xx as usize;
                    if !g.inside[cell] {
                        continue;
                    }
                    let x = g.observers[cell];
                    let dist2 = |q: V3| {
                        let (dx, dy, dz) = (x[0] - q[0], x[1] - q[1], x[2] - q[2]);
                        dx * dx + dy * dy + dz * dz
                    };
                    let d2 = dist2(*q) as f32;
                    if !d2.is_finite() {
                        continue;
                    }
                    let before = if sample == 0 {
                        f32::INFINITY
                    } else {
                        dist2(pts[i - 1]) as f32
                    };
                    // Equal minima belong to their first sample, as in the former global
                    // minimum. Neighbours are read even outside the current reach square.
                    if d2 >= before || before.is_nan() {
                        continue;
                    }
                    let after = if sample == last_sample {
                        f32::INFINITY
                    } else {
                        dist2(pts[i + 1]) as f32
                    };
                    if d2 > after || after.is_nan() {
                        continue;
                    }
                    // Interpolate squared range, not the two straight chords: on a turn the
                    // nearest point of a chord need not be near the actual CPA time. Its
                    // quadratic vertex is exact for a straight steady pass and uses the same
                    // three samples, including unequal spacing at a shortened final interval.
                    let mut bt = times[i];
                    if sample > 0 && sample < last_sample {
                        let (ha, hb) = (times[i] - times[i - 1], times[i + 1] - times[i]);
                        let centre = dist2(*q);
                        let a = (dist2(pts[i - 1]) - centre) / ha;
                        let b = (dist2(pts[i + 1]) - centre) / hb;
                        let curve = (a + b) / (ha + hb);
                        if curve > 0.0 && curve.is_finite() {
                            let slope = b - curve * hb;
                            let shift = -slope / (2.0 * curve);
                            if shift.is_finite() {
                                bt += shift.clamp(-ha, hb);
                            }
                        }
                    } else {
                        // At a real track boundary only one side exists; keep the former
                        // segment projection so a CPA just inside that first/last sample
                        // interval is not pinned to the endpoint.
                        let j = if sample == 0 { i + 1 } else { i - 1 };
                        let s = sub(pts[j], *q);
                        let ss: f64 = s.iter().map(|v| v * v).sum();
                        if ss > 0.0 && ss.is_finite() {
                            let r = sub(x, *q);
                            let u = (r[0] * s[0] + r[1] * s[1] + r[2] * s[2]) / ss;
                            if u.is_finite() {
                                bt += u.clamp(0.0, 1.0) * (times[j] - times[i]);
                            }
                        }
                    }
                    let (mut pos, mut vel) = tr.pv(bt);
                    // Squared range is not quadratic along a turning Hermite path. Keep
                    // the sampled point unless the actual proposed point is strictly closer.
                    let proposed_d2 = dist2(g.enu.from_ecef(pos));
                    if !proposed_d2.is_finite() || proposed_d2 >= dist2(*q) {
                        bt = times[i];
                        (pos, vel) = tr.pv(bt);
                    }
                    let d = dist2(g.enu.from_ecef(pos)).sqrt();
                    let v = norm(vel);
                    // The closest sample can be outside reach while the interpolated pass
                    // enters it. Apply the range gate to that pass, after finding its CPA.
                    if !d.is_finite()
                        || d > reach
                        || v < 15.0
                        || v > p.v_grid.last().copied().unwrap_or(200.0) * 1.15
                    {
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
        chunk += 1;
    }
}

/// Probability of hearing a pass at slant `d` (logistic in dB around `r50`).
fn p_heard(p: &MatchParams, r50: f64, d: f64) -> f64 {
    let margin = -20.0 * (d.max(1.0) / r50).log10();
    let k = std::f64::consts::PI / (3f64.sqrt() * p.sigma_db);
    p.p_cap / (1.0 + (-margin * k).exp())
}

/// Log evidence of one pass: ln(1 - P + P e), with e at least `e_floor`. A close pass at chance costs
/// about ln(1 - P (1 - e_floor)); a far one costs nothing either way.
fn pass_value(p: &MatchParams, r50: f64, nats: f64, d: f64) -> f64 {
    let pd = p_heard(p, r50, d);
    let ln_e = if nats < 1.0 {
        p.e_floor.ln()
    } else {
        ln_e_value(p, nats).max(p.e_floor.ln())
    };
    ln_add((1.0 - pd).ln(), pd.ln() + ln_e)
}

/// Timing penalties can make even a strong lookup contribute no evidence. Only positive-valued
/// passes participate in pixel ownership, so only they can count as independent heard flyovers.
fn counts_as_heard(value: f64, nats: f64, shared: bool) -> bool {
    value > 0.0 && nats >= HEARD_NATS && !shared
}

struct Exact {
    evidence: f64,
    r50_m: f64,
    clock: f64,
    passes: Vec<MatchedPass>,
    n_heard: usize,
    n_close: usize,
}

/// The existing regional shortlist, with geometry bounds built once for every real/decoy window,
/// clock alternative, refinement and alias. Unknown bounds retain the original exact checks.
struct ExactTrack {
    index: usize,
    bounds: Option<TrackBounds>,
}

impl ExactTrack {
    fn new(index: usize, tr: &TrackModel) -> Self {
        Self {
            index,
            bounds: tr.bounds(),
        }
    }
}

/// Evidence at a point with exact curves (real trajectory, emission-time solve, modelled wind and
/// sound speed), over a small clock sweep shared by all passes, inside this window's start bounds.
/// `None` if no pass is in reach.
fn score_exact(
    inp: &MatchInputs<'_>,
    p: &MatchParams,
    bank: &Bank,
    idx: &[ExactTrack],
    lat: f64,
    lon: f64,
    clock0: f64,
) -> Option<Exact> {
    score_exact_with_recovery(inp, p, bank, idx, lat, lon, clock0, true)
}

/// The disabled recovery path preserves the original strongest-tone assignment as a test oracle.
#[allow(clippy::too_many_arguments)]
fn score_exact_with_recovery(
    inp: &MatchInputs<'_>,
    p: &MatchParams,
    bank: &Bank,
    idx: &[ExactTrack],
    lat: f64,
    lon: f64,
    clock0: f64,
    recover_tones: bool,
) -> Option<Exact> {
    let im = inp.image;
    // Coarse votes are quantized to image frames, even for an exact fractional-second start.
    // Clamp the seed before predicting curves, then constrain the sweep too: repeated spatial
    // moves must not walk the clock out of this prior or decoy window.
    let clock0 = clock0.clamp(inp.start_lo, inp.start_hi);
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
        path: Vec<Sample>,
        full: (f32, f32),
        d: f64,
        v: f64,
        arrive: f64,
        null: Null,
        at_end: bool,
        f_arr: i64,
        /// Row offset at each frame of the middle half of the path; `i32::MIN` where unsolved.
        core: Vec<i32>,
    }
    if p.r50_m.is_empty() {
        return None;
    }
    let v_cap = p.v_grid.last().copied().unwrap_or(200.0) * 1.15;
    let mut ones: Vec<One> = Vec::new();
    let mut scratch = SweepBuffers::new(bank.nf0);
    for candidate in idx {
        if candidate
            .bounds
            .is_some_and(|b| !b.may_reach(x, p.reach_m))
        {
            continue;
        }
        let ti = candidate.index;
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
            // A grazing pass can lie inside reach between two samples that both lie outside.
            // Refine the local minimum before applying the existing physical range gate.
            if !(left && right) || !ds[i].is_finite() {
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
            let null = bank.null_at(p, cpa.speed * scale, cpa.d * scale);
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
                        &mut scratch,
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
                path: pth,
                full,
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
    let value_at = |o: &One, k: i64, r50: f64| -> (f64, i64) {
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
                pass_value(p, r50, nats, o.d) - pens[jj]
            };
            if v > best.0 {
                best = (v, k + j);
            }
        }
        best
    };
    #[derive(Clone, Copy)]
    struct Choice {
        value: f64,
        at: i64,
        shared: bool,
        score: f32,
        row: u16,
    }
    // A cached profile is needed only for a collided pass/clock. Ordinary lookups still retain
    // just their strongest row. Every profile uses the same coverage, prior and null as that row.
    let mut profiles: Vec<Vec<Option<Vec<f32>>>> = ones.iter().map(|_| Vec::new()).collect();
    let shares_pixels = |o: &One, at: i64, row: u16, claims: &[(i64, i64, usize)]| {
        let nc = (o.core.len() as i64 - 1) / 2;
        let first = o.f_arr + at - nc;
        let (mut shared, mut total) = (0usize, 0usize);
        for (q, &dr) in o.core.iter().enumerate() {
            if dr == i32::MIN {
                continue;
            }
            total += 1;
            let frame = first + q as i64;
            let here = i64::from(row) + i64::from(dr);
            let hit = claims.iter().any(|&(f2, r2, c)| {
                let q2 = frame - f2;
                let other = &ones[c].core;
                if q2 < 0 || q2 >= other.len() as i64 || other[q2 as usize] == i32::MIN {
                    return false;
                }
                let d = here - (r2 + i64::from(other[q2 as usize]));
                d.abs() <= dmax as i64 && same_pixels[(d + dmax as i64) as usize]
            });
            shared += usize::from(hit);
        }
        total > 0 && shared as f64 >= p.same_event_share * total as f64
    };
    let claim = |i: usize, choice: Choice| {
        let o = &ones[i];
        (
            o.f_arr + choice.at - (o.core.len() as i64 - 1) / 2,
            i64::from(choice.row),
            i,
        )
    };
    // A different-shaped path can touch a claimed curve for fewer central frames than the
    // shared-event cutoff, yet borrow enough of its energy to sound independently heard.
    // Only alternatives need this residual check: remove positive harmonic contributions on
    // claimed full paths. Other partials, every visible weight and the prior/null stay intact.
    let residual_score =
        |o: &One, at: i64, row: u16, upper: f32, claims: &[(i64, i64, usize)]| -> f32 {
            let Some(whitened) = &im.whitened else {
                return f32::NEG_INFINITY;
            };
            let (mut a, mut b, mut wa, mut wb) = (0f32, 0f32, 0f32, 0f32);
            for s in &o.path {
                let frame = o.f_arr + at + i64::from(s.dt);
                let here = i64::from(row) + i64::from(s.dr);
                let received = bank.r0 as i64 + here;
                if frame < 0 || frame >= im.nt as i64 || received < 0 || received >= im.nu as i64 {
                    continue;
                }
                let mut v = im.comb[frame as usize * im.nu + received as usize];
                let (mut removed, mut w2) = (0f32, 0f32);
                for (&shift, &(_, weight)) in shifts.iter().zip(&p.harmonics) {
                    let pixel = received + i64::from(shift);
                    if pixel < 0 || pixel >= im.nu as i64 {
                        continue;
                    }
                    w2 += weight * weight;
                    let contribution = weight * whitened[pixel as usize * im.nt + frame as usize];
                    if contribution > 0.0
                        && claims.iter().any(|&(first, tone, i)| {
                            let other = &ones[i];
                            let centre = first + (other.core.len() as i64 - 1) / 2;
                            let dt = frame - centre;
                            let Ok(q) = other.path.binary_search_by_key(&dt, |s| i64::from(s.dt))
                            else {
                                return false;
                            };
                            let base = bank.r0 as i64 + tone + i64::from(other.path[q].dr);
                            if base < 0 || base >= im.nu as i64 {
                                return false;
                            }
                            shifts.iter().any(|&sh| {
                                let owned = base + i64::from(sh);
                                owned >= 0 && owned < im.nu as i64 && (pixel - owned).abs() <= 1
                            })
                        })
                    {
                        removed += contribution;
                    }
                }
                if w2 > 0.0 {
                    v -= removed / w2.sqrt();
                }
                if s.approach {
                    a += s.w * v;
                    wa += s.w * s.w;
                } else {
                    b += s.w * v;
                    wb += s.w * s.w;
                }
            }
            if wa < 0.25 * o.full.0 || wb < 0.25 * o.full.1 {
                return f32::NEG_INFINITY;
            }
            let mut score = stat_of(p.stat, a * (1.0 / wa.sqrt()), b * (1.0 / wb.sqrt()));
            if let Some(q) = bank.tone_of(&inp.tracks[o.tr].typ) {
                score += o.null.beta * q[row as usize];
            }
            // The scalar weight sum can round differently from the bank's clipped-row
            // prefix sums. Suppressing support must never improve its original score.
            score.min(upper)
        };
    // First finish the original strongest-tone assignment unchanged. Recovery must not steal
    // an independent curve from a later original winner. Weak original passes still claim pixels;
    // unheard close passes still pay their original cost.
    let mut assign = |k: i64, r50: f64| -> (f64, Vec<Choice>) {
        let mut vals: Vec<Choice> = ones
            .iter()
            .map(|o| {
                let (v, at) = value_at(o, k, r50);
                let (score, row) = o.table[(at + span) as usize];
                Choice {
                    value: v,
                    at,
                    shared: false,
                    score,
                    row,
                }
            })
            .collect();
        let mut order: Vec<usize> = (0..ones.len()).filter(|&i| vals[i].value > 0.0).collect();
        order.sort_by(|&x, &y| vals[y].value.total_cmp(&vals[x].value));
        // (first frame, row of tone, index into ones) of each pass that has claimed pixels
        let mut claims: Vec<(i64, i64, usize)> = Vec::new();
        let mut collided = Vec::new();
        for &i in &order {
            let o = &ones[i];
            if shares_pixels(o, vals[i].at, vals[i].row, &claims) {
                vals[i].value = 0.0;
                vals[i].shared = true;
                collided.push(i);
            } else {
                claims.push(claim(i, vals[i]));
            }
        }
        if !recover_tones || collided.is_empty() || im.whitened.is_none() {
            return (vals.iter().map(|v| v.value).sum(), vals);
        }
        let mut alternative = |i: usize, claims: &[(i64, i64, usize)]| -> Option<Choice> {
            let o = &ones[i];
            let cache = &mut profiles[i];
            if cache.is_empty() {
                cache.resize_with(o.table.len(), || None);
            }
            let mut best: Option<Choice> = None;
            for (jj, j) in (-slack..=slack).enumerate() {
                let at = k + j;
                let strongest = o.table[(at + span) as usize].0;
                if !strongest.is_finite() {
                    continue;
                }
                let max_nats = o.null.nats(strongest) as f64;
                if max_nats < HEARD_NATS {
                    continue;
                }
                // Under the calibrator's defined 0 < kappa < 1 domain, evidence is monotone
                // in this already-known maximum. A weaker row cannot improve its bound.
                if (0.0..1.0).contains(&p.kappa) {
                    let upper = pass_value(p, r50, max_nats, o.d) - pens[jj];
                    if upper <= 0.0 || best.as_ref().is_some_and(|b| upper <= b.value) {
                        continue;
                    }
                }
                let scores = cache[(at + span) as usize].get_or_insert_with(|| {
                    scratch.profile.resize(bank.nf0, f32::NEG_INFINITY);
                    let tone = bank
                        .tone_of(&inp.tracks[o.tr].typ)
                        .map(|q| (q, o.null.beta));
                    sweep_tau(
                        im,
                        &o.path,
                        o.f_arr + at,
                        bank.r0,
                        bank.nf0,
                        p.stat,
                        &mut scratch,
                        o.full,
                        tone,
                    );
                    scratch.profile.clone()
                });
                for (row, &s) in scores.iter().enumerate() {
                    if !s.is_finite() {
                        continue;
                    }
                    // Judge peaks on the complete profile, including claimed neighbours.
                    // An unclaimed shoulder of a bright ridge is not a second emitted tone.
                    if (row > 0 && scores[row - 1] >= s)
                        || (row + 1 < scores.len() && scores[row + 1] > s)
                    {
                        continue;
                    }
                    let nats = o.null.nats(s) as f64;
                    if nats < HEARD_NATS {
                        continue;
                    }
                    let upper = pass_value(p, r50, nats, o.d) - pens[jj];
                    if ((0.0..1.0).contains(&p.kappa)
                        && (upper <= 0.0 || best.as_ref().is_some_and(|b| upper <= b.value)))
                        || shares_pixels(o, at, row as u16, claims)
                    {
                        continue;
                    }
                    let s = residual_score(o, at, row as u16, s, claims);
                    if !s.is_finite() {
                        continue;
                    }
                    let nats = o.null.nats(s) as f64;
                    if nats < HEARD_NATS {
                        continue;
                    }
                    let value = pass_value(p, r50, nats, o.d) - pens[jj];
                    if value > 0.0 && best.as_ref().is_none_or(|b| value > b.value) {
                        best = Some(Choice {
                            value,
                            at,
                            shared: false,
                            score: s,
                            row: row as u16,
                        });
                    }
                }
            }
            best
        };
        let mut pending: Vec<_> = collided
            .into_iter()
            .filter_map(|i| alternative(i, &claims).map(|choice| (i, choice, claims.len())))
            .collect();
        // A new claim can lower any residual score, even below the shared-event cutoff.
        // Stale values are upper bounds: rescore and reinsert before accepting the next best.
        while let Some(q) = (0..pending.len()).max_by(|&a, &b| {
            pending[a]
                .1
                .value
                .total_cmp(&pending[b].1.value)
                .then_with(|| pending[b].0.cmp(&pending[a].0))
        }) {
            let (i, choice, version) = pending.swap_remove(q);
            if version != claims.len() {
                if let Some(next) = alternative(i, &claims) {
                    pending.push((i, next, claims.len()));
                }
            } else {
                vals[i] = choice;
                claims.push(claim(i, choice));
            }
        }
        (vals.iter().map(|v| v.value).sum(), vals)
    };
    // best over the clock and the heard range together; a tie keeps the range listed first (the
    // shorter, in the default order)
    let mut best = (f64::NEG_INFINITY, 0i64, p.r50_m[0]);
    for &r50 in &p.r50_m {
        for k in -kmax..=kmax {
            let clock = clock0 - k as f64 * im.dt;
            if clock < inp.start_lo || clock > inp.start_hi {
                continue;
            }
            let (sum, _) = assign(k, r50);
            if sum > best.0 {
                best = (sum, k, r50);
            }
        }
    }
    let (k, r50) = (best.1, best.2);
    let clock = clock0 - k as f64 * im.dt;
    let mut passes = Vec::with_capacity(ones.len());
    let (mut n_heard, mut n_close) = (0, 0);
    let (_, assigned) = assign(k, r50);
    for (o, &choice) in ones.iter().zip(&assigned) {
        let Choice {
            value,
            at,
            shared,
            score: s,
            row: r,
        } = choice;
        let usable = s.is_finite();
        let nats = if usable { o.null.nats(s) as f64 } else { 0.0 };
        if counts_as_heard(value, nats, shared) {
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
        r50_m: r50,
        clock,
        passes,
        n_heard,
        n_close,
    })
}

/// Places the evidence may not tell from `top`, along the tracks of the flights it heard.
///
/// One heard flight fixes when its closest approach was heard, not where along its track the
/// observer stood: moving the observer `s` metres along the track and the clock `s / v` later
/// leaves that flight's curve where it was (exactly, for a straight, level, steady flight). A
/// second heard flight on a crossing track, or going the other way, breaks the tie. One on a
/// parallel track going the same way at the same speed slides with it, and every mode can then sit
/// on the same ridge: modes are at least `nms_m` apart and few, and a climb moves only a little at
/// a time. So the twin `ALIAS_STEP_M`, 2 `ALIAS_STEP_M`, ... out to `ALIAS_REACH_M` either way is
/// scored for each flight `top` heard at `HEARD_NATS`, credited or not (a superset of
/// `counts_as_heard`, so no slide is missed), wherever the place is inside the region and the
/// clock inside the real window, which `score_exact` keeps every clock inside. Nothing here
/// changes any evidence or margin, so the decoy calibration holds; the decoys get no such check,
/// and a check can only widen the real window's radius.
fn along_track_aliases(
    inp: &MatchInputs<'_>,
    p: &MatchParams,
    bank: &Bank,
    idx: &[ExactTrack],
    top: &MatchMode,
) -> Vec<MatchMode> {
    let z = inp.fine_dem.elev_m(top.lat, top.lon);
    let x = geodetic_to_ecef(top.lat, top.lon, z + MIC_HEIGHT_M);
    let enu = Enu::new(top.lat, top.lon, z + MIC_HEIGHT_M);
    let n_steps = (ALIAS_REACH_M / ALIAS_STEP_M).round() as i32;
    let mut starts: Vec<(f64, f64, f64)> = Vec::new();
    for ps in top
        .passes
        .iter()
        .filter(|q| q.nats >= HEARD_NATS && !q.shared)
    {
        // emission time near the heard closest approach; `cpa_near` finds the exact one. Keys are
        // unique per track (`icao_date_n` per leg, plus `#k` per piece), and a track heard twice, a
        // lap flown again, gives each pass its own time and so its own closest approach.
        let t_guess = top.clock_unix + ps.t_audio - ps.slant_m / BASE_C;
        let Some(cpa) = idx
            .iter()
            .map(|t| &inp.tracks[t.index])
            .filter(|t| t.key == ps.key)
            .find_map(|t| t.cpa_near(x, t_guess, 30.0))
        else {
            continue;
        };
        let v = enu.vec_from_ecef(cpa.vel);
        let vh = v[0].hypot(v[1]);
        if vh < 15.0 {
            continue;
        }
        for k in (-n_steps..=n_steps).filter(|&k| k != 0) {
            let s = k as f64 * ALIAS_STEP_M;
            let (la, lo, _) = enu.to_geodetic([s * v[0] / vh, s * v[1] / vh, 0.0]);
            let clock = top.clock_unix + s / vh;
            if inp.region.contains(la, lo) && clock >= inp.start_lo && clock <= inp.start_hi {
                starts.push((la, lo, clock));
            }
        }
    }
    starts
        .par_iter()
        .filter_map(|&(la, lo, clock)| {
            let e = score_exact(inp, p, bank, idx, la, lo, clock)?;
            Some(MatchMode {
                lat: la,
                lon: lo,
                clock_unix: e.clock,
                coarse: 0.0,
                evidence: e.evidence,
                r50_m: e.r50_m,
                n_heard: e.n_heard,
                n_unheard_close: e.n_close,
                passes: e.passes,
            })
        })
        .collect()
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
    idx: &[ExactTrack],
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

/// Fit the already heard curves on common data. Evidence finds a plausible collection of
/// passes, but its template-dependent null and d/v support are not a location likelihood:
/// moving farther from a circling aircraft can win simply by summing a longer recording.
/// Freeze the independent passes' frames, weights, half partitions and timestamp offsets,
/// then fit their physical Doppler paths with fractional frequency rows. This score never
/// becomes evidence; the unchanged scorer must justify the resulting point and clock.
fn polish_point(
    inp: &MatchInputs<'_>,
    p: &MatchParams,
    bank: &Bank,
    idx: &[ExactTrack],
    lat: f64,
    lon: f64,
    seed: Exact,
) -> (f64, f64, Exact) {
    let original = (lat, lon, seed);
    if original.2.n_heard < 2 || p.refine_modes == 0 || p.refine_steps_m.is_empty() {
        return original;
    }
    struct FitPass<'a> {
        tr: &'a TrackModel,
        row: f64,
        offset_s: f64,
        centre_s: f64,
        t_e: f64,
        height_m: f64,
        frames: Vec<(usize, bool, f32)>,
        gains: (f32, f32),
    }
    let im = inp.image;
    let z = inp.fine_dem.elev_m(lat, lon) + MIC_HEIGHT_M;
    let x = geodetic_to_ecef(lat, lon, z);
    let enu = Enu::new(lat, lon, z);
    let mut passes = Vec::new();
    for ps in &original.2.passes {
        if !counts_as_heard(ps.value, ps.nats, ps.shared) {
            continue;
        }
        let Some(tr) = idx
            .iter()
            .map(|t| &inp.tracks[t.index])
            .find(|tr| tr.key == ps.key)
        else {
            continue;
        };
        let t_guess = original.2.clock + ps.t_audio - ps.slant_m / BASE_C;
        let Some(cpa) = tr.cpa_near(x, t_guess, 2.0 * p.sample_s) else {
            continue;
        };
        let (_, _, height_m) = ecef_to_geodetic(tr.pos_at(cpa.t_e));
        let medium = inp.atmo.medium(lat, lon, cpa.t_e, z, height_m);
        let prop = Prop::from_medium(&medium, &enu);
        let half = (p.half_k * ps.slant_m / ps.speed_mps).clamp(p.half_min_s, p.half_max_s);
        let centre = ps.t_audio - ps.clock_offset_s;
        let first = (((centre - half - im.t0) / im.dt).ceil() as i64).max(0);
        let last = (((centre + half - im.t0) / im.dt).floor() as i64).min(im.nt as i64 - 1);
        let mut frames = Vec::new();
        let (mut wa, mut wb) = (0f32, 0f32);
        let (mut warm, mut hint) = (None, 0);
        for frame in first..=last {
            let time = im.t0 + frame as f64 * im.dt;
            if let Some((t_e, r, _)) = tr.solve_emission_sweep(
                original.2.clock + ps.clock_offset_s + time,
                x,
                &prop,
                warm,
                &mut hint,
                1e-4,
            ) {
                warm = Some(t_e);
                let w = (p.r_ref_m / r).min(1.0) as f32;
                let approach = time < centre;
                if approach {
                    wa += w * w;
                } else {
                    wb += w * w;
                }
                frames.push((frame as usize, approach, w));
            } else {
                warm = None;
            }
        }
        if wa > 0.0 && wb > 0.0 {
            passes.push(FitPass {
                tr,
                row: im.row_of(ps.f0_hz),
                offset_s: ps.clock_offset_s,
                centre_s: centre,
                t_e: cpa.t_e,
                height_m,
                frames,
                gains: (1.0 / wa.sqrt(), 1.0 / wb.sqrt()),
            });
        }
    }
    if passes.len() < 2 {
        return original;
    }
    let score = |la: f64, lo: f64, clock: f64| -> Option<f32> {
        let z = inp.fine_dem.elev_m(la, lo) + MIC_HEIGHT_M;
        let x = geodetic_to_ecef(la, lo, z);
        let enu = Enu::new(la, lo, z);
        let mut total = 0.0;
        for pass in &passes {
            let medium = inp.atmo.medium(la, lo, pass.t_e, z, pass.height_m);
            let prop = Prop::from_medium(&medium, &enu);
            let (mut warm, mut hint) = (None, 0);
            let mut rows = Vec::with_capacity(pass.frames.len());
            for &(frame, _, _) in &pass.frames {
                let t_r = clock + pass.offset_s + im.t0 + frame as f64 * im.dt;
                let (t_e, _, ratio) = pass
                    .tr
                    .solve_emission_sweep(t_r, x, &prop, warm, &mut hint, 1e-4)?;
                warm = Some(t_e);
                rows.push(pass.row + im.bpo * ratio.log2());
            }
            let mut best = f32::NEG_INFINITY;
            // The emitted row and Doppler offset each rounded by at most half a row.
            // Fit their combined quantization error locally, without jumping harmonics.
            for shift in -8..=8 {
                let offset = f64::from(shift) / 8.0;
                let (mut a, mut b) = (0f32, 0f32);
                let mut visible = true;
                for (&row, &(frame, approach, w)) in rows.iter().zip(&pass.frames) {
                    let u = row + offset;
                    if !(0.0..(im.nu - 1) as f64).contains(&u) {
                        visible = false;
                        break;
                    }
                    let r = u.floor() as usize;
                    let mix = (u - r as f64) as f32;
                    let left = im.comb[frame * im.nu + r];
                    let right = im.comb[frame * im.nu + r + 1];
                    let v = w * (left + mix * (right - left));
                    if approach {
                        a += v;
                    } else {
                        b += v;
                    }
                }
                if visible {
                    best = best.max(stat_of(p.stat, a * pass.gains.0, b * pass.gains.1));
                }
            }
            if !best.is_finite() {
                return None;
            }
            total += best;
        }
        Some(total)
    };
    let Some(initial) = score(lat, lon, original.2.clock) else {
        return original;
    };
    let mut cur = (lat, lon, original.2.clock, initial);
    for &step in &p.refine_steps_m {
        for _ in 0..refine_moves(p.nms_m, step) {
            let (mlat, mlon) = m_per_deg(cur.0);
            let centre = cur;
            let best = (-2i32..=2)
                .flat_map(|i| (-2i32..=2).map(move |j| (i, j)))
                .collect::<Vec<_>>()
                .into_par_iter()
                .filter_map(|(i, j)| {
                    let la = centre.0 + f64::from(i) * step / mlat;
                    let lo = centre.1 + f64::from(j) * step / mlon;
                    if !inp.region.contains(la, lo) {
                        return None;
                    }
                    std::iter::once(0)
                        .chain((-p.clock_sweep_frames..=p.clock_sweep_frames).filter(|&k| k != 0))
                        .filter_map(|k| {
                            let clock = centre.2 + k as f64 * im.dt;
                            if clock < inp.start_lo || clock > inp.start_hi {
                                return None;
                            }
                            score(la, lo, clock).map(|s| (la, lo, clock, s))
                        })
                        .max_by(|a, b| {
                            a.3.total_cmp(&b.3).then_with(|| {
                                (b.2 - centre.2).abs().total_cmp(&(a.2 - centre.2).abs())
                            })
                        })
                })
                .max_by(|a, b| a.3.total_cmp(&b.3));
            match best {
                Some(next) if next.3 > cur.3 => cur = next,
                _ => break,
            }
        }
    }
    if cur.3 <= initial {
        return original;
    }
    // Do not let the significance sweep undo the fitted shared clock. Per-flight timing
    // slack, ownership, empirical nulls and the independent-hearing threshold stay intact.
    let fixed_clock = MatchParams {
        clock_sweep_frames: 0,
        ..p.clone()
    };
    let Some(e) = score_exact(inp, &fixed_clock, bank, idx, cur.0, cur.1, cur.2) else {
        return original;
    };
    // This fit conditioned on particular source visits and emitted tones. A different
    // strongest-tone assignment cannot justify it, even if the total heard count holds.
    // Claim each final visit once, so repeated trips by one aircraft remain distinct.
    let mut claimed = vec![false; e.passes.len()];
    for pass in &passes {
        let first = im.t0 + pass.frames[0].0 as f64 * im.dt;
        let last = im.t0 + pass.frames[pass.frames.len() - 1].0 as f64 * im.dt;
        let choice = e.passes.iter().enumerate()
            .filter(|&(i, after)| {
                let arrive = after.t_audio - after.clock_offset_s;
                !claimed[i] && after.key == pass.tr.key
                    && counts_as_heard(after.value, after.nats, after.shared)
                    && (im.row_of(after.f0_hz).round() - pass.row.round()).abs() <= 1.0
                    && (first..=last).contains(&arrive)
            })
            .min_by(|(_, a), (_, b)| {
                (a.t_audio - a.clock_offset_s - pass.centre_s).abs()
                    .total_cmp(&(b.t_audio - b.clock_offset_s - pass.centre_s).abs())
            });
        let Some((i, _)) = choice else {
            return original;
        };
        claimed[i] = true;
    }
    (cur.0, cur.1, e)
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

/// One cell's votes, shared by spatial shortlisting and the later clock search. The caller
/// clears the touched bins before reusing scratch, so neither pass changes the vote arithmetic.
fn accumulate_votes(
    passes: &[Pass],
    bank: &Bank,
    im: &CombImage,
    t_base: f64,
    clock_lo: f64,
    acc: &mut [f32],
    touched: &mut Vec<u32>,
) {
    for ps in passes {
        let base = t_base + ps.arrive as f64 - im.t0 - clock_lo;
        for &(tau, val) in &bank.surfaces[ps.tpl as usize].peaks {
            let b = ((base - tau as f64 * im.dt) / im.dt).round();
            if b >= 0.0 && (b as usize) < acc.len() {
                let b = b as usize;
                if acc[b] == 0.0 {
                    touched.push(b as u32);
                }
                acc[b] += val;
            }
        }
    }
}

/// Coarse clock peaks, retaining every bin of a tied plateau. Try plateau pivots a full
/// sweep apart, plus its last bin, before the remaining bins so full-sweep coverage can
/// coalesce their work. Keep the original best seed first and stronger peaks before weaker.
fn clock_peak_candidates(votes: &[f32], span: (usize, usize), first: Top, half: i64) -> Vec<Top> {
    let mut peaks = vec![first];
    let sweep_bins = (half.max(0) as usize).saturating_mul(2).saturating_add(1);
    let mut a = span.0;
    while a <= span.1 {
        let value = votes[a];
        let mut b = a;
        while b < span.1 && votes[b + 1] == value {
            b += 1;
        }
        let left = if a == span.0 {
            f32::NEG_INFINITY
        } else {
            votes[a - 1]
        };
        let right = if b == span.1 {
            f32::NEG_INFINITY
        } else {
            votes[b + 1]
        };
        if value > 0.0 && value > left && value > right {
            let mut push = |bin: usize| {
                if bin as u32 != first.bin {
                    peaks.push(Top {
                        coarse: value,
                        cell: first.cell,
                        bin: bin as u32,
                    });
                }
            };
            for bin in (a..=b).step_by(sweep_bins) {
                push(bin);
            }
            if !(b - a).is_multiple_of(sweep_bins) {
                push(b);
            }
            for bin in a..b {
                if !(bin - a).is_multiple_of(sweep_bins) {
                    push(bin);
                }
            }
        }
        a = b + 1;
    }
    // Stable sorting preserves the plateau pivots before its remaining bins.
    peaks[1..].sort_by(|a, b| b.coarse.total_cmp(&a.coarse));
    peaks
}

/// A fractional window endpoint can put a clamped seed on another clock lattice. Being
/// inside the old sweep's interval is not enough: this exact timestamp must have been tried.
fn clock_was_swept(clock: f64, swept: &[f64], dt: f64, half: i64) -> bool {
    let reach = half as f64 * dt;
    let lo = swept.partition_point(|&seed| seed < clock - reach);
    swept[lo..]
        .iter()
        .take_while(|&&seed| seed <= clock + reach)
        .any(|&seed| {
            let k = ((seed - clock) / dt).round();
            k.abs() <= half as f64 && seed - k * dt == clock
        })
}

/// Every in-window timestamp of this clamped seed's exact sweep must already have been
/// evaluated. This tracks clock coverage only: physical pass selection and curve construction
/// still belong to each evaluated seed. Merely visiting the centre does not cover its tails.
fn clock_sweep_was_covered(
    clock: f64,
    swept: &[f64],
    dt: f64,
    half: i64,
    window: (f64, f64),
) -> bool {
    (-half..=half).all(|k| {
        let at = clock - k as f64 * dt;
        at < window.0 || at > window.1 || clock_was_swept(at, swept, dt, half)
    })
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

/// The widest recording-clock window (s) that can get `MIN_DECOYS` decoy windows, where the fetched
/// days leave room: `decoy_counts` puts floor(`decoy_span_s` / (width + `decoy_guard_s`)) on each
/// side. With the defaults that is 7200 / 20 - 60 = 300 s, a start known to +-150 s. A wider window
/// is still searched, but its verdict can neither stand nor judge a trace answer.
pub fn max_judged_window_s(p: &MatchParams) -> f64 {
    p.decoy_span_s / MIN_DECOYS.div_ceil(2) as f64 - p.decoy_guard_s
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
/// `ASSERT_MARGIN_NATS`. Places the evidence cannot rule out (the other modes, and the best place's
/// along-track aliases) widen the radius, and an answer whose radius would be wider than
/// `MAX_ASSERT_RADIUS_M` does not stand. At least two independently credited flyovers are
/// required, as in the trace search.
pub fn verdict(r: &MatchResult) -> Option<CurveVerdict> {
    let top = r.modes.first()?;
    let (margin, p) = (r.decoy_margin?, r.decoy_p?);
    // within TIED_NATS, inclusive, or better than the best; a gap that is not a number counts as
    // tied, so it cannot hide a place
    let tied = |m: &&MatchMode| {
        let gap = top.evidence - m.evidence;
        gap <= TIED_NATS || gap.is_nan()
    };
    let n_alias_tied = r.aliases.iter().filter(tied).count();
    let radius_m = r
        .modes
        .iter()
        .skip(1)
        .chain(&r.aliases)
        .filter(tied)
        .map(|m| haversine_m(top.lat, top.lon, m.lat, m.lon))
        .fold(RADIUS_FLOOR_M, f64::max);
    let enough = r.decoys.len() >= MIN_DECOYS;
    let enough_passes = top.n_heard >= 2;
    let finite = top.evidence.is_finite()
        && margin.is_finite()
        && r.decoys.iter().all(|w| w.best_evidence.is_finite());
    let clears = finite && margin > ASSERT_MARGIN_NATS;
    let tight = radius_m <= MAX_ASSERT_RADIUS_M;
    let why = format!(
        "{:.1} nats at this place and clock, {margin:+.1} over the best of {} decoy clocks (line {ASSERT_MARGIN_NATS:+.1}), {} flights heard, places within {TIED_NATS:.0} nats span {radius_m:.0} m ({n_alias_tied} of {} along their tracks){}{}",
        top.evidence,
        r.decoys.len(),
        top.n_heard,
        r.aliases.len(),
        if enough {
            ""
        } else {
            "; too few decoy clocks to judge"
        },
        if enough_passes {
            ""
        } else {
            "; at least two independent flyovers are required"
        },
    );
    Some(CurveVerdict {
        stands: enough && enough_passes && clears && tight,
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

/// Test the interpolated path within the catalogue span, at the same time resolution used for
/// closest-approach searches. A stride through original ADS-B knots can skip an entire sparse pass.
/// The caller pads the region by the acoustic reach plus 2 km for the spatial refinement.
fn track_near_region(tr: &TrackModel, pad: &BBox, span: (f64, f64), sample_s: f64) -> bool {
    let (ta, tb) = (tr.t0().max(span.0), tr.t1().min(span.1));
    if tb < ta {
        return false;
    }
    let n = (((tb - ta) / sample_s).ceil() as usize).max(1);
    (0..=n).any(|i| {
        let t = ta + (tb - ta) * i as f64 / n as f64;
        let (lat, lon, _) = ecef_to_geodetic(tr.pos_at(t));
        pad.contains(lat, lon)
    })
}

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

/// Run the whole match. `say` reports progress.
pub fn run(inp: &MatchInputs<'_>, p: &MatchParams, say: &(dyn Fn(&str) + Sync)) -> MatchResult {
    if grid_cell_count(p, &inp.region).is_none() {
        say("curve match: the region cannot be represented by a bounded local grid");
        return MatchResult {
            n_tracks: inp.tracks.len(),
            ..MatchResult::default()
        };
    }
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
    let mut window_bins: Vec<Option<(usize, usize)>> = vec![None; n_win];
    for (bin, &w) in bin_window.iter().enumerate() {
        if w == i32::MIN {
            continue;
        }
        let wi = (w as i64 + n_lo) as usize;
        match &mut window_bins[wi] {
            Some((_, hi)) => *hi = bin,
            span @ None => *span = Some((bin, bin)),
        }
    }
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
                accumulate_votes(
                    &passes[rng], &bank, im, t_base, clock_lo, &mut acc, &mut touched,
                );
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
    let keep: Vec<ExactTrack> = (0..inp.tracks.len())
        .into_par_iter()
        .filter(|&i| track_near_region(&inp.tracks[i], &pad, span, p.sample_s))
        .map(|i| ExactTrack::new(i, &inp.tracks[i]))
        .collect();
    let clock_candidates = AtomicUsize::new(0);
    let clock_scores = AtomicUsize::new(0);
    // The whole search inside one clock window. Run once for the real window and once per decoy.
    let search_window = |list: &[Top], wi: usize| -> Vec<MatchMode> {
        let Some(bins) = window_bins[wi] else {
            return Vec::new();
        };
        let shift_s = (wi as i64 - n_lo) as f64 * stride;
        let window = MatchInputs {
            start_lo: inp.start_lo + shift_s,
            start_hi: inp.start_hi + shift_s,
            ..*inp
        };
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
        // A spatial start represents its whole disc, but its strongest coarse clock can
        // be an alias. Revisit clock peaks only at these selected cells. Scratch is one
        // existing vote array per worker plus at most one candidate per bin in this window;
        // there is no new clock cap, and the spatial shortlist and refinement budgets stay.
        let mut scored: Vec<(usize, Top, Exact)> = chosen
            .par_iter()
            .enumerate()
            .map_init(
                || (vec![0f32; nbins], Vec::<u32>::new()),
                |(acc, touched), (i, m)| {
                    let cell = m.0.cell as usize;
                    let rng = offs[cell] as usize..offs[cell + 1] as usize;
                    accumulate_votes(&passes[rng], &bank, im, t_base, clock_lo, acc, touched);
                    let seeds = clock_peak_candidates(acc, bins, m.0, p.clock_sweep_frames);
                    clock_candidates.fetch_add(seeds.len(), Ordering::Relaxed);
                    for bin in touched.drain(..) {
                        acc[bin as usize] = 0.0;
                    }
                    let mut swept = Vec::new();
                    let mut best: Option<(Top, Exact)> = None;
                    for seed in seeds {
                        let clock = bin_clock(seed.bin as usize)
                            .clamp(window.start_lo, window.start_hi);
                        if clock_sweep_was_covered(
                            clock,
                            &swept,
                            im.dt,
                            p.clock_sweep_frames,
                            (window.start_lo, window.start_hi),
                        ) {
                            continue;
                        }
                        clock_scores.fetch_add(1, Ordering::Relaxed);
                        let Some(e) = score_exact(&window, p, &bank, &keep, m.1, m.2, clock) else {
                            continue;
                        };
                        let at = swept.partition_point(|&old| old < clock);
                        swept.insert(at, clock);
                        if best.as_ref().is_none_or(|b| e.evidence > b.1.evidence) {
                            best = Some((seed, e));
                        }
                    }
                    best.map(|(seed, e)| (i, seed, e))
                },
            )
            .flatten()
            .collect();
        scored.sort_by(|a, b| b.2.evidence.total_cmp(&a.2.evidence).then(a.0.cmp(&b.0)));
        let mut modes: Vec<MatchMode> = scored
            .into_par_iter()
            .enumerate()
            .filter_map(|(rank, (i, seed, e))| {
                let m = &chosen[i];
                let (la, lo, e) = if rank < p.refine_modes {
                    refine_point(
                        &window,
                        p,
                        &bank,
                        &keep,
                        m.1,
                        m.2,
                        bin_clock(seed.bin as usize),
                    )?
                } else {
                    (m.1, m.2, e)
                };
                // Physical fitting can lower evidence, so every retained mode gets it
                // before ranking, including those outside the costlier refinement budget.
                // polish_point preserves the explicit refine_modes == 0 disable.
                let (la, lo, e) = polish_point(&window, p, &bank, &keep, la, lo, e);
                Some(MatchMode {
                    lat: la,
                    lon: lo,
                    clock_unix: e.clock,
                    coarse: seed.coarse as f64,
                    evidence: e.evidence,
                    r50_m: e.r50_m,
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
    let modes = search_window(&per_window[n_lo as usize], n_lo as usize);
    let real_clock_candidates = clock_candidates.load(Ordering::Relaxed);
    let real_clock_scores = clock_scores.load(Ordering::Relaxed);
    say(&format!(
        "curve match: real window, {real_clock_candidates} coarse clock candidates, {real_clock_scores} initial exact clock starts"
    ));
    timings.push(("exact".to_string(), t.elapsed().as_secs_f64()));
    let t = std::time::Instant::now();
    let aliases = modes.first().map_or_else(Vec::new, |top| {
        along_track_aliases(inp, p, &bank, &keep, top)
    });
    timings.push(("aliases".to_string(), t.elapsed().as_secs_f64()));
    // Diagnostic only, on recordings whose place is known: evidence at a given place, best over a
    // wide range of clocks, then after refining from it (`refine_point`: the `nms_m` disc around
    // it, then the climb). Never used to choose anything.
    let probe_inputs = MatchInputs {
        start_lo: f64::NEG_INFINITY,
        start_hi: f64::INFINITY,
        ..*inp
    };
    let probe: Vec<MatchMode> = p
        .probe
        .iter()
        .flat_map(|&(la, lo, ck)| {
            let step = (2 * p.clock_sweep_frames + 1) as f64 * im.dt;
            let best = (-15i32..=15)
                .into_par_iter()
                .filter_map(|i| {
                    score_exact(&probe_inputs, p, &bank, &keep, la, lo, ck + i as f64 * step)
                })
                .max_by(|a, b| a.evidence.total_cmp(&b.evidence));
            let mut out = Vec::new();
            if let Some(e) = best {
                let clock = e.clock;
                let up = refine_point(&probe_inputs, p, &bank, &keep, la, lo, clock);
                out.push(MatchMode {
                    lat: la,
                    lon: lo,
                    clock_unix: e.clock,
                    coarse: 0.0,
                    evidence: e.evidence,
                    r50_m: e.r50_m,
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
                        r50_m: e.r50_m,
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
            match search_window(&per_window[wi], wi).first() {
                Some(m) => DecoyWindow {
                    shift_s,
                    best_coarse: m.coarse,
                    best_evidence: m.evidence,
                    r50_m: m.r50_m,
                    lat: m.lat,
                    lon: m.lon,
                    n_heard: m.n_heard,
                },
                None => DecoyWindow {
                    shift_s,
                    best_coarse: 0.0,
                    best_evidence: 0.0,
                    r50_m: 0.0,
                    lat: 0.0,
                    lon: 0.0,
                    n_heard: 0,
                },
            }
        })
        .collect();
    timings.push(("decoys".to_string(), t.elapsed().as_secs_f64()));
    say(&format!(
        "curve match: {} decoy windows, {} coarse clock candidates, {} initial exact clock starts",
        decoys.len(),
        clock_candidates.load(Ordering::Relaxed) - real_clock_candidates,
        clock_scores.load(Ordering::Relaxed) - real_clock_scores,
    ));
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
        aliases,
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
        case_scaled(amp, tone, 1.0)
    }

    /// `case` with the heard flights' horizontal offsets times `scale` (the silent ones stay put).
    fn case_scaled(amp: f64, tone: bool, scale: f64) -> Case {
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
            mk([900.0 * scale, 0.0, 700.0], [0.0, 70.0, 0.0], 200.0, "AAA"),
            mk(
                [0.0, -1500.0 * scale, 900.0],
                [65.0, 0.0, 0.0],
                450.0,
                "BBB",
            ),
            mk(
                [-1200.0 * scale, 900.0 * scale, 600.0],
                [55.0, 55.0, 0.0],
                700.0,
                "CCC",
            ),
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

    /// Only the given flights, all heard, from one point in noise: (start offset east, north, up in
    /// m; velocity in m/s; seconds after the recording starts that it passes the start offset). The
    /// region reaches `half_m` out each way.
    fn scene(heard: &[(V3, V3, f64)], half_m: f64) -> Case {
        let tracks: Vec<TrackModel> = heard
            .iter()
            .enumerate()
            .map(|(i, &(p0, v, t0))| {
                let mut t = scene_track(p0, v, t0, -1500.0, 3000.0);
                t.key = format!("F{i}");
                t.icao = t.key.clone();
                t
            })
            .collect();
        scene_of(tracks, half_m)
    }

    /// Where and when `scene` records: latitude, longitude, clock.
    const SCENE_AT: (f64, f64, f64) = (40.8765, -73.525, 1_788_895_000.0);

    /// A straight track for `scene_of`, sampled from `from` to `to` s after the recording starts.
    fn scene_track(p0: V3, v: V3, t0: f64, from: f64, to: f64) -> TrackModel {
        let (lat, lon, clock) = SCENE_AT;
        let enu = Enu::new(lat, lon, 0.0);
        straight_track(&enu, p0, v, clock + t0, clock + from, clock + to, 4.0)
    }

    /// `scene` with tracks of any shape, every one of them heard.
    fn scene_of(tracks: Vec<TrackModel>, half_m: f64) -> Case {
        scene_for(tracks, half_m, 900.0)
    }

    fn scene_for(tracks: Vec<TrackModel>, half_m: f64, dur: f64) -> Case {
        let (lat, lon, clock) = SCENE_AT;
        let enu = Enu::new(lat, lon, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let sr = 2800.0;
        let m = Prop::still(BASE_C);
        let mut buf = noise((sr * dur) as usize, 42, 0.05);
        for (i, tr) in tracks.iter().enumerate() {
            add_flight(&mut buf, sr, clock, tr, x, &m, 90.0 + 25.0 * i as f64, 0.5);
        }
        let (dlat, dlon) = m_per_deg(lat);
        Case {
            x: buf.iter().map(|&v| v as f32).collect(),
            tracks,
            region: BBox::new(
                lat - half_m / dlat,
                lon - half_m / dlon,
                lat + half_m / dlat,
                lon + half_m / dlon,
            ),
            lat,
            lon,
            clock,
        }
    }

    /// Along-track aliases of `r`'s best place at least `min_m` from it, with their gap below it.
    fn far_aliases(r: &MatchResult, min_m: f64) -> Vec<(f64, f64)> {
        let top = &r.modes[0];
        r.aliases
            .iter()
            .map(|a| {
                (
                    haversine_m(top.lat, top.lon, a.lat, a.lon),
                    top.evidence - a.evidence,
                )
            })
            .filter(|&(d, _)| d >= min_m)
            .collect()
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
        run_case_logged(c, p, lo, hi, catalog, &|_| {})
    }

    fn run_case_logged(
        c: &Case,
        p: &MatchParams,
        lo: f64,
        hi: f64,
        catalog: (f64, f64),
        say: &(dyn Fn(&str) + Sync),
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
        run(&inp, p, say)
    }

    /// Independent trajectory oracle: use the same physical solver as exact scoring,
    /// without the constant-velocity bank's analytic range or frequency functions.
    fn track_path(
        template: &[Sample],
        tr: &TrackModel,
        observer: V3,
        arrival: f64,
        dt: f64,
        p: &MatchParams,
    ) -> Vec<Sample> {
        template
            .iter()
            .map(|s| {
                let (_, range, ratio) = tr
                    .solve_emission_p(
                        arrival + f64::from(s.dt) * dt,
                        observer,
                        &Prop::still(BASE_C),
                        None,
                    )
                    .expect("the complete straight track covers the bank path");
                Sample {
                    dt: s.dt,
                    dr: (p.bins_per_octave * ratio.log2()).round() as i32,
                    w: (p.r_ref_m / range).min(1.0) as f32,
                    approach: s.dt < 0,
                }
            })
            .collect()
    }

    #[test]
    fn bank_path_weights_match_retarded_track_geometry() {
        let p = MatchParams::default();
        let enu = Enu::new(SCENE_AT.0, SCENE_AT.1, 0.0);
        for speed in [28.0, 80.0, 215.0, 263.0] {
            for distance in [400.0, 1500.0, 7999.5] {
                let template = path(&p, speed, distance, p.hop_s, p.bins_per_octave);
                for (shift_s, shift_m) in [(0.0, [0.0; 3]), (17.25, [12_000.0, -2300.0, 500.0])] {
                    let cpa = SCENE_AT.2 + shift_s;
                    let mut tr = straight_track(
                        &enu,
                        [0.0, distance, MIC_HEIGHT_M],
                        [speed, 0.0, 0.0],
                        cpa,
                        cpa - 600.0,
                        cpa + 600.0,
                        4.0,
                    );
                    let mut observer = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
                    for axis in 0..3 {
                        observer[axis] += shift_m[axis];
                        for position in &mut tr.pos {
                            position[axis] += shift_m[axis];
                        }
                    }
                    let exact = track_path(
                        &template,
                        &tr,
                        observer,
                        cpa + distance / BASE_C,
                        p.hop_s,
                        &p,
                    );
                    for (bank, trajectory) in template.iter().zip(&exact) {
                        assert_eq!(bank.dr, trajectory.dr);
                        assert_eq!(bank.approach, trajectory.approach);
                        assert!(
                            (bank.w - trajectory.w).abs() < 2e-6,
                            "v={speed}, d={distance}, shift={shift_s}, frame={}: bank {}, exact {}",
                            bank.dt,
                            bank.w,
                            trajectory.w
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn bank_path_scores_match_the_exact_fast_pcm_curve() {
        let speed = 263.0;
        let distance = 1000.0;
        let cpa = 100.0;
        let tr = scene_track(
            [0.0, distance, MIC_HEIGHT_M],
            [speed, 0.0, 0.0],
            cpa,
            -300.0,
            700.0,
        );
        let c = scene_with_visible_harmonics(vec![tr], &[115.0], 200.0, 240.0);
        let p = MatchParams::default();
        let im = build_image(&c.x, 2800, &p).unwrap();
        let template = path(&p, speed, distance, im.dt, im.bpo);
        let observer = geodetic_to_ecef(c.lat, c.lon, MIC_HEIGHT_M);
        let arrival = c.clock + cpa + distance / BASE_C;
        let exact = track_path(&template, &c.tracks[0], observer, arrival, im.dt, &p);
        let tau = ((arrival - c.clock - im.t0) / im.dt).round() as i64;
        let r0 = im.row_of(p.f0_lo).round() as usize;
        let nf0 = im.row_of(p.f0_hi).round() as usize + 1 - r0;
        let score = |samples: &[Sample]| {
            let full = samples.iter().fold((0.0, 0.0), |(a, b), s| {
                if s.approach {
                    (a + s.w * s.w, b)
                } else {
                    (a, b + s.w * s.w)
                }
            });
            sweep_tau(
                &im,
                samples,
                tau,
                r0,
                nf0,
                p.stat,
                &mut SweepBuffers::new(nf0),
                full,
                None,
            )
            .expect("both halves of the audible pass are covered")
        };
        let bank_score = score(&template);
        let exact_score = score(&exact);
        let tone = im.f_lo * ((r0 + exact_score.1) as f64 / im.bpo).exp2();
        assert!((tone / 115.0).ln().abs() < std::f64::consts::LN_2 / im.bpo);
        assert!(exact_score.0 > 0.0);
        assert_eq!(bank_score.1, exact_score.1);
        assert!(
            (bank_score.0 - exact_score.0).abs() < 0.001,
            "bank {bank_score:?}, exact physical curve {exact_score:?}"
        );
    }

    #[test]
    fn bank_null_interpolates_quantiles_without_nearest_cell_evidence_cliffs() {
        let p = MatchParams {
            v_grid: vec![40.0, 160.0],
            d_grid: vec![100.0, 1600.0],
            ..Default::default()
        };
        let nulls = [
            Null {
                q50: 10.0,
                beta: 2.0,
            },
            Null {
                q50: 18.0,
                beta: 4.0,
            },
            Null {
                q50: 26.0,
                beta: 6.0,
            },
            Null {
                q50: 42.0,
                beta: 12.0,
            },
        ];
        let bank = Bank {
            surfaces: nulls
                .into_iter()
                .map(|null| Surface {
                    null,
                    peaks: Vec::new(),
                })
                .collect(),
            r0: 0,
            nf0: 0,
            tones: Vec::new(),
        };
        // Exact nodes and clamped corners preserve their stored null bit for bit.
        for (v, d, i) in [
            (40.0, 100.0, 0),
            (40.0, 1600.0, 1),
            (160.0, 100.0, 2),
            (160.0, 1600.0, 3),
            (1.0, 1.0, 0),
            (1.0, 100_000.0, 1),
            (1000.0, 1.0, 2),
            (1000.0, 100_000.0, 3),
        ] {
            let n = bank.null_at(&p, v, d);
            assert_eq!(n.q50.to_bits(), nulls[i].q50.to_bits());
            assert_eq!(n.beta.to_bits(), nulls[i].beta.to_bits());
        }
        // The geometric midpoint has equal weights on the four fitted quantiles.
        let middle = bank.null_at(&p, 80.0, 400.0);
        assert_eq!((middle.q50, middle.beta), (24.0, 6.0));
        let q90 = |n: Null| f64::from(n.q50) + f64::from(n.beta) * 5f64.ln();
        let mean_q90 = nulls.into_iter().map(q90).sum::<f64>() / 4.0;
        assert!((q90(middle) - mean_q90).abs() < 1e-6);
        // Unequal log-axis fractions distinguish quantile interpolation from nearest lookup.
        let asymmetric = bank.null_at(&p, 40.0 * 2f64.sqrt(), 800.0);
        assert!((asymmetric.q50 - 21.5).abs() < 2e-6);
        assert!((asymmetric.beta - 5.25).abs() < 2e-6);
        assert!(asymmetric.beta > 0.0);
        // A fixed acoustic score cannot jump in evidence at either old Voronoi boundary.
        for ((v0, d0), (v1, d1)) in [
            ((80.0 * (1.0 - 1e-6), 400.0), (80.0 * (1.0 + 1e-6), 400.0)),
            ((80.0, 400.0 * (1.0 - 1e-6)), (80.0, 400.0 * (1.0 + 1e-6))),
        ] {
            let left = bank.null_at(&p, v0, d0);
            let right = bank.null_at(&p, v1, d1);
            assert!((left.nats(50.0) - right.nats(50.0)).abs() < 1e-4);
        }
    }

    #[test]
    fn bank_null_interpolation_handles_singleton_axes() {
        let nulls = [
            Null {
                q50: 10.0,
                beta: 2.0,
            },
            Null {
                q50: 30.0,
                beta: 6.0,
            },
        ];
        let bank = Bank {
            surfaces: nulls
                .into_iter()
                .map(|null| Surface {
                    null,
                    peaks: Vec::new(),
                })
                .collect(),
            r0: 0,
            nf0: 0,
            tones: Vec::new(),
        };
        for (v_grid, d_grid, v, d) in [
            (vec![80.0], vec![100.0, 1600.0], 2.0, 400.0),
            (vec![40.0, 160.0], vec![400.0], 80.0, 1.0),
        ] {
            let p = MatchParams {
                v_grid,
                d_grid,
                ..Default::default()
            };
            let n = bank.null_at(&p, v, d);
            assert_eq!((n.q50, n.beta), (20.0, 4.0));
        }
        let p = MatchParams {
            v_grid: vec![80.0],
            d_grid: vec![400.0],
            ..Default::default()
        };
        let single = Null {
            q50: -0.0,
            beta: 0.05,
        };
        let bank = Bank {
            surfaces: vec![Surface {
                null: single,
                peaks: Vec::new(),
            }],
            r0: 0,
            nf0: 0,
            tones: Vec::new(),
        };
        for (v, d) in [(80.0, 400.0), (2.0, 1e6), (1e6, 2.0)] {
            let n = bank.null_at(&p, v, d);
            assert_eq!(n.q50.to_bits(), single.q50.to_bits());
            assert_eq!(n.beta.to_bits(), single.beta.to_bits());
        }
    }

    #[test]
    fn alternative_clock_sweeps_keep_unvisited_tails_and_fractional_phases() {
        let wide = (-10.0, 10.0);
        // The centre at 2 was visited from 0, but its far edge at 4 was not.
        assert!(!clock_sweep_was_covered(2.0, &[0.0], 1.0, 2, wide));
        assert!(clock_sweep_was_covered(1.0, &[0.0, 2.0], 1.0, 2, wide));

        let wide = (90.0, 110.0);
        assert!(!clock_sweep_was_covered(102.0, &[100.0], 0.5, 4, wide));
        // Coverage can come from multiple sweeps, but covered endpoints do not fill a gap.
        assert!(clock_sweep_was_covered(100.0, &[98.0, 102.0], 0.5, 4, wide));
        assert!(!clock_sweep_was_covered(100.0, &[96.0, 104.0], 0.5, 4, wide));

        let clipped = (100.0, 100.75);
        assert!(clock_sweep_was_covered(100.5, &[100.0], 0.5, 4, clipped));
        assert!(!clock_sweep_was_covered(100.25, &[100.0], 0.5, 4, clipped));
        assert!(clock_sweep_was_covered(100.25, &[100.25], 0.5, 4, clipped));

        let fixed = (100.25, 100.25);
        assert!(!clock_sweep_was_covered(100.25, &[100.0], 0.5, 4, fixed));
        assert!(clock_sweep_was_covered(100.25, &[100.25], 0.5, 4, fixed));
    }

    #[test]
    fn alternative_clock_peaks_keep_boundaries_plateaus_and_coarse_priority() {
        let votes = [6.0, 6.0, 0.0, 4.0, 4.0, 4.0, 0.0, 3.0, 1.0, 5.0, 5.0];
        let first = Top {
            coarse: votes[1],
            cell: 7,
            bin: 1,
        };
        let peaks = clock_peak_candidates(&votes, (0, votes.len() - 1), first, 1);
        let bins: Vec<_> = peaks.iter().map(|p| p.bin).collect();
        // The original tie winner stays first; plateau pivots/endpoints precede its interior.
        assert_eq!(bins, [1, 0, 9, 10, 3, 5, 4, 7]);
        assert!(peaks.iter().all(|p| p.cell == first.cell));
        assert!(peaks.windows(2).all(|p| p[0].coarse >= p[1].coarse));
    }

    #[test]
    fn alternative_clock_peaks_cover_long_plateaus_with_bounded_exact_starts() {
        const N: usize = 2001;
        const HALF: i64 = 4;
        const DT: f64 = 0.5;
        let origin = 1000.0;
        let votes = vec![5.0; N];
        let first = Top {
            coarse: 5.0,
            cell: 0,
            bin: 1001,
        };
        let peaks = clock_peak_candidates(&votes, (0, N - 1), first, HALF);
        assert_eq!(peaks.len(), N);
        assert_eq!(peaks[0].bin, first.bin);
        let window = (
            origin - HALF as f64 * DT,
            origin + (N - 1 + HALF as usize) as f64 * DT,
        );
        let mut swept = Vec::new();
        for peak in peaks {
            let clock = origin + peak.bin as f64 * DT;
            if !clock_sweep_was_covered(clock, &swept, DT, HALF, window) {
                let at = swept.partition_point(|&old| old < clock);
                swept.insert(at, clock);
            }
        }
        // Check actual timestamps independently of the coverage predicate.
        let mut visited: Vec<_> = swept
            .iter()
            .flat_map(|&clock| (-HALF..=HALF).map(move |k| clock - k as f64 * DT))
            .collect();
        visited.sort_by(f64::total_cmp);
        visited.dedup();
        for bin in 0..N {
            for k in -HALF..=HALF {
                let clock = origin + bin as f64 * DT - k as f64 * DT;
                assert!(
                    visited.binary_search_by(|old| old.total_cmp(&clock)).is_ok(),
                    "bin {bin}, step {k} was never swept"
                );
            }
        }
        let lower_bound = (N + 2 * HALF as usize).div_ceil((2 * HALF + 1) as usize);
        assert!(
            swept.len() >= lower_bound && swept.len() <= lower_bound + 2,
            "{} starts for {N} candidates; interval-cover lower bound {lower_bound}",
            swept.len()
        );
        println!(
            "clock plateau: {N} candidates, {} exact starts, interval-cover lower bound {lower_bound}",
            swept.len()
        );
    }

    #[test]
    fn small_region_grids_keep_a_centre_observer() {
        let (lat, lon, _) = SCENE_AT;
        let (mlat, mlon) = m_per_deg(lat);
        let p = MatchParams::default();
        for half_m in [1.0, 25.0, 50.0, 99.0, 100.0] {
            let region = BBox::new(
                lat - half_m / mlat,
                lon - half_m / mlon,
                lat + half_m / mlat,
                lon + half_m / mlon,
            );
            let g = make_grid(&p, &region, &FlatElevation(0.0));
            assert_eq!(grid_cell_count(&p, &region), Some(g.inside.len()));
            let nearest = g
                .inside
                .iter()
                .enumerate()
                .filter(|&(_, &inside)| inside)
                .map(|(i, _)| {
                    let (la, lo) = g.latlon(i % g.ncols, i / g.ncols);
                    assert!(region.contains(la, lo));
                    haversine_m(lat, lon, la, lo)
                })
                .fold(f64::INFINITY, f64::min);
            assert!(
                nearest < 0.001,
                "{half_m} m half-width: nearest cell {nearest} m"
            );
        }
    }

    #[test]
    fn grid_dimensions_bound_the_geographic_projection_and_check_allocation() {
        let (lat, lon, _) = SCENE_AT;
        let p = MatchParams::default();
        for (half_lat, half_lon) in [(0.001, 0.001), (5.0, 0.025), (0.0005, 2.0), (2.0, 3.0)] {
            let region = BBox::new(
                lat - half_lat,
                lon - half_lon,
                lat + half_lat,
                lon + half_lon,
            );
            let shape = grid_shape(&p, &region).unwrap();
            assert_eq!(
                grid_cell_count(&p, &region),
                Some(shape.ncols * shape.nrows)
            );
            let enu = Enu::new(lat, lon, 0.0);
            for i in 0..=16 {
                let la = region.south + (region.north - region.south) * i as f64 / 16.0;
                for j in 0..=16 {
                    let lo = region.west + (region.east - region.west) * j as f64 / 16.0;
                    // Independently intersect the actual WGS84 normal with the tangent plane.
                    let at = enu.from_geodetic(la, lo, 0.0);
                    let (slat, clat) = la.to_radians().sin_cos();
                    let (slon, clon) = lo.to_radians().sin_cos();
                    let normal = enu.vec_from_ecef([clat * clon, clat * slon, slat]);
                    let h = -at[2] / normal[2];
                    let (east, north) = (at[0] + h * normal[0], at[1] + h * normal[1]);
                    assert!(
                        east >= shape.e0 && east <= shape.e0 + (shape.ncols - 1) as f64 * p.cell_m
                    );
                    assert!(
                        north >= shape.n0
                            && north <= shape.n0 + (shape.nrows - 1) as f64 * p.cell_m
                    );
                }
            }
        }
        let region = BBox::new(lat - 0.1, lon - 0.1, lat + 0.1, lon + 0.1);
        for cell_m in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e-12] {
            assert!(
                grid_cell_count(
                    &MatchParams {
                        cell_m,
                        ..p.clone()
                    },
                    &region
                )
                .is_none()
            );
        }
        assert!(grid_cell_count(&p, &BBox::new(-80.0, -170.0, 80.0, 170.0)).is_none());
    }

    #[test]
    fn regional_grids_cover_both_edges_of_long_thin_regions() {
        let (lat, lon, _) = SCENE_AT;
        let p = MatchParams::default();
        for region in [
            BBox::new(lat - 5.0, lon - 0.025, lat + 5.0, lon + 0.025),
            BBox::new(lat - 0.0005, lon - 2.0, lat + 0.0005, lon + 2.0),
        ] {
            let g = make_grid(&p, &region, &FlatElevation(0.0));
            let points: Vec<_> = g
                .inside
                .iter()
                .enumerate()
                .filter(|&(_, &inside)| inside)
                .map(|(i, _)| g.latlon(i % g.ncols, i / g.ncols))
                .collect();
            assert!(!points.is_empty(), "no cells in {region:?}");
            assert!(points.iter().all(|&(la, lo)| region.contains(la, lo)));
            for k in 0..=8 {
                let f = k as f64 / 8.0;
                let la = region.south + f * (region.north - region.south);
                let lo = region.west + f * (region.east - region.west);
                for (la, lo) in [
                    (la, region.west),
                    (la, region.east),
                    (region.south, lo),
                    (region.north, lo),
                ] {
                    let distance = points
                        .iter()
                        .map(|&(a, b)| haversine_m(la, lo, a, b))
                        .fold(f64::INFINITY, f64::min);
                    assert!(
                        distance <= p.cell_m * std::f64::consts::SQRT_2,
                        "grid misses boundary ({la}, {lo}) of {region:?} by {distance} m"
                    );
                }
            }
        }
    }

    #[test]
    fn coarse_passes_use_the_observer_the_grid_reports() {
        let (lat, lon, clock) = SCENE_AT;
        let p = MatchParams::default();
        let region = BBox::new(lat - 5.0, lon - 0.025, lat + 5.0, lon + 0.025);
        let ground = 1200.0;
        let g = make_grid(&p, &region, &FlatElevation(ground));
        let (cell, (la, lo)) = g
            .inside
            .iter()
            .enumerate()
            .filter(|&(_, &inside)| inside)
            .map(|(i, _)| (i, g.latlon(i % g.ncols, i / g.ncols)))
            .max_by(|a, b| a.1.0.total_cmp(&b.1.0))
            .unwrap();
        assert!(la > lat + 4.5);
        let local = Enu::new(la, lo, ground + MIC_HEIGHT_M);
        let tr = straight_track(
            &local,
            [0.0, -200.0, 700.0],
            [100.0, 0.0, 0.0],
            clock + 100.0,
            clock,
            clock + 200.0,
            4.0,
        );
        let mut stamps = Vec::new();
        stamp_track(
            &tr,
            &g,
            &p,
            &|_| BASE_C,
            clock,
            (clock, clock + 200.0),
            &mut stamps,
        );
        let passes: Vec<_> = stamps
            .iter()
            .filter(|&&(i, _)| i as usize == cell)
            .collect();
        assert_eq!(
            passes.len(),
            1,
            "the reported cell must retain its physical pass"
        );
        let expected = 100.0 + 200.0f64.hypot(700.0) / BASE_C;
        assert!(
            (f64::from(passes[0].1.arrive) - expected).abs() < 0.001,
            "coarse arrival {} differs from the reported observer's {expected}",
            passes[0].1.arrive
        );
    }

    fn observer_cell(p: &MatchParams) -> Grid {
        let (lat, lon, _) = SCENE_AT;
        Grid {
            enu: Enu::new(lat, lon, 0.0),
            region: BBox::new(lat, lon, lat, lon),
            ncols: 1,
            nrows: 1,
            e0: 0.0,
            n0: 0.0,
            step: p.cell_m,
            observers: vec![[0.0, 0.0, MIC_HEIGHT_M]],
            horizontal_shift_m: 0.0,
            inside: vec![true],
        }
    }

    /// A 120 s circuit at 63 m/s, with alternating 800/400 m heights on its near side.
    /// Every shallower visit was hidden by a closer neighbour in the old 360 s chunks.
    fn short_circuit(first_cpa: f64) -> TrackModel {
        let (lat, lon, clock) = SCENE_AT;
        let enu = Enu::new(lat, lon, 0.0);
        let omega = std::f64::consts::TAU / 120.0;
        let mut tr = scene_track(
            [300.0, 0.0, 800.0],
            [0.0, 1200.0 * omega, 0.0],
            first_cpa,
            -120.0,
            960.0,
        );
        tr.key = "short-circuit".into();
        tr.icao = tr.key.clone();
        for (i, &t) in tr.t.iter().enumerate() {
            let theta = omega * (t - clock - first_cpa);
            let (sin, cos) = theta.sin_cos();
            let (zsin, zcos) = (0.5 * theta).sin_cos();
            tr.pos[i] = enu.to_ecef([1500.0 - 1200.0 * cos, 1200.0 * sin, 600.0 + 200.0 * zcos]);
            tr.vel[i] = enu.vec_to_ecef([
                1200.0 * omega * sin,
                1200.0 * omega * cos,
                -100.0 * omega * zsin,
            ]);
        }
        tr
    }

    /// A pass just inside/outside the reach, straight or turning at 3 degrees per second.
    fn grazing_track(distance: f64, turning: bool) -> TrackModel {
        let (lat, lon, clock) = SCENE_AT;
        let enu = Enu::new(lat, lon, 0.0);
        let height = 800.0;
        let offset = (distance * distance - (height - MIC_HEIGHT_M).powi(2)).sqrt();
        let mut tr = scene_track(
            [offset, 0.0, height],
            [0.0, 100.0, 0.0],
            100.0,
            -200.0,
            1400.0,
        );
        if turning {
            let omega = std::f64::consts::TAU / 120.0;
            for (i, &t) in tr.t.iter().enumerate() {
                let (sin, cos) = (omega * (t - clock - 100.0)).sin_cos();
                tr.pos[i] = enu.to_ecef([offset + 1200.0 * (1.0 - cos), 1200.0 * sin, height]);
                tr.vel[i] = enu.vec_to_ecef([1200.0 * omega * sin, 1200.0 * omega * cos, 0.0]);
            }
        }
        tr.key = "grazing".into();
        tr.icao = tr.key.clone();
        tr
    }

    #[test]
    fn grazing_pass_keeps_an_eligible_sample_when_the_quadratic_step_is_worse() {
        let (lat, lon, clock) = SCENE_AT;
        let enu = Enu::new(lat, lon, 0.0);
        let p = MatchParams::default();
        let mut tr = scene_track(
            [0.0, 7999.8, 600.0 + MIC_HEIGHT_M],
            [100.0, 0.0, 0.0],
            4.0,
            0.0,
            8.0,
        );
        assert_eq!(tr.t.len(), 3);
        let ranges = [p.reach_m + 0.8, p.reach_m - 0.2, p.reach_m];
        for (i, (east, radial_v)) in [(-400.0f64, 0.0), (0.0, 1.0), (400.0, -5.0)]
            .into_iter()
            .enumerate()
        {
            let north = (ranges[i].powi(2) - east.powi(2) - 600.0f64.powi(2)).sqrt();
            tr.pos[i] = enu.to_ecef([east, north, 600.0 + MIC_HEIGHT_M]);
            tr.vel[i] = enu.vec_to_ecef([100.0, radial_v, 0.0]);
        }
        // The sampled squared ranges suggest a later CPA, but the actual cubic path there
        // is farther away. The valid middle sample must survive the final physical gate.
        let da = ranges[0].powi(2) - ranges[1].powi(2);
        let db = ranges[2].powi(2) - ranges[1].powi(2);
        let shift = 2.0 * (da - db) / (da + db);
        let observer = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        assert!(norm(sub(tr.pos_at(clock + 4.0 + shift), observer)) > p.reach_m);
        assert!(norm(sub(tr.pos_at(clock + 4.0), observer)) < p.reach_m);
        let mut stamped = Vec::new();
        stamp_track(
            &tr,
            &observer_cell(&p),
            &p,
            &|_| BASE_C,
            clock,
            (clock, clock + 8.0),
            &mut stamped,
        );
        assert_eq!(stamped.len(), 1);
        let expected = 4.0 + ranges[1] / BASE_C;
        assert!((stamped[0].1.arrive as f64 - expected).abs() < 0.001);
    }

    #[test]
    fn grazing_passes_reach_coarse_votes_between_range_samples() {
        let (_, _, clock) = SCENE_AT;
        let p = MatchParams::default();
        let g = observer_cell(&p);
        for turning in [false, true] {
            for distance in [p.reach_m - 0.5, p.reach_m + 0.5] {
                let tr = grazing_track(distance, turning);
                for phase in [0.0, 2.0] {
                    let mut stamped = Vec::new();
                    stamp_track(
                        &tr,
                        &g,
                        &p,
                        &|_| BASE_C,
                        clock,
                        (clock + phase, clock + 360.0 + phase),
                        &mut stamped,
                    );
                    if distance < p.reach_m {
                        let arrival = 100.0 + distance / BASE_C;
                        assert!(
                            stamped.iter().any(|(_, pass)| {
                                (pass.arrive as f64 - arrival).abs() < 0.1
                            }),
                            "eligible grazing pass lost: turning={turning}, phase={phase}"
                        );
                    } else {
                        assert!(stamped.is_empty(), "a refined pass outside reach voted");
                    }
                }
            }
        }
    }

    #[test]
    fn grazing_passes_are_heard_across_exact_clock_seeds() {
        let p = MatchParams::default();
        let mut crossing = scene_track(
            [0.0, 800.0, 600.0 + MIC_HEIGHT_M],
            [100.0, 0.0, 0.0],
            300.0,
            -200.0,
            800.0,
        );
        crossing.key = "crossing".into();
        crossing.icao = crossing.key.clone();
        // Keep enough off-pass audio for the empirical null of this broad, distant curve.
        let mut c = scene_for(
            vec![grazing_track(p.reach_m - 0.5, false), crossing],
            200.0,
            900.0,
        );
        // Its sampled range at CPA +/- 2 s exceeds reach, although the actual pass is eligible.
        let observer = geodetic_to_ecef(c.lat, c.lon, MIC_HEIGHT_M);
        assert!(norm(sub(c.tracks[0].pos_at(c.clock + 98.0), observer)) > p.reach_m);
        assert!(norm(sub(c.tracks[0].pos_at(c.clock + 102.0), observer)) > p.reach_m);
        assert!(norm(sub(c.tracks[0].pos_at(c.clock + 100.0), observer)) < p.reach_m);
        // An almost identical unrecorded path still must fail the unchanged final range gate.
        let mut outside = grazing_track(p.reach_m + 0.5, false);
        outside.key = "outside-reach".into();
        outside.icao = outside.key.clone();
        c.tracks.push(outside);
        let dem = FlatElevation(0.0);
        let atmo = UniformAtmosphere(Medium::still(BASE_C));
        let im = build_image(&c.x, 2800, &p).unwrap();
        let bank = build_bank(&im, &p);
        let idx: Vec<_> = c
            .tracks
            .iter()
            .enumerate()
            .map(|(i, tr)| ExactTrack::new(i, tr))
            .collect();
        let inp = MatchInputs {
            image: &im,
            tracks: &c.tracks,
            region: c.region,
            start_lo: c.clock,
            start_hi: c.clock + 2.0,
            duration_s: c.x.len() as f64 / 2800.0,
            catalog: (f64::NEG_INFINITY, f64::INFINITY),
            dem: &dem,
            fine_dem: &dem,
            atmo: &atmo,
        };
        // The aligned seed establishes audibility before the original failing sampling phases.
        for phase in [2.0, 0.0, 0.5, 1.0, 1.5] {
            let e = score_exact(&inp, &p, &bank, &idx, c.lat, c.lon, c.clock + phase)
                .expect("the crossing flight supplies an exact score");
            println!(
                "grazing seed {phase}: heard {}, passes {:?}",
                e.n_heard, e.passes
            );
            assert!((inp.start_lo..=inp.start_hi).contains(&e.clock));
            for key in ["grazing", "crossing"] {
                assert!(
                    e.passes.iter().any(|pass| {
                        pass.key == key && counts_as_heard(pass.value, pass.nats, pass.shared)
                    }),
                    "{key} was not independently heard at seed {phase}: {:?}",
                    e.passes
                );
            }
            assert!(e.passes.iter().all(|pass| pass.key != "outside-reach"));
        }
    }

    #[test]
    fn coarse_stamps_keep_every_short_circuit_visit_once() {
        let (_, _, clock) = SCENE_AT;
        // Both exact ownership boundaries (300 and 540 s in the default chunks) and passes
        // between survey samples must survive a final chunk whose duration is not a multiple
        // of the 4 s sample interval. Different chunk partitions must emit identical stamps.
        for first_cpa in [60.0, 60.75] {
            let tr = short_circuit(first_cpa);
            let mut reference = None;
            for (chunk_s, chunk_step_s) in [(360.0, 240.0), (240.0, 240.0), (80.0, 61.0)] {
                let p = MatchParams {
                    chunk_s,
                    chunk_step_s,
                    ..MatchParams::default()
                };
                let g = observer_cell(&p);
                let mut stamped = Vec::new();
                stamp_track(
                    &tr,
                    &g,
                    &p,
                    &|_| BASE_C,
                    clock,
                    (clock, clock + 719.0),
                    &mut stamped,
                );
                stamped.sort_by(|a, b| a.1.arrive.total_cmp(&b.1.arrive));
                assert_eq!(
                    stamped.len(),
                    6,
                    "chunk {chunk_s}/{chunk_step_s}, CPA {first_cpa}"
                );
                let keys: Vec<_> = stamped
                    .iter()
                    .map(|&(cell, pass)| (cell, pass.arrive.to_bits(), pass.tpl))
                    .collect();
                if let Some(expected) = &reference {
                    assert_eq!(&keys, expected, "chunk partition changed a pass");
                } else {
                    reference = Some(keys);
                }
                for (k, &(cell, pass)) in stamped.iter().enumerate() {
                    let height: f64 = if k % 2 == 0 { 800.0 } else { 400.0 };
                    let distance = 300.0f64.hypot(height - MIC_HEIGHT_M);
                    let heard = first_cpa + 120.0 * k as f64 + distance / BASE_C;
                    assert_eq!(cell, 0);
                    assert!(
                        (pass.arrive as f64 - heard).abs() < 0.1,
                        "visit {k}: {} vs {heard}",
                        pass.arrive
                    );
                }
            }
        }
    }

    #[test]
    fn coarse_stamps_preserve_a_straight_pass_and_track_boundary_rules() {
        let (_, _, clock) = SCENE_AT;
        let tr = scene_track(
            [0.0, 800.0, 600.0 + MIC_HEIGHT_M],
            [80.0, 0.0, 0.0],
            300.0,
            0.0,
            720.0,
        );
        let p = MatchParams {
            v_grid: vec![50.0, 80.0, 120.0],
            d_grid: vec![500.0, 1000.0, 2000.0],
            ..MatchParams::default()
        };
        let g = observer_cell(&p);
        let mut stamped = Vec::new();
        stamp_track(
            &tr,
            &g,
            &p,
            &|_| BASE_C,
            clock,
            (clock, clock + 720.0),
            &mut stamped,
        );
        assert_eq!(stamped.len(), 1);
        assert_eq!(stamped[0].0, 0);
        assert_eq!(stamped[0].1.arrive, (300.0 + 1000.0 / BASE_C) as f32);
        assert_eq!(stamped[0].1.tpl, 4);

        // A clipped catalogue span is not evidence of a pass at its edge.
        for span in [
            (clock + 100.0, clock + 290.0),
            (clock + 310.0, clock + 500.0),
        ] {
            stamped.clear();
            stamp_track(&tr, &g, &p, &|_| BASE_C, clock, span, &mut stamped);
            assert!(stamped.is_empty());
        }
        // A real track endpoint is still retained for the exact scorer's existing end rule.
        let partial = scene_track(
            [0.0, 800.0, 600.0 + MIC_HEIGHT_M],
            [80.0, 0.0, 0.0],
            300.0,
            0.0,
            292.0,
        );
        stamped.clear();
        stamp_track(
            &partial,
            &g,
            &p,
            &|_| BASE_C,
            clock,
            (clock, clock + 720.0),
            &mut stamped,
        );
        assert_eq!(stamped.len(), 1);
        let expected = 292.0 + 1000.0f64.hypot(8.0 * 80.0) / BASE_C;
        assert!((stamped[0].1.arrive as f64 - expected).abs() < 0.001);

        // A genuine CPA inside the first or last sample interval still gets sub-sample timing.
        for cpa in [1.0, 59.0] {
            let boundary = scene_track(
                [0.0, 800.0, 600.0 + MIC_HEIGHT_M],
                [80.0, 0.0, 0.0],
                cpa,
                0.0,
                60.0,
            );
            stamped.clear();
            stamp_track(
                &boundary,
                &g,
                &p,
                &|_| BASE_C,
                clock,
                (clock, clock + 720.0),
                &mut stamped,
            );
            assert_eq!(stamped.len(), 1);
            assert!((stamped[0].1.arrive as f64 - cpa - 1000.0 / BASE_C).abs() < 0.001);
        }
    }

    #[test]
    fn coarse_stamps_feed_a_short_circuit_and_crossing_flight_into_the_match() {
        let circuit = short_circuit(60.0);
        let mut crossing =
            scene_track([0.0, -600.0, 500.0], [80.0, 0.0, 0.0], 240.0, -120.0, 960.0);
        crossing.key = "crossing".into();
        crossing.icao = crossing.key.clone();
        // Actual harmonic PCM from the moving tracks, with independent fixed-seed noise.
        let c = scene_for(vec![circuit, crossing], 400.0, 360.0);
        let p = MatchParams {
            decoy_span_s: 360.0,
            ..MatchParams::default()
        };
        let r = run_case(&c, &p, c.clock - 10.0, c.clock + 10.0);
        let top = r
            .modes
            .first()
            .expect("the heard passes seed an exact match");
        let error = haversine_m(top.lat, top.lon, c.lat, c.lon);
        eprintln!(
            "short-circuit observer error {error:.6} m, clock offset {:.3} s, {} heard",
            top.clock_unix - c.clock, top.n_heard
        );
        assert!(error < 150.0, "{error} m: {top:?}");
        assert!((top.clock_unix - c.clock).abs() < 2.0, "{top:?}");
        assert!(top.n_heard >= 3, "{top:?}");
        assert!(top.passes.iter().any(|pass| {
            pass.key == "crossing" && counts_as_heard(pass.value, pass.nats, pass.shared)
        }));
        assert!(
            top.passes
                .iter()
                .filter(|pass| {
                    pass.key == "short-circuit"
                        && counts_as_heard(pass.value, pass.nats, pass.shared)
                })
                .count()
                >= 2,
            "the returning aircraft must supply distinct heard visits: {top:?}"
        );
    }

    #[test]
    fn physical_polish_compares_every_retained_mode_on_the_same_data_basis() {
        let circuit = short_circuit(60.0);
        let mut crossing =
            scene_track([0.0, -600.0, 500.0], [80.0, 0.0, 0.0], 240.0, -120.0, 960.0);
        crossing.key = "crossing".into();
        crossing.icao = crossing.key.clone();
        let c = scene_for(vec![circuit, crossing], 400.0, 360.0);
        // Several retained starts lie on the same acoustic peak. Physical refinement can
        // lower calibrated evidence, so a start outside the evidence-refinement budget
        // must receive the same fit before it competes with a refined answer.
        let p = MatchParams {
            nms_m: 200.0,
            refine_modes: 1,
            decoy_span_s: 360.0,
            ..MatchParams::default()
        };
        let r = run_case(&c, &p, c.clock - 10.0, c.clock + 10.0);
        let top = r.modes.first().expect("the independent heard passes survive");
        let error = haversine_m(top.lat, top.lon, c.lat, c.lon);
        assert!(error < 150.0, "{error} m: {top:?}");
        assert!((top.clock_unix - c.clock).abs() < 2.0, "{top:?}");
        assert!(top.n_heard >= 3, "{top:?}");
    }

    #[test]
    fn alternative_clock_peaks_recover_the_three_flight_recording() {
        let mut heard: Vec<_> = [
            ([0.0, 800.0, 600.0 + MIC_HEIGHT_M], [80.0, 0.0, 0.0], 70.0),
            ([750.0, 0.0, 650.0], [0.0, 95.0, 0.0], 190.0),
            ([600.0, -600.0, 550.0], [-70.0, -70.0, 0.0], 310.0),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (pos, vel, cpa))| {
            let mut tr = scene_track(pos, vel, cpa, -120.0, 960.0);
            tr.key = format!("heard-{i}");
            tr.icao = tr.key.clone();
            tr
        })
        .collect();
        let mut c = scene_for(heard.clone(), 200.0, 400.0);
        // The earlier sky repeats two paths. Extra parallel flights at the same slant range
        // vote for the first sound, but exact pixel ownership can credit that sound only once.
        // Only the true clock contains the third independent heard pass.
        for tr in heard.iter_mut().take(2) {
            for t in &mut tr.t {
                *t -= 600.0;
            }
            tr.key = format!("earlier-{}", tr.key);
            tr.icao = tr.key.clone();
            c.tracks.push(tr.clone());
        }
        for (i, (north, height)) in [(-800.0, 600.0), (600.0, 800.0), (-600.0, 800.0)]
            .into_iter()
            .enumerate()
        {
            let mut tr = scene_track(
                [0.0, north, height + MIC_HEIGHT_M],
                [80.0, 0.0, 0.0],
                -530.0,
                -720.0,
                360.0,
            );
            tr.key = format!("earlier-parallel-{i}");
            tr.icao = tr.key.clone();
            c.tracks.push(tr);
        }
        let p = MatchParams {
            decoy_span_s: 900.0,
            probe: Some((c.lat, c.lon, c.clock)),
            ..MatchParams::default()
        };
        // This small region fits in any selected spatial start's existing refinement disc.
        // The regression isolates lost clocks rather than changing the spatial shortlist.
        assert!(
            haversine_m(c.region.south, c.region.west, c.region.north, c.region.east) < p.nms_m
        );
        let started = std::time::Instant::now();
        let r = run_case_logged(
            &c,
            &p,
            c.clock - 610.0,
            c.clock + 10.0,
            (f64::NEG_INFINITY, f64::INFINITY),
            &|message| println!("{message}"),
        );
        let truth = r.probe.first().expect("the known observer scores");
        assert!(truth.n_heard >= 3, "{truth:?}");
        let top = r.modes.first().expect("the recording seeds an exact match");
        println!("known observer: {truth:?}");
        println!(
            "clock alternatives: {:.3} s total; stages {:?}; true evidence {:.3}, found {:.3}, clock error {:+.3} s",
            started.elapsed().as_secs_f64(),
            r.timings_s,
            truth.evidence,
            top.evidence,
            top.clock_unix - c.clock
        );
        assert!(
            (top.clock_unix - c.clock).abs() < 3.0,
            "a coarse clock alias hid the recording: {top:?}; truth {truth:?}; refined truth {:?}",
            r.probe.get(1)
        );
        assert!(haversine_m(top.lat, top.lon, c.lat, c.lon) < 200.0, "{top:?}");
        for i in 0..3 {
            assert!(
                top.passes.iter().any(|pass| {
                    pass.key == format!("heard-{i}")
                        && counts_as_heard(pass.value, pass.nats, pass.shared)
                }),
                "the recovered clock must retain heard-{i}: {top:?}"
            );
        }
    }

    /// Both knots are outside the spatial and time filters, but their interpolated pass produces
    /// coarse votes inside them. Exact scoring must retain that same flight.
    #[test]
    fn sparse_interpolated_passes_survive_the_exact_track_filter() {
        let (lat, lon) = (40.8765, -73.525);
        let enu = Enu::new(lat, lon, 0.0);
        let p = MatchParams::default();
        let region = BBox::new(lat - 0.002, lon - 0.002, lat + 0.002, lon + 0.002);
        let pad = region.padded_km(p.reach_m / 1000.0 + 2.0);
        let tr = straight_track(
            &enu,
            [0.0, 0.0, 500.0],
            [160.0, 0.0, 0.0],
            100.0,
            10.0,
            190.0,
            180.0,
        );
        assert_eq!(tr.t.len(), 2);
        for &q in &tr.pos {
            let (la, lo, _) = ecef_to_geodetic(q);
            assert!(!pad.contains(la, lo));
        }
        let span = (30.0, 170.0);
        assert!(tr.t.iter().all(|&t| t < span.0 || t > span.1));
        let grid = make_grid(&p, &region, &FlatElevation(0.0));
        let mut stamps = Vec::new();
        stamp_track(&tr, &grid, &p, &|_| BASE_C, 0.0, span, &mut stamps);
        assert!(
            !stamps.is_empty(),
            "the coarse search sees the crossing pass"
        );
        assert!(track_near_region(&tr, &pad, span, p.sample_s));
        assert!(!track_near_region(&tr, &pad, (200.0, 300.0), p.sample_s));
        assert!(!track_near_region(&tr, &pad, (10.0, 20.0), p.sample_s));

        let far = straight_track(
            &enu,
            [0.0, 30_000.0, 500.0],
            [160.0, 0.0, 0.0],
            100.0,
            10.0,
            190.0,
            180.0,
        );
        assert!(!track_near_region(&far, &pad, span, p.sample_s));
    }

    /// A conservative geometry shortcut must leave all exact evidence and pass metadata unchanged.
    #[test]
    fn spatial_bounds_preserve_exact_scores_with_many_distant_tracks() {
        let mut c = case(0.5, false);
        let p = params();
        let enu = Enu::new(c.lat, c.lon, 0.0);
        let nearby = c.tracks.len();
        for i in 0..1024 {
            let side = if i % 2 == 0 { 1.0 } else { -1.0 };
            let mut tr = straight_track(
                &enu,
                [
                    side * (28_000.0 + 60.0 * (i % 64) as f64),
                    100.0 * (i / 64) as f64,
                    1000.0,
                ],
                [0.0, 70.0, 0.0],
                c.clock + 450.0,
                c.clock - 150.0,
                c.clock + 1050.0,
                1200.0,
            );
            tr.key = format!("distant-{i}");
            c.tracks.push(tr);
        }
        let idx: Vec<_> = c
            .tracks
            .iter()
            .enumerate()
            .map(|(i, tr)| ExactTrack::new(i, tr))
            .collect();
        // None is the production fallback for unknown geometry and also the original unpruned
        // scorer. Both paths see the same full catalogue, pass order, PCM image and calibration.
        let unbounded: Vec<_> = (0..c.tracks.len())
            .map(|index| ExactTrack {
                index,
                bounds: None,
            })
            .collect();
        let dem = FlatElevation(0.0);
        let atmo = UniformAtmosphere(Medium::still(BASE_C));
        let im = build_image(&c.x, 2800, &p).unwrap();
        let bank = build_bank(&im, &p);
        let inp = MatchInputs {
            image: &im,
            tracks: &c.tracks,
            region: c.region,
            start_lo: c.clock - 30.0,
            start_hi: c.clock + 30.0,
            duration_s: c.x.len() as f64 / 2800.0,
            catalog: (f64::NEG_INFINITY, f64::INFINITY),
            dem: &dem,
            fine_dem: &dem,
            atmo: &atmo,
        };
        let signature = |e: Exact| {
            let bits: Vec<_> = [e.evidence, e.r50_m, e.clock]
                .into_iter()
                .chain(e.passes.iter().flat_map(|p| {
                    [
                        p.t_audio,
                        p.slant_m,
                        p.speed_mps,
                        p.f0_hz,
                        p.score,
                        p.nats,
                        p.value,
                        p.clock_offset_s,
                    ]
                }))
                .map(f64::to_bits)
                .collect();
            (
                bits,
                e.n_heard,
                e.n_close,
                serde_json::to_value(e.passes).unwrap(),
            )
        };
        let (mut old_time, mut bounded_time) =
            (std::time::Duration::ZERO, std::time::Duration::ZERO);
        for (east, north, dt) in [
            (0.0, 0.0, 0.0),
            (1000.0, -800.0, 1.25),
            (-2500.0, 2000.0, -1.25),
        ] {
            let (lat, lon, _) = enu.to_geodetic([east, north, 0.0]);
            let x = geodetic_to_ecef(lat, lon, MIC_HEIGHT_M);
            assert!(
                idx[nearby..]
                    .iter()
                    .all(|t| t.bounds.is_some_and(|b| !b.may_reach(x, p.reach_m)))
            );
            let now = std::time::Instant::now();
            let original =
                score_exact(&inp, &p, &bank, &unbounded, lat, lon, c.clock + dt).unwrap();
            old_time += now.elapsed();
            let now = std::time::Instant::now();
            let bounded = score_exact(&inp, &p, &bank, &idx, lat, lon, c.clock + dt).unwrap();
            bounded_time += now.elapsed();
            if east == 0.0 {
                assert!(
                    original.n_heard >= 2,
                    "the unchanged PCM must contain independent heard passes"
                );
            }
            let actual = signature(bounded);
            assert_eq!(actual, signature(original));
            let legacy =
                score_exact_with_recovery(&inp, &p, &bank, &idx, lat, lon, c.clock + dt, false)
                    .unwrap();
            assert!(legacy.passes.iter().all(|pass| !pass.shared));
            assert_eq!(actual, signature(legacy), "noncolliding results changed");
        }
        println!(
            "three exact scores, {nearby} nearby and 1024 distant tracks: original {old_time:?}, bounded {bounded_time:?}"
        );
    }

    /// Frame-quantized seeds may lie outside a sub-frame prior, but the physical prediction and
    /// every shared-clock trial must obey the supplied interval. The quarter-second boundaries
    /// make an unconstrained half-second sweep unable to pass by coincidence.
    #[test]
    fn exact_curve_scores_respect_fractional_start_bounds() {
        let c = case(0.5, false);
        let p = params();
        let dem = FlatElevation(0.0);
        let atmo = UniformAtmosphere(Medium::still(BASE_C));
        let im = build_image(&c.x, 2800, &p).unwrap();
        let bank = build_bank(&im, &p);
        let idx: Vec<_> = c
            .tracks
            .iter()
            .enumerate()
            .map(|(i, tr)| ExactTrack::new(i, tr))
            .collect();
        for (a, b) in [(1.25, 1.25), (-1.25, -1.25), (0.25, 0.4), (-0.4, -0.25)] {
            let inp = MatchInputs {
                image: &im,
                tracks: &c.tracks,
                region: c.region,
                start_lo: c.clock + a,
                start_hi: c.clock + b,
                duration_s: c.x.len() as f64 / 2800.0,
                catalog: (f64::NEG_INFINITY, f64::INFINITY),
                dem: &dem,
                fine_dem: &dem,
                atmo: &atmo,
            };
            for seed in [c.clock - 2.0, c.clock, c.clock + 2.0] {
                let e = score_exact(&inp, &p, &bank, &idx, c.lat, c.lon, seed)
                    .expect("the audible passes score inside the requested interval");
                assert!(
                    (inp.start_lo..=inp.start_hi).contains(&e.clock),
                    "{} outside [{}, {}]",
                    e.clock,
                    inp.start_lo,
                    inp.start_hi
                );
                assert!(
                    e.n_heard >= 2,
                    "the constrained score still hears the flights"
                );
            }
        }
    }

    /// Fixing the clock must still permit a spatial search, including every refinement move.
    #[test]
    fn a_fixed_fractional_clock_still_locates_the_observer() {
        let c = case(0.5, false);
        let p = MatchParams {
            decoy_span_s: 180.0,
            ..params()
        };
        let clock = c.clock + 0.25;
        let r = run_case(&c, &p, clock, clock);
        let top = r.modes.first().expect("a mode at the fixed clock");
        assert!(r.modes.iter().all(|m| m.clock_unix == clock));
        let err = haversine_m(top.lat, top.lon, c.lat, c.lon);
        assert!(err < 200.0, "error {err} m: {top:?}");
        assert!(top.n_heard >= 3, "{top:?}");
    }

    #[test]
    fn three_flights_locate_the_observer_and_beat_every_decoy() {
        let c = case(0.5, false);
        let p = MatchParams {
            probe: Some((c.lat, c.lon, c.clock)),
            ..params()
        };
        let r = run_case(&c, &p, c.clock - 60.0, c.clock + 60.0);
        let top = r.modes.first().expect("a mode");
        let err = haversine_m(top.lat, top.lon, c.lat, c.lon);
        assert!(err < 200.0, "error {err} m: {top:?}");
        // This scene has two near-tied fits, 79 m at +0.5 s and 150 m at +2.5 s: a fixed heard range
        // of 2 km lands on the first, 1 km or 4 km on the second, and so does the profile (best at
        // 4 km), with the decoy margin unchanged (6.03 against 6.10). So the best fit's clock may be
        // 3 s out, and the truth itself, place and clock, must be one the evidence cannot rule out.
        assert!(
            (top.clock_unix - c.clock).abs() < 3.0,
            "clock off by {}: {top:?}",
            top.clock_unix - c.clock
        );
        let truth = r.probe.first().expect("the truth scored");
        assert!(
            top.evidence - truth.evidence <= TIED_NATS && (truth.clock_unix - c.clock).abs() < 2.0,
            "truth {:.2} nats below at {:+.1} s",
            top.evidence - truth.evidence,
            truth.clock_unix - c.clock
        );
        assert!(top.n_heard >= 3, "{top:?}");
        assert!(
            r.decoy_margin.unwrap() > 5.0,
            "{:?} {:?}",
            r.decoy_margin,
            r.decoys
        );
        // Crossing tracks stop the slide: every along-track alias 4 km or more out is clearly worse
        // (4.9 to 8.1 nats down), so the radius stays inside MAX_ASSERT_RADIUS_M. Nearer in, one
        // alias 3 km along AAA's track is only 2.67 nats down, AAA and the silent EEE (its curve on
        // another flight's sound) explaining the audio there: a place the evidence does not rule out,
        // so the radius is 3 km.
        assert!(r.aliases.len() >= 12, "{} aliases", r.aliases.len());
        let far = far_aliases(&r, 3900.0);
        assert!(
            !far.is_empty() && far.iter().all(|&(_, gap)| gap > TIED_NATS),
            "{far:?}"
        );
        let v = verdict(&r).unwrap();
        assert!(v.radius_m <= MAX_ASSERT_RADIUS_M, "{v:?}");
    }

    /// One heard flight places the observer at its closest approach's range, not along its track:
    /// the place slides with the clock at the flight's speed. Every mode can sit on that one ridge,
    /// and the along-track check must find it tied far enough out that the answer cannot stand.
    #[test]
    fn one_heard_flight_cannot_say_where_along_its_track() {
        let c = scene(&[([900.0, 0.0, 700.0], [0.0, 70.0, 0.0], 300.0)], 10_000.0);
        let r = run_case(&c, &params(), c.clock - 120.0, c.clock + 120.0);
        let far = far_aliases(&r, 5500.0);
        assert!(far.iter().any(|&(_, gap)| gap <= TIED_NATS), "{far:?}");
        let v = verdict(&r).unwrap();
        assert!(v.radius_m > MAX_ASSERT_RADIUS_M && !v.stands, "{v:?}");
    }

    /// Two heard flights on parallel tracks, the same way at the same speed, slide together: the
    /// second flight adds evidence but no fix along the track.
    #[test]
    fn parallel_flights_the_same_way_still_slide() {
        let c = scene(
            &[
                ([900.0, 0.0, 700.0], [0.0, 70.0, 0.0], 300.0),
                ([-900.0, 0.0, 700.0], [0.0, 70.0, 0.0], 550.0),
            ],
            10_000.0,
        );
        let r = run_case(&c, &params(), c.clock - 120.0, c.clock + 120.0);
        assert!(r.modes[0].n_heard >= 2, "{:?}", r.modes[0]);
        let far = far_aliases(&r, 5500.0);
        assert!(far.iter().any(|&(_, gap)| gap <= TIED_NATS), "{far:?}");
        let v = verdict(&r).unwrap();
        assert!(v.radius_m > MAX_ASSERT_RADIUS_M && !v.stands, "{v:?}");
    }

    /// One aircraft flying the same leg twice, the same way at the same speed (a circuit), is one
    /// track with two heard passes. The second pass fixes nothing along the track, as a second
    /// aircraft on that line would not; each pass is checked from its own closest approach.
    #[test]
    fn a_lap_flown_twice_still_slides() {
        let mut lap = scene_track([900.0, 0.0, 700.0], [0.0, 70.0, 0.0], 300.0, -1500.0, 450.0);
        // back south 40 km east, out of reach, then north along the same line again
        for piece in [
            scene_track(
                [40_000.0, 0.0, 700.0],
                [0.0, -233.0, 0.0],
                500.0,
                455.0,
                545.0,
            ),
            scene_track([900.0, 0.0, 700.0], [0.0, 70.0, 0.0], 700.0, 550.0, 3000.0),
        ] {
            lap.t.extend(piece.t);
            lap.pos.extend(piece.pos);
            lap.vel.extend(piece.vel);
        }
        lap.key = "LAP".into();
        lap.icao = "LAP".into();
        let c = scene_of(vec![lap], 10_000.0);
        let r = run_case(&c, &params(), c.clock - 120.0, c.clock + 120.0);
        let top = &r.modes[0];
        let heard = top
            .passes
            .iter()
            .filter(|q| q.nats >= HEARD_NATS && !q.shared)
            .count();
        assert!(heard >= 2 && top.n_heard >= 2, "{top:?}");
        let far = far_aliases(&r, 5500.0);
        assert!(far.iter().any(|&(_, gap)| gap <= TIED_NATS), "{far:?}");
        let v = verdict(&r).unwrap();
        assert!(v.radius_m > MAX_ASSERT_RADIUS_M && !v.stands, "{v:?}");
    }

    /// The same two tracks flown in opposite directions: moving the observer along them needs the
    /// clock later for one and earlier for the other, so no twin along either survives.
    #[test]
    fn parallel_flights_opposite_ways_fix_the_place() {
        let c = scene(
            &[
                ([900.0, 0.0, 700.0], [0.0, 70.0, 0.0], 300.0),
                ([-900.0, 0.0, 700.0], [0.0, -70.0, 0.0], 550.0),
            ],
            10_000.0,
        );
        let r = run_case(&c, &params(), c.clock - 120.0, c.clock + 120.0);
        let top = &r.modes[0];
        assert!(top.n_heard >= 2, "{top:?}");
        let err = haversine_m(top.lat, top.lon, c.lat, c.lon);
        assert!(err < 300.0, "error {err} m: {top:?}");
        assert!(r.aliases.len() >= 12, "{} aliases", r.aliases.len());
        let far = far_aliases(&r, 900.0);
        assert!(far.iter().all(|&(_, gap)| gap > TIED_NATS), "{far:?}");
        assert!(verdict(&r).unwrap().radius_m <= MAX_ASSERT_RADIUS_M);
    }

    /// The same three flights heard 2.8 to 4.6 km out, with the two silent ones still 3.2 and 6.1 km
    /// out. One fixed heard range of 2 km gives the far curves a prior of 0.1 to 0.26 to be heard;
    /// profiling the range lets the place take a longer one, paying for the silent passes it then
    /// expects to hear. It must choose the longer range at the observer, still find the observer, and
    /// beat the decoys (which profile too) by more than the fixed range does.
    #[test]
    fn a_recording_that_hears_far_takes_a_long_heard_range() {
        let c = case_scaled(0.5, false, 3.0);
        let fixed = MatchParams {
            r50_m: vec![2000.0],
            ..params()
        };
        let (r, r0) = (
            run_case(&c, &params(), c.clock - 60.0, c.clock + 60.0),
            run_case(&c, &fixed, c.clock - 60.0, c.clock + 60.0),
        );
        let (top, top0) = (
            r.modes.first().expect("a mode"),
            r0.modes.first().expect("a mode"),
        );
        let err = haversine_m(top.lat, top.lon, c.lat, c.lon);
        eprintln!(
            "profiled: err {err:.0} m, r50 {} m, evidence {:.2}, margin {:?}; fixed 2 km: err {:.0} m, evidence {:.2}, margin {:?}; decoy r50s {:?}",
            top.r50_m,
            top.evidence,
            r.decoy_margin,
            haversine_m(top0.lat, top0.lon, c.lat, c.lon),
            top0.evidence,
            r0.decoy_margin,
            r.decoys.iter().map(|d| d.r50_m).collect::<Vec<_>>()
        );
        // far curves are broad, so this scene trades place against clock along the first flight's
        // track; the profile must not make that worse
        let err0 = haversine_m(top0.lat, top0.lon, c.lat, c.lon);
        assert!(
            err <= err0 + 50.0,
            "error {err} m against {err0} m: {top:?}"
        );
        assert!(top.r50_m >= 4000.0, "{top:?}");
        assert!(
            r.decoy_margin.unwrap() > r0.decoy_margin.unwrap() + 1.0,
            "{:?} vs {:?}",
            r.decoy_margin,
            r0.decoy_margin
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
        let idx: Vec<_> = c
            .tracks
            .iter()
            .enumerate()
            .map(|(i, tr)| ExactTrack::new(i, tr))
            .collect();
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

    /// Metamorphic: one heard flight cannot fix a place along its track. Moving the observer `v * dt`
    /// along a straight flight's track and the clock by `dt` leaves its curve where it was, so its
    /// evidence is unchanged. A second heard flight crossing it breaks the trade, and the moved place
    /// falls more than `TIED_NATS` behind. A second flight on a parallel track, flying the same way
    /// at the same speed, does not, so `verdict`'s two-pass rule is needed, not sufficient.
    #[test]
    fn one_flight_trades_place_and_clock_along_its_track_and_a_crossing_flight_fixes_them() {
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
        // north at 70 m/s, closest 900 m east of the observer, 200 s in
        let along = mk([900.0, 0.0, 700.0], [0.0, 70.0, 0.0], 200.0, "AAA");
        // east at 65 m/s, closest 1500 m south, 450 s in: crosses the first at 90 degrees
        let crossing = mk([0.0, -1500.0, 900.0], [65.0, 0.0, 0.0], 450.0, "BBB");
        // north at 70 m/s too, closest 1100 m west, 500 s in: parallel to the first, same speed
        let parallel = mk([-1100.0, 0.0, 800.0], [0.0, 70.0, 0.0], 500.0, "PPP");
        let p = params();
        let dem = FlatElevation(0.0);
        let atmo = UniformAtmosphere(Medium::still(BASE_C));
        let (mlat, _) = m_per_deg(lat);
        // 80 frames, far outside the clock sweep; the place moves 2.8 km north along the track
        let dt = 40.0;
        let moved_lat = lat + 70.0 * dt / mlat;
        let score = |tracks: &[TrackModel]| {
            let mut buf = noise((sr * dur) as usize, 42, 0.05);
            for (i, tr) in tracks.iter().enumerate() {
                add_flight(&mut buf, sr, clock, tr, x, &m, 90.0 + 25.0 * i as f64, 0.5);
            }
            let xs: Vec<f32> = buf.iter().map(|&v| v as f32).collect();
            let im = build_image(&xs, 2800, &p).unwrap();
            let inp = MatchInputs {
                image: &im,
                tracks,
                region: BBox::new(lat - 0.05, lon - 0.07, lat + 0.05, lon + 0.07),
                start_lo: clock - 60.0,
                start_hi: clock + 60.0,
                duration_s: dur,
                catalog: (f64::NEG_INFINITY, f64::INFINITY),
                dem: &dem,
                fine_dem: &dem,
                atmo: &atmo,
            };
            let bank = build_bank(&im, &p);
            let idx: Vec<_> = tracks
                .iter()
                .enumerate()
                .map(|(i, tr)| ExactTrack::new(i, tr))
                .collect();
            let at = score_exact(&inp, &p, &bank, &idx, lat, lon, clock).expect("observer scores");
            let moved = score_exact(&inp, &p, &bank, &idx, moved_lat, lon, clock + dt)
                .expect("moved place scores");
            (at.evidence, at.n_heard, moved.evidence, moved.n_heard)
        };
        let (a, ha, b, hb) = score(std::slice::from_ref(&along));
        assert!(ha == 1 && hb == 1, "heard {ha} and {hb}");
        assert!(
            (a - b).abs() < 1.0,
            "one flight: {a} at the observer, {b} moved"
        );
        let (a, ha, b, hb) = score(&[along.clone(), crossing]);
        assert!(ha == 2 && hb <= 1, "heard {ha} and {hb}");
        assert!(
            a - b > TIED_NATS,
            "crossing: {a} at the observer, {b} moved"
        );
        let (a, ha, b, hb) = score(&[along, parallel]);
        assert!(ha == 2 && hb == 2, "heard {ha} and {hb}");
        assert!(
            (a - b).abs() < 1.0,
            "parallel: {a} at the observer, {b} moved"
        );
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

    /// `max_judged_window_s` is the edge: that window gets `MIN_DECOYS` decoys, one a second wider
    /// does not.
    #[test]
    fn the_widest_judged_window_is_where_the_decoys_run_out() {
        let p = MatchParams::default();
        let w = max_judged_window_s(&p);
        assert_eq!(w, 300.0);
        let open = (f64::NEG_INFINITY, f64::INFINITY);
        let total = |width: f64| {
            let (lo, hi) = decoy_counts(&p, open, 1000.0, 1000.0 + width, 600.0);
            (lo + hi) as usize
        };
        assert_eq!(total(w), MIN_DECOYS);
        assert!(total(w + 1.0) < MIN_DECOYS, "{}", total(w + 1.0));
        // +-120 s, the tolerance the curve match was built on, gets 48
        assert_eq!(total(240.0), 48);
        // +-180 s, the CLI default before it was lowered, got 34 and could never judge
        assert_eq!(total(360.0), 34);
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
            r50_m: 2000.0,
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
                    r50_m: 2000.0,
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
            aliases: vec![],
        }
    }

    /// An along-track alias counts toward the radius exactly as another mode does, and one that
    /// scores better than the best place counts too.
    #[test]
    fn a_tied_alias_widens_the_radius_like_a_mode() {
        let m = ASSERT_MARGIN_NATS;
        let mut r = fake_result(m + 1.0, 48, None);
        let mut alias = r.modes[0].clone();
        alias.lat += 3.0 / 111.2;
        alias.evidence -= 1.0;
        r.aliases.push(alias.clone());
        let v = verdict(&r).unwrap();
        assert!(v.stands && (v.radius_m - 3000.0).abs() < 50.0, "{v:?}");
        assert!(v.why.contains("(1 of 1 along their tracks)"), "{}", v.why);
        // one tied 6 km off is wider than MAX_ASSERT_RADIUS_M: refused
        alias.lat = r.modes[0].lat + 6.0 / 111.2;
        r.aliases.push(alias.clone());
        assert!(!verdict(&r).unwrap().stands);
        // clearly worse, it does not count; better than the best, it does
        r.aliases.truncate(1);
        alias.evidence = r.modes[0].evidence - TIED_NATS - 0.5;
        r.aliases.push(alias.clone());
        let v = verdict(&r).unwrap();
        assert!(v.stands && v.why.contains("(1 of 2 along"), "{v:?}");
        r.aliases[1].evidence = r.modes[0].evidence + 2.0;
        assert!(!verdict(&r).unwrap().stands);
        // the margin and the best place never move
        assert_eq!(verdict(&r).unwrap().margin, m + 1.0);
        assert_eq!(verdict(&r).unwrap().lat, r.modes[0].lat);
    }

    /// Each condition can refuse on its own, and a place the evidence cannot rule
    /// out widens the radius even when it does not refuse.
    #[test]
    fn a_verdict_needs_margin_decoys_and_one_place() {
        let m = ASSERT_MARGIN_NATS;
        let v = verdict(&fake_result(m + 1.0, 48, None)).unwrap();
        assert!(v.stands, "{v:?}");
        assert_eq!(v.radius_m, RADIUS_FLOOR_M);
        // Beating the decoys with a tight radius cannot turn one flyover into a location.
        for n_heard in 0..=2 {
            let mut r = fake_result(m + 1.0, 48, None);
            r.modes[0].n_heard = n_heard;
            let v = verdict(&r).unwrap();
            assert_eq!(v.stands, n_heard == 2, "{v:?}");
            assert_eq!(
                v.why
                    .contains("at least two independent flyovers are required"),
                n_heard < 2
            );
        }
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

    #[test]
    fn timing_penalties_do_not_count_uncredited_passes_as_heard() {
        let p = MatchParams::default();
        let nats = 7.0;
        let raw = pass_value(&p, 2000.0, nats, 3100.0);
        let z = p.pass_slack_frames as f64 * p.hop_s / p.pass_clock_sigma_s;
        let value = raw - 1.5 * (1.0 + 0.5 * z * z).ln();
        // This shifted lookup beats silence and can win value_at, despite earning no credit.
        assert!(raw > 0.0 && value < 0.0);
        assert!(value > pass_value(&p, 2000.0, 0.0, 3100.0));
        assert!(!counts_as_heard(value, nats, false));
        assert!(!counts_as_heard(0.0, nats, false));
        assert!(counts_as_heard(raw, nats, false));
        assert!(!counts_as_heard(raw, nats, true));
        let passes = [(raw, nats, false), (value, nats, false), (0.0, nats, false)];
        assert_eq!(
            passes
                .into_iter()
                .filter(|&(v, n, shared)| counts_as_heard(v, n, shared))
                .count(),
            1
        );
    }

    /// A bright curve on quiet audio can score thousands of nats; its value must stay finite and
    /// keep growing, where exp() of it would overflow to infinity and tie every such place.
    #[test]
    fn huge_lookups_stay_finite() {
        let p = MatchParams::default();
        for nats in [710.0, 1500.0, 3000.0, 1e6] {
            let (v, pv) = (vote_value(&p, nats), pass_value(&p, 2000.0, nats, 700.0));
            assert!(v.is_finite() && pv.is_finite(), "{nats}: {v} {pv}");
        }
        assert!(
            pass_value(&p, 2000.0, 3000.0, 700.0) > pass_value(&p, 2000.0, 1500.0, 700.0) + 500.0
        );
        // and the small ones are what they were: ln(1 - P + P e)
        for nats in [1.0, 4.0, 30.0] {
            let pd = p_heard(&p, 2000.0, 700.0);
            let e = (p.kappa * ((1.0 - p.kappa) * nats).exp()).max(p.e_floor);
            let old = (1.0 - pd + pd * e).ln();
            assert!(
                (pass_value(&p, 2000.0, nats, 700.0) - old).abs() < 1e-9,
                "{nats}"
            );
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
        assert_eq!(b.modes[0].n_heard, a.modes[0].n_heard);
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

    /// When similar paths overlap, both flights initially prefer the same louder curve. A
    /// second distinct fundamental is still audible and must survive that first pixel claim.
    #[test]
    fn overlapping_flights_recover_distinct_tones_after_pixel_claims() {
        let (lat, lon, clock) = SCENE_AT;
        let enu = Enu::new(lat, lon, 0.0);
        let observer = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let mut tracks = vec![
            scene_track([900.0, 0.0, 700.0], [0.0, 70.0, 0.0], 450.0, -120.0, 1100.0),
            scene_track(
                [-900.0, 0.0, 700.0],
                [0.0, 70.0, 0.0],
                450.0,
                -120.0,
                1100.0,
            ),
            scene_track(
                [0.0, -1500.0, 900.0],
                [65.0, 0.0, 0.0],
                150.0,
                -120.0,
                1100.0,
            ),
        ];
        for (tr, key) in tracks.iter_mut().zip(["east", "west", "crossing"]) {
            tr.key = key.into();
            tr.icao = key.into();
        }
        let dem = FlatElevation(0.0);
        let atmo = UniformAtmosphere(Medium::still(BASE_C));
        let prop = Prop::still(BASE_C);
        let p = params();
        let score = |second_tone: Option<f64>| {
            let mut pcm = noise(2800 * 900, 42, 0.05);
            add_flight(
                &mut pcm, 2800.0, clock, &tracks[0], observer, &prop, 90.0, 0.5,
            );
            add_flight(
                &mut pcm, 2800.0, clock, &tracks[2], observer, &prop, 150.0, 0.5,
            );
            if let Some(f0) = second_tone {
                add_flight(
                    &mut pcm, 2800.0, clock, &tracks[1], observer, &prop, f0, 0.35,
                );
            }
            let pcm: Vec<f32> = pcm.into_iter().map(|v| v as f32).collect();
            let im = build_image(&pcm, 2800, &p).unwrap();
            let bank = build_bank(&im, &p);
            let idx: Vec<_> = tracks
                .iter()
                .enumerate()
                .map(|(i, tr)| ExactTrack::new(i, tr))
                .collect();
            let inp = MatchInputs {
                image: &im,
                tracks: &tracks,
                region: BBox::new(lat - 0.01, lon - 0.01, lat + 0.01, lon + 0.01),
                start_lo: clock,
                start_hi: clock,
                duration_s: 900.0,
                catalog: (f64::NEG_INFINITY, f64::INFINITY),
                dem: &dem,
                fine_dem: &dem,
                atmo: &atmo,
            };
            let exact = score_exact(&inp, &p, &bank, &idx, lat, lon, clock).unwrap();
            let seed = score_exact(&inp, &p, &bank, &idx, lat, lon, clock).unwrap();
            let (_, _, polished) = polish_point(&inp, &p, &bank, &idx, lat, lon, seed);
            assert_eq!(polished.n_heard, exact.n_heard, "{:?}", polished.passes);
            for before in exact.passes.iter().filter(|pass| {
                counts_as_heard(pass.value, pass.nats, pass.shared)
            }) {
                let after = polished.passes.iter().find(|pass| {
                    pass.key == before.key && counts_as_heard(pass.value, pass.nats, pass.shared)
                }).expect("physical fitting preserves each independent source");
                assert!(
                    (after.f0_hz - before.f0_hz).abs() < 3.0,
                    "physical fitting changed {} from {} to {} Hz",
                    before.key, before.f0_hz, after.f0_hz
                );
            }
            exact
        };
        // 114.5/90 is separated from every ratio of harmonics through order six. The
        // paths have the same Doppler shape here, but their independent tones are real.
        let distinct = score(Some(114.5));
        assert_eq!(distinct.n_heard, 3, "{:?}", distinct.passes);
        let mut tones: Vec<_> = distinct
            .passes
            .iter()
            .filter(|pass| {
                pass.key != "crossing" && counts_as_heard(pass.value, pass.nats, pass.shared)
            })
            .map(|pass| pass.f0_hz)
            .collect();
        tones.sort_by(f64::total_cmp);
        assert!(
            (tones[0] - 90.0).abs() < 3.0 && (tones[1] - 114.5).abs() < 3.0,
            "{tones:?}"
        );
        // A second catalogue trajectory, or a second source on the same tone, does not
        // supply independent acoustic evidence. Harmonics of the first stay claimed too.
        for second in [None, Some(90.0)] {
            let single = score(second);
            assert_eq!(single.n_heard, 2, "{second:?}: {:?}", single.passes);
            assert!(single.passes.iter().any(|pass| pass.shared));
        }
    }

    /// A rejected duplicate can also fit a weaker true flight. Recovering it immediately would
    /// steal that flight's curve before the original strongest-tone assignment reaches it.
    #[test]
    fn recovered_tones_cannot_displace_original_independent_matches() {
        let (lat, lon, clock) = SCENE_AT;
        let enu = Enu::new(lat, lon, 0.0);
        let observer = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let mut tracks = vec![
            scene_track([900.0, 0.0, 700.0], [0.0, 70.0, 0.0], 450.0, -120.0, 1100.0),
            scene_track(
                [-900.0, 0.0, 700.0],
                [0.0, 70.0, 0.0],
                450.0,
                -120.0,
                1100.0,
            ),
            scene_track(
                [-1100.0, 0.0, 800.0],
                [0.0, -65.0, 0.0],
                450.0,
                -120.0,
                1100.0,
            ),
        ];
        for (tr, key) in tracks.iter_mut().zip(["bright", "duplicate", "owner"]) {
            tr.key = key.into();
            tr.icao = key.into();
        }
        let prop = Prop::still(BASE_C);
        let mut pcm = noise(2800 * 900, 42, 0.05);
        for (i, f0) in [(0, 90.0), (2, 128.0)] {
            add_flight(
                &mut pcm, 2800.0, clock, &tracks[i], observer, &prop, f0, 0.5,
            );
        }
        let pcm: Vec<f32> = pcm.into_iter().map(|v| v as f32).collect();
        let p = MatchParams {
            pass_slack_frames: 0,
            r50_m: vec![2000.0],
            tone_prior: false,
            ..params()
        };
        let im = build_image(&pcm, 2800, &p).unwrap();
        let bank = build_bank(&im, &p);
        let dem = FlatElevation(0.0);
        let atmo = UniformAtmosphere(Medium::still(BASE_C));
        let inp = MatchInputs {
            image: &im,
            tracks: &tracks,
            region: BBox::new(lat - 0.01, lon - 0.01, lat + 0.01, lon + 0.01),
            start_lo: clock,
            start_hi: clock,
            duration_s: 900.0,
            catalog: (f64::NEG_INFINITY, f64::INFINITY),
            dem: &dem,
            fine_dem: &dem,
            atmo: &atmo,
        };
        let idx: Vec<_> = tracks
            .iter()
            .enumerate()
            .map(|(i, tr)| ExactTrack::new(i, tr))
            .collect();
        let old = score_exact_with_recovery(&inp, &p, &bank, &idx, lat, lon, clock, false).unwrap();
        assert_eq!(old.n_heard, 2, "{:?}", old.passes);
        assert!(old.passes.iter().any(|pass| {
            pass.key == "owner" && counts_as_heard(pass.value, pass.nats, pass.shared)
        }));
        // Without the true owner, the rejected duplicate really can recover that tone. This
        // establishes the competing claim rather than merely testing an empty recovery queue.
        let competing = score_exact(&inp, &p, &bank, &idx[..2], lat, lon, clock).unwrap();
        assert_eq!(competing.n_heard, 2, "{:?}", competing.passes);
        assert!(
            competing
                .passes
                .iter()
                .any(|pass| { pass.key == "duplicate" && (pass.f0_hz - 128.0).abs() < 4.0 })
        );
        let actual = score_exact(&inp, &p, &bank, &idx, lat, lon, clock).unwrap();
        assert_eq!(
            actual.n_heard, old.n_heard,
            "original: {:?}; recovered: {:?}",
            old.passes, actual.passes
        );
        assert_eq!(actual.evidence.to_bits(), old.evidence.to_bits());
        assert_eq!(actual.clock.to_bits(), old.clock.to_bits());
        assert_eq!(
            serde_json::to_vec(&actual.passes).unwrap(),
            serde_json::to_vec(&old.passes).unwrap()
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

    /// At each heard range: silence close in costs, silence far out costs nothing. A range only
    /// scales distance: the longest default range charges a silent pass twice as far out what 2 km
    /// charges, so a place cannot take it to credit far curves without paying for far silences.
    #[test]
    fn close_silence_costs_and_far_silence_does_not() {
        let p = MatchParams::default();
        assert!(pass_value(&p, 2000.0, 0.0, 400.0) < -1.0);
        assert!(pass_value(&p, 2000.0, 0.0, 7000.0).abs() < 0.1);
        assert!(pass_value(&p, 2000.0, 30.0, 700.0) > 10.0);
        let longest = p.r50_m.iter().copied().fold(0.0, f64::max);
        let k = longest / 2000.0;
        for d in [400.0, 1500.0, 3000.0] {
            let (a, b) = (
                pass_value(&p, longest, 0.0, k * d),
                pass_value(&p, 2000.0, 0.0, d),
            );
            assert!((a - b).abs() < 1e-9, "{d}: {a} {b}");
        }
        let far = pass_value(&p, longest, 0.0, 3000.0);
        assert!(far < -0.5, "{far}");
        // a 10-nat curve 4 km out is worth more at the longest range than at 2 km
        let (long, short) = (
            pass_value(&p, longest, 10.0, 4000.0),
            pass_value(&p, 2000.0, 10.0, 4000.0),
        );
        assert!(long > short + 1.0, "{long} {short}");
    }

    /// No default heard range expects to hear much beyond the reach: at the reach, the longest
    /// range's chance of hearing is at most one in eight.
    #[test]
    fn the_heard_ranges_stay_inside_the_reach() {
        let p = MatchParams::default();
        let longest = p.r50_m.iter().copied().fold(0.0, f64::max);
        let at_reach = p_heard(&p, longest, p.reach_m);
        assert!(at_reach <= 0.13, "{at_reach}");
        assert!(p.r50_m.windows(2).all(|w| (w[1] / w[0] - 2.0).abs() < 1e-9));
    }

    /// Deliberately simple per-tone oracle: clip each predicted pixel independently.
    #[allow(clippy::too_many_arguments)]
    fn direct_visible_tone_sweep(
        im: &CombImage,
        pth: &[Sample],
        tau: i64,
        r0: usize,
        nf0: usize,
        stat: Stat,
        full: (f32, f32),
        tone: Option<(&[f32], f32)>,
    ) -> Option<(f32, usize)> {
        let (mut best, mut at, mut any) = (f32::NEG_INFINITY, 0, false);
        for row in 0..nf0 {
            let (mut a, mut b, mut wa, mut wb) = (0f32, 0f32, 0f32, 0f32);
            for s in pth {
                let frame = tau + i64::from(s.dt);
                let received = r0 as i64 + row as i64 + i64::from(s.dr);
                if !(0..im.nt as i64).contains(&frame)
                    || !(0..im.nu as i64).contains(&received)
                {
                    continue;
                }
                let value = im.comb[frame as usize * im.nu + received as usize];
                if s.approach {
                    a += s.w * value;
                    wa += s.w * s.w;
                } else {
                    b += s.w * value;
                    wb += s.w * s.w;
                }
            }
            if wa < 0.25 * full.0 || wb < 0.25 * full.1 {
                continue;
            }
            any = true;
            let mut value = stat_of(stat, a * (1.0 / wa.sqrt()), b * (1.0 / wb.sqrt()));
            if let Some((q, beta)) = tone {
                value += beta * q[row];
            }
            if value > best {
                best = value;
                at = row;
            }
        }
        any.then_some((best, at))
    }

    #[allow(clippy::too_many_arguments)]
    fn actual_visible_tone_sweep(
        im: &CombImage,
        pth: &[Sample],
        tau: i64,
        r0: usize,
        nf0: usize,
        stat: Stat,
        full: (f32, f32),
        tone: Option<(&[f32], f32)>,
    ) -> Option<(f32, usize)> {
        let mut scratch = SweepBuffers::new(nf0);
        sweep_tau(im, pth, tau, r0, nf0, stat, &mut scratch, full, tone)
    }

    #[test]
    fn visible_tone_survives_other_tones_crossing_frequency_edges() {
        let mut im = CombImage {
            t0: 0.0,
            dt: 1.0,
            f_lo: 25.0,
            bpo: 48.0,
            nt: 3,
            nu: 12,
            comb: vec![0.0; 36],
            whitened: None,
        };
        let pth = [
            Sample { dt: -1, dr: 5, w: 1.0, approach: true },
            Sample { dt: 1, dr: -4, w: 1.0, approach: false },
        ];
        // Emitted row 2 has visible pixels at received rows 10 and 1.
        // Rows 0 and 4 have louder pixels, but lack the opposite half entirely.
        im.comb[10] = 4.0;
        im.comb[2 * im.nu + 1] = 6.0;
        im.comb[8] = 100.0;
        im.comb[2 * im.nu + 3] = 100.0;
        for stat in [Stat::Min, Stat::Sum] {
            let got = actual_visible_tone_sweep(&im, &pth, 1, 3, 5, stat, (1.0, 1.0), None)
                .expect("one emitted tone has complete approach and recede coverage");
            assert_eq!(got.1, 2);
            assert!((got.0 - stat_of(stat, 4.0, 6.0)).abs() < 1e-6);
        }
        // One visible half cannot borrow coverage from another emitted tone.
        im.comb[10] = 0.0;
        im.comb[2 * im.nu + 1] = 0.0;
        let got = actual_visible_tone_sweep(
            &im, &pth, 1, 3, 5, Stat::Min, (1.0, 1.0), None,
        ).unwrap();
        assert_eq!(got.0, 0.0);
    }

    #[test]
    fn clipped_sweeps_match_independent_per_tone_coverage_and_scores() {
        let im = CombImage {
            t0: 0.0,
            dt: 1.0,
            f_lo: 25.0,
            bpo: 48.0,
            nt: 13,
            nu: 19,
            comb: (0..13 * 19).map(|i| ((i * 13 % 37) as f32 - 18.0) * 0.25).collect(),
            whitened: None,
        };
        let pth = [
            Sample { dt: -4, dr: 8, w: 0.25, approach: true },
            Sample { dt: -2, dr: 3, w: 1.0, approach: true },
            Sample { dt: -1, dr: 1, w: 0.5, approach: true },
            Sample { dt: 0, dr: 0, w: 1.0, approach: false },
            Sample { dt: 1, dr: -2, w: 0.5, approach: false },
            Sample { dt: 4, dr: -9, w: 0.25, approach: false },
        ];
        let full = (1.3125, 1.3125);
        for nf0 in [1, 5, 9] {
            let q: Vec<_> = (0..nf0).map(|r| (r as f32 - 3.0) * 0.125).collect();
            for r0 in [0, 3, 9, 17] {
                for tau in 0..im.nt as i64 {
                    for stat in [Stat::Min, Stat::Sum] {
                        for tone in [None, Some((q.as_slice(), 0.5))] {
                            let expected = direct_visible_tone_sweep(
                                &im, &pth, tau, r0, nf0, stat, full, tone,
                            );
                            let got = actual_visible_tone_sweep(
                                &im, &pth, tau, r0, nf0, stat, full, tone,
                            );
                            match (got, expected) {
                                (None, None) => {}
                                (Some((got, row)), Some((want, want_row))) => {
                                    assert_eq!(row, want_row, "r0={r0} nf0={nf0} tau={tau}");
                                    assert!((got - want).abs() < 2e-6, "{got} != {want}");
                                }
                                pair => panic!("coverage differs: {pair:?}"),
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn production_curve_bank_stamps_fast_passes_and_keeps_its_speed_limit() {
        let (_, _, clock) = SCENE_AT;
        let p = MatchParams::default();
        let g = observer_cell(&p);
        let old_limit = geometric(28.0, 1.2, 11).last().unwrap() * 1.15;
        for speed in [215.0, 249.0, 263.0] {
            assert!(speed > old_limit);
            let tr = scene_track(
                [0.0, 900.0, 600.0 + MIC_HEIGHT_M],
                [speed, 0.0, 0.0],
                120.0,
                -200.0,
                500.0,
            );
            let mut stamped = Vec::new();
            stamp_track(
                &tr, &g, &p, &|_| BASE_C, clock,
                (clock - 200.0, clock + 500.0), &mut stamped,
            );
            assert_eq!(stamped.len(), 1, "lost {speed} m/s pass");
            let arrival = 120.0 + 900.0f64.hypot(600.0) / BASE_C;
            assert!((stamped[0].1.arrive as f64 - arrival).abs() < 0.01);
        }
        let speed = p.v_grid.last().unwrap() * 1.15 + 10.0;
        let tr = scene_track(
            [0.0, 900.0, 600.0 + MIC_HEIGHT_M],
            [speed, 0.0, 0.0],
            120.0,
            -200.0,
            500.0,
        );
        let mut stamped = Vec::new();
        stamp_track(
            &tr, &g, &p, &|_| BASE_C, clock,
            (clock - 200.0, clock + 500.0), &mut stamped,
        );
        assert!(stamped.is_empty(), "the configured speed allowance still applies");
    }

    /// New fast fixtures alone suppress harmonics beyond the sample rate's usable band.
    /// Their phase still integrates the real retarded-time ratio, independently of the bank.
    fn scene_with_visible_harmonics(
        tracks: Vec<TrackModel>,
        tones: &[f64],
        half_m: f64,
        duration_s: f64,
    ) -> Case {
        assert_eq!(tracks.len(), tones.len());
        let mut c = scene_for(Vec::new(), half_m, duration_s);
        let enu = Enu::new(c.lat, c.lon, 0.0);
        let observer = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let medium = Prop::still(BASE_C);
        let sr = 2800.0;
        let mut pcm: Vec<f64> = c.x.iter().map(|&v| f64::from(v)).collect();
        for (tr, &f0) in tracks.iter().zip(tones) {
            let (mut phase, mut hint, mut warm) = (0.0, 0usize, None);
            let (mut range, mut ratio) = (f64::INFINITY, 0.0);
            for (i, value) in pcm.iter_mut().enumerate() {
                if i % 400 == 0 {
                    match tr.solve_emission_sweep(
                        c.clock + i as f64 / sr, observer, &medium, warm, &mut hint, 1e-5,
                    ) {
                        Some((te, r, q)) => {
                            warm = Some(te);
                            range = r;
                            ratio = q;
                        }
                        None => {
                            warm = None;
                            ratio = 0.0;
                        }
                    }
                }
                if ratio <= 0.0 {
                    continue;
                }
                phase += std::f64::consts::TAU * f0 * ratio / sr;
                let amplitude = 0.5 * 1000.0 / range;
                for harmonic in 1..=4 {
                    let h = f64::from(harmonic);
                    if h * f0 * ratio <= 0.45 * sr {
                        *value += amplitude / h * (h * phase).sin();
                    }
                }
            }
        }
        c.x = pcm.into_iter().map(|v| v as f32).collect();
        c.tracks = tracks;
        c
    }

    #[test]
    fn fast_pcm_retains_visible_received_tones_above_the_old_image_crop() {
        let mut tr = scene_track(
            [0.0, 600.0, 700.0 + MIC_HEIGHT_M],
            [263.0, 0.0, 0.0], 120.0, -200.0, 900.0,
        );
        tr.key = "fast-high-tone".into();
        tr.icao = "000003".into();
        let c = scene_with_visible_harmonics(vec![tr], &[300.0], 200.0, 300.0);
        let p = MatchParams { hop_s: 1.0, ..Default::default() };
        let im = build_image(&c.x, 2800, &p).unwrap();
        let observer = geodetic_to_ecef(c.lat, c.lon, MIC_HEIGHT_M);
        let medium = Prop::still(BASE_C);
        let tr = &c.tracks[0];
        let te = c.clock + 104.0;
        let rv = sub(tr.pos_at(te), observer);
        let received = te + medium.travel(rv, norm(rv));
        let frame = ((received - c.clock - im.t0) / im.dt).round() as usize;
        assert!(frame < im.nt);
        let (_, _, ratio) = tr.solve_emission_p(
            c.clock + im.t0 + frame as f64 * im.dt, observer, &medium, None,
        ).unwrap();
        let frequency = 300.0 * ratio;
        let row = im.row_of(frequency).round() as usize;
        let old_rows = ((p.f0_hi * 2.2 / p.band_lo).log2() * p.bins_per_octave)
            .floor() as usize + 1;
        assert!(row >= old_rows, "{frequency} Hz must exercise the old crop");
        assert!(frequency < p.band_hi.min(0.45 * 2800.0));
        assert!(row < im.nu, "visible received fundamental at {frequency} Hz was cropped");
        let energy = (row.saturating_sub(1)..=(row + 1).min(im.nu - 1))
            .map(|r| im.comb[frame * im.nu + r])
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(energy > 2.0, "the predicted received tone has no visible energy: {energy}");
    }

    #[test]
    fn fast_crossing_pcm_reaches_the_curve_match_and_controls_are_refused() {
        let mut a = scene_track(
            [0.0, 600.0, 700.0 + MIC_HEIGHT_M],
            [215.0, 0.0, 0.0], 120.0, -200.0, 900.0,
        );
        a.key = "fast-east".into();
        a.icao = "000001".into();
        let mut b = scene_track(
            [700.0, 0.0, 600.0 + MIC_HEIGHT_M],
            [0.0, 263.0, 0.0], 360.0, -200.0, 900.0,
        );
        b.key = "fast-north".into();
        b.icao = "000002".into();
        let c = with_sky(
            scene_with_visible_harmonics(vec![a, b], &[90.0, 115.0], 200.0, 600.0),
            3,
        );
        // Preserve the production speed bank and all evidence/coverage/decoy gates.
        // A smaller distance bank, one-second image hops and 52 complete decoy windows
        // bound this PCM regression while exceeding the unchanged 40-decoy requirement.
        let p = MatchParams {
            hop_s: 1.0,
            d_grid: vec![500.0, 1000.0, 2000.0],
            decoy_span_s: 1680.0,
            top_modes: 4,
            refine_modes: 1,
            ..Default::default()
        };
        let r = run_case(&c, &p, c.clock - 2.0, c.clock + 2.0);
        let top = r.modes.first().expect("both fast passes reach the spatial search");
        assert!(haversine_m(top.lat, top.lon, c.lat, c.lon) < 200.0);
        assert!((top.clock_unix - c.clock).abs() < 2.0);
        assert!(top.n_heard >= 2);
        for key in ["fast-east", "fast-north"] {
            assert!(top.passes.iter().any(|pass| {
                pass.key == key && counts_as_heard(pass.value, pass.nats, pass.shared)
            }), "missing independently heard {key}");
        }
        assert!(r.decoys.len() >= MIN_DECOYS);
        assert!(verdict(&r).is_some_and(|v| v.stands));

        for reversed in [false, true] {
            let mut control = Case {
                x: if reversed {
                    c.x.iter().rev().copied().collect()
                } else {
                    noise(c.x.len(), 77, 0.05).iter().map(|&v| v as f32).collect()
                },
                tracks: c.tracks.clone(),
                region: c.region,
                lat: c.lat,
                lon: c.lon,
                clock: c.clock,
            };
            if !reversed {
                for (i, value) in control.x.iter_mut().enumerate() {
                    let t = i as f64 / 2800.0;
                    *value += (0.2 * (std::f64::consts::TAU * 90.0 * t).sin()
                        + 0.1 * (std::f64::consts::TAU * 180.0 * t).sin()) as f32;
                }
            }
            let result = run_case(&control, &p, control.clock - 2.0, control.clock + 2.0);
            assert_refused(&result);
        }
    }
}
