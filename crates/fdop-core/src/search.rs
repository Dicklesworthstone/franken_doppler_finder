//! Flight-centric geolocation search over ADS-B tracks.
//!
//! Every measured event says: "some tonal aircraft passed its closest point of approach at a
//! slant distance in `d_gate`, moving at a speed in `v_gate`, and that CPA was *heard* at
//! recording time `t_rec`". For an aircraft at position p with velocity v, the ground points
//! whose CPA is *now* lie on the line perpendicular to the horizontal velocity, displaced
//! along-track by a = H*vU/vH (H = height above the observer), at cross-track offsets b with
//! a^2 + b^2 + H^2 = d^2. Sampling every track every <= 2 s / 400 m and every such line every
//! 400 m gives, per event, a cloud of hypotheses (position, implied recording start
//! t_s = t_cpa + d/c - t_rec). Two events recorded at the same place must agree on BOTH the
//! position and t_s, so a spatiotemporal hash join of two events' clouds yields the candidate
//! (flight, flight, place, clock) combinations.
//!
//! Stages: hypotheses -> pair join -> quick trace check at the joined point -> fine 100 m grid
//! with exact CPAs, gates and joint clock-locked trace scoring -> confirmers (other events
//! explained at the same place and clock) and unheard close passes -> ranking -> 25 m
//! refinement with a chi-square confidence region.

use crate::measure::MeasuredEvent;
use crate::track::{Prop, TrackModel};
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
    /// Recording-clock time of the delayed (heard) CPA, s.
    pub t_rec: f64,
    /// Half-width of the plausible t_rec interval, s.
    pub t_tol: f64,
    pub v_gate: [f64; 2],
    pub d_gate: [f64; 2],
    pub f0: f64,
    pub f0_tol: f64,
    /// Trace noise scale used to normalise residuals (Hz).
    pub sigma_hz: f64,
    /// Effective number of independent trace points (from the fit).
    pub n_eff: f64,
    pub quality: f64,
    /// Model-misfit flag from the fit (wider gates, larger t_tol).
    pub misfit: bool,
    /// Sound speed the fit assumed (still air): fitted v and d scale by c_true / c_fit.
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
    /// Build from a quality-passing measured event. `wind_mps` widens the speed gate (ground
    /// speed vs the air-relative speed the Doppler curve encodes).
    pub fn from_measured(m: &MeasuredEvent, wind_mps: f64) -> Option<Self> {
        let fr = m.fit.as_ref()?;
        if !m.quality.ok {
            return None;
        }
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
            n_eff: fr.diagnostics.n_eff.max(3.0),
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
            cap_sigma: 5.0,
            max_quick: 3_000_000,
            max_fine: 4000,
            refine_step_m: 25.0,
            refine_half_m: 700.0,
            tonal_only: true,
            max_events: 12,
            max_hyp_per_event: 30_000_000,
            max_hyp_total: 200_000_000,
            max_confirm: 2000,
        }
    }
}

/// Hypothesis budgets (per event, total) from physical memory: a tenth of RAM for stored
/// clouds (20 bytes each), at least the 30 M / 200 M defaults, at most 2 G in total.
pub fn hyp_budget(mem_bytes: Option<u64>) -> (usize, usize) {
    let total = mem_bytes
        .map_or(200_000_000, |m| ((m / 10) / 20) as usize)
        .clamp(200_000_000, 2_000_000_000);
    ((total / 3).max(30_000_000), total)
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

/// An event's hypothesis cloud: stored when within the budget, always counted exactly, and a
/// deterministic ~1/1024 sample for the chance-match calibration.
struct HypSet {
    stored: Option<Vec<Hyp>>,
    count: usize,
    sample: Vec<Hyp>,
}

#[inline]
fn mix(a: u64, b: u64) -> u64 {
    let mut x = a.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ b.wrapping_add(0x632b_e59b_d9b4_e019);
    x ^= x >> 31;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^ (x >> 29)
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
    // fitted v and d are in units of the fit's still-air sound speed; the true values are
    // lambda times larger, lambda = (c_path + v.w / c_path) / c_fit (at most ~ +-8 %)
    let reach = p.region.padded_km(1.1 * ev.d_gate[1] / 1000.0 + 0.5);
    let t_lo_all = p.start_lo + ev.t_rec - ev.t_tol - 1.1 * ev.d_gate[1] / p.c - 5.0;
    let parts = tracks
        .par_iter()
        .enumerate()
        .map(|(ti, tr)| {
            let mut out = Vec::new();
            let te_lo = t_lo_all.max(tr.t0());
            let te_hi = (p.start_hi + ev.t_rec + ev.t_tol + 5.0).min(tr.t1());
            let mut t = te_lo;
            while t <= te_hi {
                let (pos, vel) = tr.pv(t);
                let (la, lo, h) = ecef_to_geodetic(pos);
                if !reach.contains(la, lo) {
                    t += 10.0;
                    continue;
                }
                let local = Enu::new(la, lo, 0.0);
                let ve = local.vec_from_ecef(vel);
                let speed = norm(vel);
                let vh = ve[0].hypot(ve[1]);
                let dt = (p.along_step_m / vh.max(1.0)).clamp(0.2, p.along_dt_max);
                let z0 = dem.elev_m(la, lo);
                let med = atmo.medium(la, lo, t, z0 + MIC_HEIGHT_M, h);
                let c = med.c;
                let lam = (c + (ve[0] * med.wind_e + ve[1] * med.wind_n) / c) / ev.c_fit;
                let (d_lo, d_hi) = (ev.d_gate[0] * lam, ev.d_gate[1] * lam);
                if speed >= ev.v_gate[0] * lam
                    && speed <= ev.v_gate[1] * lam
                    && vh > 12.0
                    && tr.gap_at(t) <= p.max_gap_s
                {
                    let (ux, uy) = (ve[0] / vh, ve[1] / vh);
                    let (nx, ny) = (-uy, ux);
                    let (mlat, mlon) = fdop_types::geo::m_per_deg(la);
                    let a0 = (h - z0 - MIC_HEIGHT_M) * ve[2] / vh;
                    for side in [1.0f64, -1.0] {
                        let mut b = if side > 0.0 { 0.0 } else { p.cross_step_m };
                        while b <= d_hi {
                            let (oe, on) = (a0 * ux + side * b * nx, a0 * uy + side * b * ny);
                            let (ola, olo) = (la + on / mlat, lo + oe / mlon);
                            let z = dem.elev_m(ola, olo);
                            let hh = h - z - MIC_HEIGHT_M;
                            let a = hh * ve[2] / vh;
                            let d = (a * a + b * b + hh * hh).sqrt();
                            if d > d_hi * 1.02 && b > 0.0 {
                                break;
                            }
                            if hh >= p.min_agl_m
                                && d >= d_lo
                                && d <= d_hi
                                && p.region.contains(ola, olo)
                            {
                                let t_s = t + d / c - ev.t_rec;
                                if t_s >= p.start_lo - ev.t_tol && t_s <= p.start_hi + ev.t_tol {
                                    let (e, n) = frame.en(ola, olo);
                                    out.push(Hyp {
                                        trk: ti as u32,
                                        t_e: (t - t_base) as f32,
                                        e: e as f32,
                                        n: n as f32,
                                        t_s: (t_s - t_base) as f32,
                                    });
                                }
                            }
                            b += p.cross_step_m;
                        }
                    }
                }
                t += dt;
            }
            let n = out.len();
            let sample: Vec<Hyp> = out
                .iter()
                .enumerate()
                .filter(|(k, _)| mix(ti as u64, *k as u64).is_multiple_of(1024))
                .map(|(_, h)| *h)
                .collect();
            let before = count.fetch_add(n, Ordering::Relaxed);
            if before + n > cap {
                out = Vec::new();
            }
            (out, n, sample)
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
/// A flight pair in one 1 km cell.
type CellKey = (usize, usize, u32, u32, i32, i32);

#[derive(Clone, Copy, Debug)]
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
    let r2 = r * r;
    let out: Vec<Joined> = hj
        .par_iter()
        .flat_map_iter(|b| {
            let (kx, ky, kt) = key(b.e, b.n, b.t_s);
            let mut v = Vec::new();
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dtk in -1..=1 {
                        if let Some(list) = map.get(&(kx + dx, ky + dy, kt + dtk)) {
                            for &ia in list {
                                let a = &hi[ia as usize];
                                let (de, dn) = ((a.e - b.e) as f64, (a.n - b.n) as f64);
                                if de * de + dn * dn > r2 || ((a.t_s - b.t_s) as f64).abs() > tt {
                                    continue;
                                }
                                if a.trk == b.trk
                                    && ((a.t_e - b.t_e) as f64).abs() < same_flight_min_dt
                                {
                                    continue;
                                }
                                v.push(Joined {
                                    ei,
                                    ej,
                                    ti: a.trk,
                                    tj: b.trk,
                                    te_i: a.t_e as f64 + t_base,
                                    te_j: b.t_e as f64 + t_base,
                                    e: 0.5 * (a.e + b.e) as f64,
                                    n: 0.5 * (a.n + b.n) as f64,
                                    t_s: 0.5 * (a.t_s + b.t_s) as f64 + t_base,
                                });
                            }
                        }
                    }
                }
            }
            v
        })
        .collect();
    // one representative per (flights, 600 m cell, 20 s)
    let mut seen: std::collections::HashSet<(u32, u32, i32, i32, i64)> =
        std::collections::HashSet::new();
    out.into_iter()
        .filter(|j| {
            seen.insert((
                j.ti,
                j.tj,
                (j.e / 600.0).floor() as i32,
                (j.n / 600.0).floor() as i32,
                (j.t_s / 20.0).floor() as i64,
            ))
        })
        .collect()
}

/// Frequency-ratio curve c/(c+v_r) on a uniform reception-time grid for one (track, observer).
struct RatioGrid {
    t0: f64,
    dt: f64,
    r: Vec<f64>,
}

impl RatioGrid {
    #[allow(clippy::too_many_arguments)]
    fn build(tr: &TrackModel, x: V3, m: &Prop, lo: f64, hi: f64, dt: f64, max_gap: f64) -> Self {
        let n = (((hi - lo) / dt).ceil() as usize + 1).max(2);
        let mut r = Vec::with_capacity(n);
        let mut warm: Option<f64> = None;
        for k in 0..n {
            let tu = lo + k as f64 * dt;
            match tr.solve_emission_p(tu, x, m, warm) {
                Some((te, _, ratio)) if tr.gap_at(te) <= max_gap => {
                    warm = Some(te);
                    r.push(ratio);
                }
                _ => {
                    warm = None;
                    r.push(f64::NAN);
                }
            }
        }
        Self { t0: lo, dt, r }
    }
    #[inline]
    fn at(&self, tu: f64) -> f64 {
        let u = (tu - self.t0) / self.dt;
        // also rejects NaN
        if u.partial_cmp(&0.0).is_none_or(|o| o.is_lt()) {
            return f64::NAN;
        }
        let i = u.floor() as usize;
        if i + 1 >= self.r.len() {
            return f64::NAN;
        }
        let w = u - i as f64;
        self.r[i] * (1.0 - w) + self.r[i + 1] * w
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

/// Fit of one event's trace against a model ratio curve at clock t_s. The tone is fitted in
/// closed form; misfit events may also change tone once (searched over every split).
#[allow(clippy::too_many_arguments)]
fn trace_chi(
    tt: &[f64],
    ff: &[f64],
    ratio: &dyn Fn(f64) -> f64,
    t_s: f64,
    ev: &SearchEvent,
    sigma: f64,
    cap_sigma: f64,
    robust: bool,
) -> TraceFit {
    let n = tt.len();
    let mut rr = Vec::with_capacity(n);
    let (mut sfr, mut srr) = (0.0, 0.0);
    for k in 0..n {
        let r = ratio(t_s + tt[k]);
        rr.push(r);
        if r.is_finite() {
            sfr += ff[k] * r;
            srr += r * r;
        }
    }
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
    let clamp_f0 = |f: f64| f.clamp(ev.f0 - ev.f0_tol, ev.f0 + ev.f0_tol);
    let mut f0 = clamp_f0(sfr / srr);
    let mut lim = f64::INFINITY;
    if robust {
        let mut ad: Vec<f64> = (0..n)
            .filter(|&k| rr[k].is_finite())
            .map(|k| (ff[k] - f0 * rr[k]).abs())
            .collect();
        let mad = 1.4826 * crate::spectro::median_f64(&mut ad);
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
    if robust && ev.misfit && n >= 40 {
        // prefix sums over inliers of the single-tone fit; split with the least squared error
        let (mut pa, mut pb, mut pc) = (vec![0.0; n + 1], vec![0.0; n + 1], vec![0.0; n + 1]);
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
        let seg_clamp = |f: f64| f.clamp(0.85 * ev.f0, 1.15 * ev.f0);
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
    pub radius_m: f64,
    pub radius_basis: String,
    pub step_m: f64,
    pub lats: Vec<f64>,
    pub lons: Vec<f64>,
    /// Delta chi-square relative to the best point (NaN = not scorable).
    pub dchi2: Vec<Vec<f64>>,
    /// The 95 % region reached the edge of the widest window tried.
    pub radius_lower_bound: bool,
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

/// Exact evaluation of an observer point for a set of (event, track) assignments with a joint
/// clock. Returns per-event matches and the joint clock.
struct PointEval {
    t_s: f64,
    chis: Vec<TraceFit>,
    cpas: Vec<crate::track::Cpa>,
    props: Vec<Prop>,
    total: f64,
}

/// Gate check of one CPA against an event (fitted v, d rescaled to the true medium).
fn gates_ok(ev: &SearchEvent, cpa: &crate::track::Cpa, prop: &Prop, slack: f64) -> bool {
    let lam = prop.fit_scale(cpa.vel, ev.c_fit);
    cpa.interior
        && cpa.d >= ev.d_gate[0] * lam * (1.0 - slack)
        && cpa.d <= ev.d_gate[1] * lam * (1.0 + slack)
        && cpa.speed >= ev.v_gate[0] * lam * (1.0 - slack)
        && cpa.speed <= ev.v_gate[1] * lam * (1.0 + slack)
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
        let cpa = tr.cpa_near(x, te_guess, 150.0)?;
        let prop = prop_for(atmo, o, tr, cpa.t_e);
        if gates
            && (!gates_ok(ev, &cpa, &prop, if quick { 0.12 } else { 0.0 })
                || tr.gap_at(cpa.t_e) > p.max_gap_s)
        {
            return None;
        }
        ts_imp.push(cpa.t_r_p(&prop) - ev.t_rec);
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
    let cost = |t_s: f64| -> (f64, Vec<TraceFit>) {
        let mut tot = 0.0;
        let mut v = Vec::with_capacity(assign.len());
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
                    ev.sigma_hz.max(0.6),
                    p.cap_sigma,
                    false,
                )
            } else {
                trace_chi(
                    &ev.tr_t,
                    &ev.tr_f,
                    &|tu| g.at(tu),
                    t_s,
                    ev,
                    ev.sigma_hz,
                    p.cap_sigma,
                    true,
                )
            };
            tot += ev.n_eff * r.chi;
            v.push(r);
        }
        (tot, v)
    };
    let step = if quick { 1.0 } else { 0.25 };
    let n = (((hi - lo) / step).ceil() as usize).clamp(1, 400);
    let (mut bt, mut bc) = (lo, f64::INFINITY);
    for k in 0..=n {
        let ts = lo + (hi - lo) * k as f64 / n as f64;
        let c0 = cost(ts).0;
        if c0 < bc {
            bc = c0;
            bt = ts;
        }
    }
    if !bc.is_finite() {
        return None;
    }
    if !quick {
        let (mut a, mut b) = ((bt - step).max(lo), (bt + step).min(hi));
        let gr = 0.618_033_988_749_895;
        let (mut c1, mut c2) = (b - gr * (b - a), a + gr * (b - a));
        let (mut f1, mut f2) = (cost(c1).0, cost(c2).0);
        while b - a > 0.01 {
            if f1 < f2 {
                b = c2;
                c2 = c1;
                f2 = f1;
                c1 = b - gr * (b - a);
                f1 = cost(c1).0;
            } else {
                a = c1;
                c1 = c2;
                f1 = f2;
                c2 = a + gr * (b - a);
                f2 = cost(c2).0;
            }
        }
        let tm = 0.5 * (a + b);
        if cost(tm).0 < bc {
            bt = tm;
        }
    }
    let (total, chis) = cost(bt);
    Some(PointEval {
        t_s: bt,
        chis,
        cpas,
        props,
        total,
    })
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

impl EventNull {
    /// -ln P(chance chi <= chi | gates), from the empirical null (bounded by ln(n+1)).
    /// Below the 5 % quantile the empirical CDF is too coarse (a match far better than every
    /// chance sample would cap at ln(n+1)), so the lower tail is log-normal, fitted to the null's
    /// median and 5 % quantile; capped at 25 nats.
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
        (-p.max(1e-300).ln()).min(25.0)
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
                let Some(cpa) = tr.cpa_near(o.x, h.t_e as f64 + t_base, 60.0) else {
                    continue;
                };
                let prop = prop_for(atmo, &o, tr, cpa.t_e);
                if !gates_ok(ev, &cpa, &prop, 0.05) {
                    continue;
                }
                let ts0 = cpa.t_r_p(&prop) - ev.t_rec;
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
                        ev.sigma_hz,
                        p.cap_sigma,
                        true,
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

/// Track indices sorted by start time, for time-window queries.
struct TimeIndex {
    by_t0: Vec<(f64, f64, u32)>,
    max_len: f64,
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
        Self { by_t0, max_len }
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
    exclude: &[u32],
) -> Option<(f64, EventMatch, u32)> {
    let c = p.c;
    let t_heard = t_start + ev.t_rec;
    let mut best: Option<(f64, EventMatch, u32)> = None;
    for ti in tix.overlapping(t_heard - 1.2 * ev.d_gate[1] / c - 120.0, t_heard + 30.0) {
        if exclude.contains(&ti) {
            continue;
        }
        let tr = &tracks[ti as usize];
        if p.tonal_only && !tr.engine.tonal() {
            continue;
        }
        let Some(cpa) = tr.cpa_near(
            o.x,
            t_heard - 1.1 * ev.d_gate[1].min(12_000.0) / c * 0.5,
            200.0,
        ) else {
            continue;
        };
        if cpa.d > 1.15 * ev.d_gate[1] {
            continue;
        }
        let prop = prop_for(atmo, o, tr, cpa.t_e);
        if !gates_ok(ev, &cpa, &prop, 0.0)
            || (cpa.t_r_p(&prop) - t_heard).abs() > ev.t_tol + 3.0
            || tr.gap_at(cpa.t_e) > p.max_gap_s
        {
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
            ev.sigma_hz,
            p.cap_sigma,
            true,
        );
        if chi.chi <= p.kappa && chi.cov >= 0.6 && best.as_ref().is_none_or(|b| chi.chi < b.0) {
            best = Some((
                chi.chi,
                make_match(ev, tr, &cpa, chi, &prop, "confirmer"),
                ti,
            ));
        }
    }
    best
}

/// Evidence of a candidate (nats): sum over explained events of the chance-match surprise
/// (gates + trace), minus `UNHEARD_NATS` per unheard close pass. Also sets each match's nats.
fn evidence(events: &mut [EventMatch], nulls: &[EventNull], unheard: usize) -> f64 {
    let mut s = 0.0;
    for m in events.iter_mut() {
        let (n, t) = nulls
            .iter()
            .find(|n| n.event == m.event)
            .map_or((0.0, 0.0), |n| (n.nats(m.chi), n.trace_nats(m.chi)));
        m.nats = n;
        m.trace_nats = t;
        m.core = m.chi <= CORE_CHI
            && m.dt_free.is_some_and(|d| d.abs() <= CORE_LOCK_S)
            && t >= CORE_TRACE_NATS;
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
    // events: clean fits first, then best quality; drop harmonics of a stronger event
    let mut evs: Vec<SearchEvent> = inp.events.clone();
    evs.sort_by(|a, b| {
        a.misfit
            .cmp(&b.misfit)
            .then(b.quality.total_cmp(&a.quality))
    });
    let mut keep: Vec<SearchEvent> = Vec::new();
    'ev: for e in evs {
        for k in &keep {
            if (k.t_rec - e.t_rec).abs() < 8.0 {
                let r = e.f0 / k.f0;
                for m in 1..=6u32 {
                    for q in 1..=4u32 {
                        if (r / (m as f64 / q as f64) - 1.0).abs() < 0.03 {
                            continue 'ev;
                        }
                    }
                }
            }
        }
        keep.push(e);
    }
    let evs = keep;
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
        let full = hyps.iter().filter(|h| h.is_some()).count() >= p.max_events;
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
        }
        hyps.push(set.stored);
        nulls.push(null);
    }
    funnel.hypotheses = evs
        .iter()
        .zip(&hyps)
        .map(|(e, h)| (e.id.clone(), h.as_ref().map(|v| v.len())))
        .collect();
    let pairable: Vec<usize> = (0..evs.len()).filter(|&i| hyps[i].is_some()).collect();
    // pair joins: clean-clean first, then by expected join size
    let rho = 0.5 * p.along_step_m.hypot(p.cross_step_m);
    let r_join = 2.0 * rho + 120.0;
    let slop = p.along_dt_max + p.cross_step_m / c + 2.0;
    let mut joined: Vec<Joined> = Vec::new();
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
        let mut jv = join(ha, hb, a, b, r_join, tt, 30.0, t_base);
        progress(&format!("join {}x{}: {}", evs[a].id, evs[b].id, jv.len()));
        joined.append(&mut jv);
        pairs_done += 1;
        if joined.len() > p.max_quick {
            break;
        }
    }
    if pairs_done < pairs.len() {
        warnings.push(format!("joined {pairs_done} of {} event pairs before the quick-check budget ({}); narrow the region or dates", pairs.len(), p.max_quick));
        funnel
            .truncated
            .push(format!("pairs: {} -> {pairs_done}", pairs.len()));
    }
    funnel.joined = joined.len();
    // quick check at the joined point
    let done = AtomicUsize::new(0);
    let quick: Vec<(Joined, f64)> = joined
        .par_iter()
        .filter_map(|j| {
            let k = done.fetch_add(1, Ordering::Relaxed);
            if k.is_multiple_of(500_000) && k > 0 {
                progress(&format!("quick check {k}/{}", joined.len()));
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
                .map(|(c, a)| ev_weight(&evs[a.0]) * (1.0 - c.chi / p.quick_chi).max(0.0))
                .sum();
            (worst <= p.quick_chi).then_some((*j, qs))
        })
        .collect();
    funnel.quick_passed = quick.len();
    progress(&format!("quick check passed: {}", quick.len()));
    // merge quick survivors that are the same (flights, place) and keep the best few thousand
    let mut best_by: HashMap<CellKey, (Joined, f64)> = HashMap::new();
    for (j, s) in quick {
        let k = (
            j.ei,
            j.ej,
            j.ti,
            j.tj,
            (j.e / 1000.0).floor() as i32,
            (j.n / 1000.0).floor() as i32,
        );
        match best_by.get(&k) {
            Some(o) if o.1 >= s => {}
            _ => {
                best_by.insert(k, (j, s));
            }
        }
    }
    // every flight pair's best cell is scored before any pair's second-best cell, so a budget
    // cut cannot starve the true pair behind many cells of a few look-alike pairs
    let mut groups: HashMap<PairKey, Vec<(Joined, f64)>> = HashMap::new();
    for (j, s) in best_by.into_values() {
        groups
            .entry((j.ei, j.ej, j.ti, j.tj))
            .or_default()
            .push((j, s));
    }
    let mut ranked: Vec<(usize, Joined, f64)> = Vec::new();
    for (_, mut g) in groups {
        g.sort_by(|a, b| b.1.total_cmp(&a.1));
        ranked.extend(g.into_iter().enumerate().map(|(r, (j, s))| (r, j, s)));
    }
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then(b.2.total_cmp(&a.2)));
    let mut fine_in: Vec<(Joined, f64)> = ranked.into_iter().map(|(_, j, s)| (j, s)).collect();
    if fine_in.len() > p.max_fine {
        funnel
            .truncated
            .push(format!("fine: {} -> {}", fine_in.len(), p.max_fine));
        warnings.push(format!(
            "{} quick-check survivors > {}; scoring the best {}",
            fine_in.len(),
            p.max_fine,
            p.max_fine
        ));
        fine_in.truncate(p.max_fine);
    }
    funnel.fine_scored = fine_in.len();
    // fine grid + full scoring
    let fine: Vec<Candidate> = fine_in
        .par_iter()
        .filter_map(|(j, _)| {
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
                events,
            })
        })
        .collect();
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
            let excl: Vec<u32> = cand.trk.iter().map(|t| t.1).collect();
            if let Some((_, m, ti)) =
                best_confirmer(&o, cand.t_start_utc, ev, tracks, &tix, p, atmo, &excl)
            {
                cand.events.push(m);
                cand.trk.push((ek, ti));
            }
        }
        cand.n_explained = cand.events.len();
        cand.fit_score = fit_score(&cand.events, &evs, p.kappa);
        cand.unheard_close_passes = unheard_passes(cand, &o, tracks, &tix, heard, p);
        let nu = cand.unheard_close_passes.len();
        cand.score = evidence(&mut cand.events, &nulls, nu);
    });
    cands.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.pair_chi.total_cmp(&b.pair_chi))
    });
    // lock check: every event's own preferred clock must agree with the joint clock. An event
    // that disagrees (paired or confirming) is a coincidence: drop it, refit the clock on the
    // rest and repeat. Pruning reorders the list, so iterate until the leaders are all checked.
    for _ in 0..8 {
        let n_lock = cands.len().min(20);
        if cands[..n_lock].iter().all(|c| c.lock_checked) {
            break;
        }
        cands[..n_lock]
            .par_iter_mut()
            .filter(|c| !c.lock_checked)
            .for_each(|cand| {
                lock_prune(cand, &evs, tracks, p, atmo, inp.fine_dem);
                let o = observer(cand.lat, cand.lon, inp.fine_dem);
                cand.fit_score = fit_score(&cand.events, &evs, p.kappa);
                cand.unheard_close_passes = unheard_passes(cand, &o, tracks, &tix, heard, p);
                let nu = cand.unheard_close_passes.len();
                cand.score = evidence(&mut cand.events, &nulls, nu);
                cand.lock_checked = true;
            });
        cands.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.pair_chi.total_cmp(&b.pair_chi))
        });
        cands = dedupe(cands);
    }
    // margin against the best *different explanation*: a place more than 5 km away, or one
    // that explains the events differently (no shared event-flight match, or a shared event
    // matched to another flight). The same flights at a nearby worse optimum are positional
    // ambiguity, which the refinement radius covers.
    let margin = if let Some(top) = cands.first() {
        let alt = cands[1..].iter().find(|o| {
            haversine_m(o.lat, o.lon, top.lat, top.lon) > 5000.0 || !same_explanation(top, o)
        });
        let local = cands[1..].iter().find(|o| {
            same_explanation(top, o) && haversine_m(o.lat, o.lon, top.lat, top.lon) > 1000.0
        });
        serde_json::json!({
            "top_score": top.score,
            "next_score": cands.get(1).map(|c| c.score),
            "next_distinct_score": alt.map(|a| a.score),
            "next_distinct_km": alt.map(|a| haversine_m(a.lat, a.lon, top.lat, top.lon) / 1000.0),
            "next_distinct_flights": alt.map(|a| a.events.iter().map(|m| format!("{}:{} {}", m.event, if m.reg.is_empty() { &m.icao } else { &m.reg }, m.typ)).collect::<Vec<_>>()),
            "margin_nats": alt.map(|a| top.score - a.score),
            "ratio_distinct": alt.map(|a| if top.score > 0.0 { a.score / top.score } else { f64::NAN }),
            "same_flights_alt_km": local.map(|l| haversine_m(l.lat, l.lon, top.lat, top.lon) / 1000.0),
            "same_flights_alt_margin_nats": local.map(|l| top.score - l.score),
        })
    } else {
        serde_json::json!({})
    };
    let refine = cands
        .first()
        .and_then(|top| refine_winner(top, &evs, tracks, p, inp.fine_dem, atmo));
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

/// Same place (300 m), clock (5 s) and set of event-flight matches.
fn dedupe(sorted: Vec<Candidate>) -> Vec<Candidate> {
    let key = |c: &Candidate| {
        let mut v: Vec<(String, String)> = c
            .events
            .iter()
            .map(|m| (m.event.clone(), m.flight.clone()))
            .collect();
        v.sort();
        v
    };
    let mut out: Vec<Candidate> = Vec::new();
    for f in sorted {
        let kf = key(&f);
        let dup = out.iter().any(|o| {
            haversine_m(o.lat, o.lon, f.lat, f.lon) < 300.0
                && (o.t_start_utc - f.t_start_utc).abs() < 5.0
                && key(o) == kf
        });
        if !dup {
            out.push(f);
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
/// and left no detection within 25 s of when they would have been heard.
fn unheard_passes(
    cand: &Candidate,
    o: &Obs,
    tracks: &[TrackModel],
    tix: &TimeIndex,
    heard: &[f64],
    p: &SearchParams,
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
    let explained: Vec<&str> = cand.events.iter().map(|m| m.flight.as_str()).collect();
    let (ta, tb) = (cand.t_start_utc, cand.t_start_utc + p.duration);
    for ti in tix.overlapping(ta - thr / c - 60.0, tb + 60.0) {
        let tr = &tracks[ti as usize];
        if explained.contains(&tr.key.as_str()) || (p.tonal_only && !tr.engine.tonal()) {
            continue;
        }
        for cpa in cpas_in(tr, o.x, ta - thr / c, tb, thr) {
            let t_rec = cpa.t_r(c) - cand.t_start_utc;
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
            let cpa = tr.cpa_near(o.x, m.t_cpa_heard_utc - m.d / m.c_path, 30.0)?;
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
            let mut ts = lo;
            while ts <= hi {
                let r = trace_chi(
                    &ev.tr_t,
                    &ev.tr_f,
                    &|tu| g.at(tu),
                    ts,
                    ev,
                    ev.sigma_hz,
                    p.cap_sigma,
                    true,
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
        let ti = inp
            .tracks
            .iter()
            .position(|t| {
                (&t.key == key || t.icao.eq_ignore_ascii_case(key))
                    && t.t0() < t_heard
                    && t.t1() > t_heard - 60.0
            })
            .ok_or_else(|| format!("no track {key} around {t_heard:.0}"))?;
        let tr = &inp.tracks[ti];
        let cpa = tr
            .cpa_near(o.x, t_heard - ev.d_gate[1].min(12_000.0) / p.c * 0.5, 300.0)
            .ok_or("no CPA")?;
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
        events,
    };
    let dts = free_clocks(&cand, &o, evs, inp.tracks, &pp, inp.atmo);
    for (m, d) in cand.events.iter_mut().zip(dts) {
        m.dt_free = d;
    }
    Ok(cand)
}

/// Re-score the winner's (event, flight) assignment on a fine grid; the confidence region is
/// where the total chi-square (each event's chi rescaled so its best value is 1, times n_eff)
/// is within 5.99 (2 dof, 95 %) of the minimum.
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
    // widen (and coarsen) the window until the 95 % region fits inside it
    let mut q = p.clone();
    let (mut lat0, mut lon0) = (top.lat, top.lon);
    let mut out = None;
    for _ in 0..3 {
        let r = refine_on(lat0, lon0, &assign, evs, tracks, &q, dem, atmo)?;
        let done = !r.radius_lower_bound;
        (lat0, lon0) = (r.lat, r.lon);
        out = Some(r);
        if done {
            break;
        }
        q.refine_half_m *= 2.0;
        q.refine_step_m *= 2.0;
    }
    out
}

/// One refinement grid of `p.refine_half_m` / `p.refine_step_m` around (lat0, lon0).
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
) -> Option<RefineResult> {
    let grid = local_grid(lat0, lon0, p.refine_half_m, p.refine_step_m);
    let evals: Vec<Option<PointEval>> = grid
        .par_iter()
        .map(|&(la, lo, _, _)| {
            let o = observer(la, lo, dem);
            eval_point(
                &o,
                assign,
                evs,
                tracks,
                p,
                atmo,
                p.region.contains(la, lo),
                false,
                false,
            )
        })
        .collect();
    let k_best = (0..grid.len())
        .filter(|&k| evals[k].is_some())
        .min_by(|&a, &b| {
            evals[a]
                .as_ref()
                .unwrap()
                .total
                .total_cmp(&evals[b].as_ref().unwrap().total)
        })?;
    let best = evals[k_best].as_ref()?;
    // per-event rescale: chi_e(best) -> 1 when the model fits worse than the trace noise
    let scale: Vec<f64> = best.chis.iter().map(|c| c.chi.max(1.0)).collect();
    let tot = |pe: &PointEval| -> f64 {
        pe.chis
            .iter()
            .zip(assign)
            .zip(&scale)
            .map(|((c, a), s)| ev_weight(&evs[a.0]) * evs[a.0].n_eff * c.chi / s)
            .sum()
    };
    let t_best = tot(best);
    let n = (p.refine_half_m / p.refine_step_m).round() as usize * 2 + 1;
    let mut dchi = vec![vec![f64::NAN; n]; n];
    let (bla, blo, _, _) = grid[k_best];
    let mut rmax: f64 = 0.0;
    let mut cells = 0usize;
    for (k, e) in evals.iter().enumerate() {
        let (iy, ix) = (k / n, k % n);
        if let Some(pe) = e {
            let dc = tot(pe) - t_best;
            dchi[iy][ix] = dc;
            if dc <= 5.99 {
                cells += 1;
                let (la, lo, _, _) = grid[k];
                rmax = rmax.max(haversine_m(bla, blo, la, lo));
            }
        }
    }
    let lats: Vec<f64> = (0..n).map(|iy| grid[iy * n].0).collect();
    let lons: Vec<f64> = (0..n).map(|ix| grid[ix].1).collect();
    let area_r = (cells as f64 * p.refine_step_m * p.refine_step_m / std::f64::consts::PI).sqrt();
    let floor = 75.0;
    let radius = (rmax + 0.5 * p.refine_step_m).max(floor);
    let edge = rmax >= p.refine_half_m - p.refine_step_m;
    Some(RefineResult {
        lat: bla,
        lon: blo,
        radius_m: radius,
        radius_basis: format!(
            "95% chi-square region (2 dof) of the joint trace fit over {} events ({}) on a {:.0} m grid; max extent {:.0} m, equal-area radius {:.0} m, floor {:.0} m{}",
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
            if edge {
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
        let t_rec = cpa.t_r_p(prop) - t_s;
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
            f0,
            f0_tol: 4.0,
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
    fn misfit_events_may_change_tone_once() {
        // tone 121 Hz before t = 10 s, 131 Hz after (a prop RPM change), on a Doppler-like ratio
        let ratio = |t: f64| 1.0 + 0.05 * (-t / 20.0).tanh();
        let tt: Vec<f64> = (0..480).map(|k| -60.0 + 0.25 * k as f64).collect();
        let ff: Vec<f64> = tt
            .iter()
            .map(|&t| if t < 10.0 { 121.0 } else { 131.0 } * ratio(t) + 0.03 * ((t * 7.0).sin()))
            .collect();
        let single = trace_chi(
            &tt,
            &ff,
            &ratio,
            0.0,
            &plain_event(126.0, false),
            0.12,
            5.0,
            true,
        );
        let split = trace_chi(
            &tt,
            &ff,
            &ratio,
            0.0,
            &plain_event(126.0, true),
            0.12,
            5.0,
            true,
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
            0.12,
            5.0,
            true,
        );
        assert!(one.chi < 1e-6, "{one:?}");
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
    }

    #[test]
    fn budget_scales_with_memory() {
        assert_eq!(hyp_budget(None), (66_666_666, 200_000_000));
        let (per, tot) = hyp_budget(Some(256 << 30));
        assert!(tot > 1_300_000_000 && per == tot / 3, "{per} {tot}");
        assert_eq!(hyp_budget(Some(8 << 30)).1, 200_000_000);
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

    fn locate_two(med: fdop_types::atmo::Medium) {
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
        let dem = FlatElevation(0.0);
        let atmo = fdop_types::atmo::UniformAtmosphere(med);
        let inp = SearchInputs {
            events: vec![e1, e2],
            tracks: &tracks,
            heard_times: vec![],
            coarse_dem: &dem,
            fine_dem: &dem,
            atmo: &atmo,
        };
        let res = run(&inp, &p, &|_s| {});
        assert!(!res.candidates.is_empty(), "funnel {:?}", res.funnel);
        let top = &res.candidates[0];
        let r = res.refine.as_ref().unwrap();
        let err = haversine_m(r.lat, r.lon, lat0, lon0);
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
