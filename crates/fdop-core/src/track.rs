//! ADS-B track model: cubic Hermite interpolation of ECEF position using per-sample velocity
//! vectors (velocity-Hermite). Reported ground speed / track / vertical rate are used when
//! present and consistent with the positions; otherwise velocity comes from second-order
//! finite differences. Tracks are split at gaps longer than `max_gap_s` so interpolation never
//! invents geometry across a coverage hole. Within each piece only the longest physically
//! consistent chain of positions is kept: archived legs can interleave junk positions (MLAT
//! mis-solutions, another aircraft's position under the same address) that the interpolation
//! would otherwise fly through.

use fdop_types::engine::EngineClass;
use fdop_types::geo::{Enu, V3, add, dot, ecef_to_geodetic, geodetic_to_ecef, norm, scale, sub};
use fdop_types::store::{FlightLeg, pt_flags};

#[derive(Clone, Debug)]
pub struct TrackModel {
    /// Leg key, plus `#k` when the leg was split at a gap.
    pub key: String,
    pub icao: String,
    pub reg: String,
    pub typ: String,
    pub engine: EngineClass,
    pub source: String,
    pub t: Vec<f64>,
    pub pos: Vec<V3>,
    pub vel: Vec<V3>,
    /// Share of samples whose velocity came from reported gs/track (vs finite differences).
    pub reported_vel_frac: f64,
    pub mlat_frac: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct BuildParams {
    pub max_gap_s: f64,
    /// Maximum velocity residual against the positions, allowing for reported heading changes.
    pub vel_consistency_mps: f64,
    /// Fastest vertical speed two positions of one chain may imply (m/s). GA climbs at
    /// 3-8 m/s and turboprops at up to ~15 m/s; junk positions jump kilometres in seconds.
    pub max_vz_mps: f64,
    /// Position noise allowed between two points of one chain (horizontal, vertical; m).
    pub pos_slack_m: (f64, f64),
}

impl Default for BuildParams {
    fn default() -> Self {
        Self {
            max_gap_s: 180.0,
            vel_consistency_mps: 25.0,
            max_vz_mps: 30.0,
            pos_slack_m: (250.0, 120.0),
        }
    }
}

/// Indices of the longest chain of points (time-sorted) in which every consecutive pair is
/// physically consistent: horizontal distance <= v_lim * dt + slack and height change <=
/// `max_vz_mps` * dt + slack, with v_lim from the leg's median reported ground speed. Junk
/// interleaved with a real track (even several junk points in a row) cannot form the longer
/// chain, so it drops out; a clean leg keeps every point.
fn consistent_chain(pts: &[fdop_types::store::TrackPoint], bp: &BuildParams) -> Vec<usize> {
    let n = pts.len();
    if n < 3 {
        return (0..n).collect();
    }
    let mut gs: Vec<f64> = pts
        .iter()
        .filter(|p| p.gs_mps.is_finite() && p.gs_mps > 0.0)
        .map(|p| p.gs_mps as f64)
        .collect();
    let v_lim = if gs.is_empty() {
        200.0
    } else {
        gs.sort_by(f64::total_cmp);
        let med = gs[gs.len() / 2];
        (1.5 * med + 30.0).max(80.0)
    };
    // horizontal position at the leg's mean height (ENU around the first point)
    let enu = Enu::new(pts[0].lat, pts[0].lon, 0.0);
    let xy: Vec<[f64; 2]> = pts
        .iter()
        .map(|p| {
            let e = enu.from_ecef(geodetic_to_ecef(p.lat, p.lon, 0.0));
            [e[0], e[1]]
        })
        .collect();
    let ok = |i: usize, j: usize| -> bool {
        let dt = pts[j].t - pts[i].t;
        let dh = ((xy[j][0] - xy[i][0]).powi(2) + (xy[j][1] - xy[i][1]).powi(2)).sqrt();
        let dz = (pts[j].alt_m - pts[i].alt_m).abs() as f64;
        dh <= v_lim * dt + bp.pos_slack_m.0 && dz <= bp.max_vz_mps * dt + bp.pos_slack_m.1
    };
    let mut best = vec![1usize; n];
    let mut prev = vec![usize::MAX; n];
    let mut prefix_best = vec![1usize; n];
    for j in 1..n {
        // A burst of bad reports must not hide an earlier valid predecessor. Bound the
        // window by elapsed time, not sample count. The prefix maximum is an exact upper
        // bound on every remaining chain; it makes clean tracks stop after their immediate
        // predecessor without sacrificing a longer chain across arbitrarily dense junk.
        for i in (0..j).rev() {
            if pts[j].t - pts[i].t > bp.max_gap_s || prefix_best[i] < best[j] {
                break;
            }
            if best[i] + 1 > best[j] && ok(i, j) {
                best[j] = best[i] + 1;
                prev[j] = i;
            }
        }
        prefix_best[j] = prefix_best[j - 1].max(best[j]);
    }
    let mut k = (0..n).max_by_key(|&k| (best[k], k)).unwrap_or(0);
    let mut chain = vec![k];
    while prev[k] != usize::MAX {
        k = prev[k];
        chain.push(k);
    }
    chain.reverse();
    chain
}

fn fd_velocity(t: &[f64], p: &[V3]) -> Vec<V3> {
    let n = t.len();
    let mut v = vec![[0.0; 3]; n];
    if n < 2 {
        return v;
    }
    v[0] = scale(sub(p[1], p[0]), 1.0 / (t[1] - t[0]));
    v[n - 1] = scale(sub(p[n - 1], p[n - 2]), 1.0 / (t[n - 1] - t[n - 2]));
    for i in 1..n - 1 {
        let (h0, h1) = (t[i] - t[i - 1], t[i + 1] - t[i]);
        for k in 0..3 {
            v[i][k] = (h0 * h0 * p[i + 1][k] - h1 * h1 * p[i - 1][k]
                + (h1 * h1 - h0 * h0) * p[i][k])
                / (h0 * h1 * (h0 + h1));
        }
    }
    v
}

/// A secant measures the average velocity along an arc, not its endpoint tangent. On a
/// constant-speed, constant-rate turn, the chord from knot `k` to a neighbour runs along the
/// middle heading, at `gs * sin(a/2)/(a/2)` for a turn through angle `a`. So the chord and the
/// neighbour's reported heading alone predict the tangent at `k`: half the turn lies between the
/// chord and the neighbour, and the other half on the far side of the chord. Nothing in the
/// prediction comes from the report under test. Bending it toward `k`'s own heading would excuse
/// half of a wrong heading, and a bogus report must not gain tolerance by implying a turn.
///
/// One chord that agrees is enough. A turn that starts or ends between two knots breaks the
/// constant-rate assumption on that side only, so requiring both would discard correct reports
/// wherever an aircraft rolls in or out. In steady flight and steady turns both chords predict
/// the same tangent, so a wrong report fails either way.
fn turn_consistent(
    pts: &[fdop_types::store::TrackPoint],
    pos: &[V3],
    k: usize,
    enu: &Enu,
    vu: f64,
    tolerance: f64,
) -> bool {
    let p = &pts[k];
    if p.gs_mps <= 0.0 {
        return false;
    }
    let (gs, heading) = (p.gs_mps as f64, (p.track_deg as f64).to_radians());
    let reported = [gs * heading.sin(), gs * heading.cos(), vu];
    [k.checked_sub(1), (k + 1 < pts.len()).then_some(k + 1)]
        .into_iter()
        .flatten()
        .any(|j| {
            let dt = pts[j].t - p.t;
            if dt.abs() >= 30.0 || !pts[j].track_deg.is_finite() {
                return false;
            }
            // average velocity between the two knots, in flight direction for either neighbour
            let chord = enu.vec_from_ecef(scale(sub(pos[j], pos[k]), 1.0 / dt));
            let chord_heading = chord[0].atan2(chord[1]);
            // the part of the turn between the chord and the neighbour
            let half_turn = ((pts[j].track_deg as f64).to_radians() - chord_heading
                + std::f64::consts::PI)
                .rem_euclid(std::f64::consts::TAU)
                - std::f64::consts::PI;
            // more than a half circle within 30 s is not a turn this model can vouch for
            if half_turn.abs() >= std::f64::consts::FRAC_PI_2 {
                return false;
            }
            let speed = chord[0].hypot(chord[1])
                * if half_turn.abs() < 1e-8 {
                    1.0
                } else {
                    half_turn / half_turn.sin()
                };
            let tangent_heading = chord_heading - half_turn;
            let expected = [
                speed * tangent_heading.sin(),
                speed * tangent_heading.cos(),
                chord[2],
            ];
            norm(sub(reported, expected)) <= tolerance
        })
}

/// Build track models from one stored leg (possibly several, split at gaps).
pub fn build(leg: &FlightLeg, bp: &BuildParams) -> Vec<TrackModel> {
    let mut pts: Vec<_> = leg
        .points
        .iter()
        .filter(|p| {
            p.t.is_finite() && p.lat.is_finite() && p.lon.is_finite() && p.alt_m.is_finite()
        })
        .copied()
        .collect();
    pts.sort_by(|a, b| a.t.total_cmp(&b.t));
    pts.dedup_by(|b, a| (b.t - a.t).abs() < 1e-3);
    let mut out = Vec::new();
    let mut start = 0;
    let mut part = 0;
    for i in 1..=pts.len() {
        if i == pts.len() || pts[i].t - pts[i - 1].t > bp.max_gap_s {
            let keep = consistent_chain(&pts[start..i], bp);
            let seg_owned: Vec<_> = keep.iter().map(|&k| pts[start + k]).collect();
            let seg = &seg_owned[..];
            if seg.len() >= 2 {
                let t: Vec<f64> = seg.iter().map(|p| p.t).collect();
                let pos: Vec<V3> = seg
                    .iter()
                    .map(|p| geodetic_to_ecef(p.lat, p.lon, p.alt_m as f64))
                    .collect();
                let fd = fd_velocity(&t, &pos);
                let mut vel = fd.clone();
                let mut n_rep = 0usize;
                for (k, p) in seg.iter().enumerate() {
                    if p.gs_mps.is_finite() && p.track_deg.is_finite() {
                        let (gs, trk) = (p.gs_mps as f64, (p.track_deg as f64).to_radians());
                        let enu = Enu::new(p.lat, p.lon, 0.0);
                        // vertical: reported rate, else finite difference
                        let fd_enu = enu.vec_from_ecef(fd[k]);
                        let vu = if p.vrate_mps.is_finite() {
                            p.vrate_mps as f64
                        } else {
                            fd_enu[2]
                        };
                        let rep = enu.vec_to_ecef([gs * trk.sin(), gs * trk.cos(), vu]);
                        // consistency (only meaningful when neighbours are close in time)
                        let near = (k > 0 && t[k] - t[k - 1] < 30.0)
                            || (k + 1 < t.len() && t[k + 1] - t[k] < 30.0);
                        if !near
                            || norm(sub(rep, fd[k])) <= bp.vel_consistency_mps
                            || turn_consistent(seg, &pos, k, &enu, vu, bp.vel_consistency_mps)
                        {
                            vel[k] = rep;
                            n_rep += 1;
                        }
                    }
                }
                let key = if part == 0 && i == pts.len() && start == 0 {
                    leg.key.clone()
                } else {
                    format!("{}#{}", leg.key, part)
                };
                let mlat = seg.iter().filter(|p| p.flags & pt_flags::MLAT != 0).count();
                out.push(TrackModel {
                    key,
                    icao: leg.icao.clone(),
                    reg: leg.reg.clone(),
                    typ: leg.typ.clone(),
                    engine: leg.engine,
                    source: leg.source.clone(),
                    reported_vel_frac: n_rep as f64 / seg.len() as f64,
                    mlat_frac: mlat as f64 / seg.len() as f64,
                    t,
                    pos,
                    vel,
                });
                part += 1;
            }
            start = i;
        }
    }
    out
}

impl TrackModel {
    pub fn t0(&self) -> f64 {
        self.t[0]
    }
    pub fn t1(&self) -> f64 {
        self.t[self.t.len() - 1]
    }

    #[inline]
    fn seg(&self, t: f64) -> usize {
        let n = self.t.len();
        let k = self.t.partition_point(|&x| x <= t);
        k.clamp(1, n - 1) - 1
    }

    /// `seg(t)` for a time usually in interval `hint` or the next one, as along a time sweep.
    #[inline]
    fn seg_near(&self, t: f64, hint: usize) -> usize {
        let n = self.t.len();
        for i in [hint, hint + 1] {
            if i + 1 < n && self.t[i] <= t && (t < self.t[i + 1] || i + 2 == n) {
                return i;
            }
        }
        self.seg(t)
    }

    /// Position and velocity (ECEF) at time t (clamped to the track span).
    #[inline]
    pub fn pv(&self, t: f64) -> (V3, V3) {
        let t = t.clamp(self.t0(), self.t1());
        self.pv_in(self.seg(t), t)
    }

    /// `pv` at a time already within the track span, in its interval `i`.
    #[inline]
    fn pv_in(&self, i: usize, t: f64) -> (V3, V3) {
        let (ta, tb) = (self.t[i], self.t[i + 1]);
        let h = tb - ta;
        let s = (t - ta) / h;
        let (s2, s3) = (s * s, s * s * s);
        let h00 = 2.0 * s3 - 3.0 * s2 + 1.0;
        let h10 = s3 - 2.0 * s2 + s;
        let h01 = -2.0 * s3 + 3.0 * s2;
        let h11 = s3 - s2;
        let d00 = (6.0 * s2 - 6.0 * s) / h;
        let d10 = 3.0 * s2 - 4.0 * s + 1.0;
        let d01 = (-6.0 * s2 + 6.0 * s) / h;
        let d11 = 3.0 * s2 - 2.0 * s;
        let (pa, pb, va, vb) = (self.pos[i], self.pos[i + 1], self.vel[i], self.vel[i + 1]);
        let mut p = [0.0; 3];
        let mut v = [0.0; 3];
        for k in 0..3 {
            p[k] = h00 * pa[k] + h10 * h * va[k] + h01 * pb[k] + h11 * h * vb[k];
            v[k] = d00 * pa[k] + d10 * va[k] + d01 * pb[k] + d11 * vb[k];
        }
        (p, v)
    }

    pub fn pos_at(&self, t: f64) -> V3 {
        self.pv(t).0
    }

    /// Length (s) of the sample interval containing t.
    pub fn gap_at(&self, t: f64) -> f64 {
        if t < self.t0() || t > self.t1() {
            return f64::INFINITY;
        }
        self.gap_of(self.seg(t))
    }

    /// Length (s) of sample interval `i`.
    #[inline]
    pub fn gap_of(&self, i: usize) -> f64 {
        self.t[i + 1] - self.t[i]
    }

    /// Geodetic position (lat, lon, MSL alt) and ENU velocity at time t.
    pub fn geo_at(&self, t: f64) -> ((f64, f64, f64), V3) {
        let (p, v) = self.pv(t);
        let (la, lo, h) = ecef_to_geodetic(p);
        let enu = Enu::new(la, lo, 0.0);
        ((la, lo, h), enu.vec_from_ecef(v))
    }

    /// Solve the emission time for reception time `t_r` at observer `x`:
    /// g(t_e) = t_e + |p(t_e) - x|/c - t_r = 0, Newton with g' = 1 + v_r/c.
    /// Returns (t_e, range, radial velocity) or None outside the track span.
    #[inline]
    pub fn solve_emission(
        &self,
        t_r: f64,
        x: V3,
        c: f64,
        warm: Option<f64>,
    ) -> Option<(f64, f64, f64)> {
        let (t0, t1) = (self.t0(), self.t1());
        let mut te = match warm {
            Some(w) => w.clamp(t0, t1),
            None => {
                let p = self.pos_at(t_r.clamp(t0, t1));
                (t_r - norm(sub(p, x)) / c).clamp(t0, t1)
            }
        };
        for _ in 0..30 {
            let (p, v) = self.pv(te);
            let rv = sub(p, x);
            let r = norm(rv);
            let vr = dot(rv, v) / r;
            let g = te + r / c - t_r;
            let te_new = (te - g / (1.0 + vr / c)).clamp(t0, t1);
            let done = (te_new - te).abs() < 1e-7;
            te = te_new;
            if done {
                break;
            }
        }
        let (p, v) = self.pv(te);
        let rv = sub(p, x);
        let r = norm(rv);
        let vr = dot(rv, v) / r;
        let g = te + r / c - t_r;
        (g.abs() < 1e-3 && te > t0 && te < t1).then_some((te, r, vr))
    }

    /// Emission time for reception at `t_r` through medium `m`: (t_e, range, f/f0 ratio).
    pub fn solve_emission_p(
        &self,
        t_r: f64,
        x: V3,
        m: &Prop,
        warm: Option<f64>,
    ) -> Option<(f64, f64, f64)> {
        self.solve_emission_sweep(t_r, x, m, warm, &mut 0, 1e-7)
    }

    /// `solve_emission_p` for a sweep of reception times: `hint` carries the sample interval from
    /// one solve to the next and ends in the interval of a returned t_e, and Newton stops once a
    /// step is shorter than `tol` (s).
    #[inline]
    pub fn solve_emission_sweep(
        &self,
        t_r: f64,
        x: V3,
        m: &Prop,
        warm: Option<f64>,
        hint: &mut usize,
        tol: f64,
    ) -> Option<(f64, f64, f64)> {
        let (t0, t1) = (self.t0(), self.t1());
        let mut te = match warm {
            Some(w) => w.clamp(t0, t1),
            None => {
                let t = t_r.clamp(t0, t1);
                *hint = self.seg_near(t, *hint);
                let (p, _) = self.pv_in(*hint, t);
                let rv = sub(p, x);
                (t_r - m.travel(rv, norm(rv))).clamp(t0, t1)
            }
        };
        for _ in 0..30 {
            *hint = self.seg_near(te, *hint);
            let (p, v) = self.pv_in(*hint, te);
            let rv = sub(p, x);
            let r = norm(rv);
            let (travel, rate) = m.travel_and_rate(rv, r, v);
            let g = te + travel - t_r;
            let received_rate = 1.0 + rate;
            if !g.is_finite() || !received_rate.is_finite() || received_rate <= 0.0 {
                return None;
            }
            let te_new = (te - g / received_rate).clamp(t0, t1);
            let done = (te_new - te).abs() < tol;
            te = te_new;
            if done {
                break;
            }
        }
        *hint = self.seg_near(te, *hint);
        let (p, v) = self.pv_in(*hint, te);
        let rv = sub(p, x);
        let r = norm(rv);
        let (travel, rate) = m.travel_and_rate(rv, r, v);
        let g = te + travel - t_r;
        let ratio = 1.0 / (1.0 + rate);
        (g.abs() < 1e-3 && te > t0 && te < t1 && ratio.is_finite() && ratio > 0.0)
            .then_some((te, r, ratio))
    }

    /// Closest approach to `x` near `t_guess` (emission time): (t_e, d, speed, interior).
    pub fn cpa_near(&self, x: V3, t_guess: f64, half: f64) -> Option<Cpa> {
        let lo = (t_guess - half).max(self.t0());
        let hi = (t_guess + half).min(self.t1());
        if hi <= lo {
            return None;
        }
        let n = (((hi - lo) / 2.0).ceil() as usize).max(4);
        let dist2 = |tt: f64| {
            let d = sub(self.pos_at(tt), x);
            dot(d, d)
        };
        let (mut kb, mut db) = (0usize, f64::INFINITY);
        for k in 0..=n {
            let tt = lo + (hi - lo) * k as f64 / n as f64;
            let d2 = dist2(tt);
            if d2 < db {
                db = d2;
                kb = k;
            }
        }
        let step = (hi - lo) / n as f64;
        let (mut a, mut b) = (
            lo + step * kb.saturating_sub(1) as f64,
            (lo + step * (kb + 1) as f64).min(hi),
        );
        // golden-section to 1 ms
        let gr = 0.618_033_988_749_895;
        let mut c1 = b - gr * (b - a);
        let mut c2 = a + gr * (b - a);
        let (mut f1, mut f2) = (dist2(c1), dist2(c2));
        while b - a > 1e-3 {
            if f1 < f2 {
                b = c2;
                c2 = c1;
                f2 = f1;
                c1 = b - gr * (b - a);
                f1 = dist2(c1);
            } else {
                a = c1;
                c1 = c2;
                f1 = f2;
                c2 = a + gr * (b - a);
                f2 = dist2(c2);
            }
        }
        let te = 0.5 * (a + b);
        let (p, v) = self.pv(te);
        let d = norm(sub(p, x));
        let interior =
            te > self.t0() + 0.5 && te < self.t1() - 0.5 && te > lo + 0.5 && te < hi - 0.5;
        Some(Cpa {
            t_e: te,
            d,
            speed: norm(v),
            interior,
            rv: sub(p, x),
            vel: v,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Cpa {
    pub t_e: f64,
    pub d: f64,
    pub speed: f64,
    pub interior: bool,
    /// Source minus observer at CPA (ECEF, m) and source velocity (ECEF, m/s).
    pub rv: V3,
    pub vel: V3,
}

impl Cpa {
    /// Heard time in still air of speed `c`.
    pub fn t_r(&self, c: f64) -> f64 {
        self.t_e + self.d / c
    }
    /// Heard time through a moving medium.
    pub fn t_r_p(&self, m: &Prop) -> f64 {
        self.t_e + m.travel(self.rv, self.d)
    }
}

/// Homogeneous propagation medium in ECEF, using path-averaged sound speed and wind
/// (see `fdop_types::atmo` for the averaging model). Requires positive sound speed and
/// subsonic wind; it does not model ray bending through a varying atmosphere.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Prop {
    pub c: f64,
    pub w: V3,
}

impl Prop {
    pub fn still(c: f64) -> Self {
        Self { c, w: [0.0; 3] }
    }
    /// From a path average expressed in the observer's local frame.
    pub fn from_medium(m: &fdop_types::atmo::Medium, obs: &Enu) -> Self {
        Self {
            c: m.c,
            w: obs.vec_to_ecef([m.wind_e, m.wind_n, 0.0]),
        }
    }
    /// Travel time for source-minus-observer vector `rv` of length `r`.
    /// Returns NaN for a nonpositive sound speed or wind at/above the sound speed.
    #[inline]
    pub fn travel(&self, rv: V3, r: f64) -> f64 {
        self.travel_root(rv, r).0
    }
    /// Received/emitted frequency ratio for source velocity `v`.
    #[inline]
    pub fn ratio(&self, rv: V3, r: f64, v: V3) -> f64 {
        1.0 / (1.0 + self.travel_and_rate(rv, r, v).1)
    }
    /// Positive root T of |rv + w T| = c T, and q = (c² - w²) T - w.rv.
    /// The wavefront is carried by the wind; projecting wind only onto `rv` misses the
    /// crosswind displacement. Rationalise the root for negative w.rv to avoid cancellation.
    #[inline]
    fn travel_root(&self, rv: V3, r: f64) -> (f64, f64) {
        let a = self.c * self.c - dot(self.w, self.w);
        if !(self.c > 0.0 && a > 0.0 && a.is_finite() && r >= 0.0 && r.is_finite()) {
            return (f64::NAN, f64::NAN);
        }
        if self.w == [0.0; 3] {
            return (r / self.c, self.c * r);
        }
        let b = dot(self.w, rv);
        let q = (b * b + a * r * r).sqrt();
        let travel = if b >= 0.0 {
            (b + q) / a
        } else {
            r * r / (q - b)
        };
        (travel, q)
    }
    /// Travel time and its derivative with respect to source emission time. Differentiating
    /// the same wavefront equation keeps the Newton step and the Doppler ratio consistent,
    /// and shares the root between them. At coincident source/observer the derivative is undefined.
    #[inline]
    fn travel_and_rate(&self, rv: V3, r: f64, v: V3) -> (f64, f64) {
        let (travel, q) = self.travel_root(rv, r);
        (travel, (dot(rv, v) + dot(self.w, v) * travel) / q)
    }
    /// First-order scale for the coarse gates on speeds/distances fitted in still air at
    /// `c_fit`: (c, v, d) -> lambda (c, v, d) leaves a still-air curve invariant, and the
    /// leading wind correction adds v.w/c to the effective c. This is an approximation;
    /// the scored predictions use the full homogeneous-wind travel time and Doppler ratio.
    #[inline]
    pub fn fit_scale(&self, v: V3, c_fit: f64) -> f64 {
        (self.c + dot(v, self.w) / self.c) / c_fit
    }
}

/// Straight constant-velocity synthetic track (tests and self-checks).
pub fn straight_track(
    enu: &Enu,
    p0: V3,
    vel_enu: V3,
    t0: f64,
    t_from: f64,
    t_to: f64,
    step: f64,
) -> TrackModel {
    let mut t = Vec::new();
    let mut pos = Vec::new();
    let mut vel = Vec::new();
    let mut tt = t_from;
    let v_ecef = enu.vec_to_ecef(vel_enu);
    while tt <= t_to + 1e-9 {
        let e = add(p0, scale(vel_enu, tt - t0));
        pos.push(enu.to_ecef(e));
        vel.push(v_ecef);
        t.push(tt);
        tt += step;
    }
    TrackModel {
        key: "synthetic".into(),
        icao: "000000".into(),
        reg: String::new(),
        typ: "PC12".into(),
        engine: EngineClass::Turboprop,
        source: "synthetic".into(),
        t,
        pos,
        vel,
        reported_vel_frac: 1.0,
        mlat_frac: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::cv_curve;

    // Independent oracle: advance the spherical wavefront, whose centre moves at the wind
    // velocity, until it reaches the observer. No quadratic root or Doppler formula is used.
    fn wavefront_travel(rv: V3, w: V3, c: f64) -> f64 {
        let (mut lo, mut hi) = (0.0, norm(rv) / (c - norm(w)));
        for _ in 0..80 {
            let t = 0.5 * (lo + hi);
            if norm(add(rv, scale(w, t))) > c * t {
                lo = t;
            } else {
                hi = t;
            }
        }
        0.5 * (lo + hi)
    }

    #[test]
    fn moving_medium_travel_reaches_the_advected_wavefront() {
        let rotate = |v: V3| [v[2], v[0], v[1]];
        for c in [300.0, 343.0, 360.0] {
            for rv in [
                [4000.0, 0.0, 1200.0],
                [-4000.0, 0.0, 1200.0],
                [0.0, 5000.0, 1200.0],
                [0.0, 0.0, 3000.0],
            ] {
                for w in [
                    [0.0, 0.0, 0.0],
                    [25.0, 0.0, 0.0],
                    [-25.0, 0.0, 0.0],
                    [0.0, 35.0, 0.0],
                    [10.0, -20.0, 5.0],
                ] {
                    let m = Prop { c, w };
                    let r = norm(rv);
                    let travel = m.travel(rv, r);
                    assert!((travel - wavefront_travel(rv, w, c)).abs() < 1e-11);
                    assert!((norm(add(rv, scale(w, travel))) - c * travel).abs() < 1e-8);
                    assert!(
                        (Prop { c, w: rotate(w) }.travel(rotate(rv), r) - travel).abs() < 1e-11
                    );
                }
            }
        }
        // Nearly sonic following wind stresses cancellation in the positive quadratic root.
        let c = 343.0;
        let w = c * (1.0 - 1e-12);
        let travel = Prop {
            c,
            w: [w, 0.0, 0.0],
        }
        .travel([-3000.0, 0.0, 0.0], 3000.0);
        assert!((travel - 3000.0 / (c + w)).abs() < 1e-12);
    }

    #[test]
    fn moving_medium_doppler_matches_the_one_dimensional_solution() {
        let c = 343.0;
        for side in [-1.0, 1.0] {
            for wind in [-30.0, 0.0, 30.0] {
                let m = Prop {
                    c,
                    w: [wind, 0.0, 0.0],
                };
                // A pulse towards the observer travels at c - side*wind in the ground frame.
                let pulse_speed = c - side * wind;
                let rv = [side * 3000.0, 0.0, 0.0];
                assert!((m.travel(rv, 3000.0) - 3000.0 / pulse_speed).abs() < 1e-12);
                for speed in [-140.0, 0.0, 140.0] {
                    let expected = pulse_speed / (pulse_speed + side * speed);
                    assert!(
                        (m.ratio(rv, 3000.0, [speed, 0.0, 0.0]) - expected).abs() < 1e-14,
                        "side={side} wind={wind} speed={speed}"
                    );
                }
            }
        }
    }

    #[test]
    fn moving_medium_doppler_is_the_reception_time_derivative() {
        let c = 343.0;
        let v = [140.0, -35.0, 3.0];
        let dt = 1e-3;
        for rv in [
            [4000.0, 0.0, 1200.0],
            [-4000.0, 0.0, 1200.0],
            [0.0, 5000.0, 1200.0],
        ] {
            for w in [[0.0; 3], [25.0, 0.0, 0.0], [-20.0, 30.0, 5.0]] {
                let travel_before = wavefront_travel(sub(rv, scale(v, dt)), w, c);
                let travel_after = wavefront_travel(add(rv, scale(v, dt)), w, c);
                let received_rate = 1.0 + (travel_after - travel_before) / (2.0 * dt);
                let ratio = Prop { c, w }.ratio(rv, norm(rv), v);
                assert!(
                    (ratio - 1.0 / received_rate).abs() < 1e-8,
                    "rv={rv:?} w={w:?}: ratio={ratio} received_rate={received_rate}"
                );
            }
        }
    }

    #[test]
    fn moving_medium_emission_solves_recover_known_emission_times() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let v = [140.0, 0.0, 0.0];
        let tr = straight_track(&enu, [0.0, 2000.0, 1200.0], v, 1000.0, 800.0, 1200.0, 20.0);
        let x = enu.to_ecef([0.0; 3]);
        let c = 343.0;
        for w in [[0.0; 3], [25.0, -15.0, 5.0], [-60.0, 25.0, 0.0]] {
            let m = Prop {
                c,
                w: enu.vec_to_ecef(w),
            };
            for te in [850.0, 900.0, 975.0, 1000.0, 1050.0, 1150.0] {
                let rv = [v[0] * (te - 1000.0), 2000.0, 1200.0];
                let t_r = te + wavefront_travel(rv, w, c);
                let dt = 1e-3;
                let expected_ratio = 1.0
                    / (1.0
                        + (wavefront_travel(add(rv, scale(v, dt)), w, c)
                            - wavefront_travel(sub(rv, scale(v, dt)), w, c))
                            / (2.0 * dt));
                for warm in [None, Some(tr.t0()), Some(tr.t1()), Some(te + 20.0)] {
                    let (solved, r, ratio) = tr.solve_emission_p(t_r, x, &m, warm).unwrap();
                    assert!((solved - te).abs() < 1e-7, "{solved} vs {te}, wind={w:?}");
                    assert!((r - norm(rv)).abs() < 1e-5);
                    assert!((ratio - expected_ratio).abs() < 1e-8);
                }
            }
        }
    }

    #[test]
    fn invalid_propagation_media_cannot_produce_an_emission_match() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let tr = straight_track(
            &enu,
            [0.0, 2000.0, 1200.0],
            [140.0, 0.0, 0.0],
            1000.0,
            800.0,
            1200.0,
            20.0,
        );
        let x = enu.to_ecef([0.0; 3]);
        let rv = sub(tr.pos_at(1000.0), x);
        for m in [
            Prop::still(0.0),
            Prop::still(-343.0),
            Prop::still(f64::NAN),
            Prop {
                c: 343.0,
                w: [343.0, 0.0, 0.0],
            },
            Prop {
                c: 343.0,
                w: [400.0, 0.0, 0.0],
            },
            Prop {
                c: 343.0,
                w: [f64::NAN, 0.0, 0.0],
            },
        ] {
            assert!(m.travel(rv, norm(rv)).is_nan(), "{m:?}");
            assert!(m.ratio(rv, norm(rv), tr.vel[0]).is_nan(), "{m:?}");
            assert!(tr.solve_emission_p(1010.0, x, &m, None).is_none(), "{m:?}");
        }
    }

    // Analytic, level circular motion in one tangent plane. Reported velocities are rotated
    // into each sample's own ENU frame, just as actual ADS-B ground tracks are defined.
    fn turn_state(enu: &Enu, t: f64, omega: f64) -> (V3, V3) {
        let angle = omega * t + 60.0_f64.to_radians();
        let radius = 100.0 / omega;
        (
            enu.to_ecef([radius * angle.sin(), radius * (1.0 - angle.cos()), 1200.0]),
            enu.vec_to_ecef([100.0 * angle.cos(), 100.0 * angle.sin(), 0.0]),
        )
    }

    fn turning_leg(enu: &Enu, times: &[f64], omega: f64) -> FlightLeg {
        leg_from_states(times, |t| turn_state(enu, t, omega))
    }

    // Straight, level flight due east at 100 m/s.
    fn straight_leg(enu: &Enu, times: &[f64]) -> FlightLeg {
        leg_from_states(times, |t| {
            (
                enu.to_ecef([100.0 * t, 0.0, 1200.0]),
                enu.vec_to_ecef([100.0, 0.0, 0.0]),
            )
        })
    }

    fn leg_from_states(times: &[f64], state: impl Fn(f64) -> (V3, V3)) -> FlightLeg {
        let points = times
            .iter()
            .map(|&t| {
                let (p, v) = state(t);
                let (lat, lon, alt) = ecef_to_geodetic(p);
                let [ve, vn, vu] = Enu::new(lat, lon, 0.0).vec_from_ecef(v);
                fdop_types::store::TrackPoint {
                    t: 1000.0 + t,
                    lat,
                    lon,
                    alt_m: alt as f32,
                    gs_mps: ve.hypot(vn) as f32,
                    track_deg: ve.atan2(vn).to_degrees().rem_euclid(360.0) as f32,
                    vrate_mps: vu as f32,
                    alt_src: 0,
                    flags: 0,
                }
            })
            .collect();
        FlightLeg {
            key: "synthetic-turn".into(),
            icao: "000000".into(),
            reg: String::new(),
            typ: "PC12".into(),
            source: "synthetic".into(),
            engine: EngineClass::Turboprop,
            points,
        }
    }

    #[test]
    fn sparse_turns_keep_correct_tangents_and_reconstruct_the_arc() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        // Both turn directions, unequal sampling, endpoints and headings crossing north.
        for degrees_per_second in [-3.0_f64, 3.0] {
            let omega = degrees_per_second.to_radians();
            for times in [
                vec![0.0, 25.0, 50.0, 75.0, 100.0],
                vec![0.0, 29.0, 58.0, 87.0, 116.0],
                vec![0.0, 25.0, 53.0, 77.0, 106.0],
            ] {
                let leg = turning_leg(&enu, &times, omega);
                let models = build(&leg, &BuildParams::default());
                assert_eq!(models.len(), 1);
                let model = &models[0];
                assert_eq!(model.t.len(), times.len());
                assert_eq!(model.reported_vel_frac, 1.0);
                for (&t, &v) in times.iter().zip(&model.vel) {
                    assert!(norm(sub(v, turn_state(&enu, t, omega).1)) < 0.001);
                }
                let mut finite_difference = model.clone();
                finite_difference.vel = fd_velocity(&model.t, &model.pos);
                let (mut arc_error, mut fd_error) = (0.0_f64, 0.0_f64);
                for interval in times.windows(2) {
                    for j in 1..10 {
                        let t = interval[0] + (interval[1] - interval[0]) * j as f64 / 10.0;
                        let truth = turn_state(&enu, t, omega).0;
                        arc_error = arc_error.max(norm(sub(model.pos_at(1000.0 + t), truth)));
                        fd_error =
                            fd_error.max(norm(sub(finite_difference.pos_at(1000.0 + t), truth)));
                    }
                }
                assert!(arc_error < 30.0, "Hermite arc error {arc_error:.3} m");
                assert!(fd_error > 100.0, "FD comparison error {fd_error:.3} m");
            }
        }
    }

    #[test]
    fn turning_does_not_exempt_bad_speed_heading_or_vertical_rate() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let leg = turning_leg(&enu, &[0.0, 25.0, 50.0, 75.0, 100.0], 3.0_f64.to_radians());
        for field in 0..3 {
            let mut corrupt = leg.clone();
            match field {
                0 => corrupt.points[2].gs_mps += 60.0,
                1 => corrupt.points[2].track_deg += 90.0,
                _ => corrupt.points[2].vrate_mps += 60.0,
            }
            let models = build(&corrupt, &BuildParams::default());
            assert_eq!(models.len(), 1);
            let model = &models[0];
            let fd = fd_velocity(&model.t, &model.pos);
            assert_eq!(model.vel[2], fd[2], "corrupt field {field} was accepted");
        }
        let mut missing_headings = leg.clone();
        missing_headings.points[1].track_deg = f32::NAN;
        missing_headings.points[3].track_deg = f32::NAN;
        let models = build(&missing_headings, &BuildParams::default());
        let model = &models[0];
        let fd = fd_velocity(&model.t, &model.pos);
        assert_eq!(model.vel[2], fd[2], "an unobserved turn was assumed");
    }

    #[test]
    fn a_wrong_heading_is_not_half_excused_by_the_turn_check() {
        // 20 degrees at 100 m/s is a 35 m/s tangent error, over the 25 m/s tolerance. A turn
        // check that bends its prediction toward the heading under test sees only half of it.
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let times = [0.0, 25.0, 50.0, 75.0, 100.0];
        let straight = straight_leg(&enu, &times);
        let models = build(&straight, &BuildParams::default());
        assert_eq!(models[0].reported_vel_frac, 1.0);
        for leg in [straight, turning_leg(&enu, &times, 3.0_f64.to_radians())] {
            for error in [-20.0, 20.0] {
                let mut corrupt = leg.clone();
                corrupt.points[2].track_deg += error;
                let models = build(&corrupt, &BuildParams::default());
                assert_eq!(models.len(), 1);
                let model = &models[0];
                let fd = fd_velocity(&model.t, &model.pos);
                assert_eq!(
                    model.vel[2], fd[2],
                    "{error} degree heading error was accepted"
                );
            }
        }
    }

    #[test]
    fn a_turn_starting_or_ending_between_knots_keeps_correct_reports() {
        // At 130 m/s with 29 s between knots, the knots either side of the partial arcs fail the
        // finite difference (about 35 m/s) and the chord over the partial arc (about 49 m/s).
        // Only the chord over the whole arc can vouch for them.
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let (gs, omega, start, stop) = (130.0_f64, 3.0_f64.to_radians(), 43.5_f64, 130.5_f64);
        let radius = gs / omega;
        let times = [0.0, 29.0, 58.0, 87.0, 116.0, 145.0, 174.0, 203.0];
        let leg = leg_from_states(&times, |t| {
            let a = omega * (t.clamp(start, stop) - start);
            let after = (t - stop).max(0.0);
            (
                enu.to_ecef([
                    radius * (1.0 - a.cos()) + gs * after * a.sin(),
                    gs * t.min(start) + radius * a.sin() + gs * after * a.cos(),
                    1200.0,
                ]),
                enu.vec_to_ecef([gs * a.sin(), gs * a.cos(), 0.0]),
            )
        });
        let models = build(&leg, &BuildParams::default());
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].t.len(), times.len());
        assert_eq!(models[0].reported_vel_frac, 1.0);
    }

    #[test]
    fn seg_near_agrees_with_seg_from_any_hint() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let mut tr = straight_track(
            &enu,
            [0.0, 3000.0, 1200.0],
            [100.0, 0.0, 0.0],
            0.0,
            0.0,
            50.0,
            10.0,
        );
        tr.t = vec![0.0, 1.0, 2.5, 7.0, 7.5, 30.0];
        let knots = tr.t.clone();
        let mut times = vec![-1.0, f64::NAN, 31.0];
        times.extend(&knots);
        times.extend(knots.windows(2).map(|w| 0.5 * (w[0] + w[1])));
        for &t in &times {
            for hint in 0..knots.len() + 2 {
                assert_eq!(tr.seg_near(t, hint), tr.seg(t), "t={t} hint={hint}");
            }
        }
    }

    #[test]
    fn an_emission_sweep_matches_independent_solves() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let omega = 3.0_f64.to_radians();
        let times: Vec<f64> = (0..=40).map(|k| k as f64 * 3.0).collect();
        let models = build(&turning_leg(&enu, &times, omega), &BuildParams::default());
        assert_eq!(models.len(), 1);
        let tr = &models[0];
        // Close under a turn in a crosswind, exercising changing geometry and the wind derivative.
        let x = enu.to_ecef([0.0, 0.0, 0.0]);
        let m = Prop {
            c: 343.0,
            w: enu.vec_to_ecef([20.0, -15.0, 0.0]),
        };
        let (mut hint, mut warm) = (0, None);
        let (mut solved, mut refused) = (0, 0);
        for k in 0..1000 {
            let t_r = tr.t0() - 20.0 + k as f64 * 0.2;
            let swept = tr.solve_emission_sweep(t_r, x, &m, warm, &mut hint, 1e-4);
            let cold = tr.solve_emission_p(t_r, x, &m, None);
            assert_eq!(swept.is_some(), cold.is_some(), "t_r={t_r}");
            warm = swept.map(|s| s.0);
            if let (Some(s), Some(c)) = (swept, cold) {
                assert!(
                    (s.0 - c.0).abs() < 1e-5,
                    "t_r={t_r}: t_e {} vs {}",
                    s.0,
                    c.0
                );
                assert!(
                    (s.2 - c.2).abs() < 1e-5,
                    "t_r={t_r}: ratio {} vs {}",
                    s.2,
                    c.2
                );
                assert_eq!(hint, tr.seg(s.0), "t_r={t_r}");
                assert_eq!(tr.gap_of(hint), tr.gap_at(s.0), "t_r={t_r}");
                solved += 1;
            } else {
                refused += 1;
            }
        }
        assert!(
            solved > 500 && refused > 50,
            "{solved} solved, {refused} refused"
        );
        // a solve that `tol` stops right after crossing a knot still ends in the new interval
        let still = Prop::still(343.0);
        let k = tr.t.len() / 2;
        let rv = sub(tr.pos_at(tr.t[k] + 0.05), x);
        let t_r = tr.t[k] + 0.05 + still.travel(rv, norm(rv));
        let mut hint = k - 1;
        let s = tr
            .solve_emission_sweep(t_r, x, &still, Some(tr.t[k] - 0.05), &mut hint, 1.0)
            .unwrap();
        assert!(s.0 > tr.t[k], "{} vs {}", s.0, tr.t[k]);
        assert_eq!(hint, k);
    }

    #[test]
    fn hermite_exact_on_straight_line_and_emission_matches_closed_form() {
        let enu = Enu::new(40.88, -73.52, 0.0);
        let (v, d_h, h) = (130.0, 3000.0, 1200.0);
        // flies east, passes north of the origin observer at t=1000
        let tr = straight_track(
            &enu,
            [0.0, d_h, h],
            [v, 0.0, 0.0],
            1000.0,
            600.0,
            1400.0,
            20.0,
        );
        let x = enu.to_ecef([0.0, 0.0, 0.0]);
        let d = (d_h * d_h + h * h).sqrt();
        let c = 343.0;
        for t_r in [900.0, 1000.0 + d / c, 1100.0] {
            let (te, r, vr) = tr.solve_emission(t_r, x, c, None).unwrap();
            let f_model = 100.0 * c / (c + vr);
            let f_closed = cv_curve(t_r, 100.0, v, d, 1000.0, c);
            assert!(
                (f_model - f_closed).abs() < 2e-3,
                "t_r={t_r}: {f_model} vs {f_closed}"
            );
            assert!((te + r / c - t_r).abs() < 1e-6);
        }
        let cpa = tr.cpa_near(x, 990.0, 120.0).unwrap();
        assert!(
            (cpa.t_e - 1000.0).abs() < 0.01
                && (cpa.d - d).abs() < 0.5
                && (cpa.speed - v).abs() < 1e-6
        );
    }

    /// An archived leg (shape of a real 2026-11-01 P28R) where the genuine level track at
    /// 2,097 m is interleaved with junk: 4,574 m positions (three in a row at one point), a
    /// position 6 km off-track and one at 11 km. Only the genuine points may build the model.
    #[test]
    fn junk_positions_interleaved_in_a_leg_are_dropped() {
        let enu = Enu::new(40.84, -73.52, 0.0);
        let pt = |t: f64, e: f64, n: f64, alt: f32, gs: f32| {
            let (lat, lon, _) = ecef_to_geodetic(enu.to_ecef([e, n, 0.0]));
            fdop_types::store::TrackPoint {
                t,
                lat,
                lon,
                alt_m: alt,
                gs_mps: gs,
                track_deg: 90.0,
                vrate_mps: 0.0,
                alt_src: 0,
                flags: 0,
            }
        };
        // genuine: east at 72 m/s, 4.5 km south of the observer, a point every 6 s
        let mut points: Vec<_> = (0..60)
            .map(|k| {
                let t = 1000.0 + 6.0 * k as f64;
                pt(t, -2000.0 + 72.0 * (t - 1000.0), 0.0, 2097.0, 72.0)
            })
            .collect();
        let n_real = points.len();
        for (t, e, n, alt, gs) in [
            (1007.0, -1500.0, 300.0, 4574.0, 53.0),
            (1009.5, -1200.0, 600.0, 4574.0, 86.0),
            (1011.0, -1000.0, 800.0, 4574.0, 106.0),
            (1100.5, 4600.0, 4400.0, 4574.0, 62.0),
            (1180.2, 9000.0, -6000.0, 4574.0, 172.0),
            (1250.3, 14000.0, 3000.0, 11096.0, 236.0),
        ] {
            points.push(pt(t, e, n, alt, gs));
        }
        points.sort_by(|a, b| a.t.total_cmp(&b.t));
        let leg = FlightLeg {
            key: "a49ba8_test".into(),
            icao: "a49ba8".into(),
            reg: "N39611".into(),
            typ: "P28R".into(),
            source: "prod+mlat".into(),
            engine: EngineClass::Piston,
            points,
        };
        let models = build(&leg, &BuildParams::default());
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].t.len(), n_real, "junk survived the chain filter");
        let x = enu.to_ecef([0.0, 4500.0, 0.0]);
        let closest = (1000..1360)
            .map(|t| norm(sub(models[0].pos_at(t as f64), x)))
            .fold(f64::INFINITY, f64::min);
        let true_d = (4500.0f64.powi(2) + 2097.0f64.powi(2)).sqrt();
        assert!(
            (closest - true_d).abs() < 20.0,
            "closest {closest:.0} m vs {true_d:.0} m"
        );
    }

    #[test]
    fn dense_bad_position_bursts_do_not_replace_the_real_track() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let point = |t: f64, bad: bool| {
            let p = enu.to_ecef([
                100.0 * (t - 1000.0),
                if bad { 30_000.0 } else { 0.0 },
                if bad { 12_000.0 } else { 1200.0 },
            ]);
            let (lat, lon, alt) = ecef_to_geodetic(p);
            fdop_types::store::TrackPoint {
                t,
                lat,
                lon,
                alt_m: alt as f32,
                gs_mps: 100.0,
                track_deg: 90.0,
                vrate_mps: 0.0,
                alt_src: 0,
                flags: if bad { pt_flags::MLAT } else { 0 },
            }
        };
        let genuine: Vec<_> = (0..80)
            .chain(100..180)
            .map(|i| point(1000.0 + i as f64, false))
            .collect();
        for bad_count in [2, 49, 97] {
            let mut points = genuine.clone();
            // Same physical coverage gap, with increasingly frequent corrupt positions.
            // Neither a point-count window nor the denser wrong track may remove the two
            // genuine sections, whose 21 s separation is within the allowed time gap.
            points.extend(
                (0..bad_count)
                    .map(|i| point(1079.1 + 19.0 * i as f64 / (bad_count - 1) as f64, true)),
            );
            let leg = FlightLeg {
                key: "synthetic-burst".into(),
                icao: "000000".into(),
                reg: String::new(),
                typ: "PC12".into(),
                source: "synthetic".into(),
                engine: EngineClass::Turboprop,
                points,
            };
            let models = build(&leg, &BuildParams::default());
            assert_eq!(models.len(), 1, "burst of {bad_count}");
            let model = &models[0];
            assert_eq!(
                model.t,
                genuine.iter().map(|p| p.t).collect::<Vec<_>>(),
                "burst of {bad_count} displaced genuine samples"
            );
            assert_eq!(model.mlat_frac, 0.0);
            assert_eq!(model.gap_at(1090.0), 21.0);
            let truth = enu.to_ecef([9000.0, 0.0, 1200.0]);
            assert!(norm(sub(model.pos_at(1090.0), truth)) < 1.0);
        }
    }
}
