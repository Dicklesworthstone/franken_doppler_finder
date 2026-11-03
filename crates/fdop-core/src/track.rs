//! ADS-B track model: cubic Hermite interpolation of ECEF position using per-sample velocity
//! vectors (velocity-Hermite). Reported ground speed / track / vertical rate are used when
//! present and consistent with the positions; otherwise velocity comes from second-order
//! finite differences. Tracks are split at gaps longer than `max_gap_s` so interpolation never
//! invents geometry across a coverage hole.

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
    /// Reported velocity rejected when it disagrees with finite differences by more than this.
    pub vel_consistency_mps: f64,
}

impl Default for BuildParams {
    fn default() -> Self {
        Self {
            max_gap_s: 180.0,
            vel_consistency_mps: 25.0,
        }
    }
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
            let seg = &pts[start..i];
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
                        if !near || norm(sub(rep, fd[k])) <= bp.vel_consistency_mps {
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

    /// Position and velocity (ECEF) at time t (clamped to the track span).
    #[inline]
    pub fn pv(&self, t: f64) -> (V3, V3) {
        let t = t.clamp(self.t0(), self.t1());
        let i = self.seg(t);
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
        let i = self.seg(t);
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
        let (t0, t1) = (self.t0(), self.t1());
        let mut te = match warm {
            Some(w) => w.clamp(t0, t1),
            None => {
                let p = self.pos_at(t_r.clamp(t0, t1));
                (t_r - norm(sub(p, x)) / m.c).clamp(t0, t1)
            }
        };
        for _ in 0..30 {
            let (p, v) = self.pv(te);
            let rv = sub(p, x);
            let r = norm(rv);
            let vr = dot(rv, v) / r;
            let g = te + m.travel(rv, r) - t_r;
            let te_new = (te - g / (1.0 + vr / m.c)).clamp(t0, t1);
            let done = (te_new - te).abs() < 1e-7;
            te = te_new;
            if done {
                break;
            }
        }
        let (p, v) = self.pv(te);
        let rv = sub(p, x);
        let r = norm(rv);
        let g = te + m.travel(rv, r) - t_r;
        (g.abs() < 1e-3 && te > t0 && te < t1).then(|| (te, r, m.ratio(rv, r, v)))
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

/// Propagation medium in ECEF: path-averaged sound speed and wind (see
/// `fdop_types::atmo` for the model).
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
    #[inline]
    pub fn travel(&self, rv: V3, r: f64) -> f64 {
        r / (self.c - dot(self.w, rv) / r)
    }
    /// Received/emitted frequency ratio for source velocity `v`.
    #[inline]
    pub fn ratio(&self, rv: V3, r: f64, v: V3) -> f64 {
        let vr = dot(rv, v) / r;
        self.c / (self.c + vr + dot(v, self.w) / self.c)
    }
    /// Factor that converts speeds and distances fitted with sound speed `c_fit` (still air)
    /// into true ones for a source moving with `v`: the Doppler curve is invariant under
    /// (c, v, d) -> lambda (c, v, d), and the wind adds v.w/c to the effective c.
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
}
