//! Flight-centric geolocation search over ADS-B tracks.
//!
//! Every measured event constrains a still-air fitted distance in `d_gate`, speed in
//! `v_gate`, and curve centre at recording time `t_rec`. The propagation medium maps each
//! physical aircraft CPA into these fitted coordinates; wind changes their scales separately
//! and can shift the centre from the geometric CPA's heard time.
//! For an aircraft at position p with velocity v, the ground points
//! whose CPA is *now* lie on the line perpendicular to the horizontal velocity, displaced
//! along-track by a = H*vU/vH (H = height above the observer), at cross-track offsets b with
//! a^2 + b^2 + H^2 = d^2. Sampling every track every <= 2 s / 400 m and every such line every
//! 400 m gives, per event, a cloud of hypotheses (position, implied recording start
//! t_s from the medium-adjusted fitted centre). Two events at the same place must agree on BOTH
//! position and t_s, so a spatiotemporal hash join of two events' clouds yields the candidate
//! (flight, flight, place, clock) combinations.
//!
//! Stages: hypotheses -> pair join -> quick trace check at the joined point -> fine 100 m grid
//! with exact CPAs, gates and joint clock-locked trace scoring -> confirmers (other events
//! explained at the same place and clock) and unheard close passes -> ranking -> 25 m
//! refinement with a chi-square confidence region.

use crate::measure::MeasuredEvent;
use crate::track::{Prop, TrackBounds, TrackModel};
use fdop_types::atmo::Atmosphere;
use fdop_types::geo::{Enu, V3, dot, ecef_to_geodetic, geodetic_to_ecef, haversine_m, norm, sub};
use fdop_types::{BBox, Elevation, MIC_HEIGHT_M};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

/// One measured event in the form the search needs.
#[derive(Clone, Debug, Serialize)]
pub struct SearchEvent {
    pub id: String,
    /// Recording-clock centre of the fitted still-air curve, s (not geometric CPA in wind).
    pub t_rec: f64,
    /// Half-width of the plausible t_rec interval, s.
    pub t_tol: f64,
    /// Speed interval of the equivalent still-air curve at `c_fit`, m/s.
    pub v_gate: [f64; 2],
    /// Closest-distance interval of the equivalent still-air curve at `c_fit`, m.
    pub d_gate: [f64; 2],
    /// Tone (Hz) fitted in still air; wind converts this to a candidate's emitted tone.
    pub f0: f64,
    pub f0_tol: f64,
    /// Trace noise scale used to normalise residuals (Hz).
    pub sigma_hz: f64,
    /// Effective number of independent trace points (from the fit).
    pub n_eff: f64,
    pub quality: f64,
    /// Model-misfit flag from the fit (wider gates, larger t_tol).
    pub misfit: bool,
    /// Sound speed the still-air fit assumed. Wind maps its speed and distance separately.
    pub c_fit: f64,
    #[serde(skip)]
    pub tr_t: Vec<f64>,
    #[serde(skip)]
    pub tr_f: Vec<f64>,
    /// Coarse subset of the trace for the quick check.
    #[serde(skip)]
    pub q_t: Vec<f64>,
    #[serde(skip)]
    pub q_f: Vec<f64>,
}

fn subsample(t: &[f64], f: &[f64], n: usize) -> (Vec<f64>, Vec<f64>) {
    if t.len() <= n {
        return (t.to_vec(), f.to_vec());
    }
    let (t0, t1) = (t[0], t[t.len() - 1]);
    let mut ot = Vec::with_capacity(n);
    let mut of = Vec::with_capacity(n);
    let mut k = 0usize;
    for i in 0..n {
        let target = t0 + (t1 - t0) * i as f64 / (n - 1) as f64;
        while k + 1 < t.len() && (t[k + 1] - target).abs() <= (t[k] - target).abs() {
            k += 1;
        }
        if ot.last().is_none_or(|&l: &f64| t[k] > l) {
            ot.push(t[k]);
            of.push(f[k]);
        }
    }
    (ot, of)
}

impl SearchEvent {
    /// Build from a quality-passing measured event. `wind_mps` retains the extra speed
    /// allowance in fitted coordinates; each candidate's propagation medium maps its motion.
    pub fn from_measured(m: &MeasuredEvent, wind_mps: f64) -> Option<Self> {
        if !m.quality.ok {
            return None;
        }
        Self::from_observed_trace(m, wind_mps)
    }

    // A refused fit's ridge can still establish that a detected sibling is the same
    // sound. This conversion alone never makes that fit eligible for the search.
    fn from_observed_trace(m: &MeasuredEvent, wind_mps: f64) -> Option<Self> {
        let fr = m.fit.as_ref()?;
        let b = fr.best;
        let g = &fr.ranges_generous;
        let t_tol = (b.t0r - g.t0r[0]).max(g.t0r[1] - b.t0r).max(1.0);
        let misfit = fr.warnings.iter().any(|w| {
            w.starts_with("model misfit") || w.starts_with("approach/recede sides disagree")
        });
        let mut tt = Vec::new();
        let mut ff = Vec::new();
        let mut idx: Vec<usize> = (0..m.trace.t.len()).collect();
        idx.sort_by(|&a, &c| m.trace.t[a].total_cmp(&m.trace.t[c]));
        for i in idx {
            tt.push(m.trace.t[i]);
            ff.push(m.trace.f[i]);
        }
        let (tr_t, tr_f) = subsample(&tt, &ff, 360);
        let (q_t, q_f) = subsample(&tt, &ff, 40);
        // Resampling must not create independent observations or restore information
        // removed by gaps in the measured trace.
        let n_eff = fr.diagnostics.n_eff.max(1.0).min(tr_t.len() as f64);
        Some(Self {
            id: m.id.clone(),
            t_rec: b.t0r,
            t_tol,
            v_gate: [(g.v[0] - wind_mps).max(5.0), g.v[1] + wind_mps],
            d_gate: [g.d[0].max(30.0), g.d[1]],
            f0: b.f0,
            f0_tol: (0.04 * b.f0).max(4.0),
            // trace noise plus a model-error floor (ADS-B position/velocity error, residual
            // atmosphere error): 0.2 % of f0
            sigma_hz: (b.rmse * b.rmse + (0.002 * b.f0).powi(2)).sqrt().max(0.12),
            n_eff,
            quality: m.quality.score,
            misfit,
            c_fit: fr.diagnostics.c,
            tr_t,
            tr_f,
            q_t,
            q_f,
        })
    }
}

#[derive(Clone, Debug)]
pub struct SearchParams {
    pub c: f64,
    /// Window (UTC unix s) that must contain the recording start.
    pub start_lo: f64,
    pub start_hi: f64,
    /// Recording duration (s).
    pub duration: f64,
    /// Observer must lie inside this box.
    pub region: BBox,
    pub along_dt_max: f64,
    pub along_step_m: f64,
    pub cross_step_m: f64,
    pub min_agl_m: f64,
    pub max_gap_s: f64,
    pub fine_step_m: f64,
    pub fine_half_m: f64,
    /// Lenient normalised-chi threshold for the quick check at a joined point.
    pub quick_chi: f64,
    /// Normalised chi (mean squared residual / sigma^2) for an event to count as explained.
    pub kappa: f64,
    /// Residual cap in units of sigma (bounded influence of stray trace points).
    pub cap_sigma: f64,
    /// Joined points that may reach the quick check, and cells the fine grid may score. A cut
    /// leaves rivals unchecked and fails the margin gate (`SearchResult::margin_passes`), and it
    /// can hide the best place: a flight pair's first cell can lie beyond where its third flight
    /// confirms. So these sit above the largest searches estimated from real runs, about 11 M
    /// points and 1 M cells.
    pub max_quick: usize,
    pub max_fine: usize,
    pub refine_step_m: f64,
    pub refine_half_m: f64,
    /// Use only tonal (non-jet) aircraft; unknown types are kept.
    pub tonal_only: bool,
    /// At most this many events drive the pair join (the rest can still confirm).
    pub max_events: usize,
    /// Hypothesis budgets (20 bytes each): an event over `max_hyp_per_event` is confirmer-only.
    /// [`hyp_budget`] sizes them to the machine.
    pub max_hyp_per_event: usize,
    pub max_hyp_total: usize,
    /// Candidates that get the full confirmer search (every event x every flight).
    pub max_confirm: usize,
}

const DEFAULT_CAP_SIGMA: f64 = 5.0;

impl SearchParams {
    pub fn new(start_lo: f64, start_hi: f64, duration: f64, region: BBox) -> Self {
        Self {
            c: fdop_types::C_SOUND,
            start_lo,
            start_hi,
            duration,
            region,
            along_dt_max: 2.0,
            along_step_m: 300.0,
            cross_step_m: 350.0,
            min_agl_m: 40.0,
            max_gap_s: 90.0,
            fine_step_m: 100.0,
            fine_half_m: 500.0,
            quick_chi: 60.0,
            kappa: 12.0,
            cap_sigma: DEFAULT_CAP_SIGMA,
            max_quick: 20_000_000,
            max_fine: 2_000_000,
            refine_step_m: 25.0,
            refine_half_m: 700.0,
            tonal_only: true,
            max_events: 12,
            max_hyp_per_event: 30_000_000,
            max_hyp_total: 200_000_000,
            max_confirm: 2_000_000,
        }
    }
}

/// Most hypotheses a run stores: a tenth of a 32 GiB machine's RAM, at 20 bytes each. More does
/// not search better. The pair join and the fine stage have fixed budgets (`max_quick`,
/// `max_fine`), so more hypotheses overflow them, and a comparison cut fails the margin gate. The
/// three-day reference search sends 7.5 M cells to a 2 M fine stage on 256 GiB, and scores 0.1 M
/// on 32 GiB, where it grades high. So every machine from 32 GiB up runs the same search.
pub const MAX_HYP_TOTAL: u64 = (32 << 30) / 10 / 20;

/// Hypothesis budgets (per event, total) from physical memory: a tenth of RAM for stored
/// clouds (20 bytes each), at most [`MAX_HYP_TOTAL`], which unknown memory also uses.
pub fn hyp_budget(mem_bytes: Option<u64>) -> (usize, usize) {
    let total =
        mem_bytes.map_or(MAX_HYP_TOTAL, |m| ((m / 10) / 20).clamp(1, MAX_HYP_TOTAL)) as usize;
    ((total / 3).max(1), total)
}

/// Physical memory in bytes (Linux: MemTotal or a smaller cgroup limit; macOS: hw.memsize).
pub fn physical_memory() -> Option<u64> {
    if let Ok(s) = std::fs::read_to_string("/proc/meminfo") {
        let kb: Option<u64> = s
            .lines()
            .find(|l| l.starts_with("MemTotal:"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok());
        let cg: Option<u64> = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
            .ok()
            .and_then(|v| v.trim().parse().ok());
        return match (kb.map(|k| k * 1024), cg) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
    let o = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout).trim().parse().ok()
}

/// One hypothesis, 20 bytes: times are f32 seconds after the search window start (8 ms
/// resolution over a day), positions f32 metres in the region frame.
#[derive(Clone, Copy, Debug)]
struct Hyp {
    trk: u32,
    t_e: f32,
    e: f32,
    n: f32,
    t_s: f32,
}

/// Region frame for hashing: north = (lat - lat_c) * m_lat(lat_c), east = (lon - lon_c) *
/// m_lon(lat) at the point's own latitude, so local distances are true to <0.1 % anywhere in a
/// CONUS-sized box (only distances between nearby points are ever compared).
struct Frame {
    lat_c: f64,
    lon_c: f64,
    m_lat: f64,
}

impl Frame {
    fn new(b: &BBox) -> Self {
        let (la, lo) = b.center();
        Self {
            lat_c: la,
            lon_c: lo,
            m_lat: fdop_types::geo::m_per_deg(la).0,
        }
    }
    #[inline]
    fn en(&self, lat: f64, lon: f64) -> (f64, f64) {
        (
            (lon - self.lon_c) * fdop_types::geo::m_per_deg(lat).1,
            (lat - self.lat_c) * self.m_lat,
        )
    }
    #[inline]
    fn latlon(&self, e: f64, n: f64) -> (f64, f64) {
        let lat = self.lat_c + n / self.m_lat;
        (lat, self.lon_c + e / fdop_types::geo::m_per_deg(lat).1)
    }
}

/// Observer on the terrain (+ mic height).
struct Obs {
    x: V3,
    lat: f64,
    lon: f64,
    /// Ground elevation (MSL, m).
    z: f64,
    enu: Enu,
}

fn observer(lat: f64, lon: f64, dem: &dyn Elevation) -> Obs {
    let z = dem.elev_m(lat, lon);
    Obs {
        x: geodetic_to_ecef(lat, lon, z + MIC_HEIGHT_M),
        lat,
        lon,
        z,
        enu: Enu::new(lat, lon, z + MIC_HEIGHT_M),
    }
}

/// Propagation medium between observer `o` and track `tr` around emission time `t_e`.
fn prop_for(atmo: &dyn Atmosphere, o: &Obs, tr: &TrackModel, t_e: f64) -> Prop {
    let (_, _, h) = ecef_to_geodetic(tr.pos_at(t_e));
    Prop::from_medium(
        &atmo.medium(o.lat, o.lon, t_e, o.z + MIC_HEIGHT_M, h),
        &o.enu,
    )
}

fn matching_still_air(ev: &SearchEvent, p: &SearchParams, atmo: &dyn Atmosphere) -> bool {
    p.c.is_finite()
        && p.c > 0.0
        && ev.c_fit == p.c
        && atmo.uniform_medium() == Some(fdop_types::atmo::Medium::still(p.c))
}

/// An event's hypothesis cloud: stored when within the budget, always counted exactly, and a
/// deterministic ~1/1024 sample for the chance-match calibration.
struct HypSet {
    stored: Option<Vec<Hyp>>,
    count: usize,
    sample: Vec<Hyp>,
}

/// Account for generated hypotheses in small batches, before a whole track is materialised.
/// Once the event exceeds its cap it is confirmer-only, but its exact count and deterministic
/// chance sample are still needed. At most one unaccounted batch per worker is retained.
struct HypBuilder {
    stored: Vec<Hyp>,
    sample: Vec<Hyp>,
    count: usize,
    retaining: bool,
}

impl HypBuilder {
    const BATCH: usize = 1024;

    fn new(cap: usize) -> Self {
        Self {
            stored: Vec::new(),
            sample: Vec::new(),
            count: 0,
            retaining: cap > 0,
        }
    }

    fn account(&mut self, n: usize, cap: usize, total: &AtomicUsize) {
        let before = total.fetch_add(n, Ordering::Relaxed);
        if before.saturating_add(n) > cap {
            self.stored = Vec::new();
            self.retaining = false;
        }
    }

    fn push(&mut self, h: Hyp, track: usize, cap: usize, total: &AtomicUsize) {
        if mix(track as u64, self.count as u64).is_multiple_of(1024) {
            self.sample.push(h);
        }
        self.count += 1;
        if self.retaining {
            self.stored.push(h);
        }
        if self.count.is_multiple_of(Self::BATCH) {
            self.account(Self::BATCH, cap, total);
        }
    }

    fn finish(mut self, cap: usize, total: &AtomicUsize) -> (Vec<Hyp>, usize, Vec<Hyp>) {
        self.account(self.count % Self::BATCH, cap, total);
        (self.stored, self.count, self.sample)
    }
}

#[inline]
fn mix(a: u64, b: u64) -> u64 {
    let mut x = a.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ b.wrapping_add(0x632b_e59b_d9b4_e019);
    x ^= x >> 31;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^ (x >> 29)
}

/// Find an interior cross-track offset when the regular grid misses the distance gate.
/// Each probe uses the terrain-dependent slant distance; a discontinuity that jumps over
/// the gate must not fabricate a valid sample. Twenty-four bisections resolve millimetres
/// over the detector's distance envelope while bounding work on discontinuous terrain.
fn interior_cross_track_offset(d_lo: f64, d_hi: f64, distance: impl Fn(f64) -> f64) -> Option<f64> {
    if !d_lo.is_finite() || !d_hi.is_finite() || d_lo < 0.0 || d_hi <= d_lo {
        return None;
    }
    let target = 0.5 * (d_lo + d_hi);
    let d0 = distance(0.0);
    if !d0.is_finite() || d0 >= target {
        return None;
    }
    let d1 = distance(d_hi);
    if !d1.is_finite() || d1 <= target {
        return None;
    }
    let (mut lo, mut hi) = (0.0, d_hi);
    for _ in 0..24 {
        let b = 0.5 * (lo + hi);
        let d = distance(b);
        if !d.is_finite() {
            return None;
        }
        if d >= d_lo && d <= d_hi {
            return Some(b);
        }
        if d < target {
            lo = b;
        } else {
            hi = b;
        }
    }
    None
}

/// Clip the approximate cross-track ray to the requested box before asking whether a point
/// passes its distance gate. A terrain correction may need an in-box point even when every
/// distance computed on the uncorrected CPA line lies outside that gate.
fn region_cross_track_interval(
    max_offset: f64,
    region: &BBox,
    origin: (f64, f64),
    direction: (f64, f64),
) -> Option<(f64, f64)> {
    if !max_offset.is_finite() || max_offset < 0.0 {
        return None;
    }
    let (mut lo, mut hi) = (0.0f64, max_offset);
    for (start, slope, min, max) in [
        (origin.0, direction.0, region.south, region.north),
        (origin.1, direction.1, region.west, region.east),
    ] {
        if ![start, slope, min, max].into_iter().all(f64::is_finite) || min > max {
            return None;
        }
        if slope == 0.0 {
            if start < min || start > max {
                return None;
            }
        } else {
            let (a, b) = ((min - start) / slope, (max - start) / slope);
            lo = lo.max(a.min(b));
            hi = hi.min(a.max(b));
        }
        if lo > hi {
            return None;
        }
    }
    Some((lo, hi))
}

/// A narrow region can lie entirely between regular cross-track samples. Clip the affine
/// latitude/longitude ray before probing its interior, so an otherwise valid offset outside
/// the box does not hide the permitted strip. Terrain still supplies every slant distance;
/// bisection accepts an actual in-gate point, never an interpolated height across a cliff.
fn region_cross_track_offset(
    d_lo: f64,
    d_hi: f64,
    region: &BBox,
    origin: (f64, f64),
    direction: (f64, f64),
    distance: impl Fn(f64) -> f64,
) -> Option<f64> {
    if !d_lo.is_finite() || !d_hi.is_finite() || d_lo < 0.0 || d_hi <= d_lo {
        return None;
    }
    let (lo, hi) = region_cross_track_interval(d_hi, region, origin, direction)?;
    let middle = 0.5 * (lo + hi);
    let dm = distance(middle);
    if !dm.is_finite() {
        return None;
    }
    if (d_lo..=d_hi).contains(&dm) {
        return Some(middle);
    }
    let mut endpoint = None;
    for edge in [lo, hi] {
        let de = distance(edge);
        if !de.is_finite() {
            continue;
        }
        // Preserve the existing interior probes first: an endpoint can pass distance
        // yet fail the caller's terrain-clearance or clock check.
        if endpoint.is_none() && (d_lo..=d_hi).contains(&de) {
            endpoint = Some(edge);
        }
        let lower = dm.min(de).max(d_lo);
        let upper = dm.max(de).min(d_hi);
        if lower > upper {
            continue;
        }
        let target = 0.5 * (lower + upper);
        let (mut a, mut b) = (middle, edge);
        for _ in 0..24 {
            let offset = 0.5 * (a + b);
            let d = distance(offset);
            if !d.is_finite() {
                break;
            }
            if (d_lo..=d_hi).contains(&d) {
                return Some(offset);
            }
            if (d < target) == (dm < target) {
                a = offset;
            } else {
                b = offset;
            }
        }
    }
    endpoint
}

/// Signed along-track extent of a geographic box relative to one sampled CPA line.
/// `tangent` converts latitude/longitude offsets to metres along the local flight heading.
fn region_along_projection(region: &BBox, origin: (f64, f64), tangent: (f64, f64)) -> (f64, f64) {
    let centre = region.center();
    let offset = (centre.0 - origin.0) * tangent.0 + (centre.1 - origin.1) * tangent.1;
    let half = 0.5
        * ((region.north - region.south) * tangent.0.abs()
            + (region.east - region.west) * tangent.1.abs());
    (offset, half)
}

/// Adjacent coarse CPA lines can skip an entire narrow region. Find an actual intervening
/// line through its centre, without changing the regular along-track sampling phase. Every
/// probe re-evaluates the flight and terrain; the caller still applies all hypothesis gates.
fn skipped_region_time(
    left: (f64, (f64, f64)),
    right: (f64, (f64, f64)),
    project: impl Fn(f64) -> Option<(f64, f64)>,
) -> Option<f64> {
    let (mut lo, (a, ah)) = left;
    let (mut hi, (b, bh)) = right;
    if lo >= hi || !((a > ah && b < -bh) || (a < -ah && b > bh)) {
        return None;
    }
    let mut best: Option<(f64, f64)> = None;
    for _ in 0..24 {
        let t = lo + 0.5 * (hi - lo);
        if t <= lo || t >= hi {
            break;
        }
        let (offset, half) = project(t)?;
        if !offset.is_finite() || !half.is_finite() || half < 0.0 {
            return None;
        }
        if offset.abs() <= half && best.is_none_or(|(_, old)| offset.abs() < old) {
            best = Some((t, offset.abs()));
        }
        if offset == 0.0 {
            break;
        }
        if (offset > 0.0) == (a > 0.0) {
            lo = t;
        } else {
            hi = t;
        }
    }
    best.map(|(t, _)| t)
}

#[allow(clippy::too_many_arguments)]
fn hypotheses(
    ev: &SearchEvent,
    tracks: &[TrackModel],
    p: &SearchParams,
    frame: &Frame,
    dem: &dyn Elevation,
    atmo: &dyn Atmosphere,
    t_base: f64,
    cap: usize,
) -> HypSet {
    let count = AtomicUsize::new(0);
    // Keep the established reach, expanding it when the local distance bound permits
    // a farther pass. Testing the unscaled box first loses that pass
    // before its atmosphere can be evaluated. This remains an approximate CPA locus.
    let reach = p.region.padded_km(1.1 * ev.d_gate[1] / 1000.0 + 0.5);
    let in_reach = |lat, lon, d_hi: f64| {
        reach.contains(lat, lon)
            || (d_hi.is_finite()
                && d_hi > ev.d_gate[1]
                && p.region
                    .padded_km(1.1 * d_hi / 1000.0 + 0.5)
                    .contains(lat, lon))
    };
    let t_lo_all = p.start_lo + ev.t_rec - ev.t_tol - 1.1 * ev.d_gate[1] / p.c - 5.0;
    // Preserve the established calm path only at the same sound speed used by the fit.
    // A nominal speed or one sampled weather column cannot bound a moving medium, nor
    // the actual ECEF range of a terrain-dependent coarse point. Unknown means scan.
    let ordinary_still_air = matching_still_air(ev, p, atmo);
    let parts = tracks
        .par_iter()
        .enumerate()
        .filter(|(_, tr)| !p.tonal_only || tr.engine.tonal())
        .map(|(ti, tr)| {
            let mut out = HypBuilder::new(cap);
            let te_lo = t_lo_all.max(tr.t0());
            let te_hi = (p.start_hi + ev.t_rec + ev.t_tol + 5.0).min(tr.t1());
            let project = |t| {
                let (pos, vel) = tr.pv(t);
                let (la, lo, h) = ecef_to_geodetic(pos);
                let local = Enu::new(la, lo, 0.0);
                let ve = local.vec_from_ecef(vel);
                let vh = ve[0].hypot(ve[1]);
                if vh <= 12.0
                    || (ordinary_still_air && !reach.contains(la, lo))
                    || tr.gap_at(t) > p.max_gap_s
                {
                    return None;
                }
                let z0 = dem.elev_m(la, lo);
                let med = atmo.medium(la, lo, t, z0 + MIC_HEIGHT_M, h);
                let distance_scale = if med.wind_e == 0.0 && med.wind_n == 0.0 {
                    med.c / ev.c_fit
                } else {
                    Prop::from_medium(&med, &local)
                        .fit_gate_scales(vel, ev.c_fit)?
                        .1[1]
                };
                if !in_reach(la, lo, ev.d_gate[1] * distance_scale) {
                    return None;
                }
                let (mlat, mlon) = fdop_types::geo::m_per_deg(la);
                let (ux, uy) = (ve[0] / vh, ve[1] / vh);
                let a0 = (h - z0 - MIC_HEIGHT_M) * ve[2] / vh;
                Some(region_along_projection(
                    &p.region,
                    (la + a0 * uy / mlat, lo + a0 * ux / mlon),
                    (uy * mlat, ux * mlon),
                ))
            };
            let needs_terrain_retry = std::cell::Cell::new(false);
            let sample = |t, retry: bool, out: &mut HypBuilder| {
                let (pos, vel) = tr.pv(t);
                let (la, lo, h) = ecef_to_geodetic(pos);
                let local = Enu::new(la, lo, 0.0);
                let ve = local.vec_from_ecef(vel);
                let vh = ve[0].hypot(ve[1]);
                let dt = (p.along_step_m / vh.max(1.0)).clamp(0.2, p.along_dt_max);
                if ordinary_still_air && !reach.contains(la, lo) {
                    // A close fast pass can enter and leave this entire box in ten seconds.
                    // Keep the usual along-track sampling outside too, so entry is not skipped.
                    return (dt, None);
                }
                let mut projection = None;
                let speed = norm(vel);
                let z0 = dem.elev_m(la, lo);
                let med = atmo.medium(la, lo, t, z0 + MIC_HEIGHT_M, h);
                let c = med.c;
                let (speed_scale, distance_scale) = if med.wind_e == 0.0 && med.wind_n == 0.0 {
                    let scale = c / ev.c_fit;
                    (scale, [scale; 2])
                } else if let Some(scales) =
                    Prop::from_medium(&med, &local).fit_gate_scales(vel, ev.c_fit)
                {
                    scales
                } else {
                    return (dt, None);
                };
                // Wind gives speed one exact scale, but range also depends on the unknown
                // observer's direction. Seed its conservative annulus here; exact scoring
                // compares the candidate's equivalent fitted speed and range separately.
                let (d_lo, d_hi) = (
                    ev.d_gate[0] * distance_scale[0],
                    ev.d_gate[1] * distance_scale[1],
                );
                if !in_reach(la, lo, d_hi) {
                    return (dt, None);
                }
                if speed >= ev.v_gate[0] * speed_scale
                    && speed <= ev.v_gate[1] * speed_scale
                    && vh > 12.0
                    && tr.gap_at(t) <= p.max_gap_s
                {
                    let (ux, uy) = (ve[0] / vh, ve[1] / vh);
                    let (nx, ny) = (-uy, ux);
                    let (mlat, mlon) = fdop_types::geo::m_per_deg(la);
                    let a0 = (h - z0 - MIC_HEIGHT_M) * ve[2] / vh;
                    let cross_point = |side: f64, b: f64| {
                        let (oe, on) = (a0 * ux + side * b * nx, a0 * uy + side * b * ny);
                        let (ola, olo) = (la + on / mlat, lo + oe / mlon);
                        let z = dem.elev_m(ola, olo);
                        let hh = h - z - MIC_HEIGHT_M;
                        let a = hh * ve[2] / vh;
                        let d = (a * a + b * b + hh * hh).sqrt();
                        (ola, olo, hh, d)
                    };
                    let to_hyp = |(ola, olo, hh, d): (f64, f64, f64, f64)| {
                        if !p.region.contains(ola, olo) {
                            return None;
                        }
                        let in_clock =
                            |ts: f64| ts >= p.start_lo - ev.t_tol && ts <= p.start_hi + ev.t_tol;
                        let exact_terrain_cpa = || {
                            // The coarse CPA line uses terrain beneath the aircraft. On a
                            // hillside, a climb or descent reaches this observer's actual
                            // CPA at another time AND height. Either its approximate distance
                            // or its clock can therefore fail before the real pass is tried.
                            // Already accepted points never take this bounded retry.
                            let o = observer(ola, olo, dem);
                            if !o.z.is_finite() || o.z == z0 || ve[2] == 0.0 {
                                return None;
                            }
                            let cpa = tr.cpa_nearest_time(o.x, t, 150.0)?;
                            let prop = prop_for(atmo, &o, tr, cpa.t_e);
                            let (_, _, cpa_h) = ecef_to_geodetic(tr.pos_at(cpa.t_e));
                            let agl = cpa_h - o.z - MIC_HEIGHT_M;
                            if !agl.is_finite()
                                || agl < p.min_agl_m
                                || tr.gap_at(cpa.t_e) > p.max_gap_s
                                || !gates_ok(ev, &cpa, &prop, 0.0)
                            {
                                return None;
                            }
                            let ts = cpa.t_r_fit_p(&prop) - ev.t_rec;
                            in_clock(ts).then_some((cpa.t_e, ts))
                        };
                        let ordinary_geometry = hh >= p.min_agl_m && d >= d_lo && d <= d_hi;
                        let (t_e, t_s) = if ordinary_geometry {
                            let observer_z = h - hh;
                            let path = atmo.medium(ola, olo, t, observer_z, h);
                            let centre_offset = if path.wind_e == 0.0 && path.wind_n == 0.0 {
                                // Arrival time belongs to this observer's path average, which
                                // can differ from the column beneath the aircraft. Uniform
                                // still-air providers retain exactly the same arithmetic.
                                d / path.c
                            } else {
                                // A measured still-air curve is centred at its fitted CPA,
                                // which oblique wind shifts from the geometric CPA's arrival.
                                let obs = Enu::new(ola, olo, observer_z);
                                let prop = Prop::from_medium(&path, &obs);
                                let rv = sub(pos, obs.to_ecef([0.0; 3]));
                                prop.fit_center_offset(rv, vel)
                            };
                            let ts = t + centre_offset - ev.t_rec;
                            if in_clock(ts) {
                                (t, ts)
                            } else {
                                exact_terrain_cpa()?
                            }
                        } else {
                            if !retry {
                                let z = dem.elev_m(ola, olo);
                                if z.is_finite() && z != z0 && ve[2] != 0.0 {
                                    needs_terrain_retry.set(true);
                                }
                                return None;
                            }
                            exact_terrain_cpa()?
                        };
                        let (e, n) = frame.en(ola, olo);
                        Some((
                            Hyp {
                                trk: ti as u32,
                                t_e: (t_e - t_base) as f32,
                                e: e as f32,
                                n: n as f32,
                                t_s: (t_s - t_base) as f32,
                            },
                            !ordinary_geometry,
                        ))
                    };
                    let center = cross_point(1.0, 0.0);
                    projection = Some(region_along_projection(
                        &p.region,
                        (center.0, center.1),
                        (uy * mlat, ux * mlon),
                    ));
                    let center_ok = to_hyp(center).is_some();
                    let mut retain = |hyp: Hyp, recovered: bool| {
                        if !retry || recovered {
                            out.push(hyp, ti, cap, &count);
                        }
                    };
                    for side in [1.0f64, -1.0] {
                        // The central grid point is shared by both sides.
                        let mut has_hypothesis = center_ok;
                        let mut b = if side > 0.0 { 0.0 } else { p.cross_step_m };
                        while b <= d_hi {
                            let point = if b == 0.0 {
                                center
                            } else {
                                cross_point(side, b)
                            };
                            // Rising terrain can bring a later point back inside the distance
                            // gate. Only the horizontal reach bounds this cross-track ray.
                            if let Some((hyp, recovered)) = to_hyp(point) {
                                has_hypothesis = true;
                                retain(hyp, recovered);
                            }
                            b += p.cross_step_m;
                        }
                        // A close pass can have its whole allowed annulus between grid lines.
                        // Only an accepted hypothesis replaces the interior sample: a point
                        // outside the region or clock window cannot cover this side.
                        if !has_hypothesis {
                            let fallback = interior_cross_track_offset(d_lo, d_hi, |b| {
                                if b == 0.0 {
                                    center.3
                                } else {
                                    cross_point(side, b).3
                                }
                            })
                            .and_then(|b| to_hyp(cross_point(side, b)))
                            .or_else(|| {
                                region_cross_track_offset(
                                    d_lo,
                                    d_hi,
                                    &p.region,
                                    (center.0, center.1),
                                    (side * ny / mlat, side * nx / mlon),
                                    |b| cross_point(side, b).3,
                                )
                                .and_then(|b| to_hyp(cross_point(side, b)))
                            })
                            .or_else(|| {
                                // A terrain-dependent distance rejection must not hide every
                                // point in a narrow box before its exact CPA can be checked.
                                let (lo, hi) = region_cross_track_interval(
                                    d_hi,
                                    &p.region,
                                    (center.0, center.1),
                                    (side * ny / mlat, side * nx / mlon),
                                )?;
                                let point = cross_point(side, 0.5 * (lo + hi));
                                if retry {
                                    to_hyp(point)
                                } else {
                                    // Preserve the ordinary cloud before checking this
                                    // rejected point's actual terrain-dependent CPA.
                                    let z = dem.elev_m(point.0, point.1);
                                    if z.is_finite() && z != z0 && ve[2] != 0.0 {
                                        needs_terrain_retry.set(true);
                                    }
                                    None
                                }
                            });
                            if let Some((hyp, recovered)) = fallback {
                                retain(hyp, recovered);
                            }
                        }
                    }
                }
                (dt, projection)
            };
            let sample_at = |t, out: &mut HypBuilder| {
                needs_terrain_retry.set(false);
                let sampled = sample(t, false, out);
                // A newly accepted regular point must not suppress an older successful
                // interior fallback. Keep this sample's ordinary points first, then recover
                // its rejected geometry. Even nearby ordinary points can carry a different
                // clock; only exact physical gates can reject the recovery. No deferred-point
                // buffer is needed, and both kinds of points share the same counted budget.
                if needs_terrain_retry.get() {
                    sample(t, true, out);
                }
                sampled
            };
            let scan = |lo, hi, exclusive_start, exclusive_end, out: &mut HypBuilder| {
                let mut t = lo;
                let mut previous = None;
                let mut last = None;
                while t <= hi && !(exclusive_end && t == hi) {
                    let (dt, projection) = if exclusive_start && t == lo {
                        // The previous span owns this shared sample. Keep its projection
                        // to find a narrow region crossed before the suffix's first step.
                        let (pos, vel) = tr.pv(t);
                        let (la, ln, _) = ecef_to_geodetic(pos);
                        let ve = Enu::new(la, ln, 0.0).vec_from_ecef(vel);
                        let dt = (p.along_step_m / ve[0].hypot(ve[1]).max(1.0))
                            .clamp(0.2, p.along_dt_max);
                        (dt, project(t))
                    } else {
                        sample_at(t, out)
                    };
                    last = Some(t);
                    if let (Some(left), Some(right)) = (previous, projection)
                        && let Some(extra) = skipped_region_time(left, (t, right), project)
                    {
                        sample_at(extra, out);
                    }
                    previous = projection.map(|span| (t, span));
                    let next = t + dt;
                    // The final partial interval has no regular sample at its far endpoint.
                    // Probe that endpoint only to recover a region crossed before the clock ends.
                    if (next > hi || (exclusive_end && next == hi))
                        && t < hi
                        && let (Some(left), Some(right)) = (previous, project(hi))
                        && let Some(extra) = skipped_region_time(left, (hi, right), project)
                    {
                        sample_at(extra, out);
                    }
                    t = next;
                }
                last
            };
            // Keep the established sampling phase and ordinary points first. Wind can move
            // the fitted centre to either side of the geometric emission time, so both omitted
            // spans need inspection. Unknown media provide no certified time bound. Extra work
            // grows with the retained track span; every recovered point uses the same budget.
            let mut last = scan(te_lo, te_hi, false, false, &mut out);
            if !ordinary_still_air {
                if tr.t0() < te_lo {
                    let prefix = scan(tr.t0(), te_lo.min(te_hi), false, te_lo <= te_hi, &mut out);
                    last = last.or(prefix);
                }
                if te_hi < tr.t1() {
                    let _ = scan(
                        te_hi.max(tr.t0()),
                        tr.t1(),
                        last == Some(te_hi),
                        false,
                        &mut out,
                    );
                }
            }
            out.finish(cap, &count)
        })
        .collect::<Vec<(Vec<Hyp>, usize, Vec<Hyp>)>>();
    let total: usize = parts.iter().map(|x| x.1).sum();
    let mut sample: Vec<Hyp> = parts.iter().flat_map(|x| x.2.iter().copied()).collect();
    let stored = (total <= cap).then(|| parts.into_iter().flat_map(|x| x.0).collect::<Vec<Hyp>>());
    if let Some(st) = &stored
        && sample.len() < 300
        && !st.is_empty()
    {
        // small cloud: sample it directly
        let step = (st.len() / 300).max(1);
        sample = st.iter().step_by(step).copied().collect();
    }
    if sample.len() > 400 {
        let step = sample.len() as f64 / 400.0;
        sample = (0..400)
            .map(|k| sample[(k as f64 * step) as usize])
            .collect();
    }
    HypSet {
        stored,
        count: total,
        sample,
    }
}

/// (event i, event j, track i, track j): one flight pair.
type PairKey = (usize, usize, u32, u32);
/// A flight pair in one 1 km spatial cell and one recording-clock cell.
type CellKey = (usize, usize, u32, u32, i32, i32, i64);
/// Clock resolution already retained by the pair join; later merging must not erase it.
const CLOCK_CELL_S: f64 = 20.0;
/// Minimum emission-CPA separation between events assigned to the same flight.
const SAME_FLIGHT_MIN_DT_S: f64 = 30.0;

fn clock_cell(t_s: f64) -> Option<i64> {
    let cell = (t_s / CLOCK_CELL_S).floor();
    (cell.is_finite() && cell >= i64::MIN as f64 && cell < i64::MAX as f64).then_some(cell as i64)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Joined {
    ei: usize,
    ej: usize,
    ti: u32,
    tj: u32,
    te_i: f64,
    te_j: f64,
    e: f64,
    n: f64,
    t_s: f64,
}

// Bound duplicate-bearing scratch without capping the distinct seeds that reach scoring.
// Indexed chunks keep the original input/bucket order, including the first representative.
const JOIN_BATCH_HYPOTHESES: usize = 4096;
const JOIN_CHUNK_HYPOTHESES: usize = 64;
// Score complete representatives promptly; one dense event pair must not retain all
// of its rejected seeds until the join ends. This is a buffer size, not a search cut.
const JOIN_OUTPUT_SEEDS: usize = 4096;

#[allow(clippy::too_many_arguments)]
fn join_batches(
    hi: &[Hyp],
    hj: &[Hyp],
    ei: usize,
    ej: usize,
    r: f64,
    tt: f64,
    same_flight_min_dt: f64,
    t_base: f64,
    mut consume: impl FnMut(&[Joined]),
) -> usize {
    if hi.is_empty() || hj.is_empty() {
        return 0;
    }
    let key = |e: f32, n: f32, ts: f32| {
        (
            (e as f64 / r).floor() as i32,
            (n as f64 / r).floor() as i32,
            (ts as f64 / tt).floor() as i64,
        )
    };
    let mut map: HashMap<(i32, i32, i64), Vec<u32>> = HashMap::with_capacity(hi.len() / 2);
    for (k, h) in hi.iter().enumerate() {
        map.entry(key(h.e, h.n, h.t_s)).or_default().push(k as u32);
    }
    // One representative per (flights, 600 m cell, 20 s), as in the original join.
    let cell = |j: &Joined| {
        Some((
            j.ti,
            j.tj,
            (j.e / 600.0).floor() as i32,
            (j.n / 600.0).floor() as i32,
            clock_cell(j.t_s)?,
        ))
    };
    let r2 = r * r;
    let mut seen = std::collections::HashSet::new();
    let mut ready = Vec::with_capacity(JOIN_OUTPUT_SEEDS);
    let mut joined = 0usize;
    for batch in hj.chunks(JOIN_BATCH_HYPOTHESES) {
        let parts: Vec<Vec<Joined>> = batch
            .par_chunks(JOIN_CHUNK_HYPOTHESES)
            .map(|chunk| {
                let mut local_seen = std::collections::HashSet::new();
                let mut kept = Vec::new();
                for b in chunk {
                    let (kx, ky, kt) = key(b.e, b.n, b.t_s);
                    for dx in -1..=1 {
                        for dy in -1..=1 {
                            for dtk in -1..=1 {
                                if let Some(list) = map.get(&(kx + dx, ky + dy, kt + dtk)) {
                                    for &ia in list {
                                        let a = &hi[ia as usize];
                                        let (de, dn) = ((a.e - b.e) as f64, (a.n - b.n) as f64);
                                        if de * de + dn * dn > r2
                                            || ((a.t_s - b.t_s) as f64).abs() > tt
                                        {
                                            continue;
                                        }
                                        if a.trk == b.trk
                                            && ((a.t_e - b.t_e) as f64).abs() < same_flight_min_dt
                                        {
                                            continue;
                                        }
                                        let j = Joined {
                                            ei,
                                            ej,
                                            ti: a.trk,
                                            tj: b.trk,
                                            te_i: a.t_e as f64 + t_base,
                                            te_j: b.t_e as f64 + t_base,
                                            e: 0.5 * (a.e + b.e) as f64,
                                            n: 0.5 * (a.n + b.n) as f64,
                                            t_s: 0.5 * (a.t_s + b.t_s) as f64 + t_base,
                                        };
                                        // Deduplicate at production, not after collecting even
                                        // one bucket: a dense bucket alone can make millions
                                        // of raw pairs. Earlier batches need no scratch entry.
                                        if let Some(k) = cell(&j)
                                            && !seen.contains(&k)
                                            && local_seen.insert(k)
                                        {
                                            kept.push(j);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                kept
            })
            .collect();
        // Rayon collects indexed chunks in input order. Never iterate a hash set to
        // emit candidates: later fitting depends on the original first representative.
        for part in parts {
            for j in part {
                if let Some(k) = cell(&j)
                    && seen.insert(k)
                {
                    ready.push(j);
                    if ready.len() == JOIN_OUTPUT_SEEDS {
                        joined += ready.len();
                        consume(&ready);
                        ready.clear();
                    }
                }
            }
        }
    }
    if !ready.is_empty() {
        joined += ready.len();
        consume(&ready);
    }
    joined
}

/// Merge duplicate place/clock seeds and rank every flight pair's best cell before its next.
fn ranked_fine_seeds(quick: Vec<(Joined, f64)>) -> Vec<(Joined, f64)> {
    // Hash-map iteration and exact score ties must not choose which clock or seed survives.
    let compare = |a: &(Joined, f64), b: &(Joined, f64)| {
        b.1.total_cmp(&a.1).then_with(|| {
            let (a, b) = (&a.0, &b.0);
            (a.ei, a.ej, a.ti, a.tj)
                .cmp(&(b.ei, b.ej, b.ti, b.tj))
                .then(a.t_s.total_cmp(&b.t_s))
                .then(a.e.total_cmp(&b.e))
                .then(a.n.total_cmp(&b.n))
                .then(a.te_i.total_cmp(&b.te_i))
                .then(a.te_j.total_cmp(&b.te_j))
        })
    };
    let mut best_by: HashMap<CellKey, (Joined, f64)> = HashMap::new();
    for (j, s) in quick {
        let Some(clock) = clock_cell(j.t_s) else {
            continue;
        };
        let k = (
            j.ei,
            j.ej,
            j.ti,
            j.tj,
            (j.e / 1000.0).floor() as i32,
            (j.n / 1000.0).floor() as i32,
            clock,
        );
        match best_by.get(&k) {
            Some(o) if !compare(&(j, s), o).is_lt() => {}
            _ => {
                best_by.insert(k, (j, s));
            }
        }
    }
    // Every flight pair's best cell is scored before any pair's second-best cell, so a budget
    // cut cannot starve the true pair behind many cells of a few look-alike pairs.
    let mut groups: HashMap<PairKey, Vec<(Joined, f64)>> = HashMap::new();
    for (j, s) in best_by.into_values() {
        groups
            .entry((j.ei, j.ej, j.ti, j.tj))
            .or_default()
            .push((j, s));
    }
    let mut ranked: Vec<(usize, Joined, f64)> = Vec::new();
    for (_, mut g) in groups {
        g.sort_by(compare);
        ranked.extend(g.into_iter().enumerate().map(|(r, (j, s))| (r, j, s)));
    }
    ranked.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| compare(&(a.1, a.2), &(b.1, b.2)))
    });
    ranked.into_iter().map(|(_, j, s)| (j, s)).collect()
}

/// Frequency-ratio curve f/f0 through the propagation medium on a uniform reception-time
/// grid for one (track, observer).
struct RatioGrid {
    t0: f64,
    dt: f64,
    r: Vec<f64>,
    /// Newton second/third differences for each monotone interior interval. NaN
    /// disables the cubic at boundaries, turns or gaps; the linear fallback stays.
    cubic: Vec<[f64; 2]>,
}

/// Newton stops once a step is shorter than this (s); each solve also checks its final
/// reception-time residual. Grids take most of a search's time. In the original tolerance
/// benchmark, before the exact wind derivative, this versus 1e-7 s on a real recording's fine
/// stage moved chi by under 1e-4 in 803 of 806 cells and by at most 0.024, with no cell's best
/// point changed.
const RATIO_GRID_TOL_S: f64 = 1e-4;

impl RatioGrid {
    #[allow(clippy::too_many_arguments)]
    fn build(tr: &TrackModel, x: V3, m: &Prop, lo: f64, hi: f64, dt: f64, max_gap: f64) -> Self {
        let n = (((hi - lo) / dt).ceil() as usize + 1).max(2);
        let mut r = Vec::with_capacity(n);
        let mut warm: Option<f64> = None;
        let mut hint = 0;
        for k in 0..n {
            let tu = lo + k as f64 * dt;
            match tr.solve_emission_sweep(tu, x, m, warm, &mut hint, RATIO_GRID_TOL_S) {
                Some((te, _, ratio)) if tr.gap_of(hint) <= max_gap => {
                    warm = Some(te);
                    r.push(ratio);
                }
                _ => {
                    warm = None;
                    r.push(f64::NAN);
                }
            }
        }
        Self::from_samples(lo, dt, r)
    }

    fn from_samples(t0: f64, dt: f64, r: Vec<f64>) -> Self {
        // A clock scan reads each interval many times. Its support and differences
        // depend only on the stored predictions, not on the clock being tested.
        let mut cubic = vec![[f64::NAN; 2]; r.len().saturating_sub(1)];
        for (i, values) in r.windows(4).enumerate() {
            let [a, b, c, d] = [values[0], values[1], values[2], values[3]];
            let (left, middle, right) = (b - a, c - b, d - c);
            if a.is_finite() && d.is_finite() && left * middle >= 0.0 && middle * right >= 0.0 {
                cubic[i + 1] = [middle - left, right - 2.0 * middle + left];
            }
        }
        Self { t0, dt, r, cubic }
    }
    #[inline]
    fn at(&self, tu: f64) -> f64 {
        let u = (tu - self.t0) / self.dt;
        if !u.is_finite() || u < 0.0 {
            return f64::NAN;
        }
        let i = u.floor() as usize;
        if i >= self.r.len().saturating_sub(1) {
            return f64::NAN;
        }
        let w = u - i as f64;
        let (b, c) = (self.r[i], self.r[i + 1]);
        let linear = b * (1.0 - w) + c * w;
        // Linear interpolation systematically flattens a close pass's rapid Doppler
        // curvature. Use the four-point interpolating cubic on a continuous monotone
        // stretch, without extra emission solves or a denser grid. Newton differences
        // avoid cancellation between large polynomial terms on nearly flat tails.
        let [second, third] = self.cubic[i];
        if second.is_finite() && linear.is_finite() {
            let cubic = b + w * (c - b) + w * (w - 1.0) * (0.5 * second + (w + 1.0) * third / 6.0);
            // A turn, a plateau or sparse samples must not invent a new extremum.
            // The original linear path remains at boundaries and beside gaps; a
            // missing bracketing endpoint remains missing even at an exact node.
            if cubic.is_finite() && cubic >= b.min(c) && cubic <= b.max(c) {
                return cubic;
            }
        }
        linear
    }
}

/// Fit of one event's trace against a model ratio curve at one clock.
#[derive(Clone, Copy, Debug)]
struct TraceFit {
    /// Mean capped squared residual / sigma^2.
    chi: f64,
    /// Emitted tone (Hz); before the break if there is one.
    f0: f64,
    rms: f64,
    /// Fraction of trace points the model covers.
    cov: f64,
    /// One tone change (prop RPM / power change) for misfit events: (recording time of the
    /// first point after the change, tone after it).
    brk: Option<(f64, f64)>,
}

/// Reused across a clock scan (and its events), including shorter traces after longer ones.
/// A fine cell evaluates hundreds of clocks at each observer; allocating these buffers for
/// every trace fit otherwise dominates allocation traffic without changing the model.
#[derive(Default)]
struct TraceScratch {
    ratios: Vec<f64>,
    deviations: Vec<f64>,
    prefix: [Vec<f64>; 3],
}

/// Fit of one event's trace against a model ratio curve at clock t_s. The tone is fitted in
/// closed form; misfit events may also change tone once (searched over every split).
/// `tone_scale` converts the still-air tone constraints into this candidate's emitted-frequency
/// frame. It changes neither their relative width nor the noise and trimming in observed Hz.
#[allow(clippy::too_many_arguments)]
fn trace_chi(
    tt: &[f64],
    ff: &[f64],
    ratio: &impl Fn(f64) -> f64,
    t_s: f64,
    ev: &SearchEvent,
    tone_scale: f64,
    sigma: f64,
    cap_sigma: f64,
    robust: bool,
    scratch: &mut TraceScratch,
) -> TraceFit {
    let n = tt.len();
    let source_f0 = ev.f0 * tone_scale;
    let source_tol = ev.f0_tol * tone_scale;
    if !tone_scale.is_finite()
        || tone_scale <= 0.0
        || !source_f0.is_finite()
        || !source_tol.is_finite()
        || source_tol < 0.0
    {
        return TraceFit {
            chi: f64::INFINITY,
            f0: f64::NAN,
            rms: f64::NAN,
            cov: 0.0,
            brk: None,
        };
    }
    let TraceScratch {
        ratios: rr,
        deviations: ad,
        prefix,
    } = scratch;
    rr.clear();
    rr.reserve(n);
    let (mut sfr, mut srr) = (0.0, 0.0);
    for k in 0..n {
        let r = ratio(t_s + tt[k]);
        rr.push(r);
        if r.is_finite() {
            sfr += ff[k] * r;
            srr += r * r;
        }
    }
    let rr = rr.as_slice();
    let cov = rr.iter().filter(|r| r.is_finite()).count();
    if cov < 5 || srr <= 0.0 {
        return TraceFit {
            chi: f64::INFINITY,
            f0: f64::NAN,
            rms: f64::NAN,
            cov: cov as f64 / n.max(1) as f64,
            brk: None,
        };
    }
    let clamp_f0 = |f: f64| f.clamp(source_f0 - source_tol, source_f0 + source_tol);
    let mut f0 = clamp_f0(sfr / srr);
    let mut lim = f64::INFINITY;
    if robust {
        ad.clear();
        ad.extend(
            (0..n)
                .filter(|&k| rr[k].is_finite())
                .map(|k| (ff[k] - f0 * rr[k]).abs()),
        );
        let mad = 1.4826 * crate::spectro::median_f64(ad);
        lim = (4.0 * mad).max(3.0 * sigma);
        let (mut a, mut b) = (0.0, 0.0);
        for k in 0..n {
            if rr[k].is_finite() && (ff[k] - f0 * rr[k]).abs() <= lim {
                a += ff[k] * rr[k];
                b += rr[k] * rr[k];
            }
        }
        if b > 0.0 {
            f0 = clamp_f0(a / b);
        }
    }
    let cap2 = (cap_sigma * sigma).powi(2);
    // capped chi, raw rms and coverage for tone f_a before index `k_brk` and f_b from it on
    let score = |fa: f64, fb: f64, k_brk: usize| -> (f64, f64, usize) {
        let (mut s, mut s_raw, mut m) = (0.0, 0.0, 0usize);
        for k in 0..n {
            if rr[k].is_finite() {
                let fk = if k < k_brk { fa } else { fb };
                let e2 = (ff[k] - fk * rr[k]).powi(2);
                s += e2.min(cap2);
                s_raw += e2;
                m += 1;
            } else {
                s += cap2;
            }
        }
        (
            s / (n as f64 * sigma * sigma),
            (s_raw / m.max(1) as f64).sqrt(),
            m,
        )
    };
    let (chi, rms, m) = score(f0, f0, n);
    let mut out = TraceFit {
        chi,
        f0,
        rms,
        cov: m as f64 / n as f64,
        brk: None,
    };
    if robust {
        // A minority of off-ridge points can shift least squares far enough that the
        // ensuing MAD trim accepts them. Seed a second fit from the median emitted
        // tone instead. Keep the original fit and its change-point proposal below;
        // the alternative must improve the same capped objective, with the same
        // tone bounds, trimming rule and missing-coverage penalties.
        ad.clear();
        ad.extend((0..n).filter_map(|k| {
            let tone = ff[k] / rr[k];
            (rr[k].is_finite() && rr[k] > 0.0 && tone.is_finite()).then_some(tone)
        }));
        if ad.len() == cov {
            let median_f0 = clamp_f0(crate::spectro::median_f64(ad));
            ad.clear();
            ad.extend(
                (0..n)
                    .filter(|&k| rr[k].is_finite())
                    .map(|k| (ff[k] - median_f0 * rr[k]).abs()),
            );
            let median_mad = 1.4826 * crate::spectro::median_f64(ad);
            let median_lim = (4.0 * median_mad).max(3.0 * sigma);
            let (mut a, mut b) = (0.0, 0.0);
            for k in 0..n {
                if rr[k].is_finite() && (ff[k] - median_f0 * rr[k]).abs() <= median_lim {
                    a += ff[k] * rr[k];
                    b += rr[k] * rr[k];
                }
            }
            if b > 0.0 {
                let alternative_f0 = clamp_f0(a / b);
                let (alternative_chi, alternative_rms, _) =
                    score(alternative_f0, alternative_f0, n);
                if alternative_chi < out.chi {
                    out.chi = alternative_chi;
                    out.f0 = alternative_f0;
                    out.rms = alternative_rms;
                }
            }
        }
    }
    if robust && ev.misfit && n >= 40 {
        // prefix sums over inliers of the single-tone fit; split with the least squared error
        for sums in prefix.iter_mut() {
            sums.resize(n + 1, 0.0);
            sums[0] = 0.0;
        }
        let [pa, pb, pc] = prefix;
        for k in 0..n {
            let ok = rr[k].is_finite() && (ff[k] - f0 * rr[k]).abs() <= lim.max(0.15 * ev.f0);
            let (fr, r2, f2) = if ok {
                (ff[k] * rr[k], rr[k] * rr[k], ff[k] * ff[k])
            } else {
                (0.0, 0.0, 0.0)
            };
            pa[k + 1] = pa[k] + fr;
            pb[k + 1] = pb[k] + r2;
            pc[k + 1] = pc[k] + f2;
        }
        let seg_clamp = |f: f64| f.clamp(0.85 * source_f0, 1.15 * source_f0);
        let min_side = (n / 6).max(10);
        let mut best: Option<(f64, usize, f64, f64)> = None;
        for k in min_side..n.saturating_sub(min_side) {
            let (a1, b1, c1) = (pa[k], pb[k], pc[k]);
            let (a2, b2, c2) = (pa[n] - a1, pb[n] - b1, pc[n] - c1);
            if b1 <= 0.0 || b2 <= 0.0 {
                continue;
            }
            let (fa, fb) = (seg_clamp(a1 / b1), seg_clamp(a2 / b2));
            let sse = (c1 - 2.0 * fa * a1 + fa * fa * b1) + (c2 - 2.0 * fb * a2 + fb * fb * b2);
            if best.is_none_or(|b| sse < b.0) {
                best = Some((sse, k, fa, fb));
            }
        }
        if let Some((_, k, fa, fb)) = best {
            let (chi2, rms2, m2) = score(fa, fb, k);
            if chi2 < out.chi {
                out = TraceFit {
                    chi: chi2,
                    f0: fa,
                    rms: rms2,
                    cov: m2 as f64 / n as f64,
                    brk: Some((tt[k], fb)),
                };
            }
        }
    }
    out
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct EventMatch {
    pub event: String,
    pub flight: String,
    pub icao: String,
    pub reg: String,
    pub typ: String,
    pub engine: String,
    /// Slant CPA distance (m), speed at CPA (m/s), heard-CPA UTC time.
    pub d: f64,
    pub speed: f64,
    pub alt_m: f64,
    pub t_cpa_heard_utc: f64,
    /// Path-averaged sound speed (m/s) and wind component along the flight (m/s) used.
    pub c_path: f64,
    pub wind_along: f64,
    /// The event's own fit flagged a model misfit (weighted half in the score).
    pub misfit_event: bool,
    /// Emitted tone (Hz); before the change if `f0_change` is set.
    pub f0: f64,
    /// One tone change (misfit events): (recording time it took effect, tone after it).
    pub f0_change: Option<(f64, f64)>,
    /// Mean squared capped residual / sigma^2 (1 = as good as the event's own straight-line fit).
    pub chi: f64,
    pub rms_hz: f64,
    pub coverage: f64,
    /// Role: "pair" (drove the search) or "confirmer" (explained afterwards at the locked clock).
    pub role: String,
    /// Clock offset this event alone would prefer, relative to the joint clock (s).
    pub dt_free: Option<f64>,
    /// Chance-match surprise of this match (nats) and the part of it earned by the trace fit
    /// (the rest is the gates: a flight was there at all).
    pub nats: f64,
    pub trace_nats: f64,
    /// Fits cleanly (chi <= 4), locks within 4 s and beats chance matches by >= 2 nats: the
    /// matches that locate the observer.
    pub core: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Candidate {
    pub lat: f64,
    pub lon: f64,
    pub elev_m: f64,
    /// UTC unix time of the recording start implied by the joint clock fit.
    pub t_start_utc: f64,
    pub events: Vec<EventMatch>,
    pub n_explained: usize,
    /// Evidence (nats): sum over explained events of -ln P(chance match), minus 2.3 per unheard
    /// close pass. Ranking key.
    pub score: f64,
    /// Sum over explained events of w (1 - chi/kappa) (w = 0.5 for misfit events).
    pub fit_score: f64,
    pub pair_chi: f64,
    /// Tonal aircraft that passed much closer than the explained ones while recording and left
    /// no detection in the audio.
    pub unheard_close_passes: Vec<String>,
    #[serde(skip)]
    pub(crate) trk: Vec<(usize, u32)>,
    #[serde(skip)]
    pub(crate) lock_checked: bool,
    /// The score at its fine-grid cell, once refinement has replaced the candidate.
    #[serde(skip)]
    pub(crate) fine_score: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Funnel {
    pub tracks: usize,
    pub events_used: usize,
    pub event_pairs: usize,
    /// Cloud size per event; None = over budget (confirmer only).
    pub hypotheses: Vec<(String, Option<usize>)>,
    pub joined: usize,
    pub quick_passed: usize,
    pub fine_scored: usize,
    pub fine_passed: usize,
    pub truncated: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RefineResult {
    pub lat: f64,
    pub lon: f64,
    /// Recording start at the refined position, profiled with the fixed event weights.
    pub t_start_utc: f64,
    pub radius_m: f64,
    pub radius_basis: String,
    pub step_m: f64,
    pub lats: Vec<f64>,
    pub lons: Vec<f64>,
    /// Delta chi-square relative to the best point (NaN = not scorable).
    pub dchi2: Vec<Vec<f64>>,
    /// The 95 % region reached the edge of the widest window tried.
    pub radius_lower_bound: bool,
    /// Grid nodes inside the 95 % contour; zero means its width is unresolved.
    pub region_cells: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct SearchResult {
    pub funnel: Funnel,
    /// Per-event chance-match calibration.
    pub nulls: Vec<EventNull>,
    pub candidates: Vec<Candidate>,
    pub refine: Option<RefineResult>,
    pub margin: serde_json::Value,
    pub warnings: Vec<String>,
}

/// How many candidates `search.json` keeps. A complete comparison of a busy recording passes
/// hundreds of thousands, over 500 MB of JSON built in memory, and the answer sits at the front.
/// `margin` keeps the rival's score, distance and flights wherever it ranks; past this cap its
/// place and clock are not written.
const DUMP_CANDIDATES: usize = 10_000;

/// What `search.json` holds: a result with only its best candidates, and how many there were.
#[derive(Debug, Serialize)]
pub struct SearchDump<'a> {
    pub funnel: &'a Funnel,
    pub nulls: &'a [EventNull],
    pub candidates_total: usize,
    pub candidates: &'a [Candidate],
    pub refine: &'a Option<RefineResult>,
    pub margin: &'a serde_json::Value,
    pub warnings: &'a [String],
}

impl SearchResult {
    pub fn dump(&self) -> SearchDump<'_> {
        SearchDump {
            funnel: &self.funnel,
            nulls: &self.nulls,
            candidates_total: self.candidates.len(),
            candidates: &self.candidates[..self.candidates.len().min(DUMP_CANDIDATES)],
            refine: &self.refine,
            margin: &self.margin,
            warnings: &self.warnings,
        }
    }

    /// Test the reported gap against a confidence threshold only when no comparison-budget
    /// cut was recorded. An unchecked rival can increase its score, so an incomplete search's
    /// measured gap is only an upper bound. This does not calibrate searching many events.
    pub fn margin_passes(&self, minimum_nats: f64) -> bool {
        if self.candidates.is_empty() || !self.funnel.truncated.is_empty() {
            return false;
        }
        match self.margin.get("margin_nats") {
            Some(serde_json::Value::Null) => true,
            Some(value) => value.as_f64().is_some_and(|margin| margin >= minimum_nats),
            None => false,
        }
    }
}

/// Exact evaluation of an observer point for a set of (event, track) assignments with a joint
/// clock. Returns per-event matches and the joint clock.
struct PointEval {
    t_s: f64,
    chis: Vec<TraceFit>,
    cpas: Vec<crate::track::Cpa>,
    props: Vec<Prop>,
    total: f64,
}

/// How far past its distance and speed gates a hypothesis, or a refined locating match, may sit.
/// The gates bound the search; the trace fit is what scores a match. Refinement profiles the fit
/// without gates, so its minimum can land just past a bound that a nearby grid cell passed.
const LOCATING_GATE_SLACK: f64 = 0.05;

/// Compare one physical CPA in the still-air coordinates of the measured curve.
fn gates_ok(ev: &SearchEvent, cpa: &crate::track::Cpa, prop: &Prop, slack: f64) -> bool {
    if !cpa.interior {
        return false;
    }
    if prop.w == [0.0; 3] {
        // Keep the established calm-air arithmetic and boundary comparisons exactly.
        let lam = prop.c / ev.c_fit;
        return cpa.d >= ev.d_gate[0] * lam * (1.0 - slack)
            && cpa.d <= ev.d_gate[1] * lam * (1.0 + slack)
            && cpa.speed >= ev.v_gate[0] * lam * (1.0 - slack)
            && cpa.speed <= ev.v_gate[1] * lam * (1.0 + slack);
    }
    let Some((speed, distance)) = prop.fit_geometry(cpa.rv, cpa.vel, ev.c_fit) else {
        return false;
    };
    distance >= ev.d_gate[0] * (1.0 - slack)
        && distance <= ev.d_gate[1] * (1.0 + slack)
        && speed >= ev.v_gate[0] * (1.0 - slack)
        && speed <= ev.v_gate[1] * (1.0 + slack)
}

#[allow(clippy::too_many_arguments)]
fn eval_point(
    o: &Obs,
    assign: &[(usize, u32, f64)],
    evs: &[SearchEvent],
    tracks: &[TrackModel],
    p: &SearchParams,
    atmo: &dyn Atmosphere,
    region_ok: bool,
    gates: bool,
    quick: bool,
) -> Option<PointEval> {
    eval_point_weighted(
        o, assign, evs, tracks, p, atmo, region_ok, gates, quick, None,
    )
}

/// Profile the shared recording clock with the same fixed event weights used to compare
/// observer positions. `None` preserves the uncalibrated search objective exactly.
#[allow(clippy::too_many_arguments)]
fn eval_point_weighted(
    o: &Obs,
    assign: &[(usize, u32, f64)],
    evs: &[SearchEvent],
    tracks: &[TrackModel],
    p: &SearchParams,
    atmo: &dyn Atmosphere,
    region_ok: bool,
    gates: bool,
    quick: bool,
    weights: Option<&[f64]>,
) -> Option<PointEval> {
    debug_assert!(weights.is_none_or(|w| w.len() == assign.len()));
    if !region_ok {
        return None;
    }
    let x = o.x;
    let mut cpas = Vec::with_capacity(assign.len());
    let mut props = Vec::with_capacity(assign.len());
    let mut ts_imp = Vec::with_capacity(assign.len());
    for &(ei, ti, te_guess) in assign {
        let ev = &evs[ei];
        let tr = &tracks[ti as usize];
        let cpa = tr.cpa_nearest_time(x, te_guess, 150.0)?;
        let prop = prop_for(atmo, o, tr, cpa.t_e);
        if gates
            && (!gates_ok(ev, &cpa, &prop, if quick { 0.12 } else { 0.0 })
                || tr.gap_at(cpa.t_e) > p.max_gap_s)
        {
            return None;
        }
        let ts = cpa.t_r_fit_p(&prop) - ev.t_rec;
        if !ts.is_finite() {
            return None;
        }
        ts_imp.push(ts);
        cpas.push(cpa);
        props.push(prop);
    }
    // joint clock window: every event's implied start within its tolerance
    let mut lo = f64::NEG_INFINITY;
    let mut hi = f64::INFINITY;
    for (k, &(ei, _, _)) in assign.iter().enumerate() {
        lo = lo.max(ts_imp[k] - evs[ei].t_tol - 1.0);
        hi = hi.min(ts_imp[k] + evs[ei].t_tol + 1.0);
    }
    lo = lo.max(p.start_lo - 1.0);
    hi = hi.min(p.start_hi + 1.0);
    if gates && hi < lo {
        return None;
    }
    if hi < lo {
        let m = ts_imp.iter().sum::<f64>() / ts_imp.len() as f64;
        lo = m - 5.0;
        hi = m + 5.0;
    }
    // ratio curves on a reception-time grid (warm-started emission solves); the quick check uses
    // the coarse 40-point trace subset and a 0.5 s grid
    let gdt = if quick { 0.5 } else { 0.2 };
    let grids: Vec<RatioGrid> = assign
        .iter()
        .enumerate()
        .map(|(k, &(ei, ti, _))| {
            let ev = &evs[ei];
            let tt = if quick { &ev.q_t } else { &ev.tr_t };
            let (a, b) = (tt[0], tt[tt.len() - 1]);
            RatioGrid::build(
                &tracks[ti as usize],
                x,
                &props[k],
                lo + a - 1.0,
                hi + b + 1.0,
                gdt,
                p.max_gap_s,
            )
        })
        .collect();
    let mut scratch = TraceScratch::default();
    let mut chis = Vec::with_capacity(assign.len());
    let cost = |t_s: f64| -> f64 {
        let mut tot = 0.0;
        chis.clear();
        for (k, &(ei, _, _)) in assign.iter().enumerate() {
            let ev = &evs[ei];
            let g = &grids[k];
            let r = if quick {
                trace_chi(
                    &ev.q_t,
                    &ev.q_f,
                    &|tu| g.at(tu),
                    t_s,
                    ev,
                    props[k].source_tone_scale(cpas[k].vel),
                    ev.sigma_hz.max(0.6),
                    p.cap_sigma,
                    false,
                    &mut scratch,
                )
            } else {
                trace_chi(
                    &ev.tr_t,
                    &ev.tr_f,
                    &|tu| g.at(tu),
                    t_s,
                    ev,
                    props[k].source_tone_scale(cpas[k].vel),
                    ev.sigma_hz,
                    p.cap_sigma,
                    true,
                    &mut scratch,
                )
            };
            tot += ev.n_eff * r.chi * weights.map_or(1.0, |w| w[k]);
            chis.push(r);
        }
        tot
    };
    let (bt, total) = profile_clock(lo, hi, quick, cost)?;
    Some(PointEval {
        t_s: bt,
        chis,
        cpas,
        props,
        total,
    })
}

/// Scan the shared recording clock, then polish its best interval. The last objective
/// evaluation is always at the returned clock, so callers retain that clock's trace fits.
fn profile_clock(
    lo: f64,
    hi: f64,
    quick: bool,
    mut cost: impl FnMut(f64) -> f64,
) -> Option<(f64, f64)> {
    let step = if quick { 1.0 } else { 0.25 };
    let n = (((hi - lo) / step).ceil() as usize).clamp(1, 400);
    let (mut bt, mut bc) = (lo, f64::INFINITY);
    for k in 0..=n {
        let ts = lo + (hi - lo) * k as f64 / n as f64;
        let c0 = cost(ts);
        if c0 < bc {
            bc = c0;
            bt = ts;
        }
    }
    if !bc.is_finite() {
        return None;
    }
    if !quick {
        // The scan cap widens its actual spacing on uncertain clocks. Polish the whole
        // neighbouring interval; using the nominal step leaves its best clock unreachable.
        // Narrow windows keep their original bracket and evaluation sequence exactly.
        let polish_half = step.max((hi - lo) / n as f64);
        let (mut a, mut b) = ((bt - polish_half).max(lo), (bt + polish_half).min(hi));
        let gr = 0.618_033_988_749_895;
        let (mut c1, mut c2) = (b - gr * (b - a), a + gr * (b - a));
        let (mut f1, mut f2) = (cost(c1), cost(c2));
        while b - a > 0.01 {
            if f1 < f2 {
                b = c2;
                c2 = c1;
                f2 = f1;
                c1 = b - gr * (b - a);
                f1 = cost(c1);
            } else {
                a = c1;
                c1 = c2;
                f1 = f2;
                c2 = a + gr * (b - a);
                f2 = cost(c2);
            }
        }
        let tm = 0.5 * (a + b);
        if cost(tm) < bc {
            bt = tm;
        }
    }
    Some((bt, cost(bt)))
}

fn engine_str(tr: &TrackModel) -> String {
    tr.engine.as_str().to_string()
}

fn make_match(
    ev: &SearchEvent,
    tr: &TrackModel,
    cpa: &crate::track::Cpa,
    chi: TraceFit,
    prop: &Prop,
    role: &str,
) -> EventMatch {
    let ((_, _, alt), _) = tr.geo_at(cpa.t_e);
    EventMatch {
        event: ev.id.clone(),
        flight: tr.key.clone(),
        icao: tr.icao.clone(),
        reg: tr.reg.clone(),
        typ: tr.typ.clone(),
        engine: engine_str(tr),
        d: cpa.d,
        speed: cpa.speed,
        alt_m: alt,
        t_cpa_heard_utc: cpa.t_r_p(prop),
        c_path: prop.c,
        wind_along: dot(prop.w, cpa.vel) / cpa.speed.max(1.0),
        misfit_event: ev.misfit,
        f0: chi.f0,
        f0_change: chi.brk,
        chi: chi.chi,
        rms_hz: chi.rms,
        coverage: chi.cov,
        role: role.into(),
        dt_free: None,
        nats: 0.0,
        trace_nats: 0.0,
        core: false,
    }
}

/// Fine 2-D grid of observer points around (lat, lon).
fn local_grid(lat: f64, lon: f64, half: f64, step: f64) -> Vec<(f64, f64, f64, f64)> {
    let enu = Enu::new(lat, lon, 0.0);
    let n = (half / step).round() as i64;
    let mut v = Vec::with_capacity(((2 * n + 1) * (2 * n + 1)) as usize);
    for iy in -n..=n {
        for ix in -n..=n {
            let (de, dn) = (ix as f64 * step, iy as f64 * step);
            let (la, lo, _) = enu.to_geodetic([de, dn, 0.0]);
            v.push((la, lo, de, dn));
        }
    }
    v
}

/// All CPAs of `tr` with observer `x` within [ta, tb] (emission time), closer than `dmax`.
fn cpas_in(tr: &TrackModel, x: V3, ta: f64, tb: f64, dmax: f64) -> Vec<crate::track::Cpa> {
    let (a, b) = (ta.max(tr.t0()), tb.min(tr.t1()));
    let mut out = Vec::new();
    if b <= a {
        return out;
    }
    let step = 5.0;
    let n = ((b - a) / step).ceil() as usize;
    let dist = |t: f64| norm(sub(tr.pos_at(t), x));
    let mut prev = (dist(a), dist(a + step.min(b - a)));
    for k in 1..n {
        let t = a + (k + 1) as f64 * step;
        let dk = dist(t.min(b));
        if prev.1 <= prev.0
            && prev.1 <= dk
            && prev.1 < dmax + 2000.0
            && let Some(cpa) = tr.cpa_near(x, a + k as f64 * step, 2.0 * step)
            && cpa.d <= dmax
            && !out
                .iter()
                .any(|o: &crate::track::Cpa| (o.t_e - cpa.t_e).abs() < 30.0)
        {
            out.push(cpa);
        }
        prev = (prev.1, dk);
    }
    out
}

/// Chance-match calibration of one event: how surprising a match is.
#[derive(Clone, Debug, Default, Serialize)]
pub struct EventNull {
    pub event: String,
    /// Hypotheses (flight, place, clock) consistent with the event's gates over the region.
    pub cloud: usize,
    /// -ln P(a random place and clock in the search space passes the event's gates with some
    /// flight), nats.
    pub gate_nats: f64,
    /// Trace chi of random gate-passing (flight, place, clock) samples (each with its own best
    /// clock and the best of 9 nearby places), sorted.
    #[serde(skip)]
    pub chi_null: Vec<f64>,
    pub n_null: usize,
    pub chi_null_p05: f64,
    pub chi_null_median: f64,
}

/// How far the fitted lower tail may reach past the empirical null's own resolution, ln(n+1):
/// ln(1000), three orders of magnitude. The log-normal is fitted to two quantiles only. When the
/// median and the 5 % quantile sit close together its spread is tiny, and a chance match a few
/// times below every sample was given the full 25 nats: on a development recording's decoy clock,
/// one chance flight at chi 1.05 against a null with median 8.88 and 5 % quantile 6.56 (n 315)
/// out-scored the true clock, and a wrong-date run carried 21.7 to 24.2 such nats on every build.
pub const NULL_TAIL_EXTRA_NATS: f64 = 6.907_755_278_982_137;

impl EventNull {
    /// -ln P(chance chi <= chi | gates), from the empirical null (bounded by ln(n+1)).
    /// Below the 5 % quantile the empirical CDF is too coarse (a match far better than every
    /// chance sample would cap at ln(n+1)), so the lower tail is log-normal, fitted to the null's
    /// median and 5 % quantile, and reaches at most `NULL_TAIL_EXTRA_NATS` past ln(n+1).
    pub fn trace_nats(&self, chi: f64) -> f64 {
        let n = self.chi_null.len();
        if n < 20 || !chi.is_finite() {
            return 0.0;
        }
        let k = self.chi_null.partition_point(|&c| c <= chi);
        let emp = (k as f64 + 0.5) / (n as f64 + 1.0);
        let (med, p05) = (self.chi_null_median.max(1e-6), self.chi_null_p05.max(1e-6));
        let p = if chi < p05 && med > p05 {
            let (mu, sd) = (med.ln(), (med.ln() - p05.ln()) / 1.6449);
            normal_cdf((chi.max(1e-9).ln() - mu) / sd).min(emp)
        } else {
            emp
        };
        let reach = ((n + 1) as f64).ln() + NULL_TAIL_EXTRA_NATS;
        (-p.max(1e-300).ln()).min(reach).min(25.0)
    }
    pub fn nats(&self, chi: f64) -> f64 {
        self.gate_nats + self.trace_nats(chi)
    }
}

#[allow(clippy::too_many_arguments)]
fn calibrate(
    ev: &SearchEvent,
    set: &HypSet,
    tracks: &[TrackModel],
    p: &SearchParams,
    frame: &Frame,
    dem: &dyn Elevation,
    atmo: &dyn Atmosphere,
    t_base: f64,
) -> EventNull {
    let area = (p.region.area_km2() * 1e6).max(1e6);
    let t_span = (p.start_hi - p.start_lo).max(60.0) + 2.0 * ev.t_tol;
    let cell = p.along_step_m * p.cross_step_m * 2.0 * (ev.t_tol + 1.0);
    let p_gate = ((set.count.max(1)) as f64 * cell / (area * t_span)).min(1.0);
    let last = ev.tr_t.len() - 1;
    let w = ev.t_tol + 1.0;
    let off = 0.5 * p.cross_step_m;
    let mut chi_null: Vec<f64> = set
        .sample
        .par_iter()
        .filter_map(|h| {
            let (la0, lo0) = frame.latlon(h.e as f64, h.n as f64);
            let enu = Enu::new(la0, lo0, 0.0);
            let tr = &tracks[h.trk as usize];
            let mut best = f64::INFINITY;
            let mut scratch = TraceScratch::default();
            for (de, dn) in [
                (0.0, 0.0),
                (off, 0.0),
                (-off, 0.0),
                (0.0, off),
                (0.0, -off),
                (off, off),
                (off, -off),
                (-off, off),
                (-off, -off),
            ] {
                let (la, lo, _) = enu.to_geodetic([de, dn, 0.0]);
                let o = observer(la, lo, dem);
                let Some(cpa) = tr.cpa_nearest_time(o.x, h.t_e as f64 + t_base, 60.0) else {
                    continue;
                };
                let prop = prop_for(atmo, &o, tr, cpa.t_e);
                if !gates_ok(ev, &cpa, &prop, LOCATING_GATE_SLACK) {
                    continue;
                }
                let ts0 = cpa.t_r_fit_p(&prop) - ev.t_rec;
                if !ts0.is_finite() {
                    continue;
                }
                let g = RatioGrid::build(
                    tr,
                    o.x,
                    &prop,
                    ts0 - w + ev.tr_t[0] - 1.0,
                    ts0 + w + ev.tr_t[last] + 1.0,
                    0.2,
                    p.max_gap_s,
                );
                let mut ts = ts0 - w;
                while ts <= ts0 + w {
                    let r = trace_chi(
                        &ev.tr_t,
                        &ev.tr_f,
                        &|tu| g.at(tu),
                        ts,
                        ev,
                        prop.source_tone_scale(cpa.vel),
                        ev.sigma_hz,
                        p.cap_sigma,
                        true,
                        &mut scratch,
                    );
                    if r.cov >= 0.6 {
                        best = best.min(r.chi);
                    }
                    ts += 0.25;
                }
            }
            best.is_finite().then_some(best)
        })
        .collect();
    chi_null.sort_by(|a, b| a.total_cmp(b));
    let q = |f: f64| {
        if chi_null.is_empty() {
            f64::NAN
        } else {
            chi_null[((chi_null.len() - 1) as f64 * f) as usize]
        }
    };
    EventNull {
        event: ev.id.clone(),
        cloud: set.count,
        gate_nats: -p_gate.ln(),
        n_null: chi_null.len(),
        chi_null_p05: q(0.05),
        chi_null_median: q(0.5),
        chi_null,
    }
}

pub struct SearchInputs<'a> {
    pub events: Vec<SearchEvent>,
    pub tracks: &'a [TrackModel],
    /// Recording-clock times of every detection (any strength): used to decide whether a close
    /// pass would have gone unheard.
    pub heard_times: Vec<f64>,
    pub coarse_dem: &'a dyn Elevation,
    pub fine_dem: &'a dyn Elevation,
    pub atmo: &'a dyn Atmosphere,
}

/// Score weight of an event: a fit flagged as model misfit (RPM change, contamination)
/// constrains less and is easier to match by chance.
fn ev_weight(ev: &SearchEvent) -> f64 {
    if ev.misfit { 0.5 } else { 1.0 }
}

/// Track indices sorted by start time, with conservative spatial bounds built once for
/// the immutable catalogue. A possible box hit still needs the unchanged exact CPA checks.
struct TimeIndex {
    by_t0: Vec<(f64, f64, u32)>,
    max_len: f64,
    bounds: Vec<Option<TrackBounds>>,
}

impl TimeIndex {
    fn new(tracks: &[TrackModel]) -> Self {
        let mut by_t0: Vec<(f64, f64, u32)> = tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.t0(), t.t1(), i as u32))
            .collect();
        by_t0.sort_by(|a, b| a.0.total_cmp(&b.0));
        let max_len = by_t0.iter().map(|t| t.1 - t.0).fold(0.0, f64::max);
        Self {
            by_t0,
            max_len,
            bounds: tracks.iter().map(TrackModel::bounds).collect(),
        }
    }
    /// Tracks overlapping [a, b].
    fn overlapping(&self, a: f64, b: f64) -> impl Iterator<Item = u32> + '_ {
        let lo = self.by_t0.partition_point(|t| t.0 < a - self.max_len);
        let hi = self.by_t0.partition_point(|t| t.0 <= b);
        self.by_t0[lo..hi]
            .iter()
            .filter(move |t| t.1 >= a)
            .map(|t| t.2)
    }

    /// Time-overlapping tracks whose complete interpolated path might reach the observer.
    /// Keep the original order, and fail open when a track has no valid spatial bound.
    fn overlapping_near(
        &self,
        a: f64,
        b: f64,
        x: V3,
        reach: f64,
    ) -> impl Iterator<Item = u32> + '_ {
        self.overlapping(a, b).filter(move |&ti| {
            self.bounds[ti as usize].is_none_or(|bounds| bounds.may_reach(x, reach))
        })
    }
}

/// Best explanation of event `ev` by any track at observer `o` with the recording clock fixed.
#[allow(clippy::too_many_arguments)]
fn best_confirmer(
    o: &Obs,
    t_start: f64,
    ev: &SearchEvent,
    tracks: &[TrackModel],
    tix: &TimeIndex,
    p: &SearchParams,
    atmo: &dyn Atmosphere,
    exclude: &[(u32, f64)],
) -> Option<(f64, EventMatch, u32)> {
    let c = p.c;
    let t_heard = t_start + ev.t_rec;
    // A still-air fitted distance is not a physical radius in a moving or varying medium.
    // Keep that shortcut only when its scale is exactly one; otherwise the existing exact
    // CPA gates below decide the range.
    let ordinary_still_air = matching_still_air(ev, p, atmo);
    let reach = if ordinary_still_air {
        1.15 * ev.d_gate[1]
    } else {
        f64::INFINITY
    };
    // Fitting can leave a wide CPA-time profile even when its best curve passed measurement.
    // The exact timing gate permits that interval, so neither preliminary time query may
    // discard it. Preserve the established calm windows. In moving or undeclared media,
    // a fitted centre supplies no bound on geometric emission time; examine every retained
    // visit and let the same exact timing, geometry and trace gates decide.
    let earliest = t_heard - 1.2 * ev.d_gate[1] / c - ev.t_tol - 3.0;
    let latest = t_heard + ev.t_tol + 3.0;
    let (query_lo, query_hi) = if ordinary_still_air {
        (
            (t_heard - 1.2 * ev.d_gate[1] / c - 120.0).min(earliest),
            (t_heard + 30.0).max(latest),
        )
    } else {
        (f64::NEG_INFINITY, f64::INFINITY)
    };
    let mut best: Option<(f64, EventMatch, u32)> = None;
    let mut scratch = TraceScratch::default();
    for ti in tix.overlapping_near(query_lo, query_hi, o.x, reach) {
        let tr = &tracks[ti as usize];
        if p.tonal_only && !tr.engine.tonal() {
            continue;
        }
        // Test every visit inside the permitted window: the closest pass may be at the wrong
        // recording time while a farther return explains this event. Keep one second of scan
        // support outside the new bounds because local_cpas rejects boundary minima (0.5 s).
        let guess = t_heard - 1.1 * ev.d_gate[1].min(12_000.0) / c * 0.5;
        let (scan_lo, scan_hi) = if ordinary_still_air {
            (
                (guess - 200.0).min(earliest - 1.0),
                (guess + 200.0).max(latest + 1.0),
            )
        } else {
            (tr.t0(), tr.t1())
        };
        for cpa in tr.local_cpas(o.x, scan_lo, scan_hi) {
            if cpa.d > reach {
                continue;
            }
            let prop = prop_for(atmo, o, tr, cpa.t_e);
            let fitted_time = cpa.t_r_fit_p(&prop);
            if !gates_ok(ev, &cpa, &prop, 0.0)
                || !fitted_time.is_finite()
                || (fitted_time - t_heard).abs() > ev.t_tol + 3.0
                || tr.gap_at(cpa.t_e) > p.max_gap_s
            {
                continue;
            }
            // A returning aircraft can explain another event, but one physical pass cannot.
            // Recover each excluded pass from its own emission-time seed, not the nearest
            // return in range. A missing prior CPA conservatively excludes that track.
            if exclude.iter().any(|&(used_track, t_e_guess)| {
                used_track == ti
                    && tr
                        .cpa_nearest_time(o.x, t_e_guess, SAME_FLIGHT_MIN_DT_S)
                        .is_none_or(|used| {
                            !used.interior || (cpa.t_e - used.t_e).abs() < SAME_FLIGHT_MIN_DT_S
                        })
            }) {
                continue;
            }
            let lo = t_start + ev.tr_t[0] - 1.0;
            let hi = t_start + ev.tr_t[ev.tr_t.len() - 1] + 1.0;
            let g = RatioGrid::build(tr, o.x, &prop, lo, hi, 0.2, p.max_gap_s);
            let chi = trace_chi(
                &ev.tr_t,
                &ev.tr_f,
                &|tu| g.at(tu),
                t_start,
                ev,
                prop.source_tone_scale(cpa.vel),
                ev.sigma_hz,
                p.cap_sigma,
                true,
                &mut scratch,
            );
            if chi.chi <= p.kappa && chi.cov >= 0.6 && best.as_ref().is_none_or(|b| chi.chi < b.0) {
                best = Some((
                    chi.chi,
                    make_match(ev, tr, &cpa, chi, &prop, "confirmer"),
                    ti,
                ));
            }
        }
    }
    best
}

/// Evidence of a candidate (nats): sum over explained events of the chance-match surprise
/// (gates + trace), minus `UNHEARD_NATS` per unheard close pass. Also sets each match's nats.
fn evidence(events: &mut [EventMatch], nulls: &[EventNull], unheard: usize) -> f64 {
    let mut s = 0.0;
    for m in events.iter_mut() {
        let null = nulls.iter().find(|n| n.event == m.event);
        let (n, t) = null.map_or((0.0, 0.0), |n| (n.nats(m.chi), n.trace_nats(m.chi)));
        // too few chance matches to calibrate against (tiny region): demand a near-noise fit
        let thin = null.is_none_or(|n| n.chi_null.len() < 20);
        m.nats = n;
        m.trace_nats = t;
        m.core = m.chi <= CORE_CHI
            && m.dt_free.is_some_and(|d| d.abs() <= CORE_LOCK_S)
            && (t >= CORE_TRACE_NATS || (thin && m.chi <= 2.0));
        s += n;
    }
    s - UNHEARD_NATS * unheard as f64
}

/// Log-likelihood cost of a much-closer tonal pass that left no detection.
const UNHEARD_NATS: f64 = 2.3;
/// Core-match criteria (see [`EventMatch::core`]).
const CORE_CHI: f64 = 4.0;
const CORE_LOCK_S: f64 = 4.0;
const CORE_TRACE_NATS: f64 = 2.0;

/// How far an event's own best clock may sit from the joint clock before the match counts as a
/// coincidence: its timing tolerance, within [4, 6] s. True matches observed so far lock within
/// 3 s even with a mid-pass RPM change; coincidences sat 8-12 s off.
pub fn lock_tol(ev: &SearchEvent) -> f64 {
    ev.t_tol.clamp(4.0, 6.0)
}

/// Standard normal CDF (Abramowitz-Stegun 7.1.26 erfc, |error| < 1.5e-7; exp-asymptotic tail).
fn normal_cdf(z: f64) -> f64 {
    if z < -8.0 {
        // Mills ratio
        let x = -z;
        return (-0.5 * x * x).exp() / (x * (2.0 * std::f64::consts::PI).sqrt());
    }
    let x = z.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erfc = poly * (-x * x).exp();
    if z >= 0.0 {
        1.0 - 0.5 * erfc
    } else {
        0.5 * erfc
    }
}

fn fit_score(events: &[EventMatch], evs: &[SearchEvent], kappa: f64) -> f64 {
    events
        .iter()
        .map(|m| {
            let w = evs.iter().find(|e| e.id == m.event).map_or(1.0, ev_weight);
            w * (1.0 - m.chi / kappa).max(0.0)
        })
        .sum()
}

/// A shared CPA and tone ratio suggest harmonics, but distinct measured Doppler shapes can
/// establish two independent flyovers. Compare at the same recording times; fitted CPA and
/// f0 estimates can differ between harmonics with different trace coverage.
fn harmonic_events(a: &SearchEvent, b: &SearchEvent, cap_sigma: f64) -> bool {
    if (a.t_rec - b.t_rec).abs() >= 8.0
        || !crate::physics::harmonic_frequencies(a.f0, b.f0, 8, 0.03, true)
    {
        return false;
    }
    harmonic_trace_shape(a, b, cap_sigma)
}

/// Compare observed ridges, without using fitted CPA/frequency estimates as a family screen.
/// A known survey sibling must pass this comparison before it supplies another aircraft:
/// different trace coverage can move its fitted parameters without changing the shared sound.
/// Missing two-sided overlap cannot establish independent motion.
fn harmonic_trace_shape(a: &SearchEvent, b: &SearchEvent, cap_sigma: f64) -> bool {
    let (low, high) = if a.f0 <= b.f0 { (a, b) } else { (b, a) };
    let n = low.tr_t.len().min(low.tr_f.len());
    if n < 2 {
        return true;
    }
    let mut points = Vec::new();
    let mut j = 0;
    for (&t, &f) in high.tr_t.iter().zip(&high.tr_f) {
        if t < low.tr_t[0] || t > low.tr_t[n - 1] || !f.is_finite() || f <= 0.0 {
            continue;
        }
        while j + 1 < n - 1 && low.tr_t[j + 1] < t {
            j += 1;
        }
        let dt = low.tr_t[j + 1] - low.tr_t[j];
        let (fl, fr) = (low.tr_f[j], low.tr_f[j + 1]);
        if dt <= 0.0 || !(fl > 0.0 && fr > 0.0 && fl.is_finite() && fr.is_finite()) {
            continue;
        }
        let w = (t - low.tr_t[j]) / dt;
        let fm = fl * (1.0 - w) + fr * w;
        // The full bracket span is an interpolation allowance: sparse or unequal sampling
        // must not make one curved ridge appear to be two independent flight constraints.
        let (flo, fhi) = if w == 0.0 || w == 1.0 {
            (fm, fm)
        } else {
            (fl.min(fr), fl.max(fr))
        };
        points.push((t, fm, f, flo, fhi));
    }
    if points.len() < 8
        || !points.iter().any(|p| p.0 < a.t_rec.min(b.t_rec))
        || !points.iter().any(|p| p.0 > a.t_rec.max(b.t_rec))
    {
        // No two-sided comparison means no evidence that these are different aircraft.
        return true;
    }
    let mut ratios: Vec<f64> = points.iter().map(|p| p.2 / p.1).collect();
    let mut ratio = crate::spectro::median_f64(&mut ratios);
    let sigma2 = |r: f64| high.sigma_hz.powi(2) + (r * low.sigma_hz).powi(2);
    let s2 = sigma2(ratio);
    if !s2.is_finite() || s2 <= 0.0 {
        return true;
    }
    // Robust constant-ratio fit, allowing the same bounded stray-point influence used by
    // trace scoring. Fit the scale from observations, independently of fitted f0 estimates.
    let (mut xy, mut xx) = (0.0, 0.0);
    for &(_, fm, f, flo, fhi) in &points {
        if (f - ratio * fm).abs() <= cap_sigma * s2.sqrt() + ratio * (fhi - flo) {
            xy += fm * f;
            xx += fm * fm;
        }
    }
    if xx > 0.0 {
        ratio = xy / xx;
    }
    let s2 = sigma2(ratio);
    let chi = points
        .iter()
        .map(|&(_, _, f, flo, fhi)| {
            let residual = (ratio * flo - f).max(f - ratio * fhi).max(0.0);
            (residual * residual / s2).min(cap_sigma * cap_sigma)
        })
        .sum::<f64>()
        / points.len() as f64;
    chi <= CORE_CHI
}

/// Positive two-sided observed-ridge evidence that a candidate moves differently from a
/// measured family member, even when that member's fit was refused. The existing family
/// comparison treats absent overlap or unusable noise estimates as no such evidence.
pub(crate) fn distinct_measured_motion(a: &MeasuredEvent, b: &MeasuredEvent) -> bool {
    let usable = |m: &MeasuredEvent| {
        m.fit.as_ref().is_some_and(|f| {
            f.best.f0.is_finite()
                && f.best.f0 > 0.0
                && f.best.t0r.is_finite()
                && f.best.rmse.is_finite()
        })
    };
    if !usable(a) || !usable(b) {
        return false;
    }
    match (
        SearchEvent::from_observed_trace(a, 0.0),
        SearchEvent::from_observed_trace(b, 0.0),
    ) {
        (Some(a), Some(b)) => !harmonic_trace_shape(&a, &b, DEFAULT_CAP_SIGMA),
        _ => false,
    }
}

/// Quality-passing, aircraft-like measured events under the standard search's independent-pass
/// policy. Use the same selection when counting measurement slots and supplying the final search:
/// multiple seeds or harmonics of one pass must not hide a later independent flyover.
/// Known survey siblings require distinct observed ridges even when fitted CPA/f0 estimates drift.
///
/// Wind widens the speed gates but does not change eligibility, ordering or harmonic grouping.
/// This returns every selected event; a caller with a measurement cap must apply it afterwards.
pub fn independent_measured_events(events: &[MeasuredEvent], wind_mps: f64) -> Vec<SearchEvent> {
    let eligible = events
        .iter()
        .filter(|e| e.is_aircraft_like())
        .filter_map(|e| SearchEvent::from_measured(e, wind_mps))
        .collect();
    if !events.iter().any(|e| e.detection.harmonic_of.is_some()) {
        return independent_events(eligible, DEFAULT_CAP_SIGMA);
    }
    let families = events
        .iter()
        .map(|e| {
            (
                e.id.as_str(),
                vec![(e.detection.id, e.detection.harmonic_of)],
            )
        })
        .collect();
    independent_events_with_families(eligible, families, DEFAULT_CAP_SIGMA)
}

fn independent_events_with_families(
    evs: Vec<SearchEvent>,
    mut families: HashMap<&str, Vec<(usize, Option<usize>)>>,
    cap_sigma: f64,
) -> Vec<SearchEvent> {
    // Keep the ordinary quality winner and the family hints of each duplicate it explains.
    // Otherwise a better-fitting alternate seed can erase the only link to a known sibling.
    let mut representatives = independent_events_by(evs, |a, b| {
        if !harmonic_events(a, b, cap_sigma) {
            return false;
        }
        if let Some(members) = families.remove(b.id.as_str())
            && let Some(kept) = families.get_mut(a.id.as_str())
        {
            kept.extend(members);
        }
        true
    });
    // The inherited family hint only permits comparing the representatives' actual ridges.
    // It never unions all members of a survey family or overrides distinct observed motion.
    loop {
        let before = representatives.len();
        representatives = independent_events_by(representatives, |a, b| {
            let same_family = match (families.get(a.id.as_str()), families.get(b.id.as_str())) {
                (Some(a), Some(b)) => a.iter().any(|&(a, parent_a)| {
                    b.iter().any(|&(b, parent_b)| {
                        (parent_a.is_some() || parent_b.is_some())
                            && parent_a.unwrap_or(a) == parent_b.unwrap_or(b)
                    })
                }),
                _ => false,
            };
            if !same_family || !harmonic_trace_shape(a, b, cap_sigma) {
                return false;
            }
            if let Some(members) = families.remove(b.id.as_str())
                && let Some(kept) = families.get_mut(a.id.as_str())
            {
                kept.extend(members);
            }
            true
        });
        if representatives.len() == before {
            return representatives;
        }
        // A later parent can connect two earlier, better-fitting siblings. Recheck only
        // after a removal; every further merge still requires the representatives' ridges
        // to agree, and the count strictly decreases until the grouping stabilizes.
    }
}

fn independent_events(evs: Vec<SearchEvent>, cap_sigma: f64) -> Vec<SearchEvent> {
    independent_events_by(evs, |a, b| harmonic_events(a, b, cap_sigma))
}

fn independent_events_by(
    mut evs: Vec<SearchEvent>,
    mut same_event: impl FnMut(&SearchEvent, &SearchEvent) -> bool,
) -> Vec<SearchEvent> {
    // events: clean fits first, then best quality; drop harmonics of a stronger event
    evs.sort_by(|a, b| {
        a.misfit
            .cmp(&b.misfit)
            .then(b.quality.total_cmp(&a.quality))
    });
    let mut keep: Vec<SearchEvent> = Vec::new();
    for e in evs {
        if keep.iter().any(|k| same_event(k, &e)) {
            continue;
        }
        keep.push(e);
    }
    keep
}

pub fn run(inp: &SearchInputs, p: &SearchParams, progress: &(dyn Fn(&str) + Sync)) -> SearchResult {
    let c = p.c;
    let frame = Frame::new(&p.region);
    let mut funnel = Funnel {
        tracks: inp.tracks.len(),
        ..Default::default()
    };
    let mut warnings = Vec::new();
    let tracks = inp.tracks;
    let atmo = inp.atmo;
    let evs = independent_events(inp.events.clone(), p.cap_sigma);
    funnel.events_used = evs.len();
    if evs.len() < 2 {
        warnings.push(format!(
            "only {} usable event(s): a single flyover constrains the observer to a line, not a point; record longer or include more flyovers",
            evs.len()
        ));
        return SearchResult {
            funnel,
            nulls: Vec::new(),
            candidates: Vec::new(),
            refine: None,
            margin: serde_json::json!({}),
            warnings,
        };
    }
    // hypotheses per event (clean events first, so they win the budget); an event whose cloud
    // exceeds the cap constrains too little to drive the join and is used only as a confirmer
    let t_base = p.start_lo - 86_400.0;
    let mut hyps: Vec<Option<Vec<Hyp>>> = Vec::with_capacity(evs.len());
    let mut nulls: Vec<EventNull> = Vec::with_capacity(evs.len());
    let mut total = 0usize;
    for e in &evs {
        // An empty cloud cannot seed a join. Let a later event with eligible flights use
        // that driver slot instead, while keeping the empty cloud in the reported funnel.
        let full = hyps.iter().flatten().filter(|h| !h.is_empty()).count() >= p.max_events;
        let cap = if full {
            0
        } else {
            p.max_hyp_per_event
                .min(p.max_hyp_total.saturating_sub(total))
        };
        let set = hypotheses(e, tracks, p, &frame, inp.coarse_dem, atmo, t_base, cap);
        let null = calibrate(e, &set, tracks, p, &frame, inp.coarse_dem, atmo, t_base);
        progress(&format!(
            "hypotheses {}{}: {}{}; chance match: gates {:.1} nats, null chi p05 {:.2} median {:.2} (n {})",
            e.id,
            if e.misfit { " (misfit)" } else { "" },
            set.count,
            if set.stored.is_none() {
                " (confirmer only)"
            } else {
                ""
            },
            null.gate_nats,
            null.chi_null_p05,
            null.chi_null_median,
            null.n_null
        ));
        if let Some(v) = &set.stored {
            total += v.len();
        } else if !full {
            // beyond --max-events is documented; a budget demotion must not be silent
            warnings.push(format!(
                "{}: {} hypotheses did not fit the hypothesis budget ({cap} left), so it only confirms; narrow the region or dates to let it drive the search",
                e.id, set.count
            ));
        }
        hyps.push(set.stored);
        nulls.push(null);
    }
    funnel.hypotheses = evs
        .iter()
        .zip(&hyps)
        .map(|(e, h)| (e.id.clone(), h.as_ref().map(|v| v.len())))
        .collect();
    let pairable: Vec<usize> = (0..evs.len())
        .filter(|&i| hyps[i].as_ref().is_some_and(|h| !h.is_empty()))
        .collect();
    // pair joins: clean-clean first, then by expected join size
    let rho = 0.5 * p.along_step_m.hypot(p.cross_step_m);
    let r_join = 2.0 * rho + 120.0;
    let slop = p.along_dt_max + p.cross_step_m / c + 2.0;
    let mut quick: Vec<(Joined, f64)> = Vec::new();
    let done = AtomicUsize::new(0);
    let mut pairs = Vec::new();
    for (x, &i) in pairable.iter().enumerate() {
        for &j in &pairable[x + 1..] {
            let (ni, nj) = (
                hyps[i].as_ref().map_or(0, |v| v.len()),
                hyps[j].as_ref().map_or(0, |v| v.len()),
            );
            let nmis = evs[i].misfit as u8 + evs[j].misfit as u8;
            pairs.push((nmis, ni as f64 * nj as f64, i, j));
        }
    }
    pairs.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    funnel.event_pairs = pairs.len();
    let mut pairs_done = 0usize;
    for &(_, _, i, j) in &pairs {
        let (hi, hj) = (hyps[i].as_ref().unwrap(), hyps[j].as_ref().unwrap());
        let (a, b, ha, hb) = if hi.len() <= hj.len() {
            (i, j, hi, hj)
        } else {
            (j, i, hj, hi)
        };
        let tt = evs[i].t_tol + evs[j].t_tol + slop;
        let joined = join_batches(
            ha,
            hb,
            a,
            b,
            r_join,
            tt,
            SAME_FLIGHT_MIN_DT_S,
            t_base,
            |batch| {
                funnel.joined += batch.len();
                // Only quick survivors outlive this output batch. The join still visits
                // the complete event pair, and emits the original representative order.
                let checked: Vec<(Joined, f64)> = batch
                    .par_iter()
                    .filter_map(|j| {
                        let k = done.fetch_add(1, Ordering::Relaxed);
                        if k.is_multiple_of(500_000) && k > 0 {
                            progress(&format!("quick check {k}/{} joined so far", funnel.joined));
                        }
                        let (la, lo) = frame.latlon(j.e, j.n);
                        if !p.region.contains(la, lo) {
                            return None;
                        }
                        let o = observer(la, lo, inp.coarse_dem);
                        let assign = [(j.ei, j.ti, j.te_i), (j.ej, j.tj, j.te_j)];
                        let pe = eval_point(&o, &assign, &evs, tracks, p, atmo, true, true, true)?;
                        let worst = pe.chis.iter().map(|c| c.chi).fold(0.0, f64::max);
                        let qs: f64 = pe
                            .chis
                            .iter()
                            .zip(&assign)
                            .map(|(c, a)| {
                                ev_weight(&evs[a.0]) * (1.0 - c.chi / p.quick_chi).max(0.0)
                            })
                            .sum();
                        (worst <= p.quick_chi).then_some((*j, qs))
                    })
                    .collect();
                quick.extend(checked);
            },
        );
        progress(&format!("join {}x{}: {}", evs[a].id, evs[b].id, joined));
        pairs_done += 1;
        // The budget counts every joined seed, including rejected ones, and still stops
        // only after a complete event pair. Streaming must not grant extra comparisons.
        if funnel.joined > p.max_quick {
            break;
        }
    }
    // The join is the last user of these potentially multi-gigabyte clouds. They must
    // not remain live while the fine search, confirmation and refinement allocate.
    drop(hyps);
    if pairs_done < pairs.len() {
        warnings.push(format!("joined {pairs_done} of {} event pairs before the quick-check budget ({}); narrow the region or dates", pairs.len(), p.max_quick));
        funnel
            .truncated
            .push(format!("pairs: {} -> {pairs_done}", pairs.len()));
    }
    funnel.quick_passed = quick.len();
    progress(&format!("quick check passed: {}", quick.len()));
    let mut fine_in = ranked_fine_seeds(quick);
    if fine_in.len() > p.max_fine {
        funnel
            .truncated
            .push(format!("fine: {} -> {}", fine_in.len(), p.max_fine));
        warnings.push(format!(
            "{} quick-check survivors > {}; scoring the best {}; narrow the region or dates",
            fine_in.len(),
            p.max_fine,
            p.max_fine
        ));
        fine_in.truncate(p.max_fine);
    }
    funnel.fine_scored = fine_in.len();
    // fine grid + full scoring
    let done = AtomicUsize::new(0);
    let fine: Vec<Candidate> = fine_in
        .par_iter()
        .filter_map(|(j, _)| {
            let k = done.fetch_add(1, Ordering::Relaxed);
            if k.is_multiple_of(20_000) && k > 0 {
                progress(&format!("fine grid {k}/{}", fine_in.len()));
            }
            let (la, lo) = frame.latlon(j.e, j.n);
            let assign = [(j.ei, j.ti, j.te_i), (j.ej, j.tj, j.te_j)];
            let mut best: Option<(Obs, PointEval)> = None;
            for (gla, glo, _, _) in local_grid(la, lo, p.fine_half_m, p.fine_step_m) {
                let o = observer(gla, glo, inp.fine_dem);
                if let Some(pe) = eval_point(
                    &o,
                    &assign,
                    &evs,
                    tracks,
                    p,
                    atmo,
                    p.region.contains(gla, glo),
                    true,
                    false,
                ) && best.as_ref().is_none_or(|b| pe.total < b.1.total)
                {
                    best = Some((o, pe));
                }
            }
            let (o, pe) = best?;
            // NaN chi fails too
            if pe
                .chis
                .iter()
                .any(|c| c.chi.is_nan() || c.chi > p.kappa || c.cov < 0.6)
            {
                return None;
            }
            let mut events: Vec<EventMatch> = assign
                .iter()
                .enumerate()
                .map(|(k, &(ei, ti, _))| {
                    make_match(
                        &evs[ei],
                        &tracks[ti as usize],
                        &pe.cpas[k],
                        pe.chis[k],
                        &pe.props[k],
                        "pair",
                    )
                })
                .collect();
            let score = evidence(&mut events, &nulls, 0);
            Some(Candidate {
                lat: o.lat,
                lon: o.lon,
                elev_m: o.z,
                t_start_utc: pe.t_s,
                n_explained: 2,
                score,
                fit_score: fit_score(&events, &evs, p.kappa),
                pair_chi: pe.total / (evs[j.ei].n_eff + evs[j.ej].n_eff),
                unheard_close_passes: Vec::new(),
                trk: vec![(j.ei, j.ti), (j.ej, j.tj)],
                lock_checked: false,
                fine_score: None,
                events,
            })
        })
        .collect();
    // Only the fine survivors are needed from here on.
    drop(fine_in);
    funnel.fine_passed = fine.len();
    progress(&format!("fine survivors: {}", fine.len()));
    let mut fine = fine;
    fine.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.pair_chi.total_cmp(&b.pair_chi))
    });
    let mut cands = dedupe(fine);
    // confirmers + unheard close passes for the leading candidates
    let tix = TimeIndex::new(tracks);
    let n_conf = cands.len().min(p.max_confirm);
    if n_conf < cands.len() {
        funnel
            .truncated
            .push(format!("confirmers: {} -> {n_conf}", cands.len()));
        warnings.push(format!(
            "confirmation budget: checking {n_conf} of {} fine candidates; rival comparison is incomplete",
            cands.len()
        ));
    }
    let heard = &inp.heard_times;
    let cdone = AtomicUsize::new(0);
    cands[..n_conf].par_iter_mut().for_each(|cand| {
        let k = cdone.fetch_add(1, Ordering::Relaxed);
        if k.is_multiple_of(500) && k > 0 {
            progress(&format!("confirmers {k}/{n_conf}"));
        }
        let o = observer(cand.lat, cand.lon, inp.fine_dem);
        let used: Vec<usize> = cand.trk.iter().map(|t| t.0).collect();
        for (ek, ev) in evs.iter().enumerate() {
            if used.contains(&ek) {
                continue;
            }
            let excl: Vec<(u32, f64)> = cand
                .trk
                .iter()
                .zip(&cand.events)
                .map(|(&(_, ti), m)| (ti, m.t_cpa_heard_utc - m.d / m.c_path))
                .collect();
            if let Some((_, m, ti)) =
                best_confirmer(&o, cand.t_start_utc, ev, tracks, &tix, p, atmo, &excl)
            {
                cand.events.push(m);
                cand.trk.push((ek, ti));
            }
        }
        cand.n_explained = cand.events.len();
        cand.fit_score = fit_score(&cand.events, &evs, p.kappa);
        cand.unheard_close_passes = unheard_passes(cand, &o, tracks, &tix, heard, p, atmo);
        let nu = cand.unheard_close_passes.len();
        cand.score = evidence(&mut cand.events, &nulls, nu);
    });
    cands.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.pair_chi.total_cmp(&b.pair_chi))
    });
    // A coincidence can lose a match at the clock check. Remove candidates that no longer
    // explain two distinct events and check every newly promoted leader before choosing it.
    let check_candidate = |cand: &mut Candidate| {
        lock_prune(cand, &evs, tracks, p, atmo, inp.fine_dem);
        let o = observer(cand.lat, cand.lon, inp.fine_dem);
        cand.fit_score = fit_score(&cand.events, &evs, p.kappa);
        cand.unheard_close_passes = unheard_passes(cand, &o, tracks, &tix, heard, p, atmo);
        let nu = cand.unheard_close_passes.len();
        cand.score = evidence(&mut cand.events, &nulls, nu);
    };
    cands = finalize_candidates(cands, &check_candidate);
    let (cands, refine) = refine_candidates(cands, &check_candidate, |cand| {
        let r = refine_winner(cand, &evs, tracks, p, inp.fine_dem, atmo)?;
        match refresh_refined_candidate(cand, &r, inp, &evs, p, &nulls, &tix) {
            Some(updated) => Some((updated, r)),
            None => {
                warnings.push(format!(
                    "refinement at {:.6},{:.6} did not preserve the locating matches at its fitted clock; that candidate keeps its fine-grid position and evidence",
                    r.lat, r.lon
                ));
                None
            }
        }
    });
    let margin = margin_report(&cands);
    if let Some(r) = &refine {
        progress(&format!(
            "refined: {:.6},{:.6} r={:.0} m",
            r.lat, r.lon, r.radius_m
        ));
    }
    SearchResult {
        funnel,
        nulls,
        candidates: cands,
        refine,
        margin,
        warnings,
    }
}

/// A different explanation: a place more than 5 km away, or one that explains the events
/// differently (no shared event-flight match, or a shared event matched to another flight). The
/// same flights at a nearby worse optimum are positional ambiguity, which the radius covers.
fn is_different_explanation(top: &Candidate, other: &Candidate) -> bool {
    haversine_m(other.lat, other.lon, top.lat, top.lon) > 5000.0 || !same_explanation(top, other)
}

/// The two scores a margin compares, at the same level of polish: refined against refined or, when
/// either could not be refined, both at their fine-grid cells. Refinement usually raises a score,
/// so anything else credits one side with a gain the other was never given.
fn compared_scores(top: &Candidate, rival: &Candidate) -> (f64, f64, &'static str) {
    match (top.fine_score, rival.fine_score) {
        (Some(_), Some(_)) => (top.score, rival.score, "refined"),
        (t, r) => (
            t.unwrap_or(top.score),
            r.unwrap_or(rival.score),
            "fine_grid",
        ),
    }
}

/// A rival challenges the leader at their shared level of polish. Current scores alone mix
/// refined and fine-grid candidates and can hide a closer comparable rival.
fn compared_margin(top: &Candidate, rival: &Candidate) -> f64 {
    let (t, r, _) = compared_scores(top, rival);
    t - r
}

/// The smallest comparable margin against a different explanation. Sorted candidates, leader first.
fn margin_report(cands: &[Candidate]) -> serde_json::Value {
    let Some(top) = cands.first() else {
        return serde_json::json!({});
    };
    let alt = cands[1..]
        .iter()
        .filter(|o| is_different_explanation(top, o))
        .min_by(|a, b| compared_margin(top, a).total_cmp(&compared_margin(top, b)));
    let local = cands[1..]
        .iter()
        .find(|o| same_explanation(top, o) && haversine_m(o.lat, o.lon, top.lat, top.lon) > 1000.0);
    let compared = alt.map(|a| compared_scores(top, a));
    serde_json::json!({
        "top_score": top.score,
        "next_score": cands.get(1).map(|c| c.score),
        "next_distinct_score": alt.map(|a| a.score),
        "next_distinct_km": alt.map(|a| haversine_m(a.lat, a.lon, top.lat, top.lon) / 1000.0),
        "next_distinct_flights": alt.map(|a| a.events.iter().map(|m| format!("{}:{} {}", m.event, if m.reg.is_empty() { &m.icao } else { &m.reg }, m.typ)).collect::<Vec<_>>()),
        "margin_nats": compared.map(|(t, r, _)| t - r),
        "margin_top_score": compared.map(|(t, _, _)| t),
        "margin_rival_score": compared.map(|(_, r, _)| r),
        "margin_basis": compared.map(|(_, _, basis)| basis),
        "ratio_distinct": compared.map(|(t, r, _)| if t > 0.0 { r / t } else { f64::NAN }),
        "same_flights_alt_km": local.map(|l| haversine_m(l.lat, l.lon, top.lat, top.lon) / 1000.0),
        "same_flights_alt_margin_nats": local.map(|l| top.score - l.score),
    })
}

/// Refinement can reduce a winner's evidence (for example by exposing an unheard close
/// pass). Recheck newly promoted leaders, keeping each candidate and its radius together.
/// Refine whichever different explanation has the smallest comparable gap. A newly refined rival
/// can move above an unrefined one by current score while challenging the leader less at equal
/// polish; stopping there would credit the leader with a gain that further rival never had.
/// Every supplied candidate is attempted at most once; a failed update retains the entire
/// coherent fine-grid candidate, never a mixture of old evidence and refined coordinates.
fn refine_candidates(
    cands: Vec<Candidate>,
    check: &(dyn Fn(&mut Candidate) + Sync),
    mut update: impl FnMut(&Candidate) -> Option<(Candidate, RefineResult)>,
) -> (Vec<Candidate>, Option<RefineResult>) {
    let mut states: Vec<(Candidate, Option<RefineResult>, bool)> =
        cands.into_iter().map(|c| (c, None, false)).collect();
    states.retain(|s| has_distinct_events(&s.0));
    while let Some(leader) = states.first() {
        let k = if leader.2 {
            let rival = states
                .iter()
                .enumerate()
                .filter(|(_, s)| is_different_explanation(&leader.0, &s.0))
                .min_by(|(_, a), (_, b)| {
                    let a_margin = compared_margin(&leader.0, &a.0);
                    let b_margin = compared_margin(&leader.0, &b.0);
                    a_margin.total_cmp(&b_margin)
                })
                .map(|(i, _)| i);
            let Some(rival) = rival else { break };
            rival
        } else {
            0
        };
        let (cand, radius, attempted) = &mut states[k];
        // Refinement can promote a candidate below the original leading 20. Its coarse
        // fallback must pass the same lock/pruning policy before it can become the answer.
        if !cand.lock_checked {
            check(cand);
            cand.lock_checked = true;
            states.retain(|s| has_distinct_events(&s.0));
        } else if *attempted {
            break;
        } else {
            *attempted = true;
            if let Some((updated, refined)) = update(cand) {
                let fine_score = cand.score;
                *cand = updated;
                cand.fine_score = Some(fine_score);
                *radius = Some(refined);
            }
        }
        states.sort_by(|a, b| {
            b.0.score
                .total_cmp(&a.0.score)
                .then(a.0.pair_chi.total_cmp(&b.0.pair_chi))
        });
    }
    let refined = states.first_mut().and_then(|s| s.1.take());
    (states.into_iter().map(|s| s.0).collect(), refined)
}

fn has_distinct_events(c: &Candidate) -> bool {
    c.events
        .first()
        .is_some_and(|first| c.events.iter().any(|m| m.event != first.event))
}

/// Finalize the leaders until pruning and reranking promote no unchecked candidate into
/// the leading 20. Every pass checks at least one previously unchecked candidate, so work
/// is bounded by the supplied candidate count rather than an arbitrary number of rounds.
/// A single event cannot locate the observer, even if its chance-match evidence is large.
fn finalize_candidates(
    mut cands: Vec<Candidate>,
    check: &(dyn Fn(&mut Candidate) + Sync),
) -> Vec<Candidate> {
    cands.retain(has_distinct_events);
    loop {
        let n_lock = cands.len().min(20);
        if cands[..n_lock].iter().all(|c| c.lock_checked) {
            return cands;
        }
        cands[..n_lock]
            .par_iter_mut()
            .filter(|c| !c.lock_checked)
            .for_each(|cand| {
                check(cand);
                cand.lock_checked = true;
            });
        cands.retain(has_distinct_events);
        cands.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.pair_chi.total_cmp(&b.pair_chi))
        });
        cands = dedupe(cands);
    }
}

/// Conservative Cartesian/clock cell for duplicate lookup. On the same sphere as haversine,
/// chord distance is at most arc distance: a duplicate must be in an adjacent cell on every
/// axis. The extra metre covers rounding, including at poles and the antimeridian. Clock
/// cells use exact binary division and are wider than the unchanged five-second predicate.
fn dedupe_cell(c: &Candidate) -> Option<[i64; 4]> {
    if !(-90.0..=90.0).contains(&c.lat)
        || !(-180.0..=180.0).contains(&c.lon)
        || !c.t_start_utc.is_finite()
        || c.t_start_utc.abs() > (1_u64 << 52) as f64
    {
        return None;
    }
    let (sp, cp) = c.lat.to_radians().sin_cos();
    let (sl, cl) = c.lon.to_radians().sin_cos();
    let scale = fdop_types::geo::EARTH_R / 301.0;
    Some([
        (scale * cp * cl).floor() as i64,
        (scale * cp * sl).floor() as i64,
        (scale * sp).floor() as i64,
        (c.t_start_utc / 8.0).floor() as i64,
    ])
}

#[derive(Default)]
struct DedupeGroup {
    kept: Vec<usize>,
    cells: HashMap<[i64; 4], Vec<usize>>,
    unindexed: Vec<usize>,
}

impl DedupeGroup {
    // Avoid hash-neighbor probes for small groups, which are cheaper to scan directly.
    const INDEX_AT: usize = 64;

    fn is_duplicate(&self, f: &Candidate, out: &[Candidate]) -> bool {
        let near = |&i: &usize| {
            let o = &out[i];
            haversine_m(o.lat, o.lon, f.lat, f.lon) < 300.0
                && (o.t_start_utc - f.t_start_utc).abs() < 5.0
        };
        if self.kept.len() < Self::INDEX_AT {
            return self.kept.iter().any(near);
        }
        let Some([x, y, z, t]) = dedupe_cell(f) else {
            return self.kept.iter().any(near);
        };
        // An out-of-domain longitude can still represent the same place. Both lookup
        // directions must retain the original predicate rather than silently losing it.
        if self.unindexed.iter().any(near) {
            return true;
        }
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    for dt in -1..=1 {
                        if self
                            .cells
                            .get(&[x + dx, y + dy, z + dz, t + dt])
                            .is_some_and(|indices| indices.iter().any(near))
                        {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    fn index(&mut self, i: usize, c: &Candidate) {
        if let Some(cell) = dedupe_cell(c) {
            self.cells.entry(cell).or_default().push(i);
        } else {
            self.unindexed.push(i);
        }
    }

    fn push(&mut self, i: usize, out: &[Candidate]) {
        self.kept.push(i);
        if self.kept.len() == Self::INDEX_AT {
            for j in 0..self.kept.len() {
                let k = self.kept[j];
                self.index(k, &out[k]);
            }
        } else if self.kept.len() > Self::INDEX_AT {
            self.index(i, &out[i]);
        }
    }
}

/// Same place (300 m), clock (5 s) and set of event-flight matches. Preserve the sorted
/// input's greedy representatives; large groups only compare nearby retained candidates.
fn dedupe(sorted: Vec<Candidate>) -> Vec<Candidate> {
    let mut kept_with: HashMap<Vec<(String, String)>, DedupeGroup> = HashMap::new();
    let mut out: Vec<Candidate> = Vec::new();
    for f in sorted {
        let mut matches: Vec<(String, String)> = f
            .events
            .iter()
            .map(|m| (m.event.clone(), m.flight.clone()))
            .collect();
        matches.sort();
        let kept = kept_with.entry(matches).or_default();
        if !kept.is_duplicate(&f, &out) {
            out.push(f);
            kept.push(out.len() - 1, &out);
        }
    }
    out
}

/// Do two candidates tell the same story: at least one shared event-flight match and no shared
/// event matched to different flights?
fn same_explanation(a: &Candidate, b: &Candidate) -> bool {
    let mut shared = false;
    for m in &a.events {
        if let Some(o) = b.events.iter().find(|o| o.event == m.event) {
            if o.flight != m.flight {
                return false;
            }
            shared = true;
        }
    }
    shared
}

/// Tonal aircraft that passed much closer (< half the nearest explained CPA) while recording
/// and left no detection within 25 s of their predicted fitted curve centre.
fn unheard_passes(
    cand: &Candidate,
    o: &Obs,
    tracks: &[TrackModel],
    tix: &TimeIndex,
    heard: &[f64],
    p: &SearchParams,
    atmo: &dyn Atmosphere,
) -> Vec<String> {
    let c = p.c;
    let mut out = Vec::new();
    let dmin = cand
        .events
        .iter()
        .map(|m| m.d)
        .fold(f64::INFINITY, f64::min);
    if !dmin.is_finite() {
        return out;
    }
    let thr = 0.5 * dmin;
    let (ta, tb) = (cand.t_start_utc, cand.t_start_utc + p.duration);
    let ordinary_still_air = c.is_finite()
        && c > 0.0
        && atmo.uniform_medium() == Some(fdop_types::atmo::Medium::still(c));
    let (query_lo, query_hi, scan_lo, scan_hi) = if ordinary_still_air {
        (ta - thr / c - 60.0, tb + 60.0, ta - thr / c, tb)
    } else {
        (
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::INFINITY,
        )
    };
    for ti in tix.overlapping_near(query_lo, query_hi, o.x, thr) {
        let tr = &tracks[ti as usize];
        if p.tonal_only && !tr.engine.tonal() {
            continue;
        }
        // The distance threshold already excludes every explained CPA. The same aircraft
        // can return much closer, so check those other passes against the detections too.
        for cpa in cpas_in(tr, o.x, scan_lo, scan_hi, thr) {
            let t_rec = if ordinary_still_air {
                cpa.t_r(c)
            } else {
                cpa.t_r_fit_p(&prop_for(atmo, o, tr, cpa.t_e))
            } - cand.t_start_utc;
            if !(30.0..=p.duration - 30.0).contains(&t_rec) {
                continue;
            }
            if !heard.iter().any(|&h| (h - t_rec).abs() <= 25.0) {
                out.push(format!(
                    "{} {} d={:.0} m at t_rec={:.0} s",
                    tr.key, tr.typ, cpa.d, t_rec
                ));
            }
        }
    }
    out
}

/// Each event's own best clock (relative to the joint clock) at the candidate's place.
fn free_clocks(
    cand: &Candidate,
    o: &Obs,
    evs: &[SearchEvent],
    tracks: &[TrackModel],
    p: &SearchParams,
    atmo: &dyn Atmosphere,
) -> Vec<Option<f64>> {
    cand.trk
        .iter()
        .zip(&cand.events)
        .map(|(&(ei, ti), m)| {
            let ev = &evs[ei];
            let tr = &tracks[ti as usize];
            let cpa = tr.cpa_nearest_time(o.x, m.t_cpa_heard_utc - m.d / m.c_path, 30.0)?;
            let prop = prop_for(atmo, o, tr, cpa.t_e);
            let (lo, hi) = (cand.t_start_utc - 30.0, cand.t_start_utc + 30.0);
            let g = RatioGrid::build(
                tr,
                o.x,
                &prop,
                lo + ev.tr_t[0] - 1.0,
                hi + ev.tr_t[ev.tr_t.len() - 1] + 1.0,
                0.2,
                p.max_gap_s,
            );
            let (mut bt, mut bc) = (cand.t_start_utc, f64::INFINITY);
            let mut scratch = TraceScratch::default();
            let mut ts = lo;
            while ts <= hi {
                let r = trace_chi(
                    &ev.tr_t,
                    &ev.tr_f,
                    &|tu| g.at(tu),
                    ts,
                    ev,
                    prop.source_tone_scale(cpa.vel),
                    ev.sigma_hz,
                    p.cap_sigma,
                    true,
                    &mut scratch,
                );
                if r.chi < bc {
                    bc = r.chi;
                    bt = ts;
                }
                ts += 0.25;
            }
            Some(bt - cand.t_start_utc)
        })
        .collect()
}

/// Rebuild the reported evidence at the refined position and its already-profiled clock.
/// Weak confirmers may fall away, but every assignment that defined the confidence contour
/// must survive. Refitting the clock on all matches here would undo the calibrated core fit.
fn refresh_refined_candidate(
    top: &Candidate,
    refined: &RefineResult,
    inp: &SearchInputs,
    evs: &[SearchEvent],
    p: &SearchParams,
    nulls: &[EventNull],
    tix: &TimeIndex,
) -> Option<Candidate> {
    if top.trk.len() != top.events.len()
        || !p.region.contains(refined.lat, refined.lon)
        || !refined.t_start_utc.is_finite()
        || refined.t_start_utc < p.start_lo - 1.0
        || refined.t_start_utc > p.start_hi + 1.0
    {
        return None;
    }
    let o = observer(refined.lat, refined.lon, inp.fine_dem);
    let core_only = top.events.iter().filter(|m| m.core).count() >= 2;
    let required = |k: usize| !core_only || top.events[k].core;
    // Keep the locating passes if a moved observer makes an optional match collapse onto
    // the same physical pass. The original order is preserved within each group.
    let mut order: Vec<usize> = (0..top.events.len()).collect();
    order.sort_by_key(|&k| !required(k));
    let mut used_cpas: Vec<(u32, f64)> = Vec::new();
    let mut must_keep = Vec::new();
    let mut fresh = Candidate {
        lat: refined.lat,
        lon: refined.lon,
        elev_m: o.z,
        t_start_utc: refined.t_start_utc,
        lock_checked: true,
        ..Candidate::default()
    };
    let mut scratch = TraceScratch::default();
    for k in order {
        let (ei, ti) = top.trk[k];
        let old = &top.events[k];
        let fitted = (|| {
            let ev = evs.get(ei)?;
            let tr = inp.tracks.get(ti as usize)?;
            let cpa = tr.cpa_nearest_time(o.x, old.t_cpa_heard_utc - old.d / old.c_path, 150.0)?;
            let prop = prop_for(inp.atmo, &o, tr, cpa.t_e);
            // An optional confirmer stays under `best_confirmer`'s rule, with no slack.
            let slack = if required(k) {
                LOCATING_GATE_SLACK
            } else {
                0.0
            };
            let fitted_time = cpa.t_r_fit_p(&prop);
            if !gates_ok(ev, &cpa, &prop, slack)
                || tr.gap_at(cpa.t_e) > p.max_gap_s
                || !fitted_time.is_finite()
                || (fitted_time - fresh.t_start_utc - ev.t_rec).abs() > ev.t_tol + 3.0
                || used_cpas
                    .iter()
                    .any(|&(used, te)| used == ti && (cpa.t_e - te).abs() < SAME_FLIGHT_MIN_DT_S)
            {
                return None;
            }
            let lo = fresh.t_start_utc + *ev.tr_t.first()? - 1.0;
            let hi = fresh.t_start_utc + *ev.tr_t.last()? + 1.0;
            let grid = RatioGrid::build(tr, o.x, &prop, lo, hi, 0.2, p.max_gap_s);
            let fit = trace_chi(
                &ev.tr_t,
                &ev.tr_f,
                &|tu| grid.at(tu),
                fresh.t_start_utc,
                ev,
                prop.source_tone_scale(cpa.vel),
                ev.sigma_hz,
                p.cap_sigma,
                true,
                &mut scratch,
            );
            (fit.chi <= p.kappa && fit.cov >= 0.6)
                .then(|| (cpa.t_e, make_match(ev, tr, &cpa, fit, &prop, &old.role)))
        })();
        let Some((te, matched)) = fitted else {
            if required(k) {
                return None;
            }
            continue;
        };
        fresh.events.push(matched);
        fresh.trk.push((ei, ti));
        used_cpas.push((ti, te));
        must_keep.push(required(k));
    }
    let clocks = free_clocks(&fresh, &o, evs, inp.tracks, p, inp.atmo);
    for k in (0..fresh.events.len()).rev() {
        fresh.events[k].dt_free = clocks[k];
        if !clocks[k].is_some_and(|dt| dt.is_finite() && dt.abs() <= lock_tol(&evs[fresh.trk[k].0]))
        {
            if must_keep[k] {
                return None;
            }
            fresh.events.remove(k);
            fresh.trk.remove(k);
        }
    }
    let first = fresh.events.first()?;
    if !fresh.events.iter().any(|m| m.event != first.event) {
        return None;
    }
    fresh.n_explained = fresh.events.len();
    fresh.fit_score = fit_score(&fresh.events, evs, p.kappa);
    fresh.pair_chi = fresh
        .events
        .iter()
        .zip(&fresh.trk)
        .map(|(m, &(ei, _))| evs[ei].n_eff * m.chi)
        .sum::<f64>()
        / fresh.trk.iter().map(|&(ei, _)| evs[ei].n_eff).sum::<f64>();
    fresh.unheard_close_passes =
        unheard_passes(&fresh, &o, inp.tracks, tix, &inp.heard_times, p, inp.atmo);
    fresh.score = evidence(&mut fresh.events, nulls, fresh.unheard_close_passes.len());
    Some(fresh)
}

/// Lock check with pruning: while some event's own clock is further than max(t_tol, 4 s) from
/// the joint clock, drop the worst such event and refit the joint clock and every match on the
/// rest. Leaves `dt_free` set on every remaining match.
fn lock_prune(
    cand: &mut Candidate,
    evs: &[SearchEvent],
    tracks: &[TrackModel],
    p: &SearchParams,
    atmo: &dyn Atmosphere,
    dem: &dyn Elevation,
) {
    let o = observer(cand.lat, cand.lon, dem);
    for _ in 0..cand.events.len() {
        let dts = free_clocks(cand, &o, evs, tracks, p, atmo);
        for (m, d) in cand.events.iter_mut().zip(&dts) {
            m.dt_free = *d;
        }
        let worst = cand
            .trk
            .iter()
            .zip(&dts)
            .enumerate()
            .filter_map(|(k, (&(ei, _), d))| {
                let tol = lock_tol(&evs[ei]);
                d.filter(|d| d.abs() > tol).map(|d| (k, d.abs() / tol))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1));
        let Some((k, _)) = worst else { break };
        cand.events.remove(k);
        cand.trk.remove(k);
        cand.n_explained = cand.events.len();
        if cand.events.is_empty() {
            break;
        }
        // refit the joint clock and the matches on the remaining events
        let assign: Vec<(usize, u32, f64)> = cand
            .trk
            .iter()
            .zip(&cand.events)
            .map(|(&(ei, ti), m)| (ei, ti, m.t_cpa_heard_utc - m.d / m.c_path))
            .collect();
        let Some(pe) = eval_point(&o, &assign, evs, tracks, p, atmo, true, false, false) else {
            break;
        };
        let roles: Vec<String> = cand.events.iter().map(|m| m.role.clone()).collect();
        cand.events = assign
            .iter()
            .enumerate()
            .map(|(k, &(ei, ti, _))| {
                make_match(
                    &evs[ei],
                    &tracks[ti as usize],
                    &pe.cpas[k],
                    pe.chis[k],
                    &pe.props[k],
                    &roles[k],
                )
            })
            .collect();
        cand.t_start_utc = pe.t_s;
    }
}

/// Score named (event id, track key) pairs at one observer point with the full search model
/// (no gates): the joint clock (searched within +-`clock_tol` s of `t_start_guess`), and each
/// event's match. For diagnostics and `fdop explain`.
pub fn explain_at(
    inp: &SearchInputs,
    p: &SearchParams,
    lat: f64,
    lon: f64,
    t_start_guess: f64,
    clock_tol: f64,
    pairs: &[(String, String)],
) -> Result<Candidate, String> {
    let o = observer(lat, lon, inp.fine_dem);
    let mut assign = Vec::new();
    for (eid, key) in pairs {
        let ei = inp
            .events
            .iter()
            .position(|e| &e.id == eid)
            .ok_or_else(|| format!("no search event {eid}"))?;
        let ev = &inp.events[ei];
        let t_heard = t_start_guess + ev.t_rec;
        let (ti, cpa) = if matching_still_air(ev, p, inp.atmo) {
            let ti = inp
                .tracks
                .iter()
                .position(|t| {
                    (&t.key == key || t.icao.eq_ignore_ascii_case(key))
                        && t.t0() < t_heard
                        && t.t1() > t_heard - 60.0
                })
                .ok_or_else(|| format!("no track {key} around {t_heard:.0}"))?;
            let cpa = inp.tracks[ti]
                .cpa_nearest_time(o.x, t_heard - ev.d_gate[1].min(12_000.0) / p.c * 0.5, 300.0)
                .ok_or("no CPA")?;
            (ti, cpa)
        } else {
            // A fitted centre can precede the physical CPA. Choose the named retained
            // visit by that same fitted clock, including separate legs and returning passes.
            let obs = &o;
            inp.tracks
                .iter()
                .enumerate()
                .filter(|(_, tr)| &tr.key == key || tr.icao.eq_ignore_ascii_case(key))
                .flat_map(|(ti, tr)| {
                    tr.local_cpas(obs.x, tr.t0(), tr.t1())
                        .filter_map(move |cpa| {
                            let prop = prop_for(inp.atmo, obs, tr, cpa.t_e);
                            let dt = (cpa.t_r_fit_p(&prop) - t_heard).abs();
                            dt.is_finite().then_some((ti, cpa, dt))
                        })
                })
                .min_by(|a, b| {
                    b.1.interior
                        .cmp(&a.1.interior)
                        .then_with(|| a.2.total_cmp(&b.2))
                })
                .map(|(ti, cpa, _)| (ti, cpa))
                .ok_or_else(|| format!("no track {key} around {t_heard:.0}"))?
        };
        assign.push((ei, ti as u32, cpa.t_e));
    }
    let mut pp = p.clone();
    pp.start_lo = t_start_guess - clock_tol;
    pp.start_hi = t_start_guess + clock_tol;
    let evs = &inp.events;
    let pe = eval_point(
        &o, &assign, evs, inp.tracks, &pp, inp.atmo, true, false, false,
    )
    .ok_or("evaluation failed")?;
    let events: Vec<EventMatch> = assign
        .iter()
        .enumerate()
        .map(|(k, &(ei, ti, _))| {
            make_match(
                &evs[ei],
                &inp.tracks[ti as usize],
                &pe.cpas[k],
                pe.chis[k],
                &pe.props[k],
                "explain",
            )
        })
        .collect();
    let mut cand = Candidate {
        lat,
        lon,
        elev_m: o.z,
        t_start_utc: pe.t_s,
        n_explained: events.len(),
        score: f64::NAN,
        fit_score: fit_score(&events, evs, p.kappa),
        pair_chi: pe.total / assign.iter().map(|a| evs[a.0].n_eff).sum::<f64>(),
        unheard_close_passes: Vec::new(),
        trk: assign.iter().map(|a| (a.0, a.1)).collect(),
        lock_checked: false,
        fine_score: None,
        events,
    };
    let dts = free_clocks(&cand, &o, evs, inp.tracks, &pp, inp.atmo);
    for (m, d) in cand.events.iter_mut().zip(dts) {
        m.dt_free = d;
    }
    Ok(cand)
}

/// Re-score the winner's (event, flight) assignment on a fine grid; the confidence region is
/// where the calibrated total chi-square is within 5.99 (2 dof, 95 %) of its minimum.
/// Event noise scales come from an initial uncalibrated fit and stay fixed for the spatial
/// search, shared-clock profiling and any subsequent window expansions.
fn refine_winner(
    top: &Candidate,
    evs: &[SearchEvent],
    tracks: &[TrackModel],
    p: &SearchParams,
    dem: &dyn Elevation,
    atmo: &dyn Atmosphere,
) -> Option<RefineResult> {
    // locate with the core matches (clean, locked, better than chance); weak confirmers are
    // reported but must not drag the position
    let mut assign: Vec<(usize, u32, f64)> = top
        .trk
        .iter()
        .zip(&top.events)
        .filter(|(_, m)| m.core)
        .map(|(&(ei, ti), m)| (ei, ti, m.t_cpa_heard_utc - m.d / m.c_path))
        .collect();
    if assign.len() < 2 {
        assign = top
            .trk
            .iter()
            .zip(&top.events)
            .map(|(&(ei, ti), m)| (ei, ti, m.t_cpa_heard_utc - m.d / m.c_path))
            .collect();
    }
    // Expand clipped contours; resample contours too narrow for the current grid.
    let mut q = p.clone();
    let (mut lat0, mut lon0) = (top.lat, top.lon);
    let mut out = None;
    let mut weights = None;
    for _ in 0..3 {
        let r = refine_on(
            lat0,
            lon0,
            &assign,
            evs,
            tracks,
            &q,
            dem,
            atmo,
            &mut weights,
        )?;
        let done = !r.radius_lower_bound && r.region_cells > 0;
        let resample = r.region_cells == 0;
        (lat0, lon0) = (r.lat, r.lon);
        out = Some(r);
        if done {
            break;
        }
        if resample {
            q.refine_step_m *= 0.5;
        } else {
            q.refine_half_m *= 2.0;
            q.refine_step_m *= 2.0;
        }
    }
    out
}

/// Bounded pattern search in the original grid's ENU frame. Every accepted move is
/// evaluated with the physical model; interpolation never substitutes for a trace fit.
fn polish_grid_minimum(
    mut best: (f64, f64, f64),
    half_m: f64,
    grid_step_m: f64,
    score: impl Fn(f64, f64) -> Option<f64>,
) -> (f64, f64, f64) {
    let mut step = grid_step_m * 0.5;
    for _ in 0..64 {
        if step < 0.25 {
            break;
        }
        let mut next = best;
        for (de, dn) in [
            (-1.0, -1.0),
            (0.0, -1.0),
            (1.0, -1.0),
            (-1.0, 0.0),
            (1.0, 0.0),
            (-1.0, 1.0),
            (0.0, 1.0),
            (1.0, 1.0),
        ] {
            let e = (best.0 + de * step).clamp(-half_m, half_m);
            let n = (best.1 + dn * step).clamp(-half_m, half_m);
            if let Some(cost) = score(e, n)
                && cost.is_finite()
                && cost < next.2
            {
                next = (e, n, cost);
            }
        }
        if next.2 < best.2 {
            best = next;
        } else {
            step *= 0.5;
        }
    }
    best
}

/// Profile an edge between the neighbors of its best sampled node. This finds narrow,
/// rotated contours that cross a boundary between grid nodes, using actual trace fits.
fn polish_edge_minimum(
    mut best: (f64, f64),
    lo: f64,
    hi: f64,
    grid_step_m: f64,
    score: impl Fn(f64) -> Option<f64>,
) -> (f64, f64) {
    let mut step = grid_step_m * 0.5;
    for _ in 0..64 {
        if step < 0.25 {
            break;
        }
        let mut next = best;
        for sign in [-1.0, 1.0] {
            let t = (best.0 + sign * step).clamp(lo, hi);
            if let Some(cost) = score(t)
                && cost.is_finite()
                && cost < next.1
            {
                next = (t, cost);
            }
        }
        if next.1 < best.1 {
            best = next;
        } else {
            step *= 0.5;
        }
    }
    best
}

/// One refinement grid of `p.refine_half_m` / `p.refine_step_m` around (lat0, lon0).
/// The first window estimates fixed event weights; later windows reuse that calibration.
#[allow(clippy::too_many_arguments)]
fn refine_on(
    lat0: f64,
    lon0: f64,
    assign: &[(usize, u32, f64)],
    evs: &[SearchEvent],
    tracks: &[TrackModel],
    p: &SearchParams,
    dem: &dyn Elevation,
    atmo: &dyn Atmosphere,
    weights: &mut Option<Vec<f64>>,
) -> Option<RefineResult> {
    let grid = local_grid(lat0, lon0, p.refine_half_m, p.refine_step_m);
    let frame = Enu::new(lat0, lon0, 0.0);
    let half = grid.last()?.2;
    let evaluate_at = |east: f64, north: f64, w: Option<&[f64]>| {
        let (la, lo, _) = frame.to_geodetic([east, north, 0.0]);
        eval_point_weighted(
            &observer(la, lo, dem),
            assign,
            evs,
            tracks,
            p,
            atmo,
            p.region.contains(la, lo),
            false,
            false,
            w,
        )
    };
    let evaluate = |w: Option<&[f64]>| -> Vec<Option<PointEval>> {
        grid.par_iter()
            .map(|&(la, lo, _, _)| {
                let o = observer(la, lo, dem);
                eval_point_weighted(
                    &o,
                    assign,
                    evs,
                    tracks,
                    p,
                    atmo,
                    p.region.contains(la, lo),
                    false,
                    false,
                    w,
                )
            })
            .collect()
    };
    let best_index = |evals: &[Option<PointEval>]| {
        evals
            .iter()
            .enumerate()
            .filter_map(|(k, e)| e.as_ref().map(|pe| (k, pe.total)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(k, _)| k)
    };
    let mut evals = evaluate(weights.as_deref());
    let mut k_best = best_index(&evals)?;
    let polish = |k: usize, evals: &[Option<PointEval>], w: Option<&[f64]>| {
        let (_, _, east, north) = grid[k];
        let seed = (east, north, evals[k].as_ref()?.total);
        let best = polish_grid_minimum(seed, half, p.refine_step_m, |e, n| {
            evaluate_at(e, n, w).map(|pe| pe.total)
        });
        Some((best, evaluate_at(best.0, best.1, w)?))
    };
    let mut best = polish(k_best, &evals, weights.as_deref())?;
    if weights.is_none() {
        // Grid quantization is not measurement noise. Estimate scales at the polished
        // pilot, then freeze them for every subsequent point, clock and window.
        let pilot = &best.1;
        let calibrated: Vec<f64> = pilot
            .chis
            .iter()
            .zip(assign)
            .map(|(c, a)| ev_weight(&evs[a.0]) / c.chi.max(1.0))
            .collect();
        let reprofile = calibrated.iter().any(|&w| w != 1.0);
        *weights = Some(calibrated);
        if reprofile {
            // Reweighting cached cell scores alone would leave each shared clock fitted
            // to the wrong objective and can put the reported center off its own minimum.
            evals = evaluate(weights.as_deref());
            k_best = best_index(&evals)?;
            best = polish(k_best, &evals, weights.as_deref())?;
        }
    }
    let n = (p.refine_half_m / p.refine_step_m).round() as usize * 2 + 1;
    let mut boundary = Vec::with_capacity(4);
    for side in 0..4 {
        let index = |i| match side {
            0 => i,
            1 => (n - 1) * n + i,
            2 => i * n,
            _ => i * n + n - 1,
        };
        let Some((i, cost)) = (0..n)
            .filter_map(|i| evals[index(i)].as_ref().map(|pe| (i, pe.total)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
        else {
            continue;
        };
        let coordinates = |t| match side {
            0 => (t, -half),
            1 => (t, half),
            2 => (-half, t),
            _ => (half, t),
        };
        let t0 = -half + i as f64 * p.refine_step_m;
        let edge = polish_edge_minimum(
            (t0, cost),
            (t0 - p.refine_step_m).max(-half),
            (t0 + p.refine_step_m).min(half),
            p.refine_step_m,
            |t| {
                let (e, n) = coordinates(t);
                evaluate_at(e, n, weights.as_deref()).map(|pe| pe.total)
            },
        );
        let (e, n) = coordinates(edge.0);
        boundary.push((e, n, edge.1));
        if edge.1 < best.0.2 {
            best = ((e, n, edge.1), evaluate_at(e, n, weights.as_deref())?);
        }
    }
    let ((best_e, best_n, t_best), best_eval) = best;
    let mut dchi = vec![vec![f64::NAN; n]; n];
    let (bla, blo, _) = frame.to_geodetic([best_e, best_n, 0.0]);
    let mut rmax: f64 = 0.0;
    let mut cells = 0usize;
    let mut edge = best_e.abs() >= half || best_n.abs() >= half;
    for (e, n, cost) in boundary {
        if cost - t_best <= 5.99 {
            edge = true;
            let (la, lo, _) = frame.to_geodetic([e, n, 0.0]);
            rmax = rmax.max(haversine_m(bla, blo, la, lo));
        }
    }
    for (k, e) in evals.iter().enumerate() {
        let (iy, ix) = (k / n, k % n);
        if let Some(pe) = e {
            let dc = pe.total - t_best;
            dchi[iy][ix] = dc;
            if dc <= 5.99 {
                cells += 1;
                // The grid is centered on the previous winner, not necessarily this
                // minimum. A small contour can still be clipped at any grid boundary.
                edge |= iy == 0 || iy + 1 == n || ix == 0 || ix + 1 == n;
                let (la, lo, _, _) = grid[k];
                rmax = rmax.max(haversine_m(bla, blo, la, lo));
            }
        }
    }
    let lats: Vec<f64> = (0..n).map(|iy| grid[iy * n].0).collect();
    let lons: Vec<f64> = (0..n).map(|ix| grid[ix].1).collect();
    let area_r = (cells as f64 * p.refine_step_m * p.refine_step_m / std::f64::consts::PI).sqrt();
    let floor = 75.0;
    // Half a cell diagonal covers an unsampled corner in two dimensions.
    let radius = (rmax + p.refine_step_m / std::f64::consts::SQRT_2).max(floor);
    Some(RefineResult {
        lat: bla,
        lon: blo,
        t_start_utc: best_eval.t_s,
        radius_m: radius,
        radius_basis: format!(
            "95% chi-square region (2 dof) of the joint trace fit over {} events ({}) on a {:.0} m contour grid, with position polished below grid spacing and shared clock profiled using fixed pilot-fit event noise scales; boundary minima checked between nodes; max extent {:.0} m, equal-area radius {:.0} m, floor {:.0} m{}",
            assign.len(),
            assign
                .iter()
                .map(|a| evs[a.0].id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            p.refine_step_m,
            rmax,
            area_r,
            floor,
            if cells == 0 {
                "; contour unresolved between grid nodes (radius uses the grid padding and floor only)"
            } else if edge {
                "; region touches the refine window edge (radius is a lower bound)"
            } else {
                ""
            }
        ),
        radius_lower_bound: edge,
        step_m: p.refine_step_m,
        lats,
        lons,
        dchi2: dchi,
        region_cells: cells,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::cv_curve;
    use crate::track::straight_track;
    use fdop_types::FlatElevation;

    // Collect only in tests, so the exhaustive oracle still checks the complete ordered
    // output. The production caller scores and releases each bounded output batch.
    #[allow(clippy::too_many_arguments)]
    fn join(
        hi: &[Hyp],
        hj: &[Hyp],
        ei: usize,
        ej: usize,
        r: f64,
        tt: f64,
        same_flight_min_dt: f64,
        t_base: f64,
    ) -> Vec<Joined> {
        let mut out = Vec::new();
        let count = join_batches(hi, hj, ei, ej, r, tt, same_flight_min_dt, t_base, |batch| {
            out.extend_from_slice(batch);
        });
        assert_eq!(count, out.len());
        out
    }

    // Exhaustive reference: visit every input pair in the original bucket/insertion order.
    // No hash join, parallel chunks or intermediate candidate collection is used here.
    fn exhaustive_join(hi: &[Hyp], hj: &[Hyp], r: f64, tt: f64, t_base: f64) -> Vec<Joined> {
        let cell = |h: &Hyp| {
            (
                (h.e as f64 / r).floor() as i32,
                (h.n as f64 / r).floor() as i32,
                (h.t_s as f64 / tt).floor() as i64,
            )
        };
        let mut ordered: Vec<usize> = (0..hi.len()).collect();
        ordered.sort_by_key(|&i| (cell(&hi[i]), i));
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for b in hj {
            let kb = cell(b);
            for &ia in &ordered {
                let a = &hi[ia];
                let ka = cell(a);
                if (ka.0 - kb.0).abs() > 1
                    || (ka.1 - kb.1).abs() > 1
                    || (ka.2 - kb.2).abs() > 1
                    || ((a.e - b.e) as f64).powi(2) + ((a.n - b.n) as f64).powi(2) > r * r
                    || ((a.t_s - b.t_s) as f64).abs() > tt
                    || (a.trk == b.trk && ((a.t_e - b.t_e) as f64).abs() < SAME_FLIGHT_MIN_DT_S)
                {
                    continue;
                }
                let j = Joined {
                    ei: 2,
                    ej: 5,
                    ti: a.trk,
                    tj: b.trk,
                    te_i: a.t_e as f64 + t_base,
                    te_j: b.t_e as f64 + t_base,
                    e: 0.5 * (a.e + b.e) as f64,
                    n: 0.5 * (a.n + b.n) as f64,
                    t_s: 0.5 * (a.t_s + b.t_s) as f64 + t_base,
                };
                let Some(clock) = clock_cell(j.t_s) else {
                    continue;
                };
                if seen.insert((
                    j.ti,
                    j.tj,
                    (j.e / 600.0).floor() as i32,
                    (j.n / 600.0).floor() as i32,
                    clock,
                )) {
                    out.push(j);
                }
            }
        }
        out
    }

    #[test]
    fn pair_join_preserves_every_ordered_representative_across_batches() {
        let coordinates = [-1200.0, -600.25, -600.0, -0.25, 0.0, 299.75, 600.0, 1200.0];
        let hypothesis = |i: usize| Hyp {
            trk: (i % 7) as u32,
            t_e: 100.0 + (i % 13) as f32 * 15.0,
            e: coordinates[i % coordinates.len()],
            n: coordinates[(i / coordinates.len()) % coordinates.len()],
            t_s: (i % 9) as f32 * 10.0 - 40.0,
        };
        let hi: Vec<_> = (0..80).map(hypothesis).collect();
        // Repeated cells with different representatives straddle several worker chunks and
        // multiple bounded batches. A different representative would move the next trace fit.
        let hj: Vec<_> = (0..8205).map(|i| hypothesis((i * 17) % 97)).collect();
        let unique_hi: Vec<_> = (0..40)
            .map(|i| Hyp {
                trk: i,
                ..hypothesis(0)
            })
            .collect();
        let unique_hj: Vec<_> = (40..120)
            .map(|i| Hyp {
                trk: i,
                ..hypothesis(0)
            })
            .collect();
        let same: Vec<_> = [0.0, 29.75, 30.0, 30.25, 60.0]
            .into_iter()
            .map(|t_e| Hyp {
                t_e,
                ..hypothesis(0)
            })
            .collect();
        for (a, b) in [(&hi, &hj), (&unique_hi, &unique_hj), (&same, &same)] {
            for (r, tt, base) in [(600.0, 20.0, 0.0), (625.0, 30.0, 1_700_000_019.875)] {
                let expected = exhaustive_join(a, b, r, tt, base);
                assert!(!expected.is_empty());
                for workers in [1, 4] {
                    let actual = rayon::ThreadPoolBuilder::new()
                        .num_threads(workers)
                        .build()
                        .unwrap()
                        .install(|| join(a, b, 2, 5, r, tt, SAME_FLIGHT_MIN_DT_S, base));
                    assert_eq!(actual, expected, "workers={workers}, r={r}, tt={tt}");
                }
            }
        }
        assert!(join(&[], &hj, 2, 5, 600.0, 20.0, 30.0, 0.0).is_empty());
        assert!(join(&hi, &[], 2, 5, 600.0, 20.0, 30.0, 0.0).is_empty());
    }

    #[test]
    fn pair_join_streams_complete_pair_in_bounded_batches() {
        let h = Hyp {
            trk: 0,
            t_e: 100.0,
            e: 100.0,
            n: 100.0,
            t_s: 200.0,
        };
        let hi: Vec<_> = (0..4).map(|trk| Hyp { trk, ..h }).collect();
        let hj: Vec<_> = (100..8305).map(|trk| Hyp { trk, ..h }).collect();
        // All 32,820 flight pairs are distinct. Verify every ordered output without
        // retaining them, across both output boundaries and three input batches.
        for workers in [1, 4] {
            let mut visited = 0usize;
            let mut batches = 0usize;
            let mut largest = 0usize;
            let count = rayon::ThreadPoolBuilder::new()
                .num_threads(workers)
                .build()
                .unwrap()
                .install(|| {
                    join_batches(&hi, &hj, 2, 5, 600.0, 20.0, 30.0, 0.0, |batch| {
                        assert!(!batch.is_empty());
                        assert!(batch.len() <= JOIN_OUTPUT_SEEDS);
                        batches += 1;
                        largest = largest.max(batch.len());
                        for j in batch {
                            assert_eq!(
                                *j,
                                Joined {
                                    ei: 2,
                                    ej: 5,
                                    ti: (visited % hi.len()) as u32,
                                    tj: 100 + (visited / hi.len()) as u32,
                                    te_i: 100.0,
                                    te_j: 100.0,
                                    e: 100.0,
                                    n: 100.0,
                                    t_s: 200.0,
                                }
                            );
                            visited += 1;
                        }
                    })
                });
            assert_eq!(visited, hi.len() * hj.len());
            assert_eq!(count, visited);
            assert!(batches > 1);
            assert!(largest < count);
        }
    }

    fn check_dense_pair_join(size: usize) {
        let a = Hyp {
            trk: 1,
            t_e: 100.0,
            e: 100.0,
            n: 100.0,
            t_s: 200.0,
        };
        let b = Hyp { trk: 2, ..a };
        let actual = join(&vec![a; size], &vec![b; size], 2, 5, 600.0, 20.0, 30.0, 0.0);
        assert_eq!(actual, exhaustive_join(&[a], &[b], 600.0, 20.0, 0.0));
    }

    #[test]
    #[ignore = "resource measurement: 16 million compatible hypothesis pairs"]
    fn dense_pair_join_keeps_one_seed_without_raw_pair_storage() {
        check_dense_pair_join(4096);
    }

    #[test]
    #[ignore = "resource measurement: 1 million compatible hypothesis pairs"]
    fn dense_pair_join_with_one_million_raw_matches() {
        check_dense_pair_join(1024);
    }

    #[allow(clippy::too_many_arguments)]
    fn synth_event(
        id: &str,
        tr: &TrackModel,
        x: V3,
        t_s: f64,
        f0: f64,
        c: f64,
        cpa_guess: f64,
        prop: &Prop,
    ) -> SearchEvent {
        let cpa = tr.cpa_near(x, cpa_guess, 200.0).unwrap();
        // Metadata belongs to the still-air fit; the sampled observations below remain
        // centred on the physical CPA arrival and keep their original reception times.
        let t_rec = cpa.t_r_fit_p(prop) - t_s;
        let mut tt = Vec::new();
        let mut ff = Vec::new();
        let mut k = 0;
        let mut tu = cpa.t_r_p(prop) - 60.0;
        while tu <= cpa.t_r_p(prop) + 60.0 {
            let (_, _, ratio) = tr.solve_emission_p(tu, x, prop, None).unwrap();
            let noise = 0.05 * (((k * 7919) % 13) as f64 / 6.0 - 1.0);
            tt.push(tu - t_s);
            ff.push(f0 * ratio + noise);
            tu += 0.25;
            k += 1;
        }
        let (q_t, q_f) = subsample(&tt, &ff, 40);
        SearchEvent {
            id: id.into(),
            t_rec,
            t_tol: 3.0,
            v_gate: [cpa.speed * 0.9, cpa.speed * 1.1],
            d_gate: [cpa.d * 0.8, cpa.d * 1.25],
            // The generated samples use emitted f0. SearchEvent carries the tone a still-air
            // fit measures; transform only this fixture's metadata, not its physical trace.
            f0: f0 / prop.source_tone_scale(cpa.vel),
            f0_tol: 4.0 / prop.source_tone_scale(cpa.vel),
            sigma_hz: 0.12,
            n_eff: 20.0,
            quality: 100.0,
            misfit: false,
            c_fit: c,
            tr_t: tt,
            tr_f: ff,
            q_t,
            q_f,
        }
    }

    fn plain_event(f0: f64, misfit: bool) -> SearchEvent {
        SearchEvent {
            id: "e".into(),
            t_rec: 0.0,
            t_tol: 3.0,
            v_gate: [50.0, 150.0],
            d_gate: [500.0, 9000.0],
            f0,
            f0_tol: 5.0,
            sigma_hz: 0.12,
            n_eff: 20.0,
            quality: 100.0,
            misfit,
            c_fit: 343.0,
            tr_t: vec![],
            tr_f: vec![],
            q_t: vec![],
            q_f: vec![],
        }
    }

    #[test]
    fn robust_tone_recovers_doppler_curve_despite_masked_contaminants() {
        // A second tone contaminates three points in every twenty. Least squares first
        // shifts the fitted tone, then measures MAD around that biased prediction: enough
        // contaminants survive its single trim to spoil every clean point as well.
        let tt: Vec<f64> = (0..200).map(|k| -60.0 + 120.0 * k as f64 / 199.0).collect();
        let ratio = |t| cv_curve(t, 1.0, 110.0, 2500.0, 0.0, 343.0);
        let mut scratch = TraceScratch::default();
        for shift in [-4.0, 4.0] {
            let ff: Vec<f64> = tt
                .iter()
                .enumerate()
                .map(|(k, &t)| (100.0 + if k % 20 < 3 { shift } else { 0.0 }) * ratio(t))
                .collect();
            let fit = trace_chi(
                &tt,
                &ff,
                &ratio,
                0.0,
                &plain_event(100.0, false),
                1.0,
                0.2,
                5.0,
                true,
                &mut scratch,
            );
            assert!((fit.f0 - 100.0).abs() < 1e-10, "shift={shift}: {fit:?}");
            assert!((fit.chi - 3.75).abs() < 1e-10, "shift={shift}: {fit:?}");
            assert!(fit.chi <= 4.0, "the existing core-fit gate must pass");
            assert_eq!(fit.cov, 1.0);
            assert!(fit.rms > 1.0, "raw RMS must still reveal the contamination");
            assert!(fit.brk.is_none());
        }
    }

    #[test]
    fn robust_tone_keeps_bounds_coverage_and_wrong_curve_penalties() {
        let tt: Vec<f64> = (0..200).map(|k| -60.0 + 120.0 * k as f64 / 199.0).collect();
        let ratio = |t| cv_curve(t, 1.0, 110.0, 2500.0, 0.0, 343.0);
        let ff: Vec<f64> = tt.iter().map(|&t| 100.0 * ratio(t)).collect();
        let mut scratch = TraceScratch::default();
        let event = plain_event(100.0, false);
        let wrong = trace_chi(
            &tt,
            &ff,
            &|_| 1.0,
            0.0,
            &event,
            1.0,
            0.2,
            5.0,
            true,
            &mut scratch,
        );
        assert!(
            wrong.chi > 12.0,
            "a constant tone cannot explain the curve: {wrong:?}"
        );
        let out_of_bounds: Vec<f64> = tt.iter().map(|&t| 120.0 * ratio(t)).collect();
        let bounded = trace_chi(
            &tt,
            &out_of_bounds,
            &ratio,
            0.0,
            &event,
            1.0,
            0.2,
            5.0,
            true,
            &mut scratch,
        );
        assert_eq!(bounded.f0, 105.0);
        assert_eq!(bounded.chi, 25.0);
        for available in [0usize, 4, 5, 100] {
            let partial_ratio = |t| {
                if t <= tt[available.max(1) - 1] && available > 0 {
                    ratio(t)
                } else {
                    f64::NAN
                }
            };
            let partial = trace_chi(
                &tt,
                &ff,
                &partial_ratio,
                0.0,
                &event,
                1.0,
                0.2,
                5.0,
                true,
                &mut scratch,
            );
            assert_eq!(partial.cov, available as f64 / tt.len() as f64);
            if available < 5 {
                assert!(!partial.chi.is_finite());
            } else {
                let missing_penalty = 25.0 * (1.0 - partial.cov);
                assert!((partial.chi - missing_penalty).abs() < 1e-10, "{partial:?}");
                assert!(
                    partial.chi > 12.0,
                    "unsupported trace must not pass: {partial:?}"
                );
            }
        }
    }

    #[test]
    fn reused_trace_scratch_preserves_fit_results() {
        // Recorded from the allocating implementation at 695f952. Binary-fraction inputs
        // avoid platform-dependent transcendental functions in this arithmetic oracle.
        let cases = [
            (
                "clean",
                360,
                [
                    0.10415637981994283,
                    128.0156251142463,
                    0.04034158443450885,
                    1.0,
                ],
                None,
            ),
            (
                "short",
                17,
                [
                    0.10152418931537552,
                    131.01875324932504,
                    0.03982857589787441,
                    1.0,
                ],
                None,
            ),
            (
                "step",
                480,
                [
                    0.10485764832078733,
                    120.01460581627941,
                    0.04047716337655471,
                    1.0,
                ],
                Some((0.0_f64, 136.01688873192379_f64)),
            ),
            (
                "partial",
                96,
                [
                    11.512689447271427,
                    128.01776667806186,
                    0.03959753559424399,
                    0.5416666666666666,
                ],
                None,
            ),
            (
                "step",
                96,
                [
                    0.10508603972990578,
                    120.01553737892266,
                    0.040521221239984584,
                    1.0,
                ],
                Some((0.0, 136.0176750511019)),
            ),
            (
                "missing",
                80,
                [f64::INFINITY, f64::NAN, f64::NAN, 0.0],
                None,
            ),
            (
                "outliers",
                360,
                [
                    1.6253969814093876,
                    128.01609489540493,
                    2.964880445234016,
                    1.0,
                ],
                None,
            ),
            (
                "quick",
                40,
                [25.0, 128.91811991098655, 3.1468784234305947, 1.0],
                None,
            ),
            ("missing", 0, [f64::INFINITY, f64::NAN, f64::NAN, 0.0], None),
            ("clean", 3, [f64::INFINITY, f64::NAN, f64::NAN, 1.0], None),
        ];
        let mut scratch = TraceScratch::default();
        // Repeat through the same scratch to exercise growing and shrinking both traces and
        // change-point prefixes, interspersed with early returns and non-robust quick fits.
        for _ in 0..2 {
            for (kind, n, expected, expected_break) in cases {
                let tt: Vec<f64> = (0..n).map(|k| (k as f64 - n as f64 / 2.0) / 4.0).collect();
                let ff: Vec<f64> = tt
                    .iter()
                    .enumerate()
                    .map(|(k, &t)| {
                        let tone = match kind {
                            "step" if t < 0.0 => 120.0,
                            "step" => 136.0,
                            "short" => 131.0,
                            _ => 128.0,
                        };
                        let noise = ((k * 17 % 9) as f64 - 4.0) / 64.0;
                        let outlier = if matches!(kind, "outliers" | "quick") && k % 17 == 0 {
                            12.0
                        } else {
                            0.0
                        };
                        tone * (1.0 - t / 1024.0) + noise + outlier
                    })
                    .collect();
                let ratio = |t: f64| {
                    if kind == "missing" || (kind == "partial" && !(-5.0..=8.0).contains(&t)) {
                        f64::NAN
                    } else {
                        1.0 - t / 1024.0
                    }
                };
                let fit = trace_chi(
                    &tt,
                    &ff,
                    &ratio,
                    0.125,
                    &plain_event(128.0, matches!(kind, "step" | "short")),
                    1.0,
                    0.125,
                    5.0,
                    kind != "quick",
                    &mut scratch,
                );
                for (got, want) in [fit.chi, fit.f0, fit.rms, fit.cov]
                    .into_iter()
                    .zip(expected)
                {
                    if want.is_nan() {
                        assert!(got.is_nan(), "{kind}, {n}: {fit:?}");
                    } else {
                        assert_eq!(got.to_bits(), want.to_bits(), "{kind}, {n}: {fit:?}");
                    }
                }
                assert_eq!(
                    fit.brk.map(|(a, b)| (a.to_bits(), b.to_bits())),
                    expected_break.map(|(a, b)| (a.to_bits(), b.to_bits())),
                    "{kind}, {n}: {fit:?}",
                );
            }
        }
    }

    #[test]
    fn misfit_events_may_change_tone_once() {
        // tone 121 Hz before t = 10 s, 131 Hz after (a prop RPM change), on a Doppler-like ratio
        let ratio = |t: f64| 1.0 + 0.05 * (-t / 20.0).tanh();
        let tt: Vec<f64> = (0..480).map(|k| -60.0 + 0.25 * k as f64).collect();
        let ff: Vec<f64> = tt
            .iter()
            .map(|&t| if t < 10.0 { 121.0 } else { 131.0 } * ratio(t) + 0.03 * ((t * 7.0).sin()))
            .collect();
        let mut scratch = TraceScratch::default();
        let single = trace_chi(
            &tt,
            &ff,
            &ratio,
            0.0,
            &plain_event(126.0, false),
            1.0,
            0.12,
            5.0,
            true,
            &mut scratch,
        );
        let split = trace_chi(
            &tt,
            &ff,
            &ratio,
            0.0,
            &plain_event(126.0, true),
            1.0,
            0.12,
            5.0,
            true,
            &mut scratch,
        );
        assert!(
            single.chi > 10.0 && single.brk.is_none(),
            "single {single:?}"
        );
        let (t_brk, f_after) = split.brk.expect("tone change found");
        assert!(split.chi < 1.0, "split {split:?}");
        assert!(
            (t_brk - 10.0).abs() <= 0.5
                && (split.f0 - 121.0).abs() < 0.1
                && (f_after - 131.0).abs() < 0.1,
            "{split:?}"
        );
        // a clean tone is not split
        let ff1: Vec<f64> = tt.iter().map(|&t| 126.0 * ratio(t)).collect();
        let one = trace_chi(
            &tt,
            &ff1,
            &ratio,
            0.0,
            &plain_event(126.0, true),
            1.0,
            0.12,
            5.0,
            true,
            &mut scratch,
        );
        assert!(one.chi < 1e-6, "{one:?}");
    }

    #[test]
    fn source_tone_conversion_preserves_bounds_and_invalid_scale_refusal() {
        let tt: Vec<f64> = (0..200).map(|k| -60.0 + 0.6 * k as f64).collect();
        let ratio = |t| cv_curve(t, 1.0, 110.0, 2500.0, 0.0, 343.0);
        let ev = plain_event(100.0, false);
        let mut scratch = TraceScratch::default();
        for scale in [0.8, 1.0, 1.2] {
            let ff: Vec<f64> = tt.iter().map(|&t| scale * 100.0 * ratio(t)).collect();
            for robust in [false, true] {
                let fit = trace_chi(
                    &tt,
                    &ff,
                    &ratio,
                    0.0,
                    &ev,
                    scale,
                    0.2,
                    5.0,
                    robust,
                    &mut scratch,
                );
                assert!((fit.f0 - scale * 100.0).abs() < 1e-10);
                assert!(fit.chi < 1e-15);
                assert_eq!(fit.cov, 1.0);
                let outside: Vec<f64> = tt.iter().map(|&t| scale * 120.0 * ratio(t)).collect();
                let bounded = trace_chi(
                    &tt,
                    &outside,
                    &ratio,
                    0.0,
                    &ev,
                    scale,
                    0.2,
                    5.0,
                    robust,
                    &mut scratch,
                );
                assert!((bounded.f0 - scale * 105.0).abs() < 1e-10);
                assert_eq!(
                    bounded.chi, 25.0,
                    "the unchanged tone interval remains binding"
                );
            }
        }
        for scale in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
            let fit = trace_chi(
                &tt,
                &tt,
                &ratio,
                0.0,
                &ev,
                scale,
                0.2,
                5.0,
                true,
                &mut scratch,
            );
            assert!(fit.chi.is_infinite() && fit.f0.is_nan());
        }
        let ff: Vec<f64> = tt.iter().map(|&t| 120.0 * ratio(t)).collect();
        let missing = trace_chi(
            &tt,
            &ff,
            &|_| f64::NAN,
            0.0,
            &ev,
            1.2,
            0.2,
            5.0,
            true,
            &mut scratch,
        );
        assert!(missing.chi.is_infinite());
        assert_eq!(missing.cov, 0.0);
    }

    #[test]
    fn source_tone_conversion_preserves_the_change_point_bounds() {
        let scale = 1.2;
        let ratio = |t: f64| 1.0 + 0.05 * (-t / 20.0).tanh();
        let tt: Vec<f64> = (0..480).map(|k| -60.0 + 0.25 * k as f64).collect();
        let ff: Vec<f64> = tt
            .iter()
            .map(|&t| scale * if t < 10.0 { 121.0 } else { 131.0 } * ratio(t))
            .collect();
        let fit = trace_chi(
            &tt,
            &ff,
            &ratio,
            0.0,
            &plain_event(126.0, true),
            scale,
            0.12,
            5.0,
            true,
            &mut TraceScratch::default(),
        );
        let (at, after) = fit.brk.expect("the same permitted RPM change survives conversion");
        assert!(fit.chi < 1e-15);
        assert!((at - 10.0).abs() <= 0.25);
        assert!((fit.f0 - scale * 121.0).abs() < 1e-9);
        assert!((after - scale * 131.0).abs() < 1e-9);
    }

    #[test]
    fn search_json_keeps_the_best_candidates_and_counts_the_rest() {
        let mut result = SearchResult {
            funnel: Funnel::default(),
            nulls: vec![],
            candidates: vec![Candidate::default(); DUMP_CANDIDATES + 3],
            refine: None,
            margin: serde_json::json!({"margin_nats": 7.5}),
            warnings: vec![],
        };
        result.candidates[0].score = 42.0;
        result.candidates[DUMP_CANDIDATES].score = -1.0;
        let dump = serde_json::to_value(result.dump()).unwrap();
        let kept = dump["candidates"].as_array().unwrap();
        assert_eq!(kept.len(), DUMP_CANDIDATES);
        assert_eq!(dump["candidates_total"], DUMP_CANDIDATES + 3);
        assert_eq!(kept[0]["score"], 42.0, "the answer stays at the front");
        assert!(kept.iter().all(|c| c["score"] != -1.0), "the rest are cut");
        assert_eq!(dump["margin"]["margin_nats"], 7.5);

        result.candidates.truncate(2);
        let dump = serde_json::to_value(result.dump()).unwrap();
        assert_eq!(dump["candidates"].as_array().unwrap().len(), 2);
        assert_eq!(dump["candidates_total"], 2);
    }

    #[test]
    fn margin_requires_a_candidate_and_complete_comparison() {
        let mut result = SearchResult {
            funnel: Funnel::default(),
            nulls: vec![],
            candidates: vec![Candidate::default()],
            refine: None,
            margin: serde_json::json!({"margin_nats": null}),
            warnings: vec![],
        };
        assert!(result.margin_passes(3.0));
        assert!(result.margin_passes(6.0));
        for stage in ["pairs", "fine", "confirmers"] {
            result.funnel.truncated = vec![format!("{stage}: 2 -> 1")];
            assert!(!result.margin_passes(3.0), "{stage}");
            assert!(!result.margin_passes(6.0), "{stage}");
        }

        // The same 3/6-nat thresholds apply once the comparison is complete.
        for truncated in [false, true] {
            result.funnel.truncated = if truncated {
                vec!["fine: 2 -> 1".into()]
            } else {
                vec![]
            };
            for (nats, medium, high) in [
                (2.9, false, false),
                (3.0, true, false),
                (5.9, true, false),
                (6.0, true, true),
            ] {
                result.margin = serde_json::json!({"margin_nats": nats});
                assert_eq!(result.margin_passes(3.0), medium && !truncated);
                assert_eq!(result.margin_passes(6.0), high && !truncated);
            }
        }

        // Neither an otherwise sufficient margin nor an absent rival supplies a winner.
        result.candidates.clear();
        assert!(!result.margin_passes(3.0));
        assert!(!result.margin_passes(6.0));
        result.funnel.truncated.clear();
        result.margin = serde_json::json!({"margin_nats": null});
        assert!(!result.margin_passes(3.0));
        assert!(!result.margin_passes(6.0));

        // A missing or malformed record is not an explicit no-rival result.
        result.candidates.push(Candidate::default());
        for margin in [
            serde_json::json!({}),
            serde_json::json!({"margin_nats": "unavailable"}),
        ] {
            result.margin = margin;
            assert!(!result.margin_passes(3.0));
            assert!(!result.margin_passes(6.0));
        }
    }

    #[test]
    fn margin_from_incomplete_confirmer_scores_cannot_certify_a_winner() {
        // The first candidate has two strong matches. Its rival's two initial matches are
        // weak, but two unchecked confirmers will make the rival competitive. Use the actual
        // empirical evidence calculation, without the extrapolated tail or thin-null fallback.
        let nulls: Vec<EventNull> = ["e1", "e2", "e3", "e4"]
            .into_iter()
            .map(|event| EventNull {
                event: event.into(),
                chi_null: vec![1.0; 40],
                n_null: 40,
                chi_null_p05: 1.0,
                chi_null_median: 1.0,
                ..Default::default()
            })
            .collect();
        let candidate = |flight: &str, fits: &[(&str, f64)]| {
            let mut events: Vec<EventMatch> = fits
                .iter()
                .map(|&(event, chi)| EventMatch {
                    event: event.into(),
                    flight: format!("{flight}-{event}"),
                    chi,
                    dt_free: Some(0.0),
                    ..Default::default()
                })
                .collect();
            let score = evidence(&mut events, &nulls, 0);
            Candidate {
                n_explained: events.len(),
                events,
                score,
                ..Default::default()
            }
        };
        let winner = candidate("winner", &[("e1", 0.5), ("e2", 0.5)]);
        let partial_rival = candidate("rival", &[("e1", 2.0), ("e2", 2.0)]);
        let complete_rival = candidate(
            "rival",
            &[("e1", 2.0), ("e2", 2.0), ("e3", 0.5), ("e4", 0.5)],
        );
        assert!(winner.events.iter().all(|m| m.core));
        assert!(!same_explanation(&winner, &partial_rival));
        let observed_margin = winner.score - partial_rival.score;
        assert!(observed_margin >= 6.0, "{observed_margin}");
        assert!(complete_rival.score > winner.score);
        let complete_margin = complete_rival.score - winner.score;
        assert!(complete_margin < 3.0, "{complete_margin}");

        let mut result = SearchResult {
            funnel: Funnel {
                truncated: vec!["confirmers: 2 -> 1".into()],
                ..Default::default()
            },
            nulls,
            candidates: vec![winner.clone(), partial_rival],
            refine: None,
            margin: serde_json::json!({"margin_nats": observed_margin}),
            warnings: vec![],
        };
        for threshold in [3.0, 6.0] {
            assert!(
                !result.margin_passes(threshold),
                "the provisional {observed_margin:.2}-nat gap hides a rival that overtakes the winner"
            );
        }

        // Completing the comparison both changes the winner and reveals the small true gap.
        result.funnel.truncated.clear();
        result.candidates = vec![complete_rival, winner];
        result.margin = serde_json::json!({"margin_nats": complete_margin});
        assert!(!result.margin_passes(3.0));
        assert!(!result.margin_passes(6.0));
    }

    #[test]
    fn finalization_checks_every_promoted_leader() {
        let m = |event: &str, flight: String| EventMatch {
            event: event.into(),
            flight,
            ..Default::default()
        };
        let mut cands: Vec<Candidate> = (0..180)
            .map(|i| Candidate {
                events: vec![m("e1", "A".into()), m("e2", format!("coincidence{i}"))],
                n_explained: 2,
                score: 1000.0 - i as f64,
                ..Default::default()
            })
            .collect();
        // Two separate passes by the same aircraft are permitted; independence is between
        // events, not aircraft identifiers. Both events still have to survive clock checks.
        cands.push(Candidate {
            events: vec![m("e3", "B".into()), m("e4", "B".into())],
            n_explained: 2,
            score: 1.0,
            ..Default::default()
        });
        let checked = AtomicUsize::new(0);
        let out = finalize_candidates(cands, &|cand| {
            checked.fetch_add(1, Ordering::Relaxed);
            if cand.events[0].event == "e1" {
                cand.events.pop();
                cand.n_explained = 1;
            }
        });
        assert_eq!(checked.load(Ordering::Relaxed), 181);
        assert_eq!(out.len(), 1);
        assert!(out[0].lock_checked);
        assert_eq!(out[0].events[0].event, "e3");
        assert_eq!(out[0].n_explained, 2);
    }

    fn refinement_at(lat: f64, lon: f64, t_start_utc: f64) -> RefineResult {
        RefineResult {
            lat,
            lon,
            t_start_utc,
            radius_m: 75.0,
            radius_basis: "synthetic test point".into(),
            step_m: 25.0,
            lats: vec![lat],
            lons: vec![lon],
            dchi2: vec![vec![0.0]],
            radius_lower_bound: false,
            region_cells: 1,
        }
    }

    #[test]
    fn refinement_reranks_promoted_leaders_without_mixing_candidate_state() {
        let candidates: Vec<Candidate> = [("A", 10.0), ("B", 9.0), ("C", 6.0), ("D", 1.0)]
            .into_iter()
            .enumerate()
            .map(|(i, (flight, score))| Candidate {
                lat: 40.0 + i as f64 * 0.1,
                lon: -73.0,
                t_start_utc: 1000.0,
                score,
                lock_checked: true,
                n_explained: 2,
                events: ["e1", "e2"]
                    .into_iter()
                    .map(|event| EventMatch {
                        event: event.into(),
                        flight: flight.into(),
                        ..EventMatch::default()
                    })
                    .collect(),
                ..Candidate::default()
            })
            .collect();
        // B wins after rescoring; B's refinement fails; or rescoring B promotes the
        // already-refined A again. A cached radius must always travel with its candidate.
        // If B cannot refine, A's better fine-grid score already defeats its margin; C need
        // not be refined before that comparison can stop.
        for (a_score, b_score, winner, has_radius, tried) in [
            (5.0, Some(8.0), "B", true, &["A", "B", "C"][..]),
            (5.0, None, "B", false, &["A", "B"][..]),
            (8.0, Some(7.0), "A", true, &["A", "B"][..]),
        ] {
            let mut checked = Vec::new();
            let (out, refined) = refine_candidates(candidates.clone(), &|_| {}, |c| {
                let flight = c.events[0].flight.as_str();
                checked.push(flight.to_string());
                let score = match flight {
                    "A" => a_score,
                    "B" => b_score?,
                    "C" => c.score,
                    _ => panic!("a candidate below the best rival need not be refined"),
                };
                let updated = Candidate {
                    lat: c.lat + 0.001,
                    t_start_utc: c.t_start_utc + 0.375,
                    score,
                    ..c.clone()
                };
                let r = refinement_at(updated.lat, updated.lon, updated.t_start_utc);
                Some((updated, r))
            });
            assert_eq!(checked, tried);
            assert_eq!(out[0].events[0].flight, winner);
            assert!(out.windows(2).all(|w| w[0].score >= w[1].score));
            assert_eq!(refined.is_some(), has_radius);
            if let Some(r) = refined {
                assert_eq!((out[0].lat, out[0].lon), (r.lat, r.lon));
                assert_eq!(out[0].t_start_utc, r.t_start_utc);
            } else {
                assert_eq!(out[0].lat, candidates[1].lat);
                assert_eq!(out[0].t_start_utc, candidates[1].t_start_utc);
                assert_eq!(out[0].score, candidates[1].score);
            }
        }
    }

    /// Metamorphic: a gain only one side could have cannot move the margin, whatever the gain is.
    /// Refined against refined, or both at their fine cells when either was refused.
    #[test]
    fn margin_compares_the_leader_and_its_rival_at_the_same_polish() {
        let candidate = |flight: &str, score: f64, lat: f64| Candidate {
            lat,
            lon: -73.0,
            t_start_utc: 1000.0,
            score,
            lock_checked: true,
            n_explained: 2,
            events: ["e1", "e2"]
                .into_iter()
                .map(|event| EventMatch {
                    event: event.into(),
                    flight: flight.into(),
                    ..EventMatch::default()
                })
                .collect(),
            ..Candidate::default()
        };
        // A's neighbour has the same flights about 1 km away: position ambiguity, not a rival.
        let candidates = vec![
            candidate("A", 30.0, 40.0),
            candidate("A", 29.9, 40.01),
            candidate("B", 28.0, 41.0),
            candidate("C", 27.0, 42.0),
        ];
        let run = |gains: &[(&str, Option<f64>)]| {
            let mut tried = Vec::new();
            let (out, refined) = refine_candidates(candidates.clone(), &|_| {}, |c| {
                let flight = c.events[0].flight.clone();
                tried.push(flight.clone());
                let (_, gain) = gains
                    .iter()
                    .find(|(f, _)| *f == flight)
                    .expect("only the flights a case lists are refined");
                let score = c.score + (*gain)?;
                Some((
                    Candidate { score, ..c.clone() },
                    refinement_at(c.lat, c.lon, c.t_start_utc),
                ))
            });
            let leader = out[0].events[0].flight.clone();
            (tried, leader, margin_report(&out), refined.is_some())
        };
        let nats = |report: &serde_json::Value| report["margin_nats"].as_f64().unwrap();
        for gain in [0.0, 0.5, 2.0] {
            let (tried, leader, report, has_radius) = run(&[("A", Some(gain)), ("B", Some(gain))]);
            assert_eq!(tried, ["A", "B"]);
            assert_eq!(leader, "A");
            assert!((nats(&report) - 2.0).abs() < 1e-12, "gain {gain}: {report}");
            assert_eq!(report["margin_basis"], "refined");
            assert!(has_radius);

            // A refused rival: the leader keeps its radius and reported score, not a margin the
            // rival never had the chance to answer.
            let (tried, leader, report, has_radius) = run(&[("A", Some(gain)), ("B", None)]);
            assert_eq!(tried, ["A", "B"]);
            assert_eq!(leader, "A");
            assert!((nats(&report) - 2.0).abs() < 1e-12, "gain {gain}: {report}");
            assert_eq!(report["margin_basis"], "fine_grid");
            assert_eq!(report["margin_top_score"], 30.0);
            assert_eq!(report["margin_rival_score"], 28.0);
            assert_eq!(report["top_score"], 30.0 + gain);
            assert!(has_radius);

            // A refused leader: the rival's gain does not count against it either.
            let (tried, leader, report, has_radius) = run(&[("A", None), ("B", Some(gain))]);
            assert_eq!(tried, ["A", "B"]);
            assert_eq!(leader, "A");
            assert!((nats(&report) - 2.0).abs() < 1e-12, "gain {gain}: {report}");
            assert_eq!(report["margin_basis"], "fine_grid");
            assert!(!has_radius);
        }
        // A rival that overtakes when refined takes the lead, with its own radius. At the fine
        // cells it trails A's neighbour, a different explanation from it, and leads C by 1 nat,
        // closer than A's 3 refined, so both get a refinement attempt before the margin is read.
        let (tried, leader, report, has_radius) =
            run(&[("A", Some(0.0)), ("B", Some(5.0)), ("C", Some(0.0))]);
        assert_eq!(tried, ["A", "B", "A", "C"]);
        assert_eq!(leader, "B");
        assert!((nats(&report) - 3.0).abs() < 1e-12, "{report}");
        assert_eq!(report["margin_basis"], "refined");
        assert!(has_radius);
        // Leading only because the other side was refused is no margin at all.
        let (tried, leader, report, has_radius) = run(&[("A", None), ("B", Some(5.0))]);
        assert_eq!(tried, ["A", "B"]);
        assert_eq!(leader, "B");
        assert!((nats(&report) + 2.0).abs() < 1e-12, "{report}");
        assert_eq!(report["margin_basis"], "fine_grid");
        assert!(has_radius);
        // A rival that falls when refined yields to the next one, which is then refined too.
        let (tried, leader, report, _) =
            run(&[("A", Some(1.0)), ("B", Some(-5.0)), ("C", Some(1.0))]);
        assert_eq!(tried, ["A", "B", "C"]);
        assert_eq!(leader, "A");
        assert!((nats(&report) - 3.0).abs() < 1e-12, "{report}");
        assert_eq!(report["margin_basis"], "refined");
    }

    #[test]
    fn refinement_reaches_rivals_hidden_by_earlier_gains() {
        let candidates: Vec<Candidate> = [("A", 30.0), ("B", 29.8), ("C", 29.7), ("D", 20.0)]
            .into_iter()
            .enumerate()
            .map(|(i, (flight, score))| Candidate {
                lat: 40.0 + i as f64 * 0.1,
                lon: -73.0,
                t_start_utc: 1000.0,
                score,
                lock_checked: true,
                n_explained: 2,
                events: ["e1", "e2"]
                    .into_iter()
                    .map(|event| EventMatch {
                        event: event.into(),
                        flight: flight.into(),
                        ..EventMatch::default()
                    })
                    .collect(),
                ..Candidate::default()
            })
            .collect();
        // A rises to 35 and B to 30. Comparing their refined scores gives 5 nats, but C
        // still trails A by only 0.3 at their fine cells. It must get a refinement attempt:
        // it may fail, remain behind B, or become the winner with its own place and radius.
        for (c_score, winner, margin, basis) in [
            (None, "A", 0.3, "fine_grid"),
            (Some(29.9), "A", 5.0, "refined"),
            (Some(36.0), "C", 1.0, "refined"),
        ] {
            let mut attempted = Vec::new();
            let (out, refined) = refine_candidates(
                candidates.clone(),
                &|_| panic!("these candidates already passed the clock check"),
                |c| {
                    let flight = c.events[0].flight.as_str();
                    attempted.push(flight.to_string());
                    let score = match flight {
                        "A" => 35.0,
                        "B" => 30.0,
                        "C" => c_score?,
                        _ => panic!("D cannot limit the margin"),
                    };
                    let updated = Candidate {
                        lat: c.lat + 0.001,
                        t_start_utc: c.t_start_utc + 0.375,
                        score,
                        ..c.clone()
                    };
                    let r = refinement_at(updated.lat, updated.lon, updated.t_start_utc);
                    Some((updated, r))
                },
            );
            assert_eq!(attempted, ["A", "B", "C"]);
            assert_eq!(out[0].events[0].flight, winner);
            assert!(out.windows(2).all(|w| w[0].score >= w[1].score));
            let r = refined.expect("the reported winner was refined");
            assert_eq!((out[0].lat, out[0].lon), (r.lat, r.lon));
            assert_eq!(out[0].t_start_utc, r.t_start_utc);
            let report = margin_report(&out);
            assert!(
                (report["margin_nats"].as_f64().unwrap() - margin).abs() < 1e-12,
                "{report}"
            );
            assert_eq!(report["margin_basis"], basis);
            if c_score.is_none() {
                // C stays below B in current-score order, but its fine-grid comparison
                // still controls the margin. A retains its coherent refined answer.
                assert_eq!(out[1].events[0].flight, "B");
                assert_eq!(out[2].events[0].flight, "C");
                assert_eq!(report["top_score"], 35.0);
                assert_eq!(report["margin_top_score"], 30.0);
                assert_eq!(report["margin_rival_score"], 29.7);
                assert_eq!(report["next_distinct_score"], 29.7);
                assert_eq!(out[2].lat, candidates[2].lat);
                assert_eq!(out[2].t_start_utc, candidates[2].t_start_utc);
                assert!(out[2].fine_score.is_none());
            }
        }
    }

    #[test]
    fn refinement_checks_newly_promoted_coarse_candidates_before_fallback() {
        let candidates = ["A", "B", "C", "D"]
            .into_iter()
            .enumerate()
            .map(|(i, flight)| Candidate {
                score: 10.0 - i as f64,
                t_start_utc: 1000.0,
                lock_checked: i < 2,
                n_explained: 2,
                events: ["e1", "e2"]
                    .into_iter()
                    .map(|event| EventMatch {
                        event: event.into(),
                        flight: flight.into(),
                        ..EventMatch::default()
                    })
                    .collect(),
                ..Candidate::default()
            })
            .collect();
        let checked = AtomicUsize::new(0);
        let mut attempted = Vec::new();
        let (out, refined) = refine_candidates(
            candidates,
            &|c| {
                checked.fetch_add(1, Ordering::Relaxed);
                match c.events[0].flight.as_str() {
                    "C" => {
                        c.events.pop();
                        c.n_explained = 1;
                    }
                    "D" => {}
                    _ => panic!("previously checked candidates must not be rechecked"),
                }
            },
            |c| {
                let flight = c.events[0].flight.as_str();
                attempted.push(flight.to_string());
                let score = match flight {
                    "A" => 2.0,
                    "B" => 3.0,
                    "D" => return None,
                    _ => panic!("a rejected one-event candidate cannot be refined"),
                };
                Some((
                    Candidate { score, ..c.clone() },
                    refinement_at(c.lat, c.lon, c.t_start_utc),
                ))
            },
        );
        assert_eq!(checked.load(Ordering::Relaxed), 2);
        assert_eq!(attempted, ["A", "B", "D"]);
        assert_eq!(out[0].events[0].flight, "D");
        assert!(out[0].lock_checked);
        assert_eq!(out[0].n_explained, 2);
        assert_eq!(out[0].score, 7.0);
        assert!(refined.is_none());
        assert!(out.iter().all(|c| c.events[0].flight != "C"));
    }

    #[test]
    fn clock_profile_keeps_subcell_minima_and_final_observations() {
        for base in [0.0, 1_788_895_000.0] {
            for (lo, hi) in [(-3.2, 4.1), (-21.73, 401.9), (-401.9, 21.73)] {
                let mut last = f64::NAN;
                let (clock, cost) = profile_clock(base + lo, base + hi, false, |t| {
                    last = t;
                    // Independently known minimum; an asymmetric broad window makes it
                    // miss the coarse lattice without changing the observed objective.
                    (t - base).powi(2)
                })
                .unwrap();
                assert!(
                    (clock - base).abs() < 0.01,
                    "base {base}, window {lo}..{hi}: clock error {} s",
                    clock - base
                );
                assert_eq!(last, clock);
                assert_eq!(cost, (clock - base).powi(2));
            }
        }
        assert!(profile_clock(0.0, 1000.0, false, |_| f64::INFINITY).is_none());
        let mut last = 0.0;
        assert_eq!(
            profile_clock(12.0, 12.0, false, |t| {
                last = t;
                7.0
            }),
            Some((12.0, 7.0))
        );
        assert_eq!(last, 12.0);
    }

    #[test]
    fn broad_clock_profiles_polish_the_whole_scanned_cell() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let mut case = two_flyover_case(med);
        let dem = FlatElevation(0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        let o = observer(case.obs.0, case.obs.1, &dem);
        let assign = [(0, 0, case.t_s + 1000.0), (1, 1, case.t_s + 1900.0)];
        // Broad fit intervals and an asymmetric recording-start bound put the true clock
        // between samples after the 400-cell limit enlarges their nominal 0.25 s spacing.
        // Keep the exact same observed flyovers; uncertainty must not bias their best clock.
        for event in &mut case.events {
            event.t_tol = 600.0;
        }
        for phase in [0.0, 0.2, 0.7] {
            case.p.start_lo = case.t_s - 20.73 - phase;
            case.p.start_hi = case.t_s + 400.9 - phase;
            for weights in [None, Some(&[1.0, 0.3][..])] {
                let fitted = eval_point_weighted(
                    &o,
                    &assign,
                    &case.events,
                    &case.tracks,
                    &case.p,
                    &atmo,
                    true,
                    true,
                    false,
                    weights,
                )
                .expect("both physical flyovers remain eligible");
                assert!(
                    (fitted.t_s - case.t_s).abs() < 0.05,
                    "phase {phase}, weights {weights:?}: clock error {} s",
                    fitted.t_s - case.t_s
                );
                assert!(fitted.chis.iter().all(|fit| fit.chi < 0.1));
            }
        }
    }

    // Starts from a cell whose clock is seconds off. The searches here cannot tell the clock
    // fitted at the refined point from the grid centre's: they all pass if `refine_on` reports it.
    #[test]
    fn refinement_fits_the_clock_at_the_point_it_reports() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let mut case = two_flyover_case(med);
        case.p.refine_half_m = 800.0;
        case.p.refine_step_m = 50.0;
        let dem = FlatElevation(0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        let assign = [(0, 0, case.t_s + 1000.0), (1, 1, case.t_s + 1900.0)];
        let (_, mlon) = fdop_types::geo::m_per_deg(case.obs.0);
        // A winning cell 600 m along flight A's eastbound path, where the clock reads differently.
        let lon = case.obs.1 + 600.0 / mlon;
        let cell = observer(case.obs.0, lon, &dem);
        let cell_clock = eval_point(
            &cell,
            &assign,
            &case.events,
            &case.tracks,
            &case.p,
            &atmo,
            true,
            false,
            false,
        )
        .unwrap()
        .t_s;
        assert!(
            (cell_clock - case.t_s).abs() > 1.0,
            "test must start from a cell whose clock is off: {:+.2} s",
            cell_clock - case.t_s
        );
        let r = refine_on(
            case.obs.0,
            lon,
            &assign,
            &case.events,
            &case.tracks,
            &case.p,
            &dem,
            &atmo,
            &mut None,
        )
        .unwrap();
        assert!(
            haversine_m(r.lat, r.lon, case.obs.0, case.obs.1) < 60.0,
            "refinement must recover the synthetic observer: {r:?}"
        );
        assert!(
            (r.t_start_utc - case.t_s).abs() < 0.5,
            "the start must be the one fitted at the refined point: {:+.2} s, cell {:+.2} s",
            r.t_start_utc - case.t_s,
            cell_clock - case.t_s
        );
    }

    #[test]
    fn refined_reporting_recomputes_physics_evidence_and_veto_at_one_fixed_clock() {
        struct Slope(f64, f64);
        impl Elevation for Slope {
            fn elev_m(&self, lat: f64, lon: f64) -> f64 {
                let (mlat, mlon) = fdop_types::geo::m_per_deg(self.0);
                137.0 + 0.03 * (lon - self.1) * mlon + 0.02 * (lat - self.0) * mlat
            }
            fn describe(&self) -> String {
                "synthetic slope".into()
            }
        }
        let med = fdop_types::atmo::Medium::still(343.0);
        let mut case = two_flyover_case(med);
        let t_origin = case.t_s;
        case.t_s += 0.137;
        let dem = Slope(case.obs.0, case.obs.1);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        let frame = Enu::new(case.obs.0, case.obs.1, 0.0);
        let truth = observer(case.obs.0, case.obs.1, &dem);
        for (k, (f0, offset)) in [(112.7, 1000.0), (129.5, 1900.0)].into_iter().enumerate() {
            case.events[k] = synth_event(
                &format!("e{}", k + 1),
                &case.tracks[k],
                truth.x,
                case.t_s,
                f0,
                343.0,
                t_origin + offset,
                &Prop::still(343.0),
            );
        }
        // This pass is just outside the unheard-pass threshold at the old point, but
        // inside it at the refined point. A stale veto would incorrectly remain clear.
        let mut close = straight_track(
            &frame,
            [0.0, 1200.0, 600.0],
            [90.0, 0.0, 0.0],
            t_origin + 2400.0,
            t_origin + 2000.0,
            t_origin + 2800.0,
            5.0,
        );
        close.key = "C".into();
        case.tracks.push(close);
        let tix = TimeIndex::new(&case.tracks);
        let inputs = SearchInputs {
            events: case.events.clone(),
            tracks: &case.tracks,
            heard_times: vec![],
            coarse_dem: &dem,
            fine_dem: &dem,
            atmo: &atmo,
        };
        let (lat, lon, _) = frame.to_geodetic([70.0, -100.0, 0.0]);
        let mut top = explain_at(
            &inputs,
            &case.p,
            lat,
            lon,
            case.t_s,
            10.0,
            &[("e1".into(), "A".into()), ("e2".into(), "B".into())],
        )
        .unwrap();
        let nulls: Vec<EventNull> = case
            .events
            .iter()
            .map(|e| EventNull {
                event: e.id.clone(),
                gate_nats: 6.0,
                ..EventNull::default()
            })
            .collect();
        top.unheard_close_passes = unheard_passes(
            &top,
            &observer(lat, lon, &dem),
            &case.tracks,
            &tix,
            &[],
            &case.p,
            &atmo,
        );
        top.score = evidence(&mut top.events, &nulls, top.unheard_close_passes.len());
        assert!(top.unheard_close_passes.is_empty());
        assert!((top.t_start_utc - case.t_s).abs() > 0.2);
        assert!((top.elev_m - truth.z).abs() > 0.05);
        let refined = refinement_at(case.obs.0, case.obs.1, case.t_s);
        let fresh =
            refresh_refined_candidate(&top, &refined, &inputs, &case.events, &case.p, &nulls, &tix)
                .unwrap();
        assert_eq!((fresh.lat, fresh.lon), case.obs);
        assert_eq!(fresh.t_start_utc, case.t_s);
        assert_eq!(fresh.elev_m, truth.z);
        assert_eq!(fresh.n_explained, 2);
        assert!(
            fresh
                .events
                .iter()
                .all(|m| m.core && m.dt_free.unwrap().abs() < 0.25)
        );
        assert_eq!(fresh.unheard_close_passes.len(), 1);
        assert!(fresh.unheard_close_passes[0].starts_with("C "));
        assert_eq!(fresh.score, 12.0 - UNHEARD_NATS);
        assert!(
            fresh.score < top.score,
            "the refreshed veto must change evidence"
        );

        // Evaluate the regenerated matches using direct emission-time solves, independent
        // of the refresh path's RatioGrid interpolation and the old point's fitted clock.
        let mut scratch = TraceScratch::default();
        for (&(ei, ti), matched) in fresh.trk.iter().zip(&fresh.events) {
            let ev = &case.events[ei];
            let tr = &case.tracks[ti as usize];
            let te = t_origin + if ei == 0 { 1000.0 } else { 1900.0 };
            let cpa = tr.cpa_near(truth.x, te, 150.0).unwrap();
            let prop = prop_for(&atmo, &truth, tr, cpa.t_e);
            let expected = trace_chi(
                &ev.tr_t,
                &ev.tr_f,
                &|tu| tr.solve_emission_p(tu, truth.x, &prop, None).unwrap().2,
                case.t_s,
                ev,
                prop.source_tone_scale(cpa.vel),
                ev.sigma_hz,
                case.p.cap_sigma,
                true,
                &mut scratch,
            );
            assert!((matched.chi - expected.chi).abs() < 0.002);
            assert!((matched.rms_hz - expected.rms).abs() < 0.0001);
            assert!((matched.f0 - expected.f0).abs() < 0.001);
            assert!((matched.d - cpa.d).abs() < 0.01);
            assert!((matched.t_cpa_heard_utc - cpa.t_r_p(&prop)).abs() < 0.002);
        }
        // The same physically supported clock must be refused if it lies outside the
        // requested interval, while retaining the evaluator's existing one-second slack.
        for (lo, hi, allowed) in [
            (case.t_s + 1.25, case.t_s + 10.0, false),
            (case.t_s - 10.0, case.t_s - 1.25, false),
            (case.t_s + 1.0, case.t_s + 10.0, true),
            (case.t_s - 10.0, case.t_s - 1.0, true),
        ] {
            let mut bounded = case.p.clone();
            bounded.start_lo = lo;
            bounded.start_hi = hi;
            assert_eq!(
                refresh_refined_candidate(
                    &top,
                    &refined,
                    &inputs,
                    &case.events,
                    &bounded,
                    &nulls,
                    &tix,
                )
                .is_some(),
                allowed,
                "permitted clock interval {lo}..{hi}"
            );
        }
        let mut unsupported = case.events.clone();
        for f in &mut unsupported[1].tr_f {
            *f += 30.0;
        }
        assert!(refresh_refined_candidate(
            &top, &refined, &inputs, &unsupported, &case.p, &nulls, &tix,
        ).is_none(), "losing a locating trace must discard the refined report");
        // A locating match may sit just past a gate bound that the refined search never applied,
        // but not far past it. Still air, so the gates are not rescaled and the bound is exact.
        let d = fresh.events.iter().find(|m| m.event == "e2").unwrap().d;
        for (overrun, allowed) in [(1.04, true), (1.06, false)] {
            let mut tight = case.events.clone();
            tight[1].d_gate[1] = d / overrun;
            assert_eq!(
                refresh_refined_candidate(&top, &refined, &inputs, &tight, &case.p, &nulls, &tix)
                    .is_some(),
                allowed,
                "locating match {overrun} times its upper distance bound"
            );
        }
    }

    /// Metamorphic: tightening a gate so the true point sits 1 % past it, while a grid cell still
    /// passes, must not change the answer. The search reaches the same observer either way.
    #[test]
    fn refinement_survives_a_gate_bound_just_inside_its_minimum() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let open = run_case(&two_flyover_case(med), med);
        let open_refined = open.refine.as_ref().expect("the open case refines");
        let d = open.candidates[0]
            .events
            .iter()
            .find(|m| m.event == "e2")
            .unwrap()
            .d;
        let mut case = two_flyover_case(med);
        case.events[1].d_gate[1] = d / 1.01;
        let res = run_case(&case, med);
        let r = res
            .refine
            .as_ref()
            .unwrap_or_else(|| panic!("refinement was discarded: {:?}", res.warnings));
        assert_eq!(
            (res.candidates[0].lat, res.candidates[0].lon),
            (r.lat, r.lon)
        );
        assert!(
            haversine_m(r.lat, r.lon, open_refined.lat, open_refined.lon) < 10.0,
            "same observer expected: {:.6},{:.6} vs {:.6},{:.6}",
            r.lat,
            r.lon,
            open_refined.lat,
            open_refined.lon
        );
        let m = res.candidates[0]
            .events
            .iter()
            .find(|m| m.event == "e2")
            .unwrap();
        assert!(m.core && m.d > case.events[1].d_gate[1], "{m:?}");
    }

    #[test]
    fn single_event_impostor_cannot_block_clock_consistent_pair() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let mut case = two_flyover_case(med);
        let enu = Enu::new(case.obs.0, case.obs.1, 0.0);
        let prop = Prop::from_medium(&med, &enu);
        let true_clock = case.t_s + 8.0;
        // This weak event has an uncertain CPA time but its full trace prefers a clock 8 s
        // later than A. It can pass pair gates with A, then fails the independent clock check.
        let b = &mut case.events[1];
        b.t_tol = 12.0;
        b.sigma_hz = 3.0;
        for t in b.tr_t.iter_mut().chain(&mut b.q_t) {
            *t -= 8.0;
        }
        let mut c = straight_track(
            &enu,
            [-3000.0, 0.0, 1200.0],
            [0.0, -112.0, 0.0],
            case.t_s + 2400.0,
            case.t_s + 1800.0,
            case.t_s + 3000.0,
            5.0,
        );
        c.key = "C".into();
        case.events.push(synth_event(
            "e3",
            &c,
            enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]),
            true_clock,
            120.0,
            343.0,
            case.t_s + 2400.0,
            &prop,
        ));
        case.tracks.push(c);
        let dem = FlatElevation(0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        let inp = SearchInputs {
            events: case.events.clone(),
            tracks: &case.tracks,
            heard_times: vec![],
            coarse_dem: &dem,
            fine_dem: &dem,
            atmo: &atmo,
        };
        let explain = |pairs: &[(&str, &str)], clock| {
            explain_at(
                &inp,
                &case.p,
                case.obs.0,
                case.obs.1,
                clock,
                20.0,
                &pairs
                    .iter()
                    .map(|(e, f)| (e.to_string(), f.to_string()))
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };
        let mut impostor = explain(&[("e1", "A"), ("e2", "B")], case.t_s);
        let mut valid = explain(&[("e2", "B"), ("e3", "C")], true_clock);
        assert!(impostor.events.iter().all(|m| m.chi <= case.p.kappa));
        // Controlled evidence values make the single excellent coincidence outrank the two
        // weaker real matches unless invalid candidates are removed after the clock check.
        let nulls: Vec<EventNull> = [("e1", 10.0), ("e2", 2.0), ("e3", 2.0)]
            .into_iter()
            .map(|(event, gate_nats)| EventNull {
                event: event.into(),
                gate_nats,
                ..Default::default()
            })
            .collect();
        impostor.score = evidence(&mut impostor.events, &nulls, 0);
        valid.score = evidence(&mut valid.events, &nulls, 0);
        let check = |cand: &mut Candidate| {
            lock_prune(cand, &case.events, &case.tracks, &case.p, &atmo, &dem);
            cand.score = evidence(&mut cand.events, &nulls, 0);
        };
        let mut pruned = impostor.clone();
        check(&mut pruned);
        assert_eq!(pruned.events.len(), 1);
        assert_eq!(pruned.events[0].event, "e1");
        assert!(pruned.score > valid.score);
        let out = finalize_candidates(vec![impostor, valid], &check);
        assert_eq!(out.len(), 1);
        let top = &out[0];
        assert!(top.lock_checked);
        assert_eq!(top.n_explained, 2);
        assert_eq!(top.events[0].event, "e2");
        assert_eq!(top.events[1].event, "e3");
        assert!(top.events.iter().all(|m| m.core));
        assert!((top.t_start_utc - true_clock).abs() < 1.0);
    }

    // The pre-index implementation is an independent oracle for greedy representative
    // selection, including unusual floating-point inputs and differently ordered matches.
    fn dedupe_linear_oracle(sorted: Vec<Candidate>) -> Vec<Candidate> {
        let mut kept_with: HashMap<Vec<(String, String)>, Vec<usize>> = HashMap::new();
        let mut out: Vec<Candidate> = Vec::new();
        for f in sorted {
            let mut matches: Vec<(String, String)> = f
                .events
                .iter()
                .map(|m| (m.event.clone(), m.flight.clone()))
                .collect();
            matches.sort();
            let kept = kept_with.entry(matches).or_default();
            let dup = kept.iter().any(|&i| {
                let o = &out[i];
                haversine_m(o.lat, o.lon, f.lat, f.lon) < 300.0
                    && (o.t_start_utc - f.t_start_utc).abs() < 5.0
            });
            if !dup {
                kept.push(out.len());
                out.push(f);
            }
        }
        out
    }

    fn dedupe_fixture(lat: f64, lon: f64, t_start_utc: f64) -> Candidate {
        Candidate {
            lat,
            lon,
            t_start_utc,
            events: [("e1", "A"), ("e2", "B")]
                .into_iter()
                .map(|(e, f)| EventMatch {
                    event: e.into(),
                    flight: f.into(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn dedupe_signature(candidates: &[Candidate]) -> Vec<[u64; 4]> {
        candidates
            .iter()
            .map(|c| {
                [
                    c.score.to_bits(),
                    c.lat.to_bits(),
                    c.lon.to_bits(),
                    c.t_start_utc.to_bits(),
                ]
            })
            .collect()
    }

    fn checked_dedupe(mut input: Vec<Candidate>) -> Vec<Candidate> {
        // Unique identities let the comparison check representatives and order even when
        // all the position/clock fields are identical or contain NaNs.
        for (i, c) in input.iter_mut().enumerate() {
            c.score = i as f64;
        }
        let expected = dedupe_linear_oracle(input.clone());
        let actual = dedupe(input);
        assert_eq!(dedupe_signature(&actual), dedupe_signature(&expected));
        actual
    }

    #[test]
    fn dedupe_index_preserves_boundaries_and_greedy_representatives() {
        let check = |tail: Vec<(f64, f64, f64)>, expected_kept: Option<usize>| {
            // Retain enough earlier candidates with this match set to activate the index.
            let mut input: Vec<_> = (0..96)
                .map(|i| dedupe_fixture(20.0, 30.0, -10_000.0 - 16.0 * i as f64))
                .collect();
            input.extend(tail.into_iter().map(|(a, b, t)| dedupe_fixture(a, b, t)));
            let out = checked_dedupe(input);
            if let Some(n) = expected_kept {
                assert_eq!(out.len(), 96 + n);
            }
            out.into_iter().skip(96).collect::<Vec<_>>()
        };
        let deg = |m: f64| (m / fdop_types::geo::EARTH_R).to_degrees();
        let chain = check(
            vec![
                (0.0, 0.0, 0.0),
                (0.0, deg(250.0), 0.0),
                (0.0, deg(500.0), 0.0),
            ],
            Some(2),
        );
        assert_eq!(chain[0].score, 96.0);
        assert_eq!(
            chain[1].score, 98.0,
            "a discarded middle point cannot suppress a later one"
        );
        check(
            vec![
                (0.0, 0.0, 0.0),
                (0.0, deg(299.999_999), 0.0),
                (0.0, deg(300.0), 0.0),
                (0.0, deg(300.000_001), 0.0),
            ],
            Some(2),
        );
        let clocks = check(
            vec![
                (0.0, 0.0, 0.0),
                (0.0, 0.0, 4.999_999),
                (0.0, 0.0, 5.0),
                (0.0, 0.0, -5.0),
            ],
            Some(3),
        );
        assert_eq!(
            clocks.iter().map(|c| c.score).collect::<Vec<_>>(),
            [96.0, 98.0, 99.0]
        );

        let r = fdop_types::geo::EARTH_R;
        let x_edge = ((r / 301.0).floor() * 301.0 / r).acos().to_degrees();
        let yz_edge = (301.0 / r).asin().to_degrees();
        for (lat, lon) in [(0.0, x_edge), (0.0, yz_edge), (yz_edge, 0.0), (0.0, 0.0)] {
            for clock in [-8.0, 0.0, 8.0, 1_789_574_400.0] {
                check(
                    vec![
                        (lat - 1e-8, lon - 1e-8, clock - 1e-6),
                        (lat + 1e-8, lon + 1e-8, clock + 1e-6),
                    ],
                    Some(1),
                );
            }
        }
        for points in [
            [(0.0, 179.9999, 0.0), (0.0, -179.9999, 0.0)],
            [(89.9999, 0.0, 0.0), (89.9999, 180.0, 0.0)],
            [(-89.9999, 0.0, 0.0), (-89.9999, 180.0, 0.0)],
            [(0.0, 0.0, 0.0), (0.0, 360.0, 0.0)],
            [(0.0, 360.0, 0.0), (0.0, 0.0, 0.0)],
        ] {
            check(points.to_vec(), Some(1));
        }
        let huge = (1_u64 << 52) as f64;
        for (a, b) in [
            (huge, huge + 1.0),
            (huge + 1.0, huge),
            (-huge, -huge - 1.0),
            (1e300, 1e300),
        ] {
            check(vec![(0.0, 0.0, a), (0.0, 0.0, b)], Some(1));
        }
        for (lat, lon, clock) in [
            (f64::NAN, 0.0, 0.0),
            (0.0, f64::NAN, 0.0),
            (f64::INFINITY, 0.0, 0.0),
            (0.0, f64::NEG_INFINITY, 0.0),
            (f64::MAX, f64::MAX, 0.0),
            (0.0, 0.0, f64::NAN),
            (0.0, 0.0, f64::INFINITY),
            (0.0, 0.0, f64::NEG_INFINITY),
        ] {
            check(vec![(lat, lon, clock); 2], None);
        }
    }

    #[test]
    fn dedupe_index_matches_oracle_on_shuffled_clouds() {
        let mut seed = 0x5129_4389_1234_5678_u64;
        let mut next = || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (seed >> 32) as f64 / u32::MAX as f64
        };
        let mut input = Vec::new();
        for i in 0..2_000 {
            let mut c = dedupe_fixture(
                20.0 + next() * 0.2,
                30.0 + next() * 0.2,
                [-12.0, 0.0, 1_789_574_400.0][i % 3] + next() * 24.0,
            );
            c.events[1].flight = format!("B{}", i % 3);
            input.push(c.clone());
            c.lat += 0.0001;
            c.t_start_utc += 0.25;
            c.events.reverse();
            input.push(c);
        }
        for i in (1..input.len()).rev() {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            input.swap(i, (seed as usize) % (i + 1));
        }
        checked_dedupe(input);
    }

    #[test]
    #[ignore = "timing benchmark; run with --release --ignored --nocapture"]
    fn benchmark_dedupe_large_clouds() {
        for spacing_m in [100.0, 1_000.0] {
            let step = (spacing_m / fdop_types::geo::EARTH_R).to_degrees();
            let input: Vec<_> = (0..16_000)
                .map(|i| {
                    let mut c = dedupe_fixture(
                        20.0 + (i / 200) as f64 * step,
                        30.0 + (i % 200) as f64 * step / 20.0_f64.to_radians().cos(),
                        1_789_574_400.0,
                    );
                    c.score = i as f64;
                    c
                })
                .collect();
            let reference_input = input.clone();
            let start = std::time::Instant::now();
            let expected = dedupe_linear_oracle(reference_input);
            let linear = start.elapsed();
            let start = std::time::Instant::now();
            let actual = dedupe(input);
            let indexed = start.elapsed();
            assert_eq!(dedupe_signature(&actual), dedupe_signature(&expected));
            eprintln!(
                "dedupe 16000 candidates, {spacing_m:.0} m spacing, {} retained: linear {linear:?}, indexed {indexed:?}",
                actual.len()
            );
        }
    }

    #[test]
    fn explanations_and_duplicates() {
        let m = |e: &str, f: &str| EventMatch {
            event: e.into(),
            flight: f.into(),
            ..Default::default()
        };
        let cand = |lat: f64, ms: Vec<EventMatch>| Candidate {
            lat,
            lon: -73.5,
            t_start_utc: 1000.0,
            events: ms,
            ..Default::default()
        };
        let top = cand(40.0, vec![m("e15", "A"), m("e20", "B"), m("e16", "C")]);
        assert!(
            same_explanation(&top, &cand(40.001, vec![m("e15", "A"), m("e20", "B")])),
            "subset = same story"
        );
        assert!(
            !same_explanation(&top, &cand(40.001, vec![m("e15", "A"), m("e20", "X")])),
            "shared event, other flight"
        );
        assert!(
            !same_explanation(&top, &cand(40.001, vec![m("e1", "P"), m("e4", "Q")])),
            "disjoint events"
        );
        let d = dedupe(vec![
            top.clone(),
            cand(40.0001, vec![m("e20", "B"), m("e16", "C"), m("e15", "A")]),
            cand(40.0001, vec![m("e15", "A")]),
        ]);
        assert_eq!(
            d.len(),
            2,
            "same place, clock and match set collapse regardless of order"
        );
        let far = cand(40.1, vec![m("e15", "A")]);
        let mut later = cand(40.0001, vec![m("e15", "A")]);
        later.t_start_utc += 6.0;
        let other = cand(40.0001, vec![m("e15", "Z")]);
        let first = cand(40.0, vec![m("e15", "A")]);
        let d = dedupe(vec![
            first.clone(),
            far.clone(),
            later.clone(),
            other.clone(),
            first,
        ]);
        let kept: Vec<(f64, f64, &str)> = d
            .iter()
            .map(|c| (c.lat, c.t_start_utc, c.events[0].flight.as_str()))
            .collect();
        assert_eq!(
            kept,
            [
                (40.0, later.t_start_utc - 6.0, "A"),
                (40.1, far.t_start_utc, "A"),
                (40.0001, later.t_start_utc, "A"),
                (40.0001, other.t_start_utc, "Z"),
            ],
            "another place, clock or flight is not a duplicate, and the order holds"
        );
    }

    #[test]
    fn ratio_grid_interpolation_resolves_close_passes_between_samples() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let x = enu.to_ecef([0.0, 0.0, 0.0]);
        let c = 343.0;
        let mut maxima = [[0.0_f64; 2]; 2];
        for (v, d, f0) in [
            (175.0, 200.0, 1100.0),
            (175.0, 250.0, 500.0),
            (130.0, 300.0, 100.0),
            (175.0, 800.0, 1100.0),
            (60.0, 250.0, 100.0),
            (137.0, 5000.0, 112.7),
        ] {
            // The independent oracle solves the analytic retarded quadratic, then uses
            // the radial velocity. It never calls TrackModel or the production CV kernel.
            let oracle = |u: f64| {
                let tau = (c * c * u - (c * c * v * v * u * u + d * d * (c * c - v * v)).sqrt())
                    / (c * c - v * v);
                let radial = v * v * tau / d.hypot(v * tau);
                f0 * c / (c + radial)
            };
            for t0e in [1000.0, 1_789_574_400.0] {
                let tr = straight_track(
                    &enu,
                    [0.0, d * 0.8, d * 0.6],
                    [v, 0.0, 0.0],
                    t0e,
                    t0e - 120.0,
                    t0e + 120.0,
                    5.0,
                );
                for (grid_kind, dt) in [0.2, 0.5].into_iter().enumerate() {
                    for phase in [0.07, 0.29, 0.61] {
                        let lo = t0e - 16.0 + phase * dt;
                        let g = RatioGrid::build(&tr, x, &Prop::still(c), lo, lo + 32.0, dt, 60.0);
                        let (mut error, mut linear_error) = (0.0_f64, 0.0_f64);
                        for k in 1..g.r.len() - 2 {
                            for fraction in [0.13, 0.37, 0.5, 0.83] {
                                let t = lo + (k as f64 + fraction) * dt;
                                let u = (t - lo) / dt;
                                let w = u - k as f64;
                                let linear = g.r[k] * (1.0 - w) + g.r[k + 1] * w;
                                let expected = oracle(t - t0e);
                                error = error.max((f0 * g.at(t) - expected).abs());
                                linear_error = linear_error.max((f0 * linear - expected).abs());
                            }
                        }
                        maxima[grid_kind][0] = maxima[grid_kind][0].max(linear_error);
                        maxima[grid_kind][1] = maxima[grid_kind][1].max(error);
                        let fraction = if grid_kind == 0 { 0.1 } else { 0.4 };
                        assert!(
                            error < fraction * linear_error + 2e-5,
                            "v={v} d={d} f0={f0} dt={dt} phase={phase} t0={t0e}: interpolation {error:.6} Hz, linear {linear_error:.6} Hz"
                        );
                    }
                }
            }
        }
        // Even at the top of the tone bank, the full-grid numerical error stays below
        // one eighth of the existing 0.2%-of-carrier model-error floor.
        assert!(maxima[0][1] < 0.275, "{maxima:?}");
        eprintln!(
            "fractional Doppler grids [linear, interpolated] Hz: fine {:?}, quick {:?}",
            maxima[0], maxima[1]
        );
    }

    #[test]
    fn ratio_grid_interpolation_preserves_support_and_local_bounds() {
        let polynomial = |t: f64| 1.2 - 0.02 * t + 0.0004 * t * t * t;
        let g =
            RatioGrid::from_samples(-2.0, 1.0, (-2..=4).map(|k| polynomial(k as f64)).collect());
        for t in [-0.87, -0.33, 0.15, 0.72, 1.63] {
            assert!((g.at(t) - polynomial(t)).abs() < 1e-13);
        }
        for values in [
            [f64::NAN, 1.2, 1.0, 0.9],
            [1.3, 1.2, 1.0, f64::NAN],
            [1.3, 1.2, 1.0, 8.0],
            [0.2, 1.2, 1.0, 0.9],
            [1.3, 1.2, 1.2, 0.9],
        ] {
            let g = RatioGrid::from_samples(0.0, 1.0, values.to_vec());
            for k in 0..100 {
                let t = 1.0 + k as f64 / 100.0;
                let r = g.at(t);
                assert!(r >= values[1].min(values[2]) && r <= values[1].max(values[2]));
            }
        }
        let hole = RatioGrid::from_samples(0.0, 1.0, vec![1.3, 1.2, f64::NAN, 1.0, 0.9]);
        for t in [
            1.0,
            1.3,
            1.9,
            2.0,
            2.4,
            2.9,
            -0.1,
            4.0,
            f64::NAN,
            f64::INFINITY,
            1e300,
        ] {
            assert!(hole.at(t).is_nan(), "unsupported t={t}");
        }
        assert_eq!(hole.at(3.5), 0.95);
    }

    #[test]
    fn ratio_grid_preserves_short_support_and_linear_stretches() {
        for r in [vec![], vec![1.0]] {
            let g = RatioGrid::from_samples(0.0, 0.2, r);
            for t in [-0.1, 0.0, 0.05, 0.2] {
                assert!(g.at(t).is_nan(), "a bracket is required at {t}");
            }
        }
        // Both boundary intervals and prepared interior intervals must reproduce
        // the same affine curve, even for a descending or completely flat pass.
        for n in [2, 3, 7] {
            for slope in [-0.02, 0.0, 0.02] {
                let g = RatioGrid::from_samples(
                    1000.0,
                    0.2,
                    (0..n).map(|k| 1.0 + slope * k as f64 * 0.2).collect(),
                );
                for k in 0..100 {
                    let t = 1000.0 + (n - 1) as f64 * 0.2 * k as f64 / 100.0;
                    assert!((g.at(t) - (1.0 + slope * (t - 1000.0))).abs() < 1e-14);
                }
            }
        }
    }

    #[test]
    fn a_ratio_grid_matches_independent_solves_and_skips_sample_gaps() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let mut tr = straight_track(
            &enu,
            [0.0, 3000.0, 1200.0],
            [130.0, 0.0, 0.0],
            1000.0,
            600.0,
            1400.0,
            20.0,
        );
        // a 120 s hole in the samples, over the 60 s allowed below
        let keep: Vec<usize> = (0..tr.t.len())
            .filter(|&k| !(tr.t[k] > 900.0 && tr.t[k] < 1020.0))
            .collect();
        tr.pos = keep.iter().map(|&k| tr.pos[k]).collect();
        tr.vel = keep.iter().map(|&k| tr.vel[k]).collect();
        tr.t = keep.iter().map(|&k| tr.t[k]).collect();
        let x = enu.to_ecef([0.0, 0.0, 0.0]);
        let m = Prop {
            c: 343.0,
            w: enu.vec_to_ecef([20.0, -15.0, 0.0]),
        };
        let (lo, dt) = (500.0, 0.2);
        let g = RatioGrid::build(&tr, x, &m, lo, 1500.0, dt, 60.0);
        let (mut solved, mut refused) = (0, 0);
        for (k, &ratio) in g.r.iter().enumerate() {
            let tu = lo + k as f64 * dt;
            let cold = tr
                .solve_emission_p(tu, x, &m, None)
                .filter(|c| tr.gap_at(c.0) <= 60.0);
            match cold {
                Some(c) => {
                    assert!((ratio - c.2).abs() < 1e-5, "t={tu}: {ratio} vs {}", c.2);
                    solved += 1;
                }
                None => {
                    assert!(ratio.is_nan(), "t={tu}: {ratio}");
                    refused += 1;
                }
            }
        }
        // the hole and both ends of the track
        assert!(
            solved > 2000 && refused > 1000,
            "{solved} solved, {refused} refused"
        );
    }

    #[test]
    fn budget_scales_with_memory_up_to_32_gib() {
        let ceiling = (57_266_230, 171_798_691);
        assert_eq!(hyp_budget(None), ceiling);
        // A bigger machine runs the same search as a 32 GiB one, so a result can be repeated on
        // either. Below that the budget still falls with RAM.
        for bytes in [32 << 30, (32 << 30) + 1, 33 << 30, 256 << 30, u64::MAX] {
            assert_eq!(hyp_budget(Some(bytes)), ceiling, "{bytes} bytes");
        }
        assert_eq!(hyp_budget(Some(16 << 30)), (28_633_115, 85_899_345));
        assert_eq!(std::mem::size_of::<Hyp>(), 20);
        for bytes in [512 << 20, 2 << 30, 8 << 30, 32 << 30] {
            let (per, total) = hyp_budget(Some(bytes));
            assert!(per > 0 && per <= total);
            assert!(
                total as u64 * 20 <= bytes / 10,
                "{bytes} bytes: {total} hypotheses"
            );
        }
        assert_eq!(hyp_budget(Some(0)), (1, 1));
    }

    #[test]
    fn hypothesis_builder_preserves_cloud_and_chance_sample() {
        let sizes = [(0, 1023), (3, 2049), (17, 3077)];
        let cap = sizes.iter().map(|&(_, n)| n).sum();
        let total = AtomicUsize::new(0);
        for (track, n) in sizes {
            let expected: Vec<Hyp> = (0..n)
                .map(|k| Hyp {
                    trk: track as u32,
                    t_e: k as f32,
                    e: 2.0 * k as f32,
                    n: -(k as f32),
                    t_s: 0.5 * k as f32,
                })
                .collect();
            let mut builder = HypBuilder::new(cap);
            for &h in &expected {
                builder.push(h, track, cap, &total);
            }
            let (stored, count, sample) = builder.finish(cap, &total);
            let bits = |h: &Hyp| {
                (
                    h.trk,
                    h.t_e.to_bits(),
                    h.e.to_bits(),
                    h.n.to_bits(),
                    h.t_s.to_bits(),
                )
            };
            assert_eq!(count, n);
            assert_eq!(
                stored.iter().map(bits).collect::<Vec<_>>(),
                expected.iter().map(bits).collect::<Vec<_>>()
            );
            let old_sample: Vec<_> = expected
                .iter()
                .enumerate()
                .filter(|(k, _)| mix(track as u64, *k as u64).is_multiple_of(1024))
                .map(|(_, h)| bits(h))
                .collect();
            assert_eq!(sample.iter().map(bits).collect::<Vec<_>>(), old_sample);
        }
        assert_eq!(total.load(Ordering::Relaxed), cap);
    }

    #[test]
    fn hypothesis_builder_drops_large_clouds_before_track_completion() {
        for cap in [0, 1500, 3000] {
            let total = AtomicUsize::new(0);
            let mut builders = [HypBuilder::new(cap), HypBuilder::new(cap)];
            // Interleave workers to exercise the shared event budget, including
            // a zero-budget confirmer. The exact count and chance sample survive.
            for k in 0..20_000 {
                for (track, builder) in builders.iter_mut().enumerate() {
                    builder.push(
                        Hyp {
                            trk: track as u32,
                            t_e: k as f32,
                            e: 0.0,
                            n: 0.0,
                            t_s: 0.0,
                        },
                        track,
                        cap,
                        &total,
                    );
                }
                let retained: usize = builders.iter().map(|b| b.stored.len()).sum();
                assert!(retained <= cap + builders.len() * (HypBuilder::BATCH - 1));
                if cap == 0 || k > cap + 2 * HypBuilder::BATCH {
                    assert!(builders.iter().all(|b| b.stored.capacity() == 0));
                }
            }
            for (track, builder) in builders.into_iter().enumerate() {
                let (stored, count, sample) = builder.finish(cap, &total);
                assert!(stored.is_empty());
                assert_eq!(count, 20_000);
                let expected: Vec<f32> = (0..20_000)
                    .filter(|&k| mix(track as u64, k).is_multiple_of(1024))
                    .map(|k| k as f32)
                    .collect();
                assert_eq!(sample.iter().map(|h| h.t_e).collect::<Vec<_>>(), expected);
            }
            assert_eq!(total.load(Ordering::Relaxed), 40_000);
        }
    }

    #[test]
    fn null_tail_extends_beyond_samples() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-7);
        assert!((normal_cdf(-1.6449) - 0.05).abs() < 1e-4);
        assert!((normal_cdf(-3.0) - 0.001_349_9).abs() < 1e-6);
        let chi_null: Vec<f64> = (0..400).map(|k| 5.0 + 0.1 * k as f64).collect();
        let nl = EventNull {
            chi_null_p05: chi_null[19],
            chi_null_median: chi_null[199],
            n_null: 400,
            chi_null,
            ..Default::default()
        };
        let (a, b, c) = (nl.trace_nats(30.0), nl.trace_nats(6.0), nl.trace_nats(1.0));
        assert!(
            a < 0.5 && b > 2.5 && c > b + 3.0 && c <= 25.0,
            "{a} {b} {c}"
        );
    }

    /// A narrow null (median 8.88, 5 % quantile 6.56, n 315: the shape that gave one chance match
    /// 25 trace nats) reaches ln(n+1) + NULL_TAIL_EXTRA_NATS and no further. Inside the sample the
    /// empirical value stands, and the tail still rises toward the cap.
    #[test]
    fn null_tail_reaches_three_orders_past_the_sample_and_no_further() {
        let chi_null: Vec<f64> = (0..315).map(|k| 6.0 + 0.02 * k as f64).collect();
        let nl = EventNull {
            chi_null_p05: 6.56,
            chi_null_median: 8.88,
            n_null: 315,
            chi_null,
            ..Default::default()
        };
        let reach = (316.0f64).ln() + NULL_TAIL_EXTRA_NATS;
        assert!((NULL_TAIL_EXTRA_NATS - 1000.0f64.ln()).abs() < 1e-12);
        let far = nl.trace_nats(1.05);
        assert!((far - reach).abs() < 1e-9, "{far} {reach}");
        let (mu, sd) = (8.88f64.ln(), (8.88f64.ln() - 6.56f64.ln()) / 1.6449);
        let fitted = -normal_cdf((1.05f64.ln() - mu) / sd).max(1e-300).ln();
        assert!(fitted > 25.0, "the fitted tail alone would give {fitted}");
        let k = 100;
        let inside = nl.trace_nats(nl.chi_null[k]);
        assert!(
            (inside + ((k as f64 + 1.5) / 316.0).ln()).abs() < 1e-9,
            "{inside}"
        );
        let near = nl.trace_nats(6.2);
        assert!(near < far && near > 0.0, "{near} {far}");
    }

    #[test]
    fn two_synthetic_flyovers_locate_the_observer() {
        locate_two(fdop_types::atmo::Medium::still(343.0));
    }

    #[test]
    fn two_synthetic_flyovers_in_wind_locate_the_observer() {
        locate_two(fdop_types::atmo::Medium {
            c: 339.0,
            wind_e: 7.0,
            wind_n: -5.0,
        });
    }

    #[test]
    fn tonal_jet_tracks_seed_and_locate_only_when_enabled() {
        let medium = fdop_types::atmo::Medium::still(343.0);
        let mut case = two_flyover_case(medium);
        case.p.start_lo = case.t_s - 5.0;
        case.p.start_hi = case.t_s + 5.0;
        case.p.region = BBox::new(
            case.obs.0 - 0.02,
            case.obs.1 - 0.03,
            case.obs.0 + 0.02,
            case.obs.1 + 0.03,
        );
        for tr in &mut case.tracks {
            tr.engine = fdop_types::engine::EngineClass::Jet;
            tr.typ = "C25B".into();
        }
        let excluded = run_case(&case, medium);
        assert!(excluded.candidates.is_empty());
        assert!(
            excluded
                .funnel
                .hypotheses
                .iter()
                .all(|(_, n)| *n == Some(0))
        );

        case.p.tonal_only = false;
        let included = run_case(&case, medium);
        let top = included
            .candidates
            .first()
            .expect("tonal jets must be eligible to seed the pair join");
        assert!(
            included.funnel.truncated.is_empty(),
            "{:?}",
            included.funnel
        );
        assert!(haversine_m(top.lat, top.lon, case.obs.0, case.obs.1) < 60.0);
        assert!((top.t_start_utc - case.t_s).abs() < 1.0);
        for (event, flight) in [("e1", "A"), ("e2", "B")] {
            let found = top.events.iter().find(|m| m.event == event).unwrap();
            assert_eq!(found.flight, flight);
            assert_eq!(found.engine, "jet");
        }

        // An excluded leading track must not renumber the retained tracks' hypotheses.
        // Its geometry duplicates A, making an accidental index shift observable.
        for tr in &mut case.tracks[..2] {
            tr.engine = fdop_types::engine::EngineClass::Turboprop;
            tr.typ = "PC12".into();
        }
        let mut jet = case.tracks[0].clone();
        jet.engine = fdop_types::engine::EngineClass::Jet;
        jet.typ = "C25B".into();
        jet.key = "excluded-jet".into();
        case.tracks.insert(0, jet);
        case.p.tonal_only = true;
        let ordinary = run_case(&case, medium);
        let top = ordinary.candidates.first().unwrap();
        assert!(ordinary.funnel.truncated.is_empty());
        assert!(haversine_m(top.lat, top.lon, case.obs.0, case.obs.1) < 60.0);
        assert!((top.t_start_utc - case.t_s).abs() < 1.0);
        for (event, flight) in [("e1", "A"), ("e2", "B")] {
            let found = top.events.iter().find(|m| m.event == event).unwrap();
            assert_eq!(found.flight, flight);
            assert_eq!(found.engine, "turboprop");
        }
        assert!(top.trk.iter().all(|&(_, ti)| ti == 1 || ti == 2));
    }

    /// Two synthetic flyovers of an observer (flights A and B) plus a parallel decoy (D1).
    struct TwoCase {
        tracks: Vec<TrackModel>,
        events: Vec<SearchEvent>,
        p: SearchParams,
        t_s: f64,
        obs: (f64, f64),
    }

    fn two_flyover_case(med: fdop_types::atmo::Medium) -> TwoCase {
        let c = 343.0;
        let (lat0, lon0) = (40.8765, -73.5250);
        let enu = Enu::new(lat0, lon0, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let t_s = 1_788_895_000.0;
        // flight A: eastbound, 5 km north, 1500 m, CPA at t_s+1000; flight B: northbound, 2.5 km east
        let a = straight_track(
            &enu,
            [0.0, 5000.0, 1500.0],
            [137.0, 0.0, 0.0],
            t_s + 1000.0,
            t_s + 400.0,
            t_s + 1600.0,
            5.0,
        );
        let mut b = straight_track(
            &enu,
            [2500.0, 0.0, 900.0],
            [0.0, 104.0, 0.0],
            t_s + 1900.0,
            t_s + 1300.0,
            t_s + 2500.0,
            5.0,
        );
        b.key = "B".into();
        let mut a = a;
        a.key = "A".into();
        // decoys: parallel tracks offset in time/space
        let mut d1 = straight_track(
            &enu,
            [0.0, -9000.0, 1500.0],
            [130.0, 0.0, 0.0],
            t_s + 1500.0,
            t_s + 900.0,
            t_s + 2100.0,
            5.0,
        );
        d1.key = "D1".into();
        let tracks = vec![a.clone(), b.clone(), d1];
        let prop = Prop::from_medium(&med, &enu);
        let e1 = synth_event("e1", &a, x, t_s, 112.7, c, t_s + 1000.0, &prop);
        let e2 = synth_event("e2", &b, x, t_s, 129.5, c, t_s + 1900.0, &prop);
        let region = BBox::new(lat0 - 0.15, lon0 - 0.2, lat0 + 0.15, lon0 + 0.2);
        let mut p = SearchParams::new(t_s - 3600.0, t_s + 3600.0, 3000.0, region);
        p.refine_half_m = 400.0;
        TwoCase {
            tracks,
            events: vec![e1, e2],
            p,
            t_s,
            obs: (lat0, lon0),
        }
    }

    fn run_case(case: &TwoCase, med: fdop_types::atmo::Medium) -> SearchResult {
        let dem = FlatElevation(0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        let inp = SearchInputs {
            events: case.events.clone(),
            tracks: &case.tracks,
            heard_times: vec![],
            coarse_dem: &dem,
            fine_dem: &dem,
            atmo: &atmo,
        };
        run(&inp, &case.p, &|_s| {})
    }

    #[test]
    fn measured_windy_tones_reach_pair_search_and_confirmation() {
        use crate::fit::{Side, fit_constant_velocity};

        let (lat, lon) = (40.876466, -73.524951);
        let enu = Enu::new(lat, lon, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        for start in [0.0, 1_788_895_000.0] {
            for wind in [-35.0, 35.0] {
                let medium = fdop_types::atmo::Medium {
                    c: 343.0,
                    wind_e: wind,
                    wind_n: 0.0,
                };
                let prop = Prop::from_medium(&medium, &enu);
                let mut tracks = Vec::new();
                let mut events = Vec::new();
                // Opposite directions make the two flight constraints independent. Wind is
                // along both tracks, so their fitted and geometric CPA centres also agree.
                for (key, position, speed, at, emitted) in [
                    ("A", [0.0, 3500.0, 1200.0], 175.0, 120.0, 350.0),
                    ("B", [0.0, -2500.0, 1600.0], -175.0, 360.0, 479.0),
                ] {
                    let mut tr = straight_track(
                        &enu,
                        position,
                        [speed, 0.0, 0.0],
                        start + at,
                        start - 200.0,
                        start + 800.0,
                        5.0,
                    );
                    tr.key = key.into();
                    let mut ev =
                        synth_event(key, &tr, x, start, emitted, 343.0, start + at, &prop);
                    // Fit the actual propagated, noisy frequency observations in still air.
                    // The fixture's source tone must not masquerade as a measured fit value.
                    let fit = fit_constant_velocity(&ev.tr_t, &ev.tr_f, 343.0, Side::Both)
                        .expect("a constant-wind straight pass is a still-air curve after rescaling");
                    ev.f0 = fit.best.f0;
                    ev.f0_tol = (0.04 * ev.f0).max(4.0);
                    ev.t_rec = fit.best.t0r;
                    ev.sigma_hz = (fit.best.rmse.powi(2) + (0.002 * ev.f0).powi(2)).sqrt();
                    let cpa = tr.cpa_near(x, start + at, 150.0).unwrap();
                    let factor = prop.source_tone_scale(cpa.vel);
                    assert!((ev.f0 * factor - emitted).abs() < 0.1);
                    assert!(
                        (emitted - ev.f0).abs() > ev.f0_tol,
                        "the old emitted-tone interval must exclude this real source"
                    );
                    let ratio = |t| tr.solve_emission_p(t, x, &prop, None).unwrap().2;
                    let mut scratch = TraceScratch::default();
                    for robust in [false, true] {
                        let old = trace_chi(
                            &ev.tr_t,
                            &ev.tr_f,
                            &ratio,
                            start,
                            &ev,
                            1.0,
                            ev.sigma_hz,
                            5.0,
                            robust,
                            &mut scratch,
                        );
                        let corrected = trace_chi(
                            &ev.tr_t,
                            &ev.tr_f,
                            &ratio,
                            start,
                            &ev,
                            factor,
                            ev.sigma_hz,
                            5.0,
                            robust,
                            &mut scratch,
                        );
                        assert!(old.chi > 12.0, "the old bounds refuse {key}: {old:?}");
                        assert!(corrected.chi < 0.02, "{key}: {corrected:?}");
                        assert!((corrected.f0 - emitted).abs() < 0.02);
                        assert_eq!(corrected.cov, 1.0);
                    }
                    tracks.push(tr);
                    events.push(ev);
                }
                let (mlat, mlon) = fdop_types::geo::m_per_deg(lat);
                let mut p = SearchParams::new(
                    start,
                    start,
                    600.0,
                    BBox::new(
                        lat - 50.0 / mlat,
                        lon - 50.0 / mlon,
                        lat + 50.0 / mlat,
                        lon + 50.0 / mlon,
                    ),
                );
                p.refine_half_m = 100.0;
                let case = TwoCase {
                    tracks,
                    events,
                    p,
                    t_s: start,
                    obs: (lat, lon),
                };
                let dem = FlatElevation(0.0);
                let atmo = fdop_types::atmo::UniformAtmosphere(medium);
                let o = observer(lat, lon, &dem);
                let index = TimeIndex::new(&case.tracks);
                for (i, ev) in case.events.iter().enumerate() {
                    let (chi, matched, ti) = best_confirmer(
                        &o,
                        start,
                        ev,
                        &case.tracks,
                        &index,
                        &case.p,
                        &atmo,
                        &[],
                    )
                    .expect("the same source-tone conversion must reach confirmation");
                    assert_eq!(ti as usize, i);
                    assert!(chi < 0.02 && matched.coverage >= 0.99);
                }
                check_wind_delayed_case(&case, medium, run_case(&case, medium));
            }
        }
    }

    // Supply a synthetic traced ridge to the actual fitter and measured-event adapter.
    // Detection and spectral tracing are outside these search regressions.
    #[allow(clippy::too_many_arguments)]
    fn fitted_wind_event(
        id: &str,
        tr: &TrackModel,
        x: V3,
        start: f64,
        emitted: f64,
        cpa_time: f64,
        half: usize,
        prop: &Prop,
    ) -> SearchEvent {
        use crate::detect::Detection;
        use crate::fit::{Side, fit_constant_velocity};
        use crate::measure::EventQuality;
        use crate::trace::Trace;

        let cpa = tr.cpa_nearest_time(x, cpa_time, 100.0).unwrap();
        let heard = cpa.t_r_p(prop);
        let mut trace = Trace::default();
        for k in 0..=2 * half {
            let t = heard - half as f64 + k as f64;
            let ratio = tr.solve_emission_p(t, x, prop, None).unwrap().2;
            let noise = 0.02 * (1.7 * k as f64).sin() + 0.01 * (0.41 * k as f64).sin();
            trace.t.push(t - start);
            trace.f.push(emitted * ratio + noise);
            trace.snr.push(20.0);
        }
        let fit = fit_constant_velocity(&trace.t, &trace.f, 343.0, Side::Both).unwrap();
        let b = fit.best;
        let approach = b.t0r - trace.t[0];
        let recede = trace.t.last().unwrap() - b.t0r;
        let drop = (trace.f[0] - trace.f.last().unwrap()) / (0.8 * (fit.f_pre - fit.f_post));
        assert!(trace.len() >= 80 && approach > 8.0 && recede > 8.0 && drop > 0.15);
        assert!(b.rmse < 0.03 && (28.0..=0.8 * 343.0).contains(&b.v));
        assert!((200.0..=12_000.0).contains(&b.d));
        let measured = MeasuredEvent {
            id: id.into(),
            detection: Detection {
                id: 0,
                t0r: b.t0r,
                f0: b.f0,
                v: b.v,
                d: b.d,
                score: 10.0,
                score_approach: 10.0,
                score_recede: 10.0,
                half_s: half as f64,
                harmonic_of: None,
                harmonics: vec![],
                from_contrast: false,
            },
            quality: EventQuality {
                n_points: trace.len(),
                span_approach_s: approach,
                span_recede_s: recede,
                drop_traversed: drop,
                median_snr_db: 20.0,
                rmse_hz: b.rmse,
                score: 20.0 * fit.diagnostics.n_eff.sqrt(),
                ok: true,
                reseeded: false,
                reasons: vec![],
            },
            fit: Some(fit),
            trace,
            win_s: 1.0,
            spec: None,
        };
        assert!(measured.is_aircraft_like());
        let ev = SearchEvent::from_measured(&measured, 3.0).unwrap();
        assert!(
            ev.t_tol <= 3.001,
            "the ordinary measured time floor must remain sufficient: {ev:?}"
        );
        assert!((start + ev.t_rec - cpa.t_r_fit_p(prop)).abs() < 0.05);
        assert!(gates_ok(&ev, &cpa, prop, 0.0));
        ev
    }

    #[test]
    fn measured_wind_geometry_reaches_search_and_confirmation() {
        let (lat, lon) = (40.876466, -73.524951);
        let enu = Enu::new(lat, lon, 0.0);
        let medium = fdop_types::atmo::Medium {
            c: 320.0,
            wind_e: -80.0,
            wind_n: -60.0,
        };
        let prop = Prop::from_medium(&medium, &enu);
        let atmo = fdop_types::atmo::UniformAtmosphere(medium);
        let dem = FlatElevation(0.0);
        let o = observer(lat, lon, &dem);
        for start in [0.0, 1_788_895_000.0] {
            let mut tracks = Vec::new();
            let mut events = Vec::new();
            for (key, position, velocity, at, emitted) in [
                ("A", [0.0, 2800.0, 1200.0], [180.0, 0.0, 0.0], 180.0, 180.0),
                ("B", [2800.0, 0.0, 1200.0], [0.0, 170.0, 0.0], 480.0, 230.0),
            ] {
                let mut position = position;
                position[2] += MIC_HEIGHT_M;
                // Headwind compresses the received approach: A's first sample near
                // 98 s was emitted near -163 s, before the old -100 s track boundary.
                let mut tr = straight_track(
                    &enu,
                    position,
                    velocity,
                    start + at,
                    start - 300.0,
                    start + 900.0,
                    5.0,
                );
                tr.key = key.into();
                // Use the actual fit and its ordinary generous intervals; no test-only
                // widening may hide the former speed-gate rejection.
                let ev = fitted_wind_event(key, &tr, o.x, start, emitted, start + at, 90, &prop);
                let cpa = tr.cpa_nearest_time(o.x, start + at, 100.0).unwrap();
                let old_scale = (prop.c + dot(cpa.vel, prop.w) / prop.c) / ev.c_fit;
                assert!(
                    cpa.speed < ev.v_gate[0] * old_scale,
                    "the former common scale must exclude the real aircraft: {key}"
                );
                assert!(gates_ok(&ev, &cpa, &prop, 0.0));
                for speed in [true, false] {
                    let mut wrong = ev.clone();
                    let gate = if speed {
                        &mut wrong.v_gate
                    } else {
                        &mut wrong.d_gate
                    };
                    for bound in gate {
                        *bound *= 0.5;
                    }
                    assert!(!gates_ok(&wrong, &cpa, &prop, 0.0));
                }
                tracks.push(tr);
                events.push(ev);
            }
            let (mlat, mlon) = fdop_types::geo::m_per_deg(lat);
            let mut p = SearchParams::new(
                start,
                start,
                800.0,
                BBox::new(
                    lat - 25.0 / mlat,
                    lon - 25.0 / mlon,
                    lat + 25.0 / mlat,
                    lon + 25.0 / mlon,
                ),
            );
            p.refine_half_m = 100.0;
            let case = TwoCase {
                tracks,
                events,
                p,
                t_s: start,
                obs: (lat, lon),
            };
            let tix = TimeIndex::new(&case.tracks);
            for (i, ev) in case.events.iter().enumerate() {
                let (chi, matched, ti) =
                    best_confirmer(&o, start, ev, &case.tracks, &tix, &case.p, &atmo, &[])
                        .expect("the measured wind geometry must reach exact confirmation");
                assert_eq!(ti as usize, i);
                assert!(chi < 0.01 && matched.coverage >= 0.99);
                let mut wrong = ev.clone();
                wrong.t_rec += 30.0;
                assert!(
                    best_confirmer(&o, start, &wrong, &case.tracks, &tix, &case.p, &atmo, &[])
                        .is_none()
                );
            }
            let result = check_wind_delayed_case(&case, medium, run_case(&case, medium));
            let inputs = SearchInputs {
                events: case.events.clone(),
                tracks: &case.tracks,
                heard_times: case.events.iter().map(|ev| ev.t_rec).collect(),
                coarse_dem: &dem,
                fine_dem: &dem,
                atmo: &atmo,
            };
            let nulls: Vec<_> = case
                .events
                .iter()
                .map(|ev| EventNull {
                    event: ev.id.clone(),
                    gate_nats: 6.0,
                    ..EventNull::default()
                })
                .collect();
            let fresh = refresh_refined_candidate(
                &result.candidates[0],
                &refinement_at(lat, lon, start),
                &inputs,
                &case.events,
                &case.p,
                &nulls,
                &tix,
            )
            .expect("the recovered matches must survive final exact reevaluation");
            assert_eq!(fresh.n_explained, 2);
            assert!(fresh.events.iter().all(|m| m.chi < 0.01));
        }
    }

    #[test]
    fn oblique_wind_fitted_centres_reach_pair_search_confirmation_and_refinement() {
        let (lat, lon) = (40.876466, -73.524951);
        let enu = Enu::new(lat, lon, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let medium = fdop_types::atmo::Medium {
            c: 343.0,
            wind_e: 65.0,
            wind_n: 65.0,
        };
        let prop = Prop::from_medium(&medium, &enu);
        let atmo = fdop_types::atmo::UniformAtmosphere(medium);
        let dem = FlatElevation(0.0);
        for start in [0.0, 1_788_895_000.0] {
            let mut tracks = Vec::new();
            let mut events = Vec::new();
            for (key, position, velocity, at, emitted) in [
                ("A", [0.0, 11_000.0, 1500.0], [70.0, 0.0, 0.0], 240.0, 350.0),
                ("B", [11_000.0, 0.0, 1500.0], [0.0, 70.0, 0.0], 720.0, 479.0),
            ] {
                let mut position = position;
                position[2] += MIC_HEIGHT_M;
                let mut tr = straight_track(
                    &enu,
                    position,
                    velocity,
                    start + at,
                    start - 200.0,
                    start + 1300.0,
                    5.0,
                );
                tr.key = key.into();
                let ev = fitted_wind_event(key, &tr, x, start, emitted, start + at, 180, &prop);
                let cpa = tr.cpa_nearest_time(x, start + at, 100.0).unwrap();
                assert!(
                    (cpa.t_r_p(&prop) - start - ev.t_rec).abs() > ev.t_tol + 3.0,
                    "the former geometric-arrival gate must reject this measured curve"
                );
                tracks.push(tr);
                events.push(ev);
            }
            let (mlat, mlon) = fdop_types::geo::m_per_deg(lat);
            let mut p = SearchParams::new(
                start,
                start,
                1100.0,
                BBox::new(
                    lat - 25.0 / mlat,
                    lon - 25.0 / mlon,
                    lat + 25.0 / mlat,
                    lon + 25.0 / mlon,
                ),
            );
            p.refine_half_m = 100.0;
            let case = TwoCase {
                tracks,
                events,
                p,
                t_s: start,
                obs: (lat, lon),
            };
            let o = observer(lat, lon, &dem);
            let tix = TimeIndex::new(&case.tracks);
            for (i, ev) in case.events.iter().enumerate() {
                let (chi, matched, ti) =
                    best_confirmer(&o, start, ev, &case.tracks, &tix, &case.p, &atmo, &[])
                        .expect("a measured oblique-wind curve must confirm at the true clock");
                assert_eq!(ti as usize, i);
                assert!(chi < 0.01 && matched.coverage >= 0.99);
                let cpa = case.tracks[i]
                    .cpa_nearest_time(x, start + [240.0, 720.0][i], 100.0)
                    .unwrap();
                assert!((matched.t_cpa_heard_utc - cpa.t_r_p(&prop)).abs() < 0.002);
                let mut wrong = ev.clone();
                wrong.t_rec += 30.0;
                assert!(
                    best_confirmer(&o, start, &wrong, &case.tracks, &tix, &case.p, &atmo, &[],)
                        .is_none()
                );
            }
            let result = check_wind_delayed_case(&case, medium, run_case(&case, medium));
            let inputs = SearchInputs {
                events: case.events.clone(),
                tracks: &case.tracks,
                heard_times: case.events.iter().map(|ev| ev.t_rec).collect(),
                coarse_dem: &dem,
                fine_dem: &dem,
                atmo: &atmo,
            };
            let nulls: Vec<_> = case
                .events
                .iter()
                .map(|ev| EventNull {
                    event: ev.id.clone(),
                    gate_nats: 6.0,
                    ..EventNull::default()
                })
                .collect();
            let fresh = refresh_refined_candidate(
                &result.candidates[0],
                &refinement_at(lat, lon, start),
                &inputs,
                &case.events,
                &case.p,
                &nulls,
                &tix,
            )
            .expect("the same fitted clock must survive final candidate reevaluation");
            assert_eq!(fresh.n_explained, 2);
            assert!(fresh.events.iter().all(|m| m.chi < 0.01));
            let explained = explain_at(
                &inputs,
                &case.p,
                lat,
                lon,
                start,
                1.0,
                &[("A".into(), "A".into()), ("B".into(), "B".into())],
            )
            .unwrap();
            assert!((explained.t_start_utc - start).abs() < 0.1);
            assert!(explained.events.iter().all(|m| m.chi < 0.01));
        }
    }

    #[test]
    fn fitted_centres_before_emission_keep_late_hypotheses_and_heard_passes() {
        let (lat, lon) = (40.876466, -73.524951);
        let enu = Enu::new(lat, lon, 0.0);
        let medium = fdop_types::atmo::Medium {
            c: 343.0,
            wind_e: -85.0,
            wind_n: -130.0,
        };
        let prop = Prop::from_medium(&medium, &enu);
        let atmo = fdop_types::atmo::UniformAtmosphere(medium);
        let dem = FlatElevation(0.0);
        let o = observer(lat, lon, &dem);
        for start in [0.0, 1_788_895_000.0] {
            let mut tr = straight_track(
                &enu,
                [0.0, 8000.0, 1200.0 + MIC_HEIGHT_M],
                [25.0, 0.0, 0.0],
                start + 360.0,
                start - 400.0,
                start + 1000.0,
                5.0,
            );
            tr.key = "late-cpa".into();
            let ev = fitted_wind_event(
                "late-cpa",
                &tr,
                o.x,
                start,
                350.0,
                start + 360.0,
                240,
                &prop,
            );
            let cpa = tr.cpa_nearest_time(o.x, start + 360.0, 100.0).unwrap();
            let (mlat, mlon) = fdop_types::geo::m_per_deg(lat);
            let p = SearchParams::new(
                start,
                start,
                900.0,
                BBox::new(
                    lat - 25.0 / mlat,
                    lon - 25.0 / mlon,
                    lat + 25.0 / mlat,
                    lon + 25.0 / mlon,
                ),
            );
            assert!(
                start + ev.t_rec + ev.t_tol + 5.0 < cpa.t_e - 8.0,
                "the former upper crop must end before this region's CPA crossing"
            );
            let tracks = [tr];
            let index = TimeIndex::new(&tracks);
            let frame = Frame::new(&p.region);
            let cloud = hypotheses(
                &ev,
                &tracks,
                &p,
                &frame,
                &dem,
                &atmo,
                start - 86_400.0,
                10_000,
            );
            assert!(cloud.count > 0 && cloud.stored.is_some());
            let limited = hypotheses(&ev, &tracks, &p, &frame, &dem, &atmo, start - 86_400.0, 0);
            assert_eq!(cloud.count, limited.count);
            assert!(limited.stored.is_none());
            let (chi, matched, _) = best_confirmer(&o, start, &ev, &tracks, &index, &p, &atmo, &[])
                .expect("the same measured pass must remain available as a confirmer");
            assert!(chi < 0.01 && matched.coverage >= 0.99);
            assert!((matched.t_cpa_heard_utc - cpa.t_r_p(&prop)).abs() < 0.002);

            // A nearby detected pass must not become an unheard-aircraft veto merely
            // because its fitted centre differs from a nominal still-air arrival by >25 s.
            let candidate = Candidate {
                t_start_utc: start,
                events: vec![EventMatch {
                    d: 20_000.0,
                    ..EventMatch::default()
                }],
                ..Candidate::default()
            };
            assert!((cpa.t_r(p.c) - start - ev.t_rec).abs() > 25.0);
            assert!(
                unheard_passes(&candidate, &o, &tracks, &index, &[ev.t_rec], &p, &atmo,).is_empty()
            );
            for heard in [vec![], vec![ev.t_rec + 30.0]] {
                let silent = unheard_passes(&candidate, &o, &tracks, &index, &heard, &p, &atmo);
                assert_eq!(
                    silent.len(),
                    1,
                    "the unchanged hearing window still applies"
                );
                assert!(silent[0].starts_with("late-cpa "));
            }
        }
    }

    #[test]
    fn narrow_region_between_cross_track_samples_locates_observer() {
        let (lat, lon) = (40.876466, -73.524951);
        let enu = Enu::new(lat, lon, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let t_s = 1_788_895_000.0;
        let med = fdop_types::atmo::Medium::still(343.0);
        let prop = Prop::from_medium(&med, &enu);
        let mut tracks = Vec::new();
        let mut events = Vec::new();
        // The roughly 200 m-wide box lies between b=0 and b=350 m on both tracks.
        // The original distance-only fallback also lands outside it, even though the
        // observer and both complete traces satisfy the unchanged acoustic gates.
        for (key, position, velocity, t_cpa, f0) in [
            ("A", [0.0, 175.0, 1000.0], [90.0, 0.0, 0.0], 120.0, 112.7),
            ("B", [175.0, 0.0, 1000.0], [0.0, 95.0, 0.0], 360.0, 149.3),
        ] {
            let mut tr = straight_track(
                &enu,
                position,
                velocity,
                t_s + t_cpa,
                t_s - 200.0,
                t_s + 800.0,
                5.0,
            );
            tr.key = key.into();
            let ev = synth_event(key, &tr, x, t_s, f0, 343.0, t_s + t_cpa, &prop);
            let cpa = tr.cpa_nearest_time(x, t_s + t_cpa, 30.0).unwrap();
            assert!(gates_ok(&ev, &cpa, &prop, 0.0));
            tracks.push(tr);
            events.push(ev);
        }
        let mut p = SearchParams::new(
            t_s,
            t_s,
            600.0,
            BBox::new(lat - 0.0009, lon - 0.0012, lat + 0.0009, lon + 0.0012),
        );
        p.refine_half_m = 200.0;
        let dem = FlatElevation(0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        let frame = Frame::new(&p.region);
        let counts: Vec<_> = events
            .iter()
            .map(|ev| {
                hypotheses(ev, &tracks, &p, &frame, &dem, &atmo, t_s - 86_400.0, 10_000).count
            })
            .collect();
        assert!(
            counts.iter().all(|&n| n > 0),
            "both in-region flyovers need hypotheses: {counts:?}"
        );
        let inputs = SearchInputs {
            heard_times: events.iter().map(|ev| ev.t_rec).collect(),
            events,
            tracks: &tracks,
            coarse_dem: &dem,
            fine_dem: &dem,
            atmo: &atmo,
        };
        let result = run(&inputs, &p, &|_| {});
        let winner = result
            .candidates
            .first()
            .unwrap_or_else(|| panic!("in-region flyovers were lost: {:?}", result.funnel));
        for key in ["A", "B"] {
            assert!(
                winner
                    .events
                    .iter()
                    .any(|m| m.event == key && m.flight == key)
            );
        }
        assert!(haversine_m(lat, lon, winner.lat, winner.lon) < 75.0);
        assert!((winner.t_start_utc - t_s).abs() < 0.2);
        assert!(result.funnel.truncated.is_empty());
    }

    #[test]
    fn known_clock_hillside_flyovers_reach_the_hypothesis_join() {
        struct Slope {
            lat: f64,
            lon: f64,
            grade: f64,
        }
        impl Elevation for Slope {
            fn elev_m(&self, lat: f64, lon: f64) -> f64 {
                let (mlat, mlon) = fdop_types::geo::m_per_deg(self.lat);
                1000.0 + self.grade * ((lon - self.lon) * mlon + (lat - self.lat) * mlat)
            }
            fn describe(&self) -> String {
                "synthetic planar hillside".into()
            }
        }
        let (lat, lon) = (40.876466, -73.524951);
        let (mlat, mlon) = fdop_types::geo::m_per_deg(lat);
        let start = 1_788_895_000.0;
        let enu = Enu::new(lat, lon, 1000.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let atmo = fdop_types::atmo::UniformAtmosphere(fdop_types::atmo::Medium::still(343.0));
        let mut tracks = Vec::new();
        let mut events = Vec::new();
        // At each true CPA, source-minus-observer is perpendicular to velocity.
        // One track climbs and the other descends; their horizontal headings cross.
        for (key, pos, vel, dt, f0) in [
            (
                "A",
                [-400.0, 6500.0, 1760.0 + MIC_HEIGHT_M],
                [110.0, 0.0, 25.0],
                120.0,
                112.7,
            ),
            (
                "B",
                [6500.0, 400.0, 2000.0 + MIC_HEIGHT_M],
                [0.0, 100.0, -20.0],
                360.0,
                149.3,
            ),
        ] {
            let mut tr = straight_track(
                &enu,
                pos,
                vel,
                start + dt,
                start - 200.0,
                start + 800.0,
                5.0,
            );
            tr.key = key.into();
            let mut ev = synth_event(
                key,
                &tr,
                x,
                start,
                f0,
                343.0,
                start + dt,
                &Prop::still(343.0),
            );
            ev.t_tol = 1.0;
            let cpa = tr.cpa_nearest_time(x, start + dt, 100.0).unwrap();
            assert!(gates_ok(&ev, &cpa, &Prop::still(343.0), 0.0));
            tracks.push(tr);
            events.push(ev);
        }
        let mut p = SearchParams::new(
            start,
            start,
            600.0,
            BBox::new(
                lat - 25.0 / mlat,
                lon - 25.0 / mlon,
                lat + 25.0 / mlat,
                lon + 25.0 / mlon,
            ),
        );
        p.refine_half_m = 100.0;
        let frame = Frame::new(&p.region);
        let t_base = start - 86_400.0;
        for grade in [0.0, 0.1, 0.2, -0.2] {
            let dem = Slope { lat, lon, grade };
            let generate = |ev: &SearchEvent, params: &SearchParams, cap: usize| {
                hypotheses(ev, &tracks, params, &frame, &dem, &atmo, t_base, cap)
            };
            let clouds: Vec<_> = events.iter().map(|ev| generate(ev, &p, 10_000)).collect();
            let counts: Vec<_> = clouds.iter().map(|h| h.count).collect();
            assert!(counts.iter().all(|&n| n > 0), "slope {grade}: {counts:?}");
            if grade == 0.0 {
                // Observed on the immutable implementation before the hillside retry.
                assert_eq!(counts, vec![1, 1]);
            }
            for (i, (ev, cloud)) in events.iter().zip(&clouds).enumerate() {
                assert!(
                    cloud
                        .stored
                        .as_ref()
                        .unwrap()
                        .iter()
                        .any(|h| h.trk as usize == i)
                );
                let limited = generate(ev, &p, 0);
                assert_eq!(limited.count, cloud.count);
                assert!(limited.stored.is_none());
                let mut wrong_clock = p.clone();
                wrong_clock.start_lo += 30.0;
                wrong_clock.start_hi += 30.0;
                assert_eq!(generate(ev, &wrong_clock, 10_000).count, 0);
                let mut gap = p.clone();
                gap.max_gap_s = 4.0; // these track knots are five seconds apart
                assert_eq!(generate(ev, &gap, 10_000).count, 0);
                let mut below_ground = p.clone();
                below_ground.min_agl_m = 5000.0;
                assert_eq!(generate(ev, &below_ground, 10_000).count, 0);
                let mut too_slow = ev.clone();
                too_slow.v_gate = [5.0, 30.0];
                assert_eq!(generate(&too_slow, &p, 10_000).count, 0);
                let mut too_close = ev.clone();
                too_close.d_gate = [20.0, 30.0];
                assert_eq!(generate(&too_close, &p, 10_000).count, 0);
            }
            let result = run(
                &SearchInputs {
                    events: events.clone(),
                    tracks: &tracks,
                    heard_times: events.iter().map(|ev| ev.t_rec).collect(),
                    coarse_dem: &dem,
                    fine_dem: &dem,
                    atmo: &atmo,
                },
                &p,
                &|_| {},
            );
            let top = result
                .candidates
                .first()
                .unwrap_or_else(|| panic!("hillside {grade}: no location; {:?}", result.funnel));
            for key in ["A", "B"] {
                assert!(
                    top.events
                        .iter()
                        .any(|m| { m.event == key && m.flight == key && m.core && m.chi < 1.0 })
                );
            }
            let error = haversine_m(lat, lon, top.lat, top.lon);
            assert!(error < 75.0, "hillside {grade}: error {error} m");
            assert!((top.t_start_utc - start).abs() < 0.2);
            assert!(result.funnel.truncated.is_empty());
            eprintln!(
                "hillside {grade}: counts {counts:?}, error {error:.6} m, clock {:.6} s",
                top.t_start_utc - start
            );
        }
    }

    #[test]
    fn hillside_cpa_survives_an_approximate_distance_rejection() {
        struct Slope {
            lat: f64,
            lon: f64,
            grade: f64,
        }
        impl Elevation for Slope {
            fn elev_m(&self, lat: f64, lon: f64) -> f64 {
                let (mlat, mlon) = fdop_types::geo::m_per_deg(self.lat);
                1000.0 + self.grade * ((lon - self.lon) * mlon + (lat - self.lat) * mlat)
            }
            fn describe(&self) -> String {
                "synthetic planar hillside".into()
            }
        }
        let (lat, lon) = (40.876466, -73.524951);
        let (mlat, mlon) = fdop_types::geo::m_per_deg(lat);
        let start = 1_788_895_000.0;
        let enu = Enu::new(lat, lon, 1000.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let atmo = fdop_types::atmo::UniformAtmosphere(fdop_types::atmo::Medium::still(343.0));
        let mut p = SearchParams::new(
            start,
            start,
            600.0,
            BBox::new(
                lat - 25.0 / mlat,
                lon - 25.0 / mlon,
                lat + 25.0 / mlat,
                lon + 25.0 / mlon,
            ),
        );
        p.refine_half_m = 100.0;
        let frame = Frame::new(&p.region);
        let t_base = start - 86_400.0;
        // Sample different phases of the regular cross-track grid. The 40% hillside
        // previously lost both flights before the exact CPA/clock retry was reachable.
        for across in [6500.0, 6650.0, 7000.0] {
            let mut tracks = Vec::new();
            let mut events = Vec::new();
            for (key, pos, vel, dt, f0) in [
                (
                    "A",
                    [-1000.0, across, 6000.0 + MIC_HEIGHT_M],
                    [90.0, 0.0, 15.0],
                    120.0,
                    112.7,
                ),
                (
                    "B",
                    [across, 6000.0 * 15.0 / 95.0, 6000.0 + MIC_HEIGHT_M],
                    [0.0, 95.0, -15.0],
                    360.0,
                    149.3,
                ),
            ] {
                let mut tr = straight_track(
                    &enu,
                    pos,
                    vel,
                    start + dt,
                    start - 200.0,
                    start + 800.0,
                    5.0,
                );
                tr.key = key.into();
                let mut ev = synth_event(
                    key,
                    &tr,
                    x,
                    start,
                    f0,
                    343.0,
                    start + dt,
                    &Prop::still(343.0),
                );
                let cpa = tr.cpa_nearest_time(x, start + dt, 100.0).unwrap();
                // Keep the ordinary 3 s timing and full 25% multiplicative distance
                // floor, with the true range near its upper edge, not a narrow gate.
                ev.d_gate = [cpa.d * 1.001 / (1.25 * 1.25), cpa.d * 1.001];
                assert!(gates_ok(&ev, &cpa, &Prop::still(343.0), 0.0));
                tracks.push(tr);
                events.push(ev);
            }
            for grade in [0.0, 0.2, 0.4, -0.4] {
                let dem = Slope { lat, lon, grade };
                let generate = |ev: &SearchEvent, params: &SearchParams, cap: usize| {
                    hypotheses(ev, &tracks, params, &frame, &dem, &atmo, t_base, cap)
                };
                for (i, ev) in events.iter().enumerate() {
                    let cloud = generate(ev, &p, 10_000);
                    // At these two lattice-aligned offsets the original in-box fallback
                    // survives alongside the exact recovery of the central grid point.
                    // The former whole-track retry guard silently omitted that second
                    // valid point, although its recording-clock hypothesis is different.
                    let expected = if i == 0 && grade == 0.2 && across != 6500.0 {
                        2
                    } else {
                        1
                    };
                    assert_eq!(
                        cloud.count, expected,
                        "offset {across}, slope {grade}, {}",
                        ev.id
                    );
                    let kept = cloud.stored.as_ref().unwrap();
                    for h in kept {
                        assert_eq!(h.trk as usize, i);
                        let (la, lo) = frame.latlon(f64::from(h.e), f64::from(h.n));
                        let o = observer(la, lo, &dem);
                        let tr = &tracks[i];
                        let cpa = tr
                            .cpa_nearest_time(o.x, t_base + f64::from(h.t_e), 150.0)
                            .unwrap();
                        let prop = prop_for(&atmo, &o, tr, cpa.t_e);
                        let (_, _, height) = ecef_to_geodetic(tr.pos_at(cpa.t_e));
                        assert!(gates_ok(ev, &cpa, &prop, 0.0));
                        assert!(height - o.z - MIC_HEIGHT_M >= p.min_agl_m);
                        assert!(tr.gap_at(cpa.t_e) <= p.max_gap_s);
                        assert!((cpa.t_r_fit_p(&prop) - ev.t_rec - start).abs() <= ev.t_tol);
                    }
                    if expected == 2 {
                        assert!((kept[0].e - kept[1].e).hypot(kept[0].n - kept[1].n) < 50.0);
                        assert!((kept[0].t_s - kept[1].t_s).abs() > 1.0);
                    }
                    if grade == 0.4 {
                        let limited = generate(ev, &p, 0);
                        assert_eq!(limited.count, cloud.count);
                        assert!(limited.stored.is_none());
                        let mut wrong_clock = p.clone();
                        wrong_clock.start_lo += 30.0;
                        wrong_clock.start_hi += 30.0;
                        assert_eq!(generate(ev, &wrong_clock, 10_000).count, 0);
                        let mut gap = p.clone();
                        gap.max_gap_s = 4.0;
                        assert_eq!(generate(ev, &gap, 10_000).count, 0);
                        let mut clearance = p.clone();
                        clearance.min_agl_m = 20_000.0;
                        assert_eq!(generate(ev, &clearance, 10_000).count, 0);
                        let mut too_slow = ev.clone();
                        too_slow.v_gate = [5.0, 30.0];
                        assert_eq!(generate(&too_slow, &p, 10_000).count, 0);
                        let mut too_close = ev.clone();
                        too_close.d_gate = [20.0, 30.0];
                        assert_eq!(generate(&too_close, &p, 10_000).count, 0);
                    }
                }
                let result = run(
                    &SearchInputs {
                        events: events.clone(),
                        tracks: &tracks,
                        heard_times: events.iter().map(|ev| ev.t_rec).collect(),
                        coarse_dem: &dem,
                        fine_dem: &dem,
                        atmo: &atmo,
                    },
                    &p,
                    &|_| {},
                );
                let top = result.candidates.first().unwrap_or_else(|| {
                    panic!(
                        "offset {across}, slope {grade}: no location; {:?}",
                        result.funnel
                    )
                });
                assert!(haversine_m(lat, lon, top.lat, top.lon) < 75.0);
                assert!((top.t_start_utc - start).abs() < 0.2);
                assert!(result.funnel.truncated.is_empty());
                for key in ["A", "B"] {
                    assert!(
                        top.events.iter().any(|m| {
                            m.event == key && m.flight == key && m.core && m.chi < 1.0
                        })
                    );
                }
            }
        }
    }

    #[test]
    fn hillside_recovery_survives_other_ordinary_visits() {
        struct Slope {
            lat: f64,
            lon: f64,
        }
        impl Elevation for Slope {
            fn elev_m(&self, lat: f64, lon: f64) -> f64 {
                let (mlat, mlon) = fdop_types::geo::m_per_deg(self.lat);
                1000.0 + 0.4 * ((lon - self.lon) * mlon + (lat - self.lat) * mlat)
            }
            fn describe(&self) -> String {
                "synthetic planar hillside".into()
            }
        }
        let (lat, lon) = (40.876466, -73.524951);
        let (mlat, mlon) = fdop_types::geo::m_per_deg(lat);
        let start = 1_788_895_000.0;
        let enu = Enu::new(lat, lon, 1000.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let dem = Slope { lat, lon };
        let atmo = fdop_types::atmo::UniformAtmosphere(fdop_types::atmo::Medium::still(343.0));
        let mut tracks = Vec::new();
        let mut events = Vec::new();
        for (key, pos, vel, dt, f0) in [
            (
                "A",
                [-1000.0, 6500.0, 6000.0 + MIC_HEIGHT_M],
                [90.0, 0.0, 15.0],
                120.0,
                112.7,
            ),
            (
                "B",
                [6500.0, 6000.0 * 15.0 / 95.0, 6000.0 + MIC_HEIGHT_M],
                [0.0, 95.0, -15.0],
                360.0,
                149.3,
            ),
        ] {
            let mut tr = straight_track(
                &enu,
                pos,
                vel,
                start + dt,
                start - 200.0,
                start + 800.0,
                5.0,
            );
            tr.key = key.into();
            let mut ev = synth_event(
                key,
                &tr,
                x,
                start,
                f0,
                343.0,
                start + dt,
                &Prop::still(343.0),
            );
            let cpa = tr.cpa_nearest_time(x, start + dt, 100.0).unwrap();
            ev.d_gate = [cpa.d * 1.001 / (1.25 * 1.25), cpa.d * 1.001];
            tracks.push(tr);
            events.push(ev);
        }
        let mut p = SearchParams::new(
            start,
            start,
            600.0,
            BBox::new(
                lat - 25.0 / mlat,
                lon - 25.0 / mlon,
                lat + 25.0 / mlat,
                lon + 25.0 / mlon,
            ),
        );
        p.refine_half_m = 100.0;
        let check_location = |tracks: &[TrackModel], p: &SearchParams| {
            let result = run(
                &SearchInputs {
                    events: events.clone(),
                    tracks,
                    heard_times: events.iter().map(|ev| ev.t_rec).collect(),
                    coarse_dem: &dem,
                    fine_dem: &dem,
                    atmo: &atmo,
                },
                p,
                &|_| {},
            );
            let top = result.candidates.first().unwrap_or_else(|| {
                panic!("hillside visits lost the location; {:?}", result.funnel)
            });
            let error = haversine_m(lat, lon, top.lat, top.lon);
            eprintln!(
                "location error {error} m, clock {} s",
                top.t_start_utc - start
            );
            assert!(error < 75.0, "location error {error} m");
            assert!((top.t_start_utc - start).abs() < 0.2);
            assert!(result.funnel.truncated.is_empty());
        };
        check_location(&tracks, &p);
        // An ordinary point on the same cross-track slice can be hundreds of metres
        // from an eligible rejected point. Local coverage must not discard that hole.
        let farther = straight_track(
            &enu,
            [-1000.0, 6650.0, 6000.0 + MIC_HEIGHT_M],
            [90.0, 0.0, 15.0],
            start + 120.0,
            start - 200.0,
            start + 800.0,
            5.0,
        );
        let mut ev = synth_event(
            "far",
            &farther,
            x,
            start,
            112.7,
            343.0,
            start + 120.0,
            &Prop::still(343.0),
        );
        let cpa = farther.cpa_nearest_time(x, start + 120.0, 100.0).unwrap();
        ev.d_gate = [cpa.d * 1.001 / (1.25 * 1.25), cpa.d * 1.001];
        let mut wide = p.clone();
        wide.region = BBox::new(
            lat - 25.0 / mlat,
            lon - 25.0 / mlon,
            lat + 2000.0 / mlat,
            lon + 2000.0 / mlon,
        );
        let frame = Frame::new(&wide.region);
        let cloud = hypotheses(
            &ev,
            &[farther],
            &wide,
            &frame,
            &dem,
            &atmo,
            start - 86_400.0,
            10_000,
        );
        let kept = cloud.stored.unwrap();
        let (_, truth_north) = frame.en(lat, lon);
        assert!(
            kept.iter().any(|near| {
                (f64::from(near.n) - truth_north).abs() < 25.0
                    && kept.iter().any(|far| {
                        (near.e - far.e).abs() < 1.0
                            && (f64::from(far.n) - truth_north).abs() > p.cross_step_m * 0.5
                    })
            }),
            "a different cross-track point at the same source sample must not cover the observer: {kept:?}"
        );
        // Extend each catalogue trajectory with a level return at a different recording
        // clock. The original heard passes and the observer are unchanged. Each return
        // supplies ordinary hypotheses, but they cannot explain the two heard events at
        // one shared clock and must not erase the earlier hillside hypotheses.
        for (i, tr) in tracks.iter_mut().enumerate() {
            let dt = [120.0, 360.0][i];
            let cutoff = tr.t.partition_point(|&t| t <= start + dt + 120.0);
            tr.t.truncate(cutoff);
            tr.pos.truncate(cutoff);
            tr.vel.truncate(cutoff);
            let return_cpa = start + dt + [1200.0, 2400.0][i];
            let (pos, vel) = if i == 0 {
                ([0.0, 6500.0, 6000.0 + MIC_HEIGHT_M], [90.0, 0.0, 0.0])
            } else {
                ([6500.0, 0.0, 6000.0 + MIC_HEIGHT_M], [0.0, 95.0, 0.0])
            };
            let next = straight_track(
                &enu,
                pos,
                vel,
                return_cpa,
                return_cpa - 120.0,
                return_cpa + 120.0,
                5.0,
            );
            let bridge = TrackModel {
                t: vec![tr.t1(), next.t0()],
                pos: vec![*tr.pos.last().unwrap(), next.pos[0]],
                vel: vec![*tr.vel.last().unwrap(), next.vel[0]],
                ..tr.clone()
            };
            let mut time = tr.t1() + 10.0;
            while time < next.t0() {
                let (pos, vel) = bridge.pv(time);
                tr.t.push(time);
                tr.pos.push(pos);
                tr.vel.push(vel);
                time += 10.0;
            }
            tr.t.extend(next.t);
            tr.pos.extend(next.pos);
            tr.vel.extend(next.vel);
        }
        p.start_hi = start + 3600.0;
        let frame = Frame::new(&p.region);
        for (i, ev) in events.iter().enumerate() {
            let cloud = hypotheses(
                ev,
                &tracks,
                &p,
                &frame,
                &dem,
                &atmo,
                start - 86_400.0,
                10_000,
            );
            let clocks: Vec<_> = cloud
                .stored
                .as_ref()
                .unwrap()
                .iter()
                .filter(|h| h.trk as usize == i)
                .map(|h| h.t_s as f64 - 86_400.0)
                .collect();
            eprintln!("returning {} clocks: {clocks:?}", ev.id);
            assert!(clocks.iter().any(|clock| clock.abs() < 0.5));
            assert!(
                clocks
                    .iter()
                    .any(|clock| (*clock - [1200.0, 2400.0][i]).abs() < 0.5)
            );
            for cap in [0, 1, cloud.count] {
                let limited =
                    hypotheses(ev, &tracks, &p, &frame, &dem, &atmo, start - 86_400.0, cap);
                assert_eq!(limited.count, cloud.count);
                assert_eq!(limited.stored.is_some(), cap >= cloud.count);
            }
        }
        check_location(&tracks, &p);
    }

    fn wind_delayed_case(med: fdop_types::atmo::Medium, start: f64) -> TwoCase {
        let (lat, lon) = (40.876466, -73.524951);
        let enu = Enu::new(lat, lon, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let prop = Prop::from_medium(&med, &enu);
        let mut tracks = Vec::new();
        let mut events = Vec::new();
        for (key, pos, vel, dt, f0) in [
            ("A", [0.0, 8000.0, 1200.0], [90.0, 0.0, 0.0], 120.0, 112.7),
            ("B", [6500.0, 0.0, 900.0], [0.0, 95.0, 0.0], 360.0, 149.3),
        ] {
            let mut tr = straight_track(
                &enu,
                pos,
                vel,
                start + dt,
                start - 200.0,
                start + 800.0,
                5.0,
            );
            tr.key = key.into();
            let mut event = synth_event(key, &tr, x, start, f0, 343.0, start + dt, &prop);
            // A well-measured pass can reach this existing one-second tolerance floor.
            // It must not need enough artificial uncertainty to absorb the wind delay.
            event.t_tol = 1.0;
            events.push(event);
            tracks.push(tr);
        }
        let (mlat, mlon) = fdop_types::geo::m_per_deg(lat);
        let mut p = SearchParams::new(
            start,
            start,
            600.0,
            BBox::new(
                lat - 50.0 / mlat,
                lon - 50.0 / mlon,
                lat + 50.0 / mlat,
                lon + 50.0 / mlon,
            ),
        );
        p.refine_half_m = 100.0;
        TwoCase {
            tracks,
            events,
            p,
            t_s: start,
            obs: (lat, lon),
        }
    }

    fn check_wind_delayed_case(
        case: &TwoCase,
        med: fdop_types::atmo::Medium,
        result: SearchResult,
    ) -> SearchResult {
        let top = result
            .candidates
            .first()
            .unwrap_or_else(|| panic!("medium {med:?}, start {}: {:?}", case.t_s, result.funnel));
        for key in ["A", "B"] {
            assert!(
                top.events
                    .iter()
                    .any(|m| { m.event == key && m.flight == key && m.core && m.chi < 1.0 })
            );
        }
        let error = haversine_m(case.obs.0, case.obs.1, top.lat, top.lon);
        assert!(error < 75.0, "medium {med:?}: error {error} m");
        assert!((top.t_start_utc - case.t_s).abs() < 0.2);
        assert!(result.funnel.truncated.is_empty());
        eprintln!(
            "wind-delayed medium {med:?}: error {error:.6} m clock {:.6} s; funnel {:?}",
            top.t_start_utc - case.t_s,
            result.funnel
        );
        result
    }

    struct CountedAtmosphere {
        medium: fdop_types::atmo::Medium,
        advertise: bool,
        calls: AtomicUsize,
    }

    fn wind_scaled_reach_case(med: fdop_types::atmo::Medium, start: f64) -> TwoCase {
        let mut case = wind_delayed_case(med, start);
        let enu = Enu::new(case.obs.0, case.obs.1, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let prop = Prop::from_medium(&med, &enu);
        case.tracks[0] = straight_track(
            &enu,
            [0.0, 12_000.0, 1200.0],
            [230.0, 0.0, 0.0],
            start + 120.0,
            start - 200.0,
            start + 800.0,
            5.0,
        );
        case.tracks[0].key = "A".into();
        for (i, (id, dt, f0)) in [("A", 120.0, 112.7), ("B", 360.0, 149.3)]
            .into_iter()
            .enumerate()
        {
            let tr = &case.tracks[i];
            let cpa = tr.cpa_nearest_time(x, start + dt, 30.0).unwrap();
            let (fitted_speed, fitted_distance) =
                prop.fit_geometry(cpa.rv, cpa.vel, 343.0).unwrap();
            let mut ev = synth_event(id, tr, x, start, f0, 343.0, start + dt, &prop);
            ev.t_tol = 1.0;
            ev.v_gate = [0.99 * fitted_speed, 1.01 * fitted_speed];
            ev.d_gate = [0.99 * fitted_distance, 1.01 * fitted_distance];
            assert!(gates_ok(&ev, &cpa, &prop, 0.0));
            if i == 0 {
                let ((lat, lon, _), _) = tr.geo_at(cpa.t_e);
                let old_reach = case.p.region.padded_km(1.1 * ev.d_gate[1] / 1000.0 + 0.5);
                assert!(!old_reach.contains(lat, lon));
                assert!(cpa.d > 1.15 * ev.d_gate[1]);
            }
            case.events[i] = ev;
        }
        case
    }

    #[test]
    fn calm_spatial_weather_keeps_clock_consistent_pair_hypotheses() {
        struct SpatialCalm {
            lat: f64,
            lon: f64,
        }
        impl Atmosphere for SpatialCalm {
            fn medium(
                &self,
                lat: f64,
                lon: f64,
                _t: f64,
                _za: f64,
                _zb: f64,
            ) -> fdop_types::atmo::Medium {
                // Every tested observer uses the same cool path average. The warmer
                // columns beneath the distant aircraft are not that observer's medium.
                let radius = haversine_m(self.lat, self.lon, lat, lon);
                let weight = ((radius - 2000.0) / 18_000.0).clamp(0.0, 1.0);
                fdop_types::atmo::Medium::still(330.0 + 13.0 * weight)
            }
            fn describe(&self) -> String {
                "synthetic spatial calm weather".into()
            }
        }
        let med = fdop_types::atmo::Medium::still(330.0);
        let dem = FlatElevation(0.0);
        for start in [0.0, 1_788_895_000.0] {
            let mut case = wind_delayed_case(med, start);
            let enu = Enu::new(case.obs.0, case.obs.1, 0.0);
            let o = observer(case.obs.0, case.obs.1, &dem);
            let atmo = SpatialCalm {
                lat: case.obs.0,
                lon: case.obs.1,
            };
            let prop = Prop::still(med.c);
            for (i, (key, pos, vel, dt, f0)) in [
                ("A", [0.0, 30_000.0, 1200.0], [90.0, 0.0, 0.0], 120.0, 112.7),
                ("B", [27_000.0, 0.0, 900.0], [0.0, 95.0, 0.0], 360.0, 149.3),
            ]
            .into_iter()
            .enumerate()
            {
                let mut tr = straight_track(
                    &enu,
                    pos,
                    vel,
                    start + dt,
                    start - 200.0,
                    start + 800.0,
                    5.0,
                );
                tr.key = key.into();
                let cpa = tr.cpa_nearest_time(o.x, start + dt, 30.0).unwrap();
                let (v, d) = prop.fit_geometry(cpa.rv, cpa.vel, 343.0).unwrap();
                let mut event = synth_event(key, &tr, o.x, start, f0, 343.0, start + dt, &prop);
                event.v_gate = [0.95 * v - 3.0, 1.05 * v + 3.0];
                event.d_gate = [0.9 * d, 1.1 * d];
                event.t_tol = 1.0;
                assert!(gates_ok(&event, &cpa, &prop, 0.0));
                let ((lat, lon, height), _) = tr.geo_at(cpa.t_e);
                let under_aircraft = atmo.medium(lat, lon, cpa.t_e, MIC_HEIGHT_M, height);
                assert_eq!(under_aircraft.c, 343.0);
                let old_start = cpa.t_e + cpa.d / under_aircraft.c - event.t_rec;
                assert!(old_start < start - event.t_tol - 1.0);
                case.tracks[i] = tr;
                case.events[i] = event;
            }
            let frame = Frame::new(&case.p.region);
            let t_base = start - 86_400.0;
            for (i, event) in case.events.iter().enumerate() {
                let uniform = fdop_types::atmo::UniformAtmosphere(med);
                let control = hypotheses(
                    event,
                    &case.tracks,
                    &case.p,
                    &frame,
                    &dem,
                    &uniform,
                    t_base,
                    10_000,
                );
                assert!(
                    control
                        .stored
                        .as_ref()
                        .unwrap()
                        .iter()
                        .any(|h| h.trk as usize == i)
                );
                let cloud = hypotheses(
                    event,
                    &case.tracks,
                    &case.p,
                    &frame,
                    &dem,
                    &atmo,
                    t_base,
                    10_000,
                );
                assert!(
                    cloud
                        .stored
                        .as_ref()
                        .unwrap()
                        .iter()
                        .any(|h| h.trk as usize == i),
                    "the observer's own medium must retain flight {} at start {start}: {} hypotheses",
                    event.id,
                    cloud.count
                );
            }
            let inputs = SearchInputs {
                events: case.events.clone(),
                tracks: &case.tracks,
                heard_times: vec![],
                coarse_dem: &dem,
                fine_dem: &dem,
                atmo: &atmo,
            };
            check_wind_delayed_case(&case, med, run(&inputs, &case.p, &|_| {}));
        }
    }

    #[test]
    fn wind_scaled_distances_reach_the_pair_join_and_confirmers() {
        let med = fdop_types::atmo::Medium {
            // A deliberately different sound speed makes the physical range exceed
            // the fitted range; wind alone cannot do so at the same sound speed.
            c: 410.0,
            wind_e: 80.0,
            wind_n: 0.0,
        };
        for start in [0.0, 1_788_895_000.0] {
            let case = wind_scaled_reach_case(med, start);
            let dem = FlatElevation(0.0);
            let o = observer(case.obs.0, case.obs.1, &dem);
            let atmo = fdop_types::atmo::UniformAtmosphere(med);
            let frame = Frame::new(&case.p.region);
            let tix = TimeIndex::new(&case.tracks);
            let event = &case.events[0];
            let cloud = hypotheses(
                event,
                &case.tracks,
                &case.p,
                &frame,
                &dem,
                &atmo,
                start - 86_400.0,
                10_000,
            );
            assert!(
                cloud.stored.as_ref().unwrap().iter().any(|h| h.trk == 0),
                "the wind-scaled range must be tested before rejecting this flight"
            );
            let explain = |event: &SearchEvent, atmo: &dyn Atmosphere, exclude: &[(u32, f64)]| {
                best_confirmer(&o, start, event, &case.tracks, &tix, &case.p, atmo, exclude)
            };
            let (chi, matched, ti) = explain(event, &atmo, &[])
                .expect("the same valid flight must also be available as a confirmer");
            assert_eq!(ti, 0);
            assert_eq!(matched.flight, "A");
            assert!(chi < 1.0);
            let unknown = CountedAtmosphere {
                medium: med,
                advertise: false,
                calls: AtomicUsize::new(0),
            };
            let (other_chi, _, other_ti) = explain(event, &unknown, &[]).unwrap();
            assert_eq!(other_ti, ti);
            assert_eq!(other_chi.to_bits(), chi.to_bits());
            assert!(explain(event, &atmo, &[(0, start + 120.0)]).is_none());
            let mut wrong_range = event.clone();
            wrong_range.d_gate = [event.d_gate[0] * 0.5, event.d_gate[1] * 0.5];
            assert!(explain(&wrong_range, &atmo, &[]).is_none());
            let mut wrong_clock = event.clone();
            wrong_clock.t_rec += 30.0;
            assert!(explain(&wrong_clock, &atmo, &[]).is_none());
            check_wind_delayed_case(&case, med, run_case(&case, med));
        }
    }

    #[test]
    fn uncertain_cpa_times_keep_physical_confirmers() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        let dem = FlatElevation(0.0);
        for start in [0.0, 1_788_895_000.0] {
            let mut case = wind_delayed_case(med, start);
            case.p.duration = 1000.0;
            case.p.max_events = 2;
            let enu = Enu::new(case.obs.0, case.obs.1, 0.0);
            let o = observer(case.obs.0, case.obs.1, &dem);
            let mut third = straight_track(
                &enu,
                [0.0, 11_000.0, 1500.0],
                [110.0, 0.0, 0.0],
                start + 720.0,
                start + 640.0,
                start + 800.0,
                5.0,
            );
            third.key = "uncertain-third".into();
            let exact = synth_event(
                "uncertain-third",
                &third,
                o.x,
                start,
                183.7,
                343.0,
                start + 720.0,
                &Prop::still(343.0),
            );
            case.tracks.push(third);
            let tix = TimeIndex::new(&case.tracks);
            let explain = |ev: &SearchEvent, exclude: &[(u32, f64)]| {
                best_confirmer(&o, start, ev, &case.tracks, &tix, &case.p, &atmo, exclude)
            };
            let (baseline_chi, baseline, baseline_ti) = explain(&exact, &[]).unwrap();
            assert_eq!(baseline_ti, 2);
            assert!(baseline_chi < 1.0);
            // Exercise the SearchEvent API's uncertainty contract independently of fitting:
            // the recorded trace and recording clock stay fixed while the coarse CPA
            // estimate moves. This deliberately broad profile is not a measurement claim.
            for shift in [-300.0, 300.0] {
                let mut uncertain = exact.clone();
                uncertain.t_rec += shift;
                uncertain.t_tol = shift.abs() + 1.0;
                let old_lo = start + uncertain.t_rec - 1.2 * uncertain.d_gate[1] / case.p.c - 120.0;
                let old_hi = start + uncertain.t_rec + 30.0;
                assert!(
                    !tix.overlapping(old_lo, old_hi).any(|ti| ti == 2),
                    "the old catalogue window must discard the true third flight"
                );
                let (chi, matched, ti) = explain(&uncertain, &[])
                    .expect("a permitted CPA uncertainty must not hide the physical trace match");
                assert_eq!(ti, 2);
                assert!(chi < 1.0);
                assert!((matched.t_cpa_heard_utc - baseline.t_cpa_heard_utc).abs() < 0.002);
                assert!((chi - baseline_chi).abs() < 1e-8);
                assert!(explain(&uncertain, &[(2, start + 720.0)]).is_none());
                let mut too_narrow = uncertain.clone();
                too_narrow.t_tol = 20.0;
                assert!(explain(&too_narrow, &[]).is_none());
                let mut wrong_trace = uncertain.clone();
                wrong_trace.tr_f.fill(183.7);
                assert!(explain(&wrong_trace, &[]).is_none());
                let mut wrong_clock = case.p.clone();
                wrong_clock.start_lo += 120.0;
                wrong_clock.start_hi += 120.0;
                assert!(
                    best_confirmer(
                        &o,
                        start + 120.0,
                        &uncertain,
                        &case.tracks,
                        &tix,
                        &wrong_clock,
                        &atmo,
                        &[],
                    )
                    .is_none()
                );
                case.events.truncate(2);
                case.events.push(uncertain);
                let result = check_wind_delayed_case(&case, med, run_case(&case, med));
                let top = &result.candidates[0];
                assert!(top.events.iter().any(|m| {
                    m.flight == "uncertain-third"
                        && m.event == "uncertain-third"
                        && m.role == "confirmer"
                        && m.core
                        && m.chi < 1.0
                }));
            }
        }
    }

    #[test]
    fn uncertain_cpa_window_covers_both_observed_sides() {
        let start = 1_788_895_000.0;
        let med = fdop_types::atmo::Medium::still(343.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        let dem = FlatElevation(0.0);
        let (lat, lon) = (40.876466, -73.524951);
        let enu = Enu::new(lat, lon, 0.0);
        let o = observer(lat, lon, &dem);
        let track = straight_track(
            &enu,
            [0.0, 11_000.0, 1500.0],
            [90.0, 0.0, 0.0],
            start + 600.0,
            start + 200.0,
            start + 1000.0,
            5.0,
        );
        let prop = Prop::still(343.0);
        let mut event = synth_event(
            "wide-profile",
            &track,
            o.x,
            start,
            149.3,
            343.0,
            start + 600.0,
            &prop,
        );
        let true_heard = event.t_rec;
        event.tr_t = (0..=560)
            .map(|k| true_heard - 280.0 + f64::from(k))
            .collect();
        event.tr_f = event
            .tr_t
            .iter()
            .map(|&t| {
                149.3
                    * track
                        .solve_emission_p(start + t, o.x, &prop, None)
                        .unwrap()
                        .2
            })
            .collect();
        (event.q_t, event.q_f) = subsample(&event.tr_t, &event.tr_f, 40);
        let tracks = [track];
        let tix = TimeIndex::new(&tracks);
        let p = SearchParams::new(start, start, 1200.0, BBox::new(lat, lon, lat, lon));
        for shift in [-240.0, 240.0] {
            event.t_rec = true_heard + shift;
            event.t_tol = shift.abs() + 1.0;
            assert!(event.t_rec - event.tr_t[0] >= 8.0);
            assert!(event.tr_t.last().unwrap() - event.t_rec >= 8.0);
            let heard = start + event.t_rec;
            assert!(
                tix.overlapping(heard - 1.2 * event.d_gate[1] / p.c - 120.0, heard + 30.0)
                    .any(|ti| ti == 0),
                "the long track already reaches the old time index"
            );
            let guess = heard - 1.1 * event.d_gate[1].min(12_000.0) / p.c * 0.5;
            assert!(
                tracks[0]
                    .local_cpas(o.x, guess - 200.0, guess + 200.0)
                    .all(|cpa| !cpa.interior),
                "the old local window, not catalogue filtering, loses this pass"
            );
            let (chi, matched, ti) =
                best_confirmer(&o, start, &event, &tracks, &tix, &p, &atmo, &[])
                    .expect("both observed sides and their allowed CPA must remain searchable");
            assert_eq!(ti, 0);
            assert!(chi < 1e-6, "physical trace chi={chi}");
            assert!((matched.t_cpa_heard_utc - start - true_heard).abs() < 0.002);
        }
    }

    #[test]
    fn uncertain_cpa_time_boundary_keeps_close_pass_interior() {
        let start = 1_788_895_000.0;
        let (lat, lon) = (40.876466, -73.524951);
        let enu = Enu::new(lat, lon, 0.0);
        let o = observer(lat, lon, &FlatElevation(0.0));
        let atmo = fdop_types::atmo::UniformAtmosphere(fdop_types::atmo::Medium::still(343.0));
        let tracks = [straight_track(
            &enu,
            [0.0, 400.0, 300.0],
            [90.0, 0.0, 0.0],
            start + 500.0,
            start + 100.0,
            start + 900.0,
            5.0,
        )];
        let mut event = synth_event(
            "close-time-boundary",
            &tracks[0],
            o.x,
            start,
            149.3,
            343.0,
            start + 500.0,
            &Prop::still(343.0),
        );
        let true_heard = event.t_rec;
        event.d_gate = [490.0, 510.0];
        let tix = TimeIndex::new(&tracks);
        let p = SearchParams::new(start, start, 1200.0, BBox::new(lat, lon, lat, lon));
        let (baseline_chi, _, _) =
            best_confirmer(&o, start, &event, &tracks, &tix, &p, &atmo, &[]).unwrap();
        assert!(baseline_chi < 1.0);
        event.t_tol = 301.0;
        // Place the true received CPA just inside the exact timing gate's early edge.
        // At close range, the propagation allowance provides less than the local CPA
        // scanner's half-second interior margin; it needs explicit scan support.
        event.t_rec += event.t_tol + 3.0 - 0.01;
        let (chi, matched, ti) = best_confirmer(&o, start, &event, &tracks, &tix, &p, &atmo, &[])
            .expect("an allowed close pass must not become an artificial scan endpoint");
        assert_eq!(ti, 0);
        // synth_event deliberately includes 0.05 Hz noise. The uncertainty change must
        // preserve its exact physical score, not require a noiseless trace.
        assert!((chi - baseline_chi).abs() < 1e-10);
        assert!((matched.t_cpa_heard_utc - start - true_heard).abs() < 0.002);
        event.t_rec += 0.02;
        assert!(best_confirmer(&o, start, &event, &tracks, &tix, &p, &atmo, &[]).is_none());
    }

    impl Atmosphere for CountedAtmosphere {
        fn medium(&self, _: f64, _: f64, _: f64, _: f64, _: f64) -> fdop_types::atmo::Medium {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.medium
        }
        fn describe(&self) -> String {
            "synthetic counted atmosphere".into()
        }
        fn uniform_medium(&self) -> Option<fdop_types::atmo::Medium> {
            self.advertise.then_some(self.medium)
        }
    }

    #[test]
    fn wind_delayed_flyovers_before_the_still_air_crop_reach_the_join() {
        let med = fdop_types::atmo::Medium {
            c: 343.0,
            wind_e: 0.0,
            // With the physical fitted distance, this keeps the old crop more than
            // two seconds after the true CPA instead of relying on unscaled metadata.
            wind_n: 90.0,
        };
        for start in [0.0, 1_788_895_000.0] {
            let mut case = wind_delayed_case(med, start);
            let enu = Enu::new(case.obs.0, case.obs.1, 0.0);
            let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
            let prop = Prop::from_medium(&med, &enu);
            case.tracks[0] = straight_track(
                &enu,
                [0.0, 20_000.0, 2000.0],
                [90.0, 0.0, 0.0],
                start + 120.0,
                start - 200.0,
                start + 800.0,
                5.0,
            );
            case.tracks[0].key = "A".into();
            for (i, (id, dt, f0)) in [("A", 120.0, 112.7), ("B", 360.0, 149.3)]
                .into_iter()
                .enumerate()
            {
                let tr = &case.tracks[i];
                let cpa = tr.cpa_nearest_time(x, start + dt, 30.0).unwrap();
                let (fitted_speed, fitted_distance) =
                    prop.fit_geometry(cpa.rv, cpa.vel, 343.0).unwrap();
                let mut ev = synth_event(id, tr, x, start, f0, 343.0, start + dt, &prop);
                ev.t_tol = 1.0;
                ev.v_gate = [0.99 * fitted_speed, 1.01 * fitted_speed];
                ev.d_gate = [0.99 * fitted_distance, 1.01 * fitted_distance];
                assert!(gates_ok(&ev, &cpa, &prop, 0.0));
                if i == 0 {
                    let old_lo = start + ev.t_rec - ev.t_tol - 1.1 * ev.d_gate[1] / 343.0 - 5.0;
                    assert!(
                        old_lo > cpa.t_e + 2.0,
                        "the old crop must start after the 100 m region's CPA crossing"
                    );
                }
                case.events[i] = ev;
            }
            check_wind_delayed_case(&case, med, run_case(&case, med));
            // A custom provider with no global declaration must recover the same pair.
            let atmo = CountedAtmosphere {
                medium: med,
                advertise: false,
                calls: AtomicUsize::new(0),
            };
            let dem = FlatElevation(0.0);
            let inputs = SearchInputs {
                events: case.events.clone(),
                tracks: &case.tracks,
                heard_times: case.events.iter().map(|ev| ev.t_rec).collect(),
                coarse_dem: &dem,
                fine_dem: &dem,
                atmo: &atmo,
            };
            check_wind_delayed_case(&case, med, run(&inputs, &case.p, &|_| {}));
            let frame = Frame::new(&case.p.region);
            let cloud = hypotheses(
                &case.events[0],
                &case.tracks,
                &case.p,
                &frame,
                &dem,
                &atmo,
                start - 86_400.0,
                10_000,
            );
            assert!(cloud.stored.as_ref().unwrap().iter().any(|h| h.trk == 0));
            let limited = hypotheses(
                &case.events[0],
                &case.tracks,
                &case.p,
                &frame,
                &dem,
                &atmo,
                start - 86_400.0,
                0,
            );
            assert_eq!(limited.count, cloud.count);
            assert!(limited.stored.is_none());
        }
    }

    #[test]
    fn unknown_atmospheres_preserve_calm_clouds_and_count_the_extra_work() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let start = 1_788_895_000.0;
        let mut case = wind_delayed_case(med, start);
        case.p.start_lo = start - 120.0;
        case.p.start_hi = start + 120.0;
        let enu = Enu::new(case.obs.0, case.obs.1, 0.0);
        // Match the pipeline's 1200 s pre-padding. Earlier flights pass the same place,
        // so the work comparison includes genuine geometry/atmosphere evaluations.
        for i in 0..10 {
            let mut tr = straight_track(
                &enu,
                [0.0, 8000.0, 1200.0],
                [90.0, 0.0, 0.0],
                start - 1100.0 + 100.0 * i as f64,
                case.p.start_lo - 1200.0,
                case.p.start_hi + case.p.duration + 1200.0,
                5.0,
            );
            tr.key = format!("earlier{i}");
            case.tracks.push(tr);
        }
        let dem = FlatElevation(0.0);
        let frame = Frame::new(&case.p.region);
        let bits = |set: &HypSet| {
            set.stored
                .as_ref()
                .unwrap()
                .iter()
                .map(|h| {
                    (
                        h.trk,
                        h.t_e.to_bits(),
                        h.e.to_bits(),
                        h.n.to_bits(),
                        h.t_s.to_bits(),
                    )
                })
                .collect::<Vec<_>>()
        };
        for c_fit in [343.0, 300.0] {
            let mut ev = case.events[0].clone();
            ev.c_fit = c_fit;
            let evaluate = |advertise| {
                let atmo = CountedAtmosphere {
                    medium: med,
                    advertise,
                    calls: AtomicUsize::new(0),
                };
                let started = std::time::Instant::now();
                let set = hypotheses(
                    &ev,
                    &case.tracks,
                    &case.p,
                    &frame,
                    &dem,
                    &atmo,
                    start - 86_400.0,
                    100_000,
                );
                (set, atmo.calls.load(Ordering::Relaxed), started.elapsed())
            };
            let (known, known_calls, known_time) = evaluate(true);
            let (unknown, unknown_calls, unknown_time) = evaluate(false);
            assert_eq!(known.count, unknown.count);
            assert_eq!(bits(&known), bits(&unknown));
            if c_fit == 343.0 {
                assert!(known.count > 0);
                assert!(unknown_calls > known_calls);
            } else {
                // A different fitting speed must not take the nominal still-air shortcut.
                assert_eq!(unknown_calls, known_calls);
            }
            eprintln!(
                "fit c={c_fit}: known {known_calls} atmosphere evaluations in {known_time:?}; unknown {unknown_calls} in {unknown_time:?}; {} identical hypotheses",
                known.count
            );
        }
    }

    #[test]
    fn narrow_clock_search_keeps_wind_delayed_flyovers() {
        for (east, north) in [(0.0, 35.0), (35.0, 0.0), (0.0, -35.0), (-35.0, 0.0)] {
            let med = fdop_types::atmo::Medium {
                c: 343.0,
                wind_e: east,
                wind_n: north,
            };
            for start in [0.0, 1_788_895_000.0] {
                let case = wind_delayed_case(med, start);
                check_wind_delayed_case(&case, med, run_case(&case, med));
            }
        }
    }

    #[test]
    fn narrow_clock_still_air_search_control() {
        let med = fdop_types::atmo::Medium::still(343.0);
        for start in [0.0, 1_788_895_000.0] {
            let case = wind_delayed_case(med, start);
            let result = check_wind_delayed_case(&case, med, run_case(&case, med));
            // Record the complete result, including its evidence and contour, for the
            // immutable before/after still-air comparison used with this regression.
            let fingerprint = serde_json::to_vec(&result)
                .unwrap()
                .into_iter()
                .fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
                    (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
                });
            eprintln!("still-air start {start}: result fingerprint {fingerprint:016x}");
        }
    }

    #[test]
    fn narrow_clock_uses_wind_on_the_observers_path() {
        struct LocalWind {
            centre: (f64, f64),
        }
        impl Atmosphere for LocalWind {
            fn medium(
                &self,
                lat: f64,
                lon: f64,
                _: f64,
                _: f64,
                _: f64,
            ) -> fdop_types::atmo::Medium {
                let range = haversine_m(self.centre.0, self.centre.1, lat, lon);
                fdop_types::atmo::Medium {
                    c: 343.0,
                    wind_e: 0.0,
                    wind_n: 35.0 * ((2000.0 - range) / 1500.0).clamp(0.0, 1.0),
                }
            }
            fn describe(&self) -> String {
                "synthetic observer-local wind".into()
            }
        }
        let med = fdop_types::atmo::Medium {
            c: 343.0,
            wind_e: 0.0,
            wind_n: 35.0,
        };
        let case = wind_delayed_case(med, 1_788_895_000.0);
        let atmo = LocalWind { centre: case.obs };
        let dem = FlatElevation(0.0);
        let o = observer(case.obs.0, case.obs.1, &dem);
        for (event, tr) in case.events.iter().zip(&case.tracks) {
            let cpa = tr
                .cpa_nearest_time(o.x, case.t_s + event.t_rec, 100.0)
                .unwrap();
            let ((lat, lon, h), _) = tr.geo_at(cpa.t_e);
            assert_eq!(atmo.medium(lat, lon, cpa.t_e, MIC_HEIGHT_M, h).wind_n, 0.0);
            assert_eq!(atmo.medium(o.lat, o.lon, cpa.t_e, MIC_HEIGHT_M, h), med);
        }
        let result = run(
            &SearchInputs {
                events: case.events.clone(),
                tracks: &case.tracks,
                heard_times: vec![],
                coarse_dem: &dem,
                fine_dem: &dem,
                atmo: &atmo,
            },
            &case.p,
            &|_| {},
        );
        check_wind_delayed_case(&case, med, result);
    }

    #[test]
    fn narrow_region_does_not_skip_close_fast_flyovers() {
        let (lat, lon) = (40.876466, -73.524951);
        let enu = Enu::new(lat, lon, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let t_s = 1_788_895_000.0;
        let med = fdop_types::atmo::Medium::still(343.0);
        let prop = Prop::from_medium(&med, &enu);
        let mut tracks = Vec::new();
        let mut events = Vec::new();
        for (key, position, velocity, t_cpa, f0) in [
            (
                "A",
                [0.0, 60.0, 220.0 + MIC_HEIGHT_M],
                [250.0, 0.0, 0.0],
                120.0,
                112.7,
            ),
            (
                "B",
                [100.0, 0.0, 220.0 + MIC_HEIGHT_M],
                [0.0, 263.0, 0.0],
                360.0,
                149.3,
            ),
        ] {
            let mut tr = straight_track(
                &enu,
                position,
                velocity,
                t_s + t_cpa,
                t_s - 600.0,
                t_s + 1200.0,
                5.0,
            );
            tr.key = key.into();
            let mut ev = synth_event(key, &tr, x, t_s, f0, 343.0, t_s + t_cpa, &prop);
            // The existing generous timing/distance floors for a model-misfit event. A
            // wider tolerance changes the scan's initial phase, not whether a pass exists.
            ev.t_tol = 9.0;
            ev.misfit = true;
            ev.d_gate[1] *= 1.75 / 1.25;
            let cpa = tr.cpa_nearest_time(x, t_s + t_cpa, 30.0).unwrap();
            assert!(gates_ok(&ev, &cpa, &prop, 0.0));
            tracks.push(tr);
            events.push(ev);
        }
        let mut p = SearchParams::new(
            t_s,
            t_s,
            600.0,
            BBox::new(lat - 0.002, lon - 0.002, lat + 0.002, lon + 0.002),
        );
        p.refine_half_m = 200.0;
        let dem = FlatElevation(0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        let frame = Frame::new(&p.region);
        let counts: Vec<_> = events
            .iter()
            .map(|ev| {
                hypotheses(ev, &tracks, &p, &frame, &dem, &atmo, t_s - 86_400.0, 10_000).count
            })
            .collect();
        assert!(
            counts.iter().all(|&n| n > 0),
            "both real passes need hypotheses: {counts:?}"
        );
        let inputs = SearchInputs {
            heard_times: events.iter().map(|ev| ev.t_rec).collect(),
            events,
            tracks: &tracks,
            coarse_dem: &dem,
            fine_dem: &dem,
            atmo: &atmo,
        };
        let result = run(&inputs, &p, &|_| {});
        let winner = result.candidates.first().unwrap_or_else(|| {
            panic!(
                "both real passes were skipped: counts={counts:?}, funnel={:?}",
                result.funnel
            )
        });
        assert!(
            winner
                .events
                .iter()
                .any(|m| m.event == "A" && m.flight == "A")
        );
        assert!(
            winner
                .events
                .iter()
                .any(|m| m.event == "B" && m.flight == "B")
        );
        assert!(haversine_m(lat, lon, winner.lat, winner.lon) < 75.0);
        assert!((winner.t_start_utc - t_s).abs() < 0.2);
        assert!(result.funnel.truncated.is_empty());
    }

    fn circuit_flyover_case(med: fdop_types::atmo::Medium) -> TwoCase {
        let (lat, lon) = (40.8765, -73.5250);
        let enu = Enu::new(lat, lon, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let t_s = 1_788_895_000.0;
        let omega = 2.0 * std::f64::consts::PI / 800.0;
        let mut circuit = straight_track(
            &enu,
            [1000.0, 0.0, 900.0],
            [0.0, -5000.0 * omega, 0.0],
            t_s + 400.0,
            t_s,
            t_s + 1600.0,
            5.0,
        );
        circuit.key = "circuit".into();
        // One continuous aircraft track, with two real CPAs a full circuit apart.
        for (i, &t) in circuit.t.iter().enumerate() {
            let theta = std::f64::consts::PI + omega * (t - t_s - 400.0);
            let (sin, cos) = theta.sin_cos();
            circuit.pos[i] = enu.to_ecef([6000.0 + 5000.0 * cos, 5000.0 * sin, 900.0]);
            circuit.vel[i] = enu.vec_to_ecef([-5000.0 * omega * sin, 5000.0 * omega * cos, 0.0]);
        }
        let mut crossing = straight_track(
            &enu,
            [0.0, 2000.0, 1200.0],
            [100.0, 0.0, 0.0],
            t_s + 800.0,
            t_s + 200.0,
            t_s + 1400.0,
            5.0,
        );
        crossing.key = "crossing".into();
        let prop = Prop::from_medium(&med, &enu);
        let first = synth_event("first", &circuit, x, t_s, 112.7, 343.0, t_s + 400.0, &prop);
        let middle = synth_event(
            "middle",
            &crossing,
            x,
            t_s,
            129.5,
            343.0,
            t_s + 800.0,
            &prop,
        );
        let mut returning = synth_event(
            "returning",
            &circuit,
            x,
            t_s,
            114.5,
            343.0,
            t_s + 1200.0,
            &prop,
        );
        returning.quality = 90.0;
        let region = BBox::new(lat - 0.025, lon - 0.035, lat + 0.025, lon + 0.035);
        let mut p = SearchParams::new(t_s - 10.0, t_s + 10.0, 1600.0, region);
        // The returning pass must confirm; it cannot create a second pair seed instead.
        p.max_events = 2;
        p.refine_half_m = 400.0;
        TwoCase {
            tracks: vec![circuit, crossing],
            events: vec![first, middle, returning],
            p,
            t_s,
            obs: (lat, lon),
        }
    }

    /// A 120 s circuit at 63 m/s, with a slow climb/descent giving alternating 800 m and
    /// 400 m heights at the near side. The farther visit at 300 s has closer neighbours on
    /// both sides inside the old CPA windows; it must still match its own recording time.
    fn narrow_circuit_case(med: fdop_types::atmo::Medium) -> TwoCase {
        let (lat, lon) = (40.8765, -73.5250);
        let enu = Enu::new(lat, lon, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let t_s = 1_788_895_000.0;
        let omega = std::f64::consts::TAU / 120.0;
        let mut circuit = straight_track(
            &enu,
            [300.0, 0.0, 800.0],
            [0.0, 1200.0 * omega, 0.0],
            t_s + 300.0,
            t_s,
            t_s + 900.0,
            2.0,
        );
        circuit.key = "narrow-circuit".into();
        for (i, &t) in circuit.t.iter().enumerate() {
            let theta = omega * (t - t_s - 300.0);
            let (sin, cos) = theta.sin_cos();
            let (zsin, zcos) = (0.5 * theta).sin_cos();
            circuit.pos[i] =
                enu.to_ecef([1500.0 - 1200.0 * cos, 1200.0 * sin, 600.0 + 200.0 * zcos]);
            circuit.vel[i] = enu.vec_to_ecef([
                1200.0 * omega * sin,
                1200.0 * omega * cos,
                -100.0 * omega * zsin,
            ]);
        }
        let prop = Prop::from_medium(&med, &enu);
        // Synthesize each known pass on a separately clipped copy, so event creation cannot
        // accidentally choose its closer neighbour or rely on the new CPA selector.
        let event = |id: &str, at: f64, f0: f64| {
            let lo = circuit.t.partition_point(|&t| t < t_s + at - 80.0);
            let hi = circuit.t.partition_point(|&t| t <= t_s + at + 80.0);
            let isolated = TrackModel {
                t: circuit.t[lo..hi].to_vec(),
                pos: circuit.pos[lo..hi].to_vec(),
                vel: circuit.vel[lo..hi].to_vec(),
                ..circuit.clone()
            };
            synth_event(id, &isolated, x, t_s, f0, 343.0, t_s + at, &prop)
        };
        let near = event("near-visit", 420.0, 112.7);
        let mut far = event("far-visit", 300.0, 151.2);
        far.quality = 90.0;
        let mut crossing = straight_track(
            &enu,
            [0.0, 1500.0, 900.0],
            [90.0, 0.0, 0.0],
            t_s + 650.0,
            t_s + 100.0,
            t_s + 850.0,
            5.0,
        );
        crossing.key = "crossing".into();
        let other = synth_event(
            "crossing",
            &crossing,
            x,
            t_s,
            129.5,
            343.0,
            t_s + 650.0,
            &prop,
        );
        let region = BBox::new(lat - 0.015, lon - 0.02, lat + 0.015, lon + 0.02);
        let mut p = SearchParams::new(t_s - 10.0, t_s + 10.0, 850.0, region);
        p.max_events = 2;
        p.refine_half_m = 400.0;
        TwoCase {
            tracks: vec![circuit, crossing],
            events: vec![near, other, far],
            p,
            t_s,
            obs: (lat, lon),
        }
    }

    #[test]
    fn narrow_circuit_pair_preserves_both_emission_hypotheses() {
        for med in [
            fdop_types::atmo::Medium::still(343.0),
            fdop_types::atmo::Medium {
                c: 339.0,
                wind_e: 7.0,
                wind_n: -5.0,
            },
        ] {
            let case = narrow_circuit_case(med);
            let dem = FlatElevation(0.0);
            let o = observer(case.obs.0, case.obs.1, &dem);
            let atmo = fdop_types::atmo::UniformAtmosphere(med);
            let nearer = case.tracks[0]
                .cpa_near(o.x, case.t_s + 300.0, 150.0)
                .unwrap();
            assert!((nearer.t_e - case.t_s - 300.0).abs() > 100.0);
            assert!(nearer.d < case.events[2].d_gate[0]);
            let assign = [(2, 0, case.t_s + 300.0), (0, 0, case.t_s + 420.0)];
            for quick in [true, false] {
                let pe = eval_point(
                    &o,
                    &assign,
                    &case.events,
                    &case.tracks,
                    &case.p,
                    &atmo,
                    true,
                    true,
                    quick,
                )
                .expect("both known passes must survive their pair's clock and trace checks");
                assert!((pe.cpas[0].t_e - case.t_s - 300.0).abs() < 0.01);
                assert!((pe.cpas[1].t_e - case.t_s - 420.0).abs() < 0.01);
                assert!((pe.t_s - case.t_s).abs() < 0.1, "{}", pe.t_s - case.t_s);
                assert!(pe.chis.iter().all(|c| c.chi < 1.0 && c.cov >= 0.99));
            }
        }
    }

    #[test]
    fn narrow_circuit_confirmer_uses_the_clock_and_excludes_only_the_used_pass() {
        for med in [
            fdop_types::atmo::Medium::still(343.0),
            fdop_types::atmo::Medium {
                c: 339.0,
                wind_e: 7.0,
                wind_n: -5.0,
            },
        ] {
            let case = narrow_circuit_case(med);
            let dem = FlatElevation(0.0);
            let o = observer(case.obs.0, case.obs.1, &dem);
            let atmo = fdop_types::atmo::UniformAtmosphere(med);
            let tix = TimeIndex::new(&case.tracks);
            let explain = |exclude: &[(u32, f64)]| {
                best_confirmer(
                    &o,
                    case.t_s,
                    &case.events[2],
                    &case.tracks,
                    &tix,
                    &case.p,
                    &atmo,
                    exclude,
                )
            };
            let (chi, matched, track) = explain(&[(0, case.t_s + 420.0)])
                .expect("a closer pass already heard must not hide the farther return");
            assert_eq!(track, 0);
            assert!(chi < 1.0, "{matched:?}");
            let cpa = case.tracks[0]
                .cpa_nearest_time(o.x, case.t_s + 300.0, 30.0)
                .unwrap();
            let prop = prop_for(&atmo, &o, &case.tracks[0], cpa.t_e);
            assert!((matched.t_cpa_heard_utc - cpa.t_r_p(&prop)).abs() < 0.01);
            assert!((cpa.t_r_fit_p(&prop) - case.t_s - case.events[2].t_rec).abs() < 0.01);
            let emission_seed = matched.t_cpa_heard_utc - matched.d / matched.c_path;
            assert!(
                explain(&[(0, emission_seed)]).is_none(),
                "a second tone cannot count the same physical pass twice"
            );
        }
    }

    #[test]
    fn spatial_bounds_keep_grazing_and_unknown_tracks() {
        let case = narrow_circuit_case(fdop_types::atmo::Medium::still(343.0));
        let o = observer(case.obs.0, case.obs.1, &FlatElevation(0.0));
        // A track exactly 1 km from the observer on one ECEF box face, including
        // negative Earth-scale coordinates. A small inward radius must prune it.
        let grazing = TrackModel {
            t: vec![case.t_s, case.t_s + 2.0],
            pos: vec![
                [o.x[0] + 1000.0, o.x[1], o.x[2] - 1.0],
                [o.x[0] + 1000.0, o.x[1], o.x[2] + 1.0],
            ],
            vel: vec![[0.0, 0.0, 1.0]; 2],
            ..case.tracks[0].clone()
        };
        let mut far = grazing.clone();
        for p in &mut far.pos {
            p[0] += 100_000.0;
        }
        let mut unknown = far.clone();
        unknown.vel[0][0] = f64::NAN;
        let tracks = [far, grazing, unknown];
        let index = TimeIndex::new(&tracks);
        assert!(index.bounds[2].is_none());
        let query = |radius| {
            index
                .overlapping_near(case.t_s, case.t_s + 2.0, o.x, radius)
                .collect::<Vec<_>>()
        };
        assert_eq!(query(1000.0), vec![1, 2]);
        assert_eq!(query(999.99), vec![2]);
        assert_eq!(query(f64::NAN), vec![0, 1, 2]);
        assert_eq!(query(f64::INFINITY), vec![0, 1, 2]);
        assert_eq!(
            index
                .overlapping_near(case.t_s + 3.0, case.t_s + 4.0, o.x, 1000.0)
                .count(),
            0
        );
    }

    #[test]
    fn spatial_bounds_preserve_confirmers_and_unheard_passes() {
        for med in [
            fdop_types::atmo::Medium::still(343.0),
            fdop_types::atmo::Medium {
                c: 339.0,
                wind_e: 7.0,
                wind_n: -5.0,
            },
        ] {
            let mut case = narrow_circuit_case(med);
            let enu = Enu::new(case.obs.0, case.obs.1, 0.0);
            let dem = FlatElevation(0.0);
            let o = observer(case.obs.0, case.obs.1, &dem);
            let atmo = fdop_types::atmo::UniformAtmosphere(med);
            let prop = Prop::from_medium(&med, &enu);
            // The knots stay more than 4 km away while the central 80 s Hermite
            // segment comes within 800 m. The outer segments cover the full heard
            // trace, including sound delay, within the unchanged 90 s track-gap gate.
            let bowed = TrackModel {
                key: "between-knots".into(),
                t: [400.0, 460.0, 540.0, 600.0].map(|t| case.t_s + t).to_vec(),
                pos: vec![
                    enu.to_ecef([11100.0, -7000.0, 700.0]),
                    enu.to_ecef([3000.0, -2800.0, 700.0]),
                    enu.to_ecef([3000.0, 2800.0, 700.0]),
                    enu.to_ecef([11100.0, 7000.0, 700.0]),
                ],
                vel: vec![
                    enu.vec_to_ecef([-135.0, 70.0, 0.0]),
                    enu.vec_to_ecef([-135.0, 70.0, 0.0]),
                    enu.vec_to_ecef([135.0, 70.0, 0.0]),
                    enu.vec_to_ecef([135.0, 70.0, 0.0]),
                ],
                ..case.tracks[0].clone()
            };
            assert!(bowed.pos.iter().all(|&p| norm(sub(p, o.x)) > 4000.0));
            let cpa = bowed.cpa_near(o.x, case.t_s + 500.0, 80.0).unwrap();
            assert!(cpa.interior && cpa.d < 800.0);
            let event = synth_event(
                "between-knots",
                &bowed,
                o.x,
                case.t_s,
                143.5,
                343.0,
                case.t_s + 500.0,
                &prop,
            );
            let knot_box_distance = norm(std::array::from_fn(|k| {
                let lo = bowed.pos.iter().map(|p| p[k]).fold(f64::INFINITY, f64::min);
                let hi = bowed
                    .pos
                    .iter()
                    .map(|p| p[k])
                    .fold(f64::NEG_INFINITY, f64::max);
                (lo - o.x[k]).max(o.x[k] - hi).max(0.0)
            }));
            assert!(knot_box_distance > 1.15 * event.d_gate[1]);
            case.events.push(event);
            case.tracks.push(bowed);
            let mut silent = straight_track(
                &enu,
                [100.0, 0.0, 100.0],
                [0.0, 55.0, 0.0],
                case.t_s + 500.0,
                case.t_s + 100.0,
                case.t_s + 850.0,
                5.0,
            );
            silent.key = "silent-close-pass".into();
            let heard = silent
                .cpa_near(o.x, case.t_s + 500.0, 100.0)
                .unwrap()
                .t_r_fit_p(&prop)
                - case.t_s;
            case.tracks.push(silent);
            let far_start = case.tracks.len();
            for k in 0..1024 {
                let mut far = straight_track(
                    &enu,
                    [40_000.0 + 10.0 * k as f64, -40_000.0, 1200.0],
                    [70.0, 0.0, 0.0],
                    case.t_s + 450.0,
                    case.t_s,
                    case.t_s + 900.0,
                    450.0,
                );
                far.key = format!("distant-{k}");
                case.tracks.push(far);
            }
            let bounded = TimeIndex::new(&case.tracks);
            // This is the former complete path: the identical time index and
            // scorer, with no track ever excluded by a spatial bound.
            let unbounded = TimeIndex {
                by_t0: bounded.by_t0.clone(),
                max_len: bounded.max_len,
                bounds: vec![None; case.tracks.len()],
            };
            assert_eq!(
                bounded.overlapping(case.t_s, case.t_s + 900.0).count(),
                case.tracks.len()
            );
            assert!(
                bounded
                    .overlapping_near(case.t_s, case.t_s + 900.0, o.x, 5000.0)
                    .all(|ti| (ti as usize) < far_start)
            );
            let evaluate = |index: &TimeIndex| {
                let mut matches = Vec::new();
                for (east, north, dt) in [(0.0, 0.0, 0.0), (37.0, -61.0, 0.5), (0.0, 0.0, 30.0)] {
                    let (lat, lon, _) = enu.to_geodetic([east, north, 0.0]);
                    let obs = observer(lat, lon, &dem);
                    for ev in &case.events {
                        for exclude in [vec![], vec![(0, case.t_s + 420.0)]] {
                            matches.push(best_confirmer(
                                &obs,
                                case.t_s + dt,
                                ev,
                                &case.tracks,
                                index,
                                &case.p,
                                &atmo,
                                &exclude,
                            ));
                        }
                    }
                }
                let candidate = Candidate {
                    t_start_utc: case.t_s,
                    events: [0, 2]
                        .map(|k| matches[k].as_ref().unwrap().1.clone())
                        .to_vec(),
                    ..Candidate::default()
                };
                let unheard: Vec<_> = [vec![], vec![heard], vec![heard + 30.0]]
                    .iter()
                    .map(|heard| {
                        unheard_passes(&candidate, &o, &case.tracks, index, heard, &case.p, &atmo)
                    })
                    .collect();
                (matches, unheard)
            };
            let before = std::time::Instant::now();
            let expected = evaluate(&unbounded);
            let unbounded_elapsed = before.elapsed();
            let before = std::time::Instant::now();
            let actual = evaluate(&bounded);
            let bounded_elapsed = before.elapsed();
            // Full metadata, floating-point results, selected track and ordering
            // must match, including absent matches and same-pass exclusions.
            assert_eq!(
                serde_json::to_vec(&actual).unwrap(),
                serde_json::to_vec(&expected).unwrap()
            );
            assert!(actual.0[1].is_none(), "one pass cannot confirm twice");
            assert_eq!(actual.0[4].as_ref().unwrap().1.flight, "narrow-circuit");
            assert_eq!(actual.0[6].as_ref().unwrap().1.flight, "between-knots");
            assert_eq!(actual.1[0].len(), 1);
            assert!(actual.1[0][0].starts_with("silent-close-pass "));
            assert!(actual.1[1].is_empty());
            assert_eq!(actual.1[2], actual.1[0]);
            eprintln!(
                "1024 distant tracks, {med:?}: confirmation/unheard unbounded {unbounded_elapsed:?}, bounded {bounded_elapsed:?}"
            );
        }
    }

    #[test]
    fn narrow_circuit_farther_visit_survives_search_and_refinement() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let case = narrow_circuit_case(med);
        let result = run_case(&case, med);
        assert!(result.funnel.hypotheses[2].1.is_none());
        let top = result.candidates.first().expect("two locating passes");
        assert_eq!(top.n_explained, 3, "{:?}", top.events);
        let far = top.events.iter().find(|e| e.event == "far-visit").unwrap();
        assert_eq!(far.flight, "narrow-circuit");
        assert_eq!(far.role, "confirmer");
        assert!(far.core, "{far:?}");
        assert!((top.t_start_utc - case.t_s).abs() < 1.0);
        let refined = result.refine.as_ref().expect("all three passes refine");
        assert!(haversine_m(refined.lat, refined.lon, case.obs.0, case.obs.1) < 60.0);
    }

    #[test]
    fn fine_seeds_keep_clock_cells_and_merge_same_clock_duplicates_deterministically() {
        // This Unix time lies exactly on a 20 s boundary. Fractional seconds on either
        // side must keep the join's distinct clock cells when the spatial cells are merged.
        let start = 1_788_895_000.0;
        let seed = Joined {
            ei: 0,
            ej: 1,
            ti: 0,
            tj: 1,
            te_i: start + 300.0,
            te_j: start + 600.0,
            e: 100.0,
            n: 100.0,
            t_s: start + 0.125,
        };
        let before = Joined {
            t_s: start - 0.125,
            ..seed
        };
        let later = Joined {
            te_i: seed.te_i + 1200.0,
            te_j: seed.te_j + 1200.0,
            t_s: seed.t_s + 1200.0,
            ..seed
        };
        let better = Joined { e: 150.0, ..seed };
        let tied = Joined { e: 200.0, ..seed };
        let other_pair = Joined { tj: 2, ..seed };
        let mut input = vec![
            (seed, 0.4),
            (tied, 0.9),
            (before, 0.5),
            (later, 0.5),
            (better, 0.9),
            (other_pair, 0.1),
        ];
        let summarize = |seeds: Vec<(Joined, f64)>| {
            seeds
                .into_iter()
                .map(|(j, s)| (j.tj, j.t_s, j.e, s))
                .collect::<Vec<_>>()
        };
        let expected = vec![
            (1, seed.t_s, better.e, 0.9),
            // The other flight pair still gets its first cell before this pair's second.
            (2, seed.t_s, seed.e, 0.1),
            (1, before.t_s, seed.e, 0.5),
            (1, later.t_s, seed.e, 0.5),
        ];
        assert_eq!(summarize(ranked_fine_seeds(input.clone())), expected);
        input.reverse();
        assert_eq!(summarize(ranked_fine_seeds(input.clone())), expected);
        for t_s in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300] {
            input.push((Joined { t_s, ..seed }, 1.0));
        }
        assert_eq!(summarize(ranked_fine_seeds(input)), expected);
    }

    #[test]
    fn repeated_flight_pair_keeps_both_recording_clocks() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let (lat, lon) = (40.8765, -73.5250);
        let enu = Enu::new(lat, lon, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let t_s = 1_788_895_000.0;
        let period = 1200.0;
        let omega = std::f64::consts::TAU / period;
        let prop = Prop::from_medium(&med, &enu);
        let mut tracks = Vec::new();
        let mut events = Vec::new();
        for (key, radius, distance, altitude, cpa, rotate, f0) in [
            ("northbound", 10_000.0, 1500.0, 900.0, 300.0, false, 112.7),
            ("eastbound", 15_000.0, 2500.0, 1200.0, 600.0, true, 129.5),
        ] {
            let mut tr = straight_track(
                &enu,
                [distance, 0.0, altitude],
                [0.0, radius * omega, 0.0],
                t_s + cpa,
                t_s,
                t_s + period + 900.0,
                5.0,
            );
            tr.key = key.into();
            // Identical laps produce the same two curves at two possible recording clocks.
            // Reduce the phase before sin/cos so the repeated track samples agree exactly.
            for (i, &t) in tr.t.iter().enumerate() {
                let (sin, cos) = (omega * (t - t_s - cpa).rem_euclid(period)).sin_cos();
                let mut position = [distance + radius * (1.0 - cos), radius * sin, altitude];
                let mut velocity = [radius * omega * sin, radius * omega * cos, 0.0];
                if rotate {
                    position.swap(0, 1);
                    velocity.swap(0, 1);
                }
                tr.pos[i] = enu.to_ecef(position);
                tr.vel[i] = enu.vec_to_ecef(velocity);
            }
            events.push(synth_event(key, &tr, x, t_s, f0, 343.0, t_s + cpa, &prop));
            tracks.push(tr);
        }
        // Only the later clock has this third flyover. It may confirm the pair, but cannot
        // rescue a lost clock by seeding its own pair: the two repeating events drive the join.
        let mut confirmer = straight_track(
            &enu,
            [2000.0, -2000.0, 1200.0],
            [70.0, 70.0, 0.0],
            t_s + period + 800.0,
            t_s + period + 600.0,
            t_s + period + 1000.0,
            5.0,
        );
        confirmer.key = "later-only".into();
        let mut event = synth_event(
            "later-only",
            &confirmer,
            x,
            t_s + period,
            160.0,
            343.0,
            t_s + period + 800.0,
            &prop,
        );
        event.quality = 90.0;
        events.push(event);
        tracks.push(confirmer);
        let region = BBox::new(lat - 0.006, lon - 0.008, lat + 0.006, lon + 0.008);
        let mut p = SearchParams::new(t_s - 60.0, t_s + period + 60.0, 900.0, region);
        p.max_events = 2;
        p.refine_half_m = 100.0;
        let case = TwoCase {
            tracks,
            events,
            p,
            t_s,
            obs: (lat, lon),
        };
        let result = run_case(&case, med);
        assert!(result.funnel.truncated.is_empty(), "{:?}", result.funnel);
        assert!(result.funnel.hypotheses[2].1.is_none());
        for start in [t_s, t_s + period] {
            assert!(
                result.candidates.iter().any(|candidate| {
                    (candidate.t_start_utc - start).abs() < 3.0
                        && haversine_m(candidate.lat, candidate.lon, lat, lon) < 150.0
                        && candidate.events.len() >= 2
                }),
                "lost recording clock {start}, funnel {:?}: {:?}",
                result.funnel,
                result
                    .candidates
                    .iter()
                    .map(|c| (c.t_start_utc - t_s, c.lat, c.lon))
                    .collect::<Vec<_>>()
            );
        }
        let winner = result.candidates.first().unwrap();
        assert!((winner.t_start_utc - (t_s + period)).abs() < 1.0);
        assert_eq!(winner.events.len(), 3);
        let matched = winner
            .events
            .iter()
            .find(|m| m.event == "later-only")
            .unwrap();
        assert_eq!(matched.flight, "later-only");
        assert_eq!(matched.role, "confirmer");
    }

    #[test]
    fn returning_aircraft_pass_can_confirm_a_candidate() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let case = circuit_flyover_case(med);
        let result = run_case(&case, med);
        assert_eq!(result.funnel.events_used, 3);
        assert!(result.funnel.hypotheses[2].1.is_none());
        let top = result
            .candidates
            .first()
            .expect("the two initial flyovers locate the observer");
        assert_eq!(top.n_explained, 3, "{:?}", top.events);
        let returning = top.events.iter().find(|m| m.event == "returning").unwrap();
        assert_eq!(returning.flight, "circuit");
        assert_eq!(returning.role, "confirmer");
        assert!(returning.core, "{returning:?}");
        assert!((top.t_start_utc - case.t_s).abs() < 1.0);
        let refined = result.refine.as_ref().unwrap();
        assert!(haversine_m(refined.lat, refined.lon, case.obs.0, case.obs.1) < 60.0);
    }

    #[test]
    fn same_aircraft_pass_cannot_confirm_twice() {
        for med in [
            fdop_types::atmo::Medium::still(343.0),
            fdop_types::atmo::Medium {
                c: 339.0,
                wind_e: 7.0,
                wind_n: -5.0,
            },
        ] {
            let case = circuit_flyover_case(med);
            let dem = FlatElevation(0.0);
            let o = observer(case.obs.0, case.obs.1, &dem);
            let atmo = fdop_types::atmo::UniformAtmosphere(med);
            let tix = TimeIndex::new(&case.tracks);
            let mut duplicate = case.events[0].clone();
            duplicate.id = "another-tone-of-first".into();
            let explain = |exclude: &[(u32, f64)]| {
                best_confirmer(
                    &o,
                    case.t_s,
                    &duplicate,
                    &case.tracks,
                    &tix,
                    &case.p,
                    &atmo,
                    exclude,
                )
            };
            let (_, original, _) = explain(&[]).expect("the original pass must fit");
            let emission_seed = original.t_cpa_heard_utc - original.d / original.c_path;
            assert!(
                explain(&[(0, emission_seed)]).is_none(),
                "one pass cannot supply two matches"
            );
        }
    }

    #[test]
    fn unheard_closer_return_of_an_explained_aircraft_counts() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let mut case = circuit_flyover_case(med);
        let dem = FlatElevation(0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        let o = observer(case.obs.0, case.obs.1, &dem);
        let tix = TimeIndex::new(&case.tracks);
        let candidate = explain_at(
            &SearchInputs {
                events: case.events[..2].to_vec(),
                tracks: &case.tracks,
                heard_times: vec![],
                coarse_dem: &dem,
                fine_dem: &dem,
                atmo: &atmo,
            },
            &case.p,
            case.obs.0,
            case.obs.1,
            case.t_s,
            5.0,
            &[
                ("first".into(), "circuit".into()),
                ("middle".into(), "crossing".into()),
            ],
        )
        .unwrap();
        // Explained passes and an equally distant return cannot fall inside half the
        // nearest explained distance, even without a separate list of heard detections.
        assert!(unheard_passes(&candidate, &o, &case.tracks, &tix, &[], &case.p, &atmo).is_empty());

        // Between the two circuit passes, smoothly shift the circle 900 m toward the
        // observer and descend 700 m. The original explained traces remain unchanged.
        let enu = Enu::new(case.obs.0, case.obs.1, 0.0);
        let circuit = &mut case.tracks[0];
        for (i, &t) in circuit.t.iter().enumerate() {
            let u = ((t - case.t_s - 600.0) / 400.0).clamp(0.0, 1.0);
            let shift = u * u * (3.0 - 2.0 * u);
            let rate = 6.0 * u * (1.0 - u) / 400.0;
            let pos = enu.from_ecef(circuit.pos[i]);
            let vel = enu.vec_from_ecef(circuit.vel[i]);
            circuit.pos[i] = enu.to_ecef([pos[0] - 900.0 * shift, pos[1], pos[2] - 700.0 * shift]);
            circuit.vel[i] =
                enu.vec_to_ecef([vel[0] - 900.0 * rate, vel[1], vel[2] - 700.0 * rate]);
        }
        let close = circuit.cpa_near(o.x, case.t_s + 1200.0, 100.0).unwrap();
        assert!(candidate.events.iter().all(|m| close.d < 0.5 * m.d));
        let t_rec = close.t_r(case.p.c) - candidate.t_start_utc;
        // The fixture changed the catalogue after indexing; production keeps it immutable.
        // Rebuild its bounds, and compare with the original unscreened time query.
        let mut unbounded = tix;
        unbounded.bounds.fill(None);
        let baseline = unheard_passes(
            &candidate,
            &o,
            &case.tracks,
            &unbounded,
            &[],
            &case.p,
            &atmo,
        );
        assert_eq!(
            baseline.len(),
            1,
            "the unpruned return was missed: {baseline:?}"
        );
        let tix = TimeIndex::new(&case.tracks);
        let silent = unheard_passes(&candidate, &o, &case.tracks, &tix, &[], &case.p, &atmo);
        assert_eq!(silent, baseline);
        assert_eq!(
            silent.len(),
            1,
            "the silent close return was missed: {silent:?}"
        );
        assert!(silent[0].starts_with("circuit "), "{silent:?}");
        assert!(
            unheard_passes(&candidate, &o, &case.tracks, &tix, &[t_rec], &case.p, &atmo).is_empty()
        );
        // An unrelated detection outside the existing 25-second hearing window cannot excuse it.
        assert_eq!(
            unheard_passes(
                &candidate,
                &o,
                &case.tracks,
                &tix,
                &[t_rec + 30.0],
                &case.p,
                &atmo,
            )
            .len(),
            1
        );
    }

    #[test]
    fn over_budget_events_only_confirm_and_say_so() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let mut case = two_flyover_case(med);
        case.p.max_hyp_per_event = 10;
        let res = run_case(&case, med);
        assert!(res.candidates.is_empty(), "funnel {:?}", res.funnel);
        for id in ["e1", "e2"] {
            assert!(
                res.warnings
                    .iter()
                    .any(|w| w.starts_with(&format!("{id}: ")) && w.contains("only confirms")),
                "{:?}",
                res.warnings
            );
        }
    }

    #[test]
    fn quick_pair_budget_counts_rejected_seeds_before_starting_another_pair() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let mut case = two_flyover_case(med);
        let enu = Enu::new(case.obs.0, case.obs.1, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let prop = Prop::from_medium(&med, &enu);
        let mut third = synth_event(
            "e3",
            &case.tracks[2],
            x,
            case.t_s,
            90.0,
            343.0,
            case.t_s + 1500.0,
            &prop,
        );
        // Keep the known A/B pair first; the third actual pass still supplies two
        // more pairable combinations that the zero budget must leave unchecked.
        third.misfit = true;
        case.events.push(third);
        case.p.region = BBox::new(
            case.obs.0 - 0.01,
            case.obs.1 - 0.01,
            case.obs.0 + 0.01,
            case.obs.1 + 0.01,
        );
        case.p.start_lo = case.t_s - 30.0;
        case.p.start_hi = case.t_s + 30.0;
        case.p.max_quick = 0;
        case.p.max_fine = 0;
        // Force all quick checks to fail. Counting survivors instead of joined seeds
        // would now join every pair, changing the budget's meaning and hiding its cut.
        case.p.quick_chi = -1.0;
        let result = run_case(&case, med);
        assert_eq!(result.funnel.events_used, 3);
        assert_eq!(result.funnel.event_pairs, 3);
        assert!(result.funnel.joined > 0, "{:?}", result.funnel);
        assert_eq!(result.funnel.quick_passed, 0);
        assert_eq!(result.funnel.fine_scored, 0);
        assert_eq!(result.funnel.truncated, ["pairs: 3 -> 1"]);
        assert!(result.candidates.is_empty());
        assert!(!result.margin_passes(3.0));
    }

    /// The confirmer cap is a cut like the pair and fine budgets: `run` must record it in the
    /// funnel and warn, because `margin_passes` lets a missing rival pass only when no cut was
    /// recorded.
    #[test]
    fn confirmer_cap_is_reported() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let mut case = two_flyover_case(med);
        case.p.max_confirm = usize::MAX;
        let res = run_case(&case, med);
        assert!(!res.candidates.is_empty(), "{:?}", res.funnel);
        assert!(
            !res.funnel
                .truncated
                .iter()
                .any(|s| s.starts_with("confirmers")),
            "{:?}",
            res.funnel
        );
        case.p.max_confirm = 0;
        let res = run_case(&case, med);
        assert!(
            res.funnel
                .truncated
                .iter()
                .any(|s| s.starts_with("confirmers: ") && s.ends_with(" -> 0")),
            "{:?}",
            res.funnel
        );
        assert!(
            res.warnings
                .iter()
                .any(|w| w.contains("rival comparison is incomplete")),
            "{:?}",
            res.warnings
        );
    }

    #[test]
    fn rising_terrain_can_reenter_the_distance_gate() {
        struct Hillside {
            lat: f64,
            side: f64,
        }
        impl Elevation for Hillside {
            fn elev_m(&self, lat: f64, _lon: f64) -> f64 {
                let across = self.side * (lat - self.lat) * fdop_types::geo::m_per_deg(self.lat).0;
                1000.0 * ((across - 400.0) / 400.0).clamp(0.0, 1.0)
            }
            fn describe(&self) -> String {
                "synthetic hillside".into()
            }
        }

        let (lat, lon) = (40.8765, -73.5250);
        let enu = Enu::new(lat, lon, 0.0);
        let track = straight_track(
            &enu,
            [0.0, 0.0, 2000.0],
            [100.0, 0.0, 0.0],
            1000.0,
            900.0,
            1100.0,
            5.0,
        );
        let med = fdop_types::atmo::Medium::still(343.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        let p = SearchParams::new(
            498.0,
            502.0,
            1000.0,
            BBox::new(lat - 0.03, lon - 0.03, lat + 0.03, lon + 0.03),
        );
        let frame = Frame::new(&p.region);
        let t_base = p.start_lo - 86_400.0;
        for side in [-1.0, 1.0] {
            let dem = Hillside { lat, side };
            let obs_lat = lat + side * 1050.0 / fdop_types::geo::m_per_deg(lat).0;
            let o = observer(obs_lat, lon, &dem);
            let ev = synth_event(
                "hill",
                &track,
                o.x,
                500.0,
                118.0,
                343.0,
                1000.0,
                &Prop::from_medium(&med, &enu),
            );
            // Near the flight, the valley floor is outside the slant-distance gate.
            // Farther across the ray, rising ground brings the observer back inside it.
            assert!(ev.d_gate[1] < 2000.0 - MIC_HEIGHT_M);
            let flat = hypotheses(
                &ev,
                std::slice::from_ref(&track),
                &p,
                &frame,
                &FlatElevation(0.0),
                &atmo,
                t_base,
                100_000,
            );
            assert_eq!(flat.count, 0, "the valley floor remains outside the gate");
            let cloud = hypotheses(
                &ev,
                std::slice::from_ref(&track),
                &p,
                &frame,
                &dem,
                &atmo,
                t_base,
                100_000,
            );
            let closest = cloud
                .stored
                .unwrap()
                .iter()
                .map(|h| {
                    let (la, lo) = frame.latlon(h.e as f64, h.n as f64);
                    haversine_m(la, lo, obs_lat, lon)
                })
                .fold(f64::INFINITY, f64::min);
            assert!(closest < p.cross_step_m / 2.0, "side {side}: {closest} m");
        }
    }

    #[test]
    fn outside_region_grid_points_do_not_suppress_interior_hypotheses() {
        struct Ravine {
            lat: f64,
            side: f64,
            center_elev: f64,
        }
        impl Elevation for Ravine {
            fn elev_m(&self, lat: f64, _lon: f64) -> f64 {
                let b = self.side * (lat - self.lat) * fdop_types::geo::m_per_deg(self.lat).0;
                if b <= 350.0 {
                    self.center_elev - (self.center_elev - 350.0) * (b / 350.0).clamp(0.0, 1.0)
                } else if b <= 400.0 {
                    350.0 + 250.0 * (b - 350.0) / 50.0
                } else {
                    600.0 + (b - 400.0).clamp(0.0, 300.0)
                }
            }
            fn describe(&self) -> String {
                "synthetic ravine".into()
            }
        }

        let (lat, lon) = (40.8765, -73.5250);
        let mlat = fdop_types::geo::m_per_deg(lat).0;
        let enu = Enu::new(lat, lon, 0.0);
        let track = straight_track(
            &enu,
            [0.0, 0.0, 1100.0 + MIC_HEIGHT_M],
            [100.0, 0.0, 0.0],
            1000.0,
            900.0,
            1100.0,
            5.0,
        );
        let med = fdop_types::atmo::Medium::still(343.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        for (side, center_elev) in [(-1.0, 1000.0), (1.0, 1000.0), (-1.0, 500.0), (1.0, 500.0)] {
            let dem = Ravine {
                lat,
                side,
                center_elev,
            };
            let (edge_a, edge_b) = (lat + side * 100.0 / mlat, lat + side * 500.0 / mlat);
            let p = SearchParams::new(
                498.0,
                502.0,
                1000.0,
                BBox::new(
                    edge_a.min(edge_b),
                    lon - 0.02,
                    edge_a.max(edge_b),
                    lon + 0.02,
                ),
            );
            let obs_lat = lat + side * 400.0 / mlat;
            let o = observer(obs_lat, lon, &dem);
            let ev = synth_event(
                "ravine",
                &track,
                o.x,
                500.0,
                118.0,
                343.0,
                1000.0,
                &Prop::from_medium(&med, &enu),
            );
            let distance_at = |b| {
                let x = observer(lat + side * b / mlat, lon, &dem).x;
                norm(sub(track.pos_at(1000.0), x))
            };
            // Continuing beyond the too-distant 350 m sample reaches valid terrain at
            // 700 m, outside the requested region. That rejected point cannot replace the
            // interior sample near the actual observer, 400 m across and 500 m below.
            assert!(distance_at(350.0) > 1.02 * ev.d_gate[1]);
            assert!((ev.d_gate[0]..=ev.d_gate[1]).contains(&distance_at(700.0)));
            assert!(!p.region.contains(lat + side * 700.0 / mlat, lon));
            if center_elev == 500.0 {
                // The shared center also needs all gates, even when its distance fits.
                assert!((ev.d_gate[0]..=ev.d_gate[1]).contains(&distance_at(0.0)));
                assert!(!p.region.contains(lat, lon));
            }
            let frame = Frame::new(&p.region);
            let cloud = hypotheses(
                &ev,
                std::slice::from_ref(&track),
                &p,
                &frame,
                &dem,
                &atmo,
                p.start_lo - 86_400.0,
                100_000,
            );
            assert!(
                cloud.count > 0,
                "side {side}: the observer lost every hypothesis"
            );
            let closest = cloud
                .stored
                .unwrap()
                .iter()
                .map(|h| {
                    let (la, lo) = frame.latlon(h.e as f64, h.n as f64);
                    assert!(p.region.contains(la, lo));
                    haversine_m(la, lo, obs_lat, lon)
                })
                .fold(f64::INFINITY, f64::min);
            assert!(closest < p.cross_step_m / 2.0, "side {side}: {closest} m");
        }
    }

    #[test]
    fn unmatched_events_do_not_consume_pair_driver_slots() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let mut case = two_flyover_case(med);
        case.p.max_events = 2;
        let enu = Enu::new(case.obs.0, case.obs.1, 0.0);
        // A stronger synthetic flyover from an aircraft absent from the archive. None of the
        // archived flights passes its speed gate, so it cannot help seed a pair join.
        let absent = straight_track(
            &enu,
            [0.0, -3500.0, 1000.0],
            [55.0, 0.0, 0.0],
            case.t_s + 400.0,
            case.t_s,
            case.t_s + 800.0,
            5.0,
        );
        let mut unmatched = synth_event(
            "unarchived",
            &absent,
            enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]),
            case.t_s,
            95.0,
            343.0,
            case.t_s + 400.0,
            &Prop::from_medium(&med, &enu),
        );
        unmatched.quality = 200.0;
        case.events.insert(0, unmatched);

        let res = run_case(&case, med);
        assert_eq!(res.funnel.events_used, 3);
        assert_eq!(res.funnel.hypotheses[0], ("unarchived".into(), Some(0)));
        assert!(!res.candidates.is_empty(), "funnel {:?}", res.funnel);
        assert_eq!(res.funnel.event_pairs, 1);
        let top = &res.candidates[0];
        let r = res.refine.as_ref().unwrap();
        let err = haversine_m(r.lat, r.lon, case.obs.0, case.obs.1);
        assert!(err < 60.0, "observer error {err:.1} m");
        assert!((top.t_start_utc - case.t_s).abs() < 1.0);
        for (event, flight) in [("e1", "A"), ("e2", "B")] {
            assert!(
                top.events
                    .iter()
                    .any(|m| m.event == event && m.flight == flight && m.role == "pair"),
                "matches {:?}",
                top.events
            );
        }
    }

    fn tight_region_case(
        half: (f64, f64),
        heading: f64,
        t_s: f64,
        clock_half: f64,
        clock_shift: f64,
    ) -> TwoCase {
        let (lat, lon) = (40.876466, -73.524951);
        let enu = Enu::new(lat, lon, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let angle = heading.to_radians();
        let rotate = |v: V3| {
            [
                v[0] * angle.cos() - v[1] * angle.sin(),
                v[0] * angle.sin() + v[1] * angle.cos(),
                v[2],
            ]
        };
        let mut tracks = Vec::new();
        let mut events = Vec::new();
        for (key, pos, vel, dt, f0) in [
            ("A", [0.0, 175.0, 1000.0], [90.0, 0.0, 0.0], 120.0, 112.7),
            ("B", [175.0, 0.0, 1000.0], [0.0, 95.0, 0.0], 360.0, 149.3),
        ] {
            let mut tr = straight_track(
                &enu,
                rotate(pos),
                rotate(vel),
                t_s + dt,
                t_s - 200.0,
                t_s + 800.0,
                5.0,
            );
            tr.key = key.into();
            events.push(synth_event(
                key,
                &tr,
                x,
                t_s,
                f0,
                343.0,
                t_s + dt,
                &Prop::still(343.0),
            ));
            tracks.push(tr);
        }
        let (mlat, mlon) = fdop_types::geo::m_per_deg(lat);
        let mut p = SearchParams::new(
            t_s + clock_shift - clock_half,
            t_s + clock_shift + clock_half,
            600.0,
            BBox::new(
                lat - half.1 / mlat,
                lon - half.0 / mlon,
                lat + half.1 / mlat,
                lon + half.0 / mlon,
            ),
        );
        p.refine_half_m = 200.0;
        TwoCase {
            tracks,
            events,
            p,
            t_s,
            obs: (lat, lon),
        }
    }

    #[test]
    fn tight_regions_keep_flyovers_between_along_track_samples() {
        let epoch = 1_788_895_000.0;
        let scenarios = [
            ((100.0, 100.0), 0.0, epoch, 0.0, 0.0), // original 200 m control
            ((50.0, 50.0), 0.0, epoch, 0.0, 0.0),
            ((25.0, 25.0), 0.0, epoch, 0.0, 0.0),
            ((50.0, 50.0), 90.0, epoch, 0.0, 0.0),
            ((50.0, 50.0), 45.0, epoch, 0.0, 0.0),
            ((50.0, 50.0), -45.0, epoch, 0.0, 0.0),
            ((50.0, 50.0), 180.0, epoch, 0.0, 0.0),
            ((50.0, 50.0), 0.0, epoch, 10.0, 0.25),
            ((50.0, 50.0), 0.0, epoch, 10.0, 0.75),
            ((50.0, 50.0), 0.0, epoch, 10.0, 1.25),
            ((50.0, 50.0), 0.0, epoch, 10.0, 1.75),
            ((50.0, 50.0), 0.0, 0.0, 0.0, 0.0),
            ((50.0, 50.0), 0.0, 0.0, 10.0, 0.75),
            ((25.0, 1000.0), 0.0, epoch, 0.0, 0.0),
            ((1000.0, 25.0), 0.0, epoch, 0.0, 0.0),
        ];
        for (half, heading, start, clock_half, phase) in scenarios {
            let case = tight_region_case(half, heading, start, clock_half, phase);
            let result = run_case(&case, fdop_types::atmo::Medium::still(343.0));
            let top = result.candidates.first().unwrap_or_else(|| {
                panic!("half={half:?} heading={heading} epoch={start} clock_half={clock_half} phase={phase}: {:?}", result.funnel)
            });
            for key in ["A", "B"] {
                assert!(
                    top.events
                        .iter()
                        .any(|m| m.event == key && m.flight == key && m.core)
                );
            }
            assert!(haversine_m(case.obs.0, case.obs.1, top.lat, top.lon) < 75.0);
            assert!((top.t_start_utc - start).abs() < 0.2);
            assert!(result.funnel.truncated.is_empty());
        }
    }

    #[test]
    fn tight_region_hypotheses_keep_accounting_and_physical_gates() {
        let case = tight_region_case((50.0, 50.0), 0.0, 1_788_895_000.0, 0.0, 0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(fdop_types::atmo::Medium::still(343.0));
        let generate = |ev: &SearchEvent, p: &SearchParams, ground: f64, cap: usize| {
            hypotheses(
                ev,
                &case.tracks,
                p,
                &Frame::new(&p.region),
                &FlatElevation(ground),
                &atmo,
                p.start_lo - 86_400.0,
                cap,
            )
        };
        let ev = &case.events[0];
        let complete = generate(ev, &case.p, 0.0, 10_000);
        assert!(complete.count > 0);
        assert_eq!(complete.stored.as_ref().unwrap().len(), complete.count);
        let cap = complete.count - 1;
        let limited = generate(ev, &case.p, 0.0, cap);
        let repeat = generate(ev, &case.p, 0.0, cap);
        assert_eq!(limited.count, complete.count);
        assert!(limited.stored.is_none());
        let signature = |hs: &[Hyp]| {
            hs.iter()
                .map(|h| {
                    (
                        h.trk,
                        h.t_e.to_bits(),
                        h.e.to_bits(),
                        h.n.to_bits(),
                        h.t_s.to_bits(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(signature(&limited.sample), signature(&repeat.sample));
        assert_eq!(generate(ev, &case.p, 1000.0, 10_000).count, 0);
        let mut too_slow = ev.clone();
        too_slow.v_gate = [30.0, 40.0];
        assert_eq!(generate(&too_slow, &case.p, 0.0, 10_000).count, 0);
        let mut too_close = ev.clone();
        too_close.d_gate = [20.0, 30.0];
        assert_eq!(generate(&too_close, &case.p, 0.0, 10_000).count, 0);
        let mut wrong_clock = case.p.clone();
        wrong_clock.start_lo += 30.0;
        wrong_clock.start_hi += 31.0;
        assert_eq!(generate(ev, &wrong_clock, 0.0, 10_000).count, 0);
    }

    #[test]
    fn region_cross_track_offsets_stay_inside_the_box_and_distance_gate() {
        let region = BBox::new(-0.001, -0.001, 0.001, 0.001);
        for (origin, direction) in [
            ((0.002, 0.0), (-0.00001, 0.0)),
            ((-0.002, 0.0), (0.00001, 0.0)),
            ((0.0, 0.002), (0.0, -0.00001)),
            ((0.0, -0.002), (0.0, 0.00001)),
            ((0.002, 0.002), (-0.00001, -0.00001)),
        ] {
            let b = region_cross_track_offset(800.0, 1500.0, &region, origin, direction, |b| {
                b.hypot(1000.0)
            })
            .unwrap();
            assert!((b - 200.0).abs() < 1e-10);
            assert!(region.contains(origin.0 + b * direction.0, origin.1 + b * direction.1));
            assert!((800.0..=1500.0).contains(&b.hypot(1000.0)));
        }
        // Only a small part of the clipped strip meets the distance gate. Exercise
        // both increasing slant range and falling range over rising terrain.
        for descending in [false, true] {
            let distance = |b: f64| {
                if descending {
                    1500.0 - b
                } else {
                    b.hypot(1000.0)
                }
            };
            let (d_lo, d_hi) = if descending {
                (1220.0, 1240.0)
            } else {
                (270.0f64.hypot(1000.0), 290.0f64.hypot(1000.0))
            };
            let b = region_cross_track_offset(
                d_lo,
                d_hi,
                &region,
                (0.002, 0.0),
                (-0.00001, 0.0),
                distance,
            )
            .unwrap();
            assert!(b > 200.0 && b < 300.0);
            assert!(region.contains(0.002 - b * 0.00001, 0.0));
            assert!((d_lo..=d_hi).contains(&distance(b)));
        }
    }

    #[test]
    fn skipped_region_times_follow_the_actual_plane_between_samples() {
        for epoch in [0.0, 1_788_895_000.0] {
            for direction in [-1.0, 1.0] {
                // Changing heading or height makes this projection nonlinear in time.
                // Its independently known centre crossing is 0.8 seconds after the start.
                let project = |t: f64| {
                    let dt = t - epoch;
                    Some((direction * 100.0 * (dt - 0.8) * (1.0 + 0.2 * dt), 5.0))
                };
                // Include a shortened final interval as well as an ordinary two-second step.
                for end in [1.0, 2.0] {
                    let clock = skipped_region_time(
                        (epoch, project(epoch).unwrap()),
                        (epoch + end, project(epoch + end).unwrap()),
                        project,
                    )
                    .unwrap();
                    assert!((clock - epoch - 0.8).abs() < 1e-5);
                }
            }
        }
        for right in [(15.0, 5.0), (0.0, 5.0), (-5.0, 5.0)] {
            assert!(
                skipped_region_time((0.0, (10.0, 5.0)), (2.0, right), |_| {
                    panic!("a region not skipped by both lines needs no extra sampling")
                })
                .is_none()
            );
        }
        assert!(skipped_region_time((0.0, (10.0, 5.0)), (2.0, (-10.0, 5.0)), |_| None).is_none());
        assert!(
            skipped_region_time((0.0, (10.0, 5.0)), (2.0, (-10.0, 5.0)), |t| {
                // A discontinuous projection never actually enters the requested strip.
                Some((if t < 1.0 { 10.0 } else { -10.0 }, 5.0))
            })
            .is_none()
        );
    }

    #[test]
    fn region_cross_track_offsets_keep_inclusive_endpoints() {
        let unit = 1.0 / 1024.0;
        let step = unit / 128.0;
        let region = BBox::new(0.0, 0.0, 2.0 * unit, 2.0 * unit);
        let distance = |b: f64| b.hypot(1024.0);
        for (origin, direction) in [
            ((-unit, unit), (step, 0.0)),
            ((3.0 * unit, unit), (-step, 0.0)),
            ((unit, -unit), (0.0, step)),
            ((unit, 3.0 * unit), (0.0, -step)),
        ] {
            // Only the endpoint is eligible; every interior probe misses the gate.
            // Dyadic coordinates make the clipping boundaries exactly representable.
            for (expected, d_lo, d_hi) in [
                (128.0, 1024.0, distance(128.0)),
                (384.0, distance(384.0), 2048.0),
            ] {
                let b = region_cross_track_offset(d_lo, d_hi, &region, origin, direction, distance)
                    .expect("the clipped endpoint satisfies both inclusive gates");
                assert_eq!(b, expected);
                assert!(region.contains(origin.0 + b * direction.0, origin.1 + b * direction.1));
                assert!((d_lo..=d_hi).contains(&distance(b)));
            }
        }
    }

    #[test]
    fn region_cross_track_offsets_do_not_invent_eligible_points() {
        let region = BBox::new(-0.001, -0.001, 0.001, 0.001);
        for direction in [(0.0, 0.00001), (0.00001, 0.0)] {
            assert!(
                region_cross_track_offset(800.0, 1500.0, &region, (0.002, 0.0), direction, |b| {
                    b.hypot(1000.0)
                })
                .is_none(),
                "a parallel or outward ray cannot enter the region"
            );
        }
        let probe = |d_lo, d_hi, distance: fn(f64) -> f64| {
            region_cross_track_offset(d_lo, d_hi, &region, (0.002, 0.0), (-0.00001, 0.0), distance)
        };
        assert!(probe(10.0, 50.0, |b| b).is_none(), "region is beyond reach");
        assert!(probe(800.0, 1500.0, |b| b.hypot(2000.0)).is_none());
        assert!(probe(1200.0, 1500.0, |b| if b < 260.0 { 1000.0 } else { 2000.0 }).is_none());
        assert!(probe(800.0, 1500.0, |_| f64::NAN).is_none());
    }

    #[test]
    fn cross_track_gap_samples_obey_terrain_distance_gates() {
        for slope in [-0.2, 0.0, 0.2] {
            // The ground rises or falls across the gap; use its height at every probe.
            let distance = |b: f64| b.hypot(100.0 + slope * b);
            let b = interior_cross_track_offset(215.0, 336.0, distance).unwrap();
            assert!(b > 0.0 && b < 350.0);
            assert!((215.0..=336.0).contains(&distance(b)));
        }
        assert!(
            interior_cross_track_offset(215.0, 336.0, |b| b.hypot(400.0)).is_none(),
            "aircraft above the distance gate cannot produce a hypothesis"
        );
        assert!(
            interior_cross_track_offset(215.0, 336.0, |b| if b < 150.0 { 100.0 } else { 500.0 })
                .is_none(),
            "a terrain discontinuity must not manufacture an in-gate point"
        );
        assert!(interior_cross_track_offset(215.0, 336.0, |_| f64::NAN).is_none());
    }

    #[test]
    fn close_flyovers_between_cross_track_grid_lines_locate_the_observer() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let (lat0, lon0) = (40.8765, -73.5250);
        let enu = Enu::new(lat0, lon0, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let t_s = 1_788_895_000.0;
        let prop = Prop::from_medium(&med, &enu);
        // Both passes lie within the detector's >=250 m distance envelope. Their generous
        // distance gates fit between the b=0 and b=350 m cross-track samples: neither grid
        // line intersects the allowed annulus, on either side of the aircraft.
        let mut a = straight_track(
            &enu,
            [0.0, 250.0, 100.0],
            [55.0, 0.0, 0.0],
            t_s + 400.0,
            t_s + 200.0,
            t_s + 600.0,
            2.0,
        );
        a.key = "A".into();
        let mut b = straight_track(
            &enu,
            [250.0, 0.0, 100.0],
            [0.0, 70.0, 0.0],
            t_s + 900.0,
            t_s + 700.0,
            t_s + 1100.0,
            2.0,
        );
        b.key = "B".into();
        let e1 = synth_event("e1", &a, x, t_s, 112.7, 343.0, t_s + 400.0, &prop);
        let e2 = synth_event("e2", &b, x, t_s, 129.5, 343.0, t_s + 900.0, &prop);
        let region = BBox::new(lat0 - 0.01, lon0 - 0.015, lat0 + 0.01, lon0 + 0.015);
        let mut p = SearchParams::new(t_s - 3600.0, t_s + 3600.0, 1300.0, region);
        p.refine_half_m = 200.0;
        let case = TwoCase {
            tracks: vec![a, b],
            events: vec![e1, e2],
            p,
            t_s,
            obs: (lat0, lon0),
        };

        let res = run_case(&case, med);
        assert!(
            res.funnel
                .hypotheses
                .iter()
                .all(|(_, n)| n.is_some_and(|n| n > 0)),
            "real close flyovers must have hypotheses: {:?}",
            res.funnel
        );
        assert!(!res.candidates.is_empty(), "funnel {:?}", res.funnel);
        let top = &res.candidates[0];
        let r = res.refine.as_ref().unwrap();
        let err = haversine_m(r.lat, r.lon, lat0, lon0);
        assert_eq!((top.lat, top.lon), (r.lat, r.lon));
        assert_eq!(top.t_start_utc, r.t_start_utc);
        assert_eq!(res.margin["top_score"].as_f64(), Some(top.score));
        assert!(err < 60.0, "observer error {err:.1} m");
        assert!((top.t_start_utc - t_s).abs() < 1.0);
        for (event, flight) in [("e1", "A"), ("e2", "B")] {
            assert!(
                top.events
                    .iter()
                    .any(|m| m.event == event && m.flight == flight && m.role == "pair"),
                "matches {:?}",
                top.events
            );
        }
        // An interior point still needs the original terrain clearance. Raising the ground
        // under these same tracks must not let the fallback invent a low-AGL hypothesis.
        let raised_ground = FlatElevation(90.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        let cloud = hypotheses(
            &case.events[0],
            &case.tracks,
            &case.p,
            &Frame::new(&case.p.region),
            &raised_ground,
            &atmo,
            case.p.start_lo - 86_400.0,
            10_000,
        );
        assert_eq!(cloud.count, 0, "minimum AGL still applies to gap samples");
    }

    fn harmonic_trace_pair() -> (SearchEvent, SearchEvent) {
        let c = 343.0;
        let mut fundamental = plain_event(100.0, false);
        fundamental.id = "fundamental".into();
        fundamental.t_rec = 100.0;
        fundamental.tr_t = (0..240).map(|i| 40.0 + 0.5 * i as f64).collect();
        fundamental.tr_f = fundamental
            .tr_t
            .iter()
            .map(|&t| {
                cv_curve(t, 100.0, 100.0, 3000.0, 100.0 - 3000.0 / c, c) + 0.06 * (t * 1.7).sin()
            })
            .collect();
        // Same physical pass, but a different analysis window/cadence, independent noise,
        // and fitted f0/CPA offsets. The observed frequency ratio is still five.
        let mut fifth = plain_event(505.0, false);
        fifth.id = "fifth".into();
        fifth.t_rec = 101.0;
        fifth.tr_t = (0..120).map(|i| 60.15 + 0.65 * i as f64).collect();
        fifth.tr_f = fifth
            .tr_t
            .iter()
            .map(|&t| {
                cv_curve(t, 500.0, 100.0, 3000.0, 100.0 - 3000.0 / c, c) + 0.15 * (t * 2.3).sin()
            })
            .collect();
        (fundamental, fifth)
    }

    #[test]
    fn harmonic_noise_sampling_and_fit_offsets_do_not_split_one_pass() {
        let (fundamental, mut fifth) = harmonic_trace_pair();
        for (a, b) in [(&fundamental, &fifth), (&fifth, &fundamental)] {
            assert!(harmonic_events(a, b, 5.0));
        }
        // A real shift in the observed curve must not be fitted away by aligning the CPAs.
        let mut shifted = fifth.clone();
        shifted.t_rec += 6.0;
        for t in &mut shifted.tr_t {
            *t += 6.0;
        }
        assert!(!harmonic_events(&fundamental, &shifted, 5.0));
        // Missing two-sided support cannot establish an independent aircraft.
        let n = fifth.tr_t.partition_point(|&t| t < fundamental.t_rec);
        fifth.tr_t.truncate(n);
        fifth.tr_f.truncate(n);
        assert!(harmonic_events(&fundamental, &fifth, 5.0));
    }

    #[test]
    fn known_family_observations_override_fitted_harmonic_screens() {
        let (fundamental, fifth) = harmonic_trace_pair();
        // Deliberately perturb fitted metadata while preserving measured harmonic ridges.
        // This tests selection invariance, not an assertion that the fitter makes these errors.
        for (t_rec, f0) in [(112.0, 505.0), (101.0, 540.0), (112.0, 540.0)] {
            let mut offset = fifth.clone();
            offset.t_rec = t_rec;
            offset.f0 = f0;
            for (a, b) in [(&fundamental, &offset), (&offset, &fundamental)] {
                assert!(!harmonic_events(a, b, 5.0));
                assert!(harmonic_trace_shape(a, b, 5.0));
            }
            assert_eq!(
                independent_events(vec![fundamental.clone(), offset.clone()], 5.0).len(),
                2,
                "unrelated events retain the fitted-parameter screen",
            );
            assert_eq!(
                independent_events_by(vec![fundamental.clone(), offset.clone()], |a, b| {
                    harmonic_trace_shape(a, b, 5.0)
                })
                .len(),
                1,
                "known-family ridges cannot supply duplicate aircraft evidence",
            );
            // An overlapping approach alone cannot distinguish two aircraft, even when
            // fitted metadata fails the generic screen. A family hint is not new evidence.
            let n = offset.tr_t.partition_point(|&t| t < fundamental.t_rec);
            offset.tr_t.truncate(n);
            offset.tr_f.truncate(n);
            assert_eq!(
                independent_events_by(vec![fundamental.clone(), offset], |a, b| {
                    harmonic_trace_shape(a, b, 5.0)
                })
                .len(),
                1,
            );
        }
        // The family hint must still permit genuinely different observed motion. Shifting
        // the entire received ridge changes its shape at shared times, beyond fit metadata.
        let mut shifted = fifth;
        shifted.t_rec = 112.0;
        for t in &mut shifted.tr_t {
            *t += 6.0;
        }
        for (a, b) in [(&fundamental, &shifted), (&shifted, &fundamental)] {
            assert!(!harmonic_trace_shape(a, b, 5.0));
        }
        assert_eq!(
            independent_events_by(vec![fundamental, shifted], |a, b| {
                harmonic_trace_shape(a, b, 5.0)
            })
            .len(),
            2,
            "observed disagreement still recovers a distinct sibling",
        );
    }

    #[test]
    fn family_selection_retains_links_from_duplicate_primary_seeds() {
        let (fundamental, mut fifth) = harmonic_trace_pair();
        fifth.t_rec = 112.0;
        fifth.quality = 120.0;
        let mut alternate = fundamental.clone();
        alternate.id = "alternate".into();
        alternate.quality = 110.0;
        let families = HashMap::from([
            ("fundamental", vec![(0, None)]),
            ("fifth", vec![(1, Some(0))]),
            ("alternate", vec![(2, None)]),
        ]);
        let events = vec![fundamental.clone(), fifth.clone(), alternate.clone()];
        let generic = independent_events(events.clone(), 5.0);
        assert_eq!(generic.len(), 2);
        assert_eq!(generic[1].id, "alternate", "keep the better primary fit");
        let selected = independent_events_with_families(events, families.clone(), 5.0);
        assert_eq!(selected.len(), 1, "a discarded parent must retain its link");
        assert_eq!(selected[0].id, "fifth", "keep the best observed fit");

        // An equal numeric detection ID without an explicit sibling relation adds no hint.
        let mut unrelated = families.clone();
        unrelated.insert("fifth", vec![(0, None)]);
        assert_eq!(
            independent_events_with_families(
                vec![fundamental.clone(), fifth.clone(), alternate.clone()],
                unrelated,
                5.0,
            )
            .len(),
            2,
        );

        // Inheriting the parent's hint must not merge different observed Doppler motion.
        for t in &mut fifth.tr_t {
            *t += 6.0;
        }
        let distinct =
            independent_events_with_families(vec![fundamental, fifth, alternate], families, 5.0);
        assert_eq!(distinct.len(), 2);
        assert_eq!(distinct[1].id, "alternate");
    }

    #[test]
    fn family_selection_rechecks_higher_quality_siblings_after_a_parent_merge() {
        let (fundamental, mut fifth) = harmonic_trace_pair();
        fifth.t_rec = 112.0;
        fifth.quality = 130.0;
        let mut seventh = fifth.clone();
        seventh.id = "seventh".into();
        seventh.f0 *= 1.4;
        seventh.sigma_hz *= 1.4;
        for f in &mut seventh.tr_f {
            *f *= 1.4;
        }
        seventh.t_rec = 124.0;
        seventh.quality = 120.0;
        let mut alternate = fundamental.clone();
        alternate.id = "alternate".into();
        alternate.quality = 110.0;
        let families = HashMap::from([
            ("fundamental", vec![(0, None)]),
            ("fifth", vec![(1, Some(0))]),
            ("alternate", vec![(2, None)]),
            ("seventh", vec![(3, Some(2))]),
        ]);
        let events = vec![
            fundamental.clone(),
            fifth.clone(),
            alternate.clone(),
            seventh.clone(),
        ];
        assert_eq!(independent_events(events.clone(), 5.0).len(), 3);
        let selected = independent_events_with_families(events, families.clone(), 5.0);
        assert_eq!(selected.len(), 1, "both siblings share observed motion");
        assert_eq!(selected[0].id, "fifth", "keep the best observed fit");

        // A propagated parent hint is still insufficient to merge disagreeing ridges.
        for t in &mut seventh.tr_t {
            *t += 6.0;
        }
        let distinct = independent_events_with_families(
            vec![fundamental, fifth, alternate, seventh],
            families,
            5.0,
        );
        assert_eq!(distinct.len(), 2);
        assert_eq!(distinct[1].id, "seventh");
    }

    #[test]
    fn simultaneous_independent_flyovers_locate_the_observer() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let mut case = two_flyover_case(med);
        let enu = Enu::new(case.obs.0, case.obs.1, 0.0);
        let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
        let prop = Prop::from_medium(&med, &enu);
        // Move B's pass so its *received* CPA coincides with A's, with a 2:1 tone ratio.
        // Different Doppler shapes still supply two independent flight constraints.
        let t_b = case.t_s + 1900.0 + case.events[0].t_rec - case.events[1].t_rec;
        let mut b = straight_track(
            &enu,
            [2500.0, 0.0, 900.0],
            [0.0, 104.0, 0.0],
            t_b,
            t_b - 600.0,
            t_b + 600.0,
            5.0,
        );
        b.key = "B".into();
        case.events[1] = synth_event(
            "e2",
            &b,
            x,
            case.t_s,
            2.0 * case.events[0].f0,
            343.0,
            t_b,
            &prop,
        );
        case.tracks[1] = b;
        assert!((case.events[0].t_rec - case.events[1].t_rec).abs() < 0.01);

        let res = run_case(&case, med);
        assert_eq!(res.funnel.events_used, 2, "{:?}", res.warnings);
        assert!(!res.candidates.is_empty(), "funnel {:?}", res.funnel);
        let top = &res.candidates[0];
        let r = res.refine.as_ref().unwrap();
        let err = haversine_m(r.lat, r.lon, case.obs.0, case.obs.1);
        assert!(err < 60.0, "observer error {err:.1} m");
        assert!((top.t_start_utc - case.t_s).abs() < 1.0);
        for (event, flight) in [("e1", "A"), ("e2", "B")] {
            assert!(
                top.events
                    .iter()
                    .any(|m| m.event == event && m.flight == flight && m.role == "pair"),
                "matches {:?}",
                top.events
            );
        }
    }

    #[test]
    fn harmonic_higher_orders_never_count_as_independent_flyovers() {
        let med = fdop_types::atmo::Medium::still(343.0);
        for order in [5.0, 7.0, 8.0] {
            for harmonic_stronger in [false, true] {
                let mut case = two_flyover_case(med);
                let mut harmonic = case.events[0].clone();
                harmonic.id = format!("harmonic_{order}");
                harmonic.f0 *= order;
                harmonic.f0_tol *= order;
                harmonic.sigma_hz *= order;
                for f in harmonic.tr_f.iter_mut().chain(harmonic.q_f.iter_mut()) {
                    *f *= order;
                }
                harmonic.quality = if harmonic_stronger { 200.0 } else { 50.0 };
                case.events = vec![case.events[0].clone(), harmonic];

                let res = run_case(&case, med);
                assert_eq!(
                    res.funnel.events_used, 1,
                    "order {order}, harmonic stronger: {harmonic_stronger}"
                );
                assert!(res.candidates.is_empty());
            }
        }
    }

    #[test]
    fn subgrid_polish_respects_the_window_and_unscorable_points() {
        let loss = |e: f64, n: f64| (e - 80.0).powi(2) + (n + 3.0).powi(2);
        for limit in [10.0, 20.0] {
            let polished = polish_grid_minimum((0.0, 0.0, loss(0.0, 0.0)), 20.0, 10.0, |e, n| {
                (e <= limit).then(|| loss(e, n))
            });
            assert_eq!(polished.0, limit);
            assert!((polished.1 + 3.0).abs() < 0.25);
            assert_eq!(polished.2, loss(polished.0, polished.1));
        }
    }

    #[test]
    fn parallel_flights_keep_the_position_clock_ambiguity_visible() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let dem = FlatElevation(0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        let mut missed_boundary_on_grid = false;
        // Along-track position can trade exactly against a shared recording clock when
        // the two aircraft have the same velocity. Finer numerical optimization must not
        // turn this unidentifiable geometry into a closed spatial confidence contour.
        for angle in [0.0_f64, 7.0, 17.0] {
            let mut case = two_flyover_case(med);
            let enu = Enu::new(case.obs.0, case.obs.1, 0.0);
            let (sin, cos) = angle.to_radians().sin_cos();
            let rotate = |[e, n, z]: V3| [e * cos - n * sin, e * sin + n * cos, z];
            for (i, (p0, dt, f0)) in [
                ([0.0, 5000.0, 1500.0], 1000.0, 112.7),
                ([0.0, -3500.0, 1200.0], 1900.0, 129.5),
            ]
            .into_iter()
            .enumerate()
            {
                let tr = straight_track(
                    &enu,
                    rotate(p0),
                    rotate([137.0, 0.0, 0.0]),
                    case.t_s + dt,
                    case.t_s + dt - 600.0,
                    case.t_s + dt + 600.0,
                    5.0,
                );
                let x = enu.to_ecef([0.0, 0.0, MIC_HEIGHT_M]);
                let mut ev = synth_event(
                    &format!("parallel_{i}"),
                    &tr,
                    x,
                    case.t_s,
                    f0,
                    343.0,
                    case.t_s + dt,
                    &Prop::still(343.0),
                );
                // Deliberately thin, high-information contours stress grid aliasing;
                // these noiseless traces do not assert a realistic measurement floor.
                ev.sigma_hz = 0.03;
                ev.n_eff = ev.tr_t.len() as f64;
                for (&t, f) in ev.tr_t.iter().zip(&mut ev.tr_f) {
                    *f = f0
                        * tr.solve_emission_p(t + case.t_s, x, &Prop::still(343.0), None)
                            .unwrap()
                            .2;
                }
                (ev.q_t, ev.q_f) = subsample(&ev.tr_t, &ev.tr_f, 40);
                case.events[i] = ev;
                case.tracks[i] = tr;
            }
            let assign = [(0, 0, case.t_s + 1000.0), (1, 1, case.t_s + 1900.0)];
            // Shift along the unresolved direction: an interior node lies in the thin
            // contour, while its rotated boundary intersections fall between nodes.
            let (lat, lon, _) = enu.to_geodetic([13.0 * cos, 13.0 * sin, 0.0]);
            let mut weights = None;
            for half in [200.0, 400.0] {
                case.p.refine_half_m = half;
                case.p.refine_step_m = half / 4.0;
                let r = refine_on(
                    lat,
                    lon,
                    &assign,
                    &case.events,
                    &case.tracks,
                    &case.p,
                    &dem,
                    &atmo,
                    &mut weights,
                )
                .unwrap();
                let last = r.dchi2.len() - 1;
                let sampled_edge = r.dchi2.iter().enumerate().any(|(iy, row)| {
                    row.iter().enumerate().any(|(ix, &dc)| {
                        (iy == 0 || ix == 0 || iy == last || ix == last) && dc <= 5.99
                    })
                });
                missed_boundary_on_grid |= !sampled_edge && r.region_cells > 0;
                assert!(
                    r.radius_lower_bound,
                    "{angle} degree parallel tracks falsely bounded: {r:?}"
                );
                assert!(
                    r.radius_m >= half,
                    "extent must include the unresolved direction"
                );
            }
        }
        assert!(
            missed_boundary_on_grid,
            "fixture must expose a thin contour crossing between edge nodes"
        );
    }

    fn analytic_two_flyover_case() -> (TwoCase, [(usize, u32, f64); 2]) {
        let med = fdop_types::atmo::Medium::still(343.0);
        let mut case = two_flyover_case(med);
        case.t_s += 0.137;
        let assign = [
            (0, 0, case.t_s - 0.137 + 1000.0),
            (1, 1, case.t_s - 0.137 + 1900.0),
        ];
        // Independent forward model: analytic constant-velocity CPA and closed-form
        // retarded Doppler, not the interpolated track/Newton solver being calibrated.
        for (k, (p0, velocity, f0)) in [
            ([0.0, 5000.0, 1500.0], [137.0, 0.0, 0.0], 112.7),
            ([2500.0, 0.0, 900.0], [0.0, 104.0, 0.0], 129.5),
        ]
        .into_iter()
        .enumerate()
        {
            let relative = sub(p0, [0.0, 0.0, MIC_HEIGHT_M]);
            let dt = -dot(relative, velocity) / dot(velocity, velocity);
            let at_cpa = fdop_types::geo::add(relative, fdop_types::geo::scale(velocity, dt));
            let (d, v, te) = (norm(at_cpa), norm(velocity), assign[k].2 + dt);
            let ev = &mut case.events[k];
            ev.t_rec = te + d / 343.0 - case.t_s;
            ev.tr_t = (0..=120).map(|i| ev.t_rec - 60.0 + i as f64).collect();
            ev.tr_f = ev
                .tr_t
                .iter()
                .map(|&t| cv_curve(t, f0, v, d, te - case.t_s, 343.0))
                .collect();
            (ev.q_t, ev.q_f) = subsample(&ev.tr_t, &ev.tr_f, 40);
        }
        (case, assign)
    }

    #[test]
    fn close_pass_refinement_is_stable_across_prediction_grid_phases() {
        let dem = FlatElevation(0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(fdop_types::atmo::Medium::still(343.0));
        let mut maximum_error = 0.0_f64;
        for phase in [0.07, 0.29, 0.61] {
            let (mut case, assign) = analytic_two_flyover_case();
            let enu = Enu::new(case.obs.0, case.obs.1, 0.0);
            for (k, (position, velocity, f0)) in [
                ([0.0, 160.0, 120.0 + MIC_HEIGHT_M], [175.0, 0.0, 0.0], 500.0),
                ([180.0, 0.0, 140.0 + MIC_HEIGHT_M], [0.0, 130.0, 0.0], 450.0),
            ]
            .into_iter()
            .enumerate()
            {
                let relative = sub(position, [0.0, 0.0, MIC_HEIGHT_M]);
                let (d, v, te) = (norm(relative), norm(velocity), assign[k].2);
                case.tracks[k] =
                    straight_track(&enu, position, velocity, te, te - 120.0, te + 120.0, 5.0);
                let event = &mut case.events[k];
                event.t_rec = te + d / 343.0 - case.t_s;
                event.v_gate = [0.9 * v, 1.1 * v];
                event.d_gate = [0.8 * d, 1.25 * d];
                event.f0 = f0;
                event.f0_tol = 0.04 * f0;
                event.sigma_hz = 0.002 * f0;
                event.n_eff = 7.0;
                event.tr_t = (0..=180)
                    .map(|i| event.t_rec - 18.0 + (i as f64 + phase) * 0.2)
                    .collect();
                // Known reception-time curve, independent of the interpolated ADS-B path.
                event.tr_f = event
                    .tr_t
                    .iter()
                    .map(|&t| cv_curve(t, f0, v, d, te - case.t_s, 343.0))
                    .collect();
                (event.q_t, event.q_f) = subsample(&event.tr_t, &event.tr_f, 40);
            }
            case.p.refine_half_m = 75.0;
            case.p.refine_step_m = 25.0;
            let (lat, lon, _) = enu.to_geodetic([13.4, -9.2, 0.0]);
            let refined = refine_on(
                lat,
                lon,
                &assign,
                &case.events,
                &case.tracks,
                &case.p,
                &dem,
                &atmo,
                &mut None,
            )
            .unwrap();
            let error = haversine_m(refined.lat, refined.lon, case.obs.0, case.obs.1);
            maximum_error = maximum_error.max(error);
            assert!(
                error < 0.5,
                "phase {phase}: close-pass observer error {error:.6} m"
            );
            assert!((refined.t_start_utc - case.t_s).abs() < 0.01);
            assert!(refined.radius_m >= 75.0);
        }
        eprintln!(
            "close-pass interpolation calibration: maximum observer error {maximum_error:.6} m"
        );
    }

    #[test]
    fn subgrid_contour_aliasing_stays_unresolved_until_resampled() {
        let (mut case, assign) = analytic_two_flyover_case();
        for event in &mut case.events {
            event.n_eff = 120.0;
        }
        case.p.refine_half_m = 200.0;
        case.p.refine_step_m = 100.0;
        let dem = FlatElevation(0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(fdop_types::atmo::Medium::still(343.0));
        let frame = Enu::new(case.obs.0, case.obs.1, 0.0);
        let (lat, lon, _) = frame.to_geodetic([37.0, -42.0, 0.0]);
        let coarse = refine_on(
            lat,
            lon,
            &assign,
            &case.events,
            &case.tracks,
            &case.p,
            &dem,
            &atmo,
            &mut Some(vec![1.0, 1.0]),
        )
        .unwrap();
        assert_eq!(
            coarse.region_cells, 0,
            "fixture must leave the contour between grid nodes"
        );
        assert!(
            !coarse.radius_lower_bound,
            "fixture isolates undersampling from a clipped boundary"
        );
        assert!(haversine_m(coarse.lat, coarse.lon, case.obs.0, case.obs.1) < 3.0);

        let inputs = SearchInputs {
            events: case.events.clone(),
            tracks: &case.tracks,
            heard_times: vec![],
            coarse_dem: &dem,
            fine_dem: &dem,
            atmo: &atmo,
        };
        let top = explain_at(
            &inputs,
            &case.p,
            lat,
            lon,
            case.t_s,
            10.0,
            &[("e1".into(), "A".into()), ("e2".into(), "B".into())],
        )
        .unwrap();
        let resolved =
            refine_winner(&top, &case.events, &case.tracks, &case.p, &dem, &atmo).unwrap();
        assert!(resolved.region_cells > 0);
        assert!(!resolved.radius_lower_bound, "{resolved:?}");
        assert!(
            resolved.step_m < coarse.step_m,
            "an unresolved contour needs finer sampling"
        );
        assert!(resolved.radius_m >= 75.0);
        assert!(haversine_m(resolved.lat, resolved.lon, case.obs.0, case.obs.1) < 3.0);
    }

    #[test]
    fn subgrid_polish_recovers_analytic_observers_across_grid_phases() {
        let (case, assign) = analytic_two_flyover_case();
        let dem = FlatElevation(0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(fdop_types::atmo::Medium::still(343.0));
        let enu = Enu::new(case.obs.0, case.obs.1, 0.0);
        let mut max_polished_error = 0.0f64;
        let mut max_grid_error = 0.0f64;
        for grid_step in [25.0, 50.0, 100.0] {
            for (phase_e, phase_n) in [(0.37, -0.42), (-0.43, 0.28), (0.20, 0.44)] {
                let (lat, lon, _) =
                    enu.to_geodetic([phase_e * grid_step, phase_n * grid_step, 0.0]);
                let frame = Enu::new(lat, lon, 0.0);
                let half = 2.0 * grid_step;
                let score = |e, n| {
                    let (la, lo, _) = frame.to_geodetic([e, n, 0.0]);
                    eval_point_weighted(
                        &observer(la, lo, &dem),
                        &assign,
                        &case.events,
                        &case.tracks,
                        &case.p,
                        &atmo,
                        true,
                        false,
                        false,
                        Some(&[1.0, 1.0]),
                    )
                    .map(|pe| pe.total)
                };
                let seed = local_grid(lat, lon, half, grid_step)
                    .iter()
                    .map(|&(_, _, e, n)| (e, n, score(e, n).unwrap()))
                    .min_by(|a, b| a.2.total_cmp(&b.2))
                    .unwrap();
                let polished = polish_grid_minimum(seed, half, grid_step, score);
                let (gla, glo, _) = frame.to_geodetic([seed.0, seed.1, 0.0]);
                let (pla, plo, _) = frame.to_geodetic([polished.0, polished.1, 0.0]);
                let grid_error = haversine_m(gla, glo, case.obs.0, case.obs.1);
                let polished_error = haversine_m(pla, plo, case.obs.0, case.obs.1);
                assert!(polished.2 <= seed.2);
                assert!(
                    polished_error < 3.0,
                    "{grid_step} m grid, phase {phase_e}/{phase_n}: {polished_error} m"
                );
                assert!(polished_error < 0.2 * grid_error);
                max_polished_error = max_polished_error.max(polished_error);
                max_grid_error = max_grid_error.max(grid_error);
                eprintln!(
                    "grid {grid_step} m phase {phase_e}/{phase_n}: cell error {grid_error:.3} m, polished {polished_error:.3} m"
                );
            }
        }
        eprintln!(
            "analytic grid calibration: max cell error {max_grid_error:.3} m, max polished error {max_polished_error:.3} m"
        );
    }

    #[test]
    fn calibrated_point_profiles_the_shared_clock_with_its_event_weights() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let mut case = two_flyover_case(med);
        // The second event prefers a recording start 2 s late. Give it less weight,
        // as refinement does when its residuals exceed the assumed trace noise.
        case.events[1].t_rec -= 2.0;
        for t in case.events[1].tr_t.iter_mut() {
            *t -= 2.0;
        }
        for t in case.events[1].q_t.iter_mut() {
            *t -= 2.0;
        }
        let dem = FlatElevation(0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        let o = observer(case.obs.0, case.obs.1, &dem);
        let assign = [(0, 0, case.t_s + 1000.0), (1, 1, case.t_s + 1900.0)];
        let evaluate = |assign: &[(usize, u32, f64)], weights| {
            eval_point_weighted(
                &o,
                assign,
                &case.events,
                &case.tracks,
                &case.p,
                &atmo,
                true,
                false,
                false,
                weights,
            )
            .unwrap()
        };
        let baseline = eval_point(
            &o,
            &assign,
            &case.events,
            &case.tracks,
            &case.p,
            &atmo,
            true,
            false,
            false,
        )
        .unwrap();
        let ones = evaluate(&assign, Some(&[1.0, 1.0][..]));
        assert_eq!(ones.t_s, baseline.t_s);
        assert_eq!(ones.total, baseline.total);
        for (a, b) in ones.chis.iter().zip(&baseline.chis) {
            assert_eq!(
                (a.chi, a.f0, a.rms, a.cov, a.brk),
                (b.chi, b.f0, b.rms, b.cov, b.brk)
            );
        }

        let weights = [1.0, 0.02];
        let weighted = evaluate(&assign, Some(&weights[..]));
        assert!((baseline.t_s - case.t_s).abs() > 0.5);
        assert!((weighted.t_s - case.t_s).abs() < 0.15);
        let cost_at_old_clock: f64 = baseline
            .chis
            .iter()
            .zip(&assign)
            .zip(weights)
            .map(|((fit, a), w)| w * case.events[a.0].n_eff * fit.chi)
            .sum();
        assert!(weighted.total < cost_at_old_clock);

        // Independent clock oracle: solve emission time directly for every trace point,
        // without the interpolated RatioGrid or the optimizer used by eval_point.
        let exact_cost = |offset: f64| -> f64 {
            let mut scratch = TraceScratch::default();
            assign
                .iter()
                .enumerate()
                .map(|(k, &(ei, ti, te))| {
                    let tr = &case.tracks[ti as usize];
                    let ev = &case.events[ei];
                    let prop = prop_for(&atmo, &o, tr, te);
                    let fit = trace_chi(
                        &ev.tr_t,
                        &ev.tr_f,
                        &|tu| tr.solve_emission_p(tu, o.x, &prop, None).unwrap().2,
                        case.t_s + offset,
                        ev,
                        prop.source_tone_scale(tr.pv(te).1),
                        ev.sigma_hz,
                        case.p.cap_sigma,
                        true,
                        &mut scratch,
                    );
                    weights[k] * ev.n_eff * fit.chi
                })
                .sum()
        };
        let oracle = (0..=200)
            .map(|i| -1.0 + 0.02 * i as f64)
            .min_by(|a, b| exact_cost(*a).total_cmp(&exact_cost(*b)))
            .unwrap();
        assert!((weighted.t_s - case.t_s - oracle).abs() < 0.025);
        let reversed = evaluate(&[assign[1], assign[0]], Some(&[weights[1], weights[0]][..]));
        assert_eq!(weighted.t_s, reversed.t_s);
        assert_eq!(weighted.total, reversed.total);
    }

    #[test]
    fn calibrated_refinement_recovers_clean_pair_and_has_nonnegative_deltas() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let mut case = two_flyover_case(med);
        case.p.refine_half_m = 200.0;
        let enu = Enu::new(case.obs.0, case.obs.1, 0.0);
        let third = straight_track(
            &enu,
            [0.0, 3000.0, 1200.0],
            [120.0, 0.0, 0.0],
            case.t_s + 2600.0,
            case.t_s + 2000.0,
            case.t_s + 3200.0,
            5.0,
        );
        // A third, noisier pass has a systematic 80 m transverse position error.
        // Alternating trace noise makes its extra variance identifiable at the pilot fit.
        let mut dirty = synth_event(
            "noisy",
            &third,
            enu.to_ecef([0.0, 80.0, MIC_HEIGHT_M]),
            case.t_s,
            160.0,
            343.0,
            case.t_s + 2600.0,
            &Prop::from_medium(&med, &enu),
        );
        for (k, f) in dirty.tr_f.iter_mut().enumerate() {
            *f += if k % 2 == 0 { 0.3 } else { -0.3 };
        }
        (dirty.q_t, dirty.q_f) = subsample(&dirty.tr_t, &dirty.tr_f, 40);
        case.events.push(dirty);
        case.tracks.push(third);
        let assign = [
            (0, 0, case.t_s + 1000.0),
            (1, 1, case.t_s + 1900.0),
            (2, 3, case.t_s + 2600.0),
        ];
        let dem = FlatElevation(0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        // Keep the true observer off grid, so recovery cannot rely on a central fixture cell.
        let (lat, lon, _) = enu.to_geodetic([7.0, -9.0, 0.0]);
        let grid = local_grid(lat, lon, case.p.refine_half_m, case.p.refine_step_m);
        let pilot: Vec<PointEval> = grid
            .par_iter()
            .map(|&(la, lo, _, _)| {
                eval_point(
                    &observer(la, lo, &dem),
                    &assign,
                    &case.events,
                    &case.tracks,
                    &case.p,
                    &atmo,
                    true,
                    false,
                    false,
                )
                .unwrap()
            })
            .collect();
        let k_pilot = (0..pilot.len())
            .min_by(|&a, &b| pilot[a].total.total_cmp(&pilot[b].total))
            .unwrap();
        let old_objective = |pe: &PointEval| -> f64 {
            pe.chis
                .iter()
                .zip(&pilot[k_pilot].chis)
                .zip(&assign)
                .map(|((fit, scale), a)| case.events[a.0].n_eff * fit.chi / scale.chi.max(1.0))
                .sum()
        };
        let old_min = pilot
            .iter()
            .map(old_objective)
            .fold(f64::INFINITY, f64::min);
        eprintln!(
            "pilot index {k_pilot} ENU {:?}, raw total {}, chis {:?}; mixed minimum {old_min}, mixed at pilot {}",
            (grid[k_pilot].2, grid[k_pilot].3),
            pilot[k_pilot].total,
            pilot[k_pilot]
                .chis
                .iter()
                .map(|c| c.chi)
                .collect::<Vec<_>>(),
            old_objective(&pilot[k_pilot]),
        );
        assert!(
            old_min < old_objective(&pilot[k_pilot]) - 0.1,
            "fixture must expose the old negative-delta surface"
        );

        let mut weights = None;
        let refined = refine_on(
            lat,
            lon,
            &assign,
            &case.events,
            &case.tracks,
            &case.p,
            &dem,
            &atmo,
            &mut weights,
        )
        .unwrap();
        let delta: Vec<f64> = refined.dchi2.iter().flatten().copied().collect();
        assert!(delta.iter().all(|&dc| dc.is_finite() && dc >= 0.0));
        let minimum = (0..delta.len())
            .min_by(|&a, &b| delta[a].total_cmp(&delta[b]))
            .unwrap();
        let evaluate = |la, lo| {
            eval_point_weighted(
                &observer(la, lo, &dem),
                &assign,
                &case.events,
                &case.tracks,
                &case.p,
                &atmo,
                true,
                false,
                false,
                weights.as_deref(),
            )
            .unwrap()
            .total
        };
        let center_cost = evaluate(refined.lat, refined.lon);
        let cell_cost = evaluate(grid[minimum].0, grid[minimum].1);
        assert!(center_cost <= cell_cost);
        assert!((cell_cost - center_cost - delta[minimum]).abs() < 1e-8);
        assert!(weights.as_ref().unwrap()[2] < 0.25);
        let pilot_error = haversine_m(grid[k_pilot].0, grid[k_pilot].1, case.obs.0, case.obs.1);
        let refined_error = haversine_m(refined.lat, refined.lon, case.obs.0, case.obs.1);
        assert!(
            refined_error < 30.0,
            "calibrated error {refined_error:.1} m"
        );
        assert!(
            refined_error < 0.5 * pilot_error,
            "pilot {pilot_error:.1} m, calibrated {refined_error:.1} m"
        );
        eprintln!(
            "noisy third pass: pilot error {pilot_error:.1} m, calibrated {refined_error:.1} m, weights {weights:?}"
        );
    }

    #[test]
    fn refinement_expands_when_the_contour_reaches_an_offset_grid_edge() {
        let med = fdop_types::atmo::Medium::still(343.0);
        let mut case = two_flyover_case(med);
        case.p.refine_half_m = 100.0;
        case.p.refine_step_m = 25.0;
        let dem = FlatElevation(0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        let assign = [(0, 0, case.t_s + 1000.0), (1, 1, case.t_s + 1900.0)];
        let (mlat, mlon) = fdop_types::geo::m_per_deg(case.obs.0);
        // A coarse winner can put the true minimum on any edge of the refinement grid.
        // Its small contour extent about that minimum does not mean the contour is closed.
        for (east, north) in [(100.0, 0.0), (-100.0, 0.0), (0.0, 100.0), (0.0, -100.0)] {
            let lat = case.obs.0 + north / mlat;
            let lon = case.obs.1 + east / mlon;
            let mut weights = None;
            let r = refine_on(
                lat,
                lon,
                &assign,
                &case.events,
                &case.tracks,
                &case.p,
                &dem,
                &atmo,
                &mut weights,
            )
            .unwrap();
            let last = r.dchi2.len() - 1;
            let touches_edge = r.dchi2.iter().enumerate().any(|(iy, row)| {
                row.iter()
                    .enumerate()
                    .any(|(ix, &dc)| (iy == 0 || iy == last || ix == 0 || ix == last) && dc <= 5.99)
            });
            assert!(
                touches_edge,
                "test must exercise a clipped contour: {east}, {north}"
            );
            assert!(
                r.radius_lower_bound,
                "contour at grid edge was treated as complete: {east}, {north}, {r:?}"
            );

            // Re-centering and widening must recover a complete contour and the known point.
            let mut wide = case.p.clone();
            wide.refine_half_m *= 2.0;
            wide.refine_step_m *= 2.0;
            let first_weights = weights.clone();
            let complete = refine_on(
                r.lat,
                r.lon,
                &assign,
                &case.events,
                &case.tracks,
                &wide,
                &dem,
                &atmo,
                &mut weights,
            )
            .unwrap();
            assert_eq!(
                weights, first_weights,
                "expansion must not recalibrate the objective"
            );
            assert!(!complete.radius_lower_bound, "{complete:?}");
            assert!(
                haversine_m(complete.lat, complete.lon, case.obs.0, case.obs.1) < 35.0,
                "refined location must recover the synthetic observer"
            );
        }
    }

    fn locate_two(med: fdop_types::atmo::Medium) {
        let c = 343.0;
        let case = two_flyover_case(med);
        let (t_s, (lat0, lon0)) = (case.t_s, case.obs);
        let res = run_case(&case, med);
        assert!(!res.candidates.is_empty(), "funnel {:?}", res.funnel);
        let top = &res.candidates[0];
        let r = res.refine.as_ref().unwrap();
        let err = haversine_m(r.lat, r.lon, lat0, lon0);
        assert_eq!((top.lat, top.lon), (r.lat, r.lon));
        assert_eq!(top.t_start_utc, r.t_start_utc);
        assert_eq!(res.margin["top_score"].as_f64(), Some(top.score));
        assert!(
            err < 60.0,
            "refined {:.6},{:.6} err {err:.0} m; top {:?}",
            r.lat,
            r.lon,
            top.events
        );
        assert!(
            (top.t_start_utc - t_s).abs() < 1.0,
            "clock {}",
            top.t_start_utc - t_s
        );
        let m1 = top.events.iter().find(|m| m.event == "e1").unwrap();
        let m2 = top.events.iter().find(|m| m.event == "e2").unwrap();
        assert_eq!((m1.flight.as_str(), m2.flight.as_str()), ("A", "B"));
        eprintln!(
            "err {err:.1} m radius {:.0} m clock {:.3} s chi {:.2}/{:.2} funnel {:?}",
            r.radius_m,
            top.t_start_utc - t_s,
            m1.chi,
            m2.chi,
            res.funnel
        );
        let _ = cv_curve(0.0, 1.0, 1.0, 1.0, 0.0, c);
    }
}
