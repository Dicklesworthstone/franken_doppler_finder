//! Constant-velocity retarded-time Doppler model.
//!
//! Emission time t_e, reception t_r = t_e + r(t_e)/c, tau = t_e - t0e, u = t_r - t0e:
//!   (c^2 - v^2) tau^2 - 2 c^2 u tau + (c^2 u^2 - d^2) = 0
//!   tau = (c^2 u - sqrt(c^2 v^2 u^2 + d^2 (c^2 - v^2))) / (c^2 - v^2)   (root with t_e <= t_r)
//!   v_r = v^2 tau / r (positive = receding),  f = f0 c / (c + v_r)
//! The received (delayed) CPA time is t0r = t0e + d/c.

/// Emission-time offset tau (s) relative to the emission-time CPA t0e.
#[inline]
pub fn retarded_tau(t_r: f64, t0e: f64, v: f64, d: f64, c: f64) -> f64 {
    let u = t_r - t0e;
    let (c2, v2) = (c * c, v * v);
    (c2 * u - (c2 * v2 * u * u + d * d * (c2 - v2)).sqrt()) / (c2 - v2)
}

/// Range and radial velocity at emission offset tau.
#[inline]
pub fn cv_radial(tau: f64, v: f64, d: f64) -> (f64, f64) {
    let r = (d * d + v * v * tau * tau).sqrt();
    (r, v * v * tau / r)
}

/// Received frequency for a constant-velocity straight pass (retarded model).
#[inline]
pub fn cv_curve(t_r: f64, f0: f64, v: f64, d: f64, t0e: f64, c: f64) -> f64 {
    let tau = retarded_tau(t_r, t0e, v, d, c);
    let (_, vr) = cv_radial(tau, v, d);
    f0 * c / (c + vr)
}

/// Frequency ratio f/f0 at reception offset `u_r` from the delayed CPA t0r.
#[inline]
pub fn cv_ratio_rel_t0r(u_r: f64, v: f64, d: f64, c: f64) -> f64 {
    // t_r = t0r + u_r, t0e = t0r - d/c
    let tau = retarded_tau(u_r + d / c, 0.0, v, d, c);
    let (_, vr) = cv_radial(tau, v, d);
    c / (c + vr)
}

/// Readouts of the reference fit tool: approach/recede asymptotes and |df/dt| at CPA.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct Readouts {
    pub f_pre: f64,
    pub f_post: f64,
    pub max_roc: f64,
}

pub fn readouts(f0: f64, v: f64, d: f64, c: f64) -> Readouts {
    Readouts {
        f_pre: f0 * c / (c - v),
        f_post: f0 * c / (c + v),
        max_roc: f0 * v * v / (c * d),
    }
}

/// STFT window (s) that keeps a chirp of rate `roc` (Hz/s) inside about one bin:
/// win ~ 1/sqrt(roc), clamped to [0.25, 2.0].
pub fn window_for_roc(roc: f64) -> f64 {
    if !roc.is_finite() || roc <= 0.0 {
        return 2.0;
    }
    (1.0 / roc.sqrt()).clamp(0.25, 2.0)
}

/// Small-integer tone ratios, independent of which member of a harmonic family is louder.
/// Compare the larger frequency to the smaller so the same relative tolerance applies in
/// both strength orders, including a strong fifth harmonic and its weaker fundamental.
pub(crate) fn harmonic_frequencies(
    a: f64,
    b: f64,
    max_order: u32,
    tolerance: f64,
    include_unison: bool,
) -> bool {
    if !(a.is_finite() && b.is_finite() && a > 0.0 && b > 0.0) {
        return false;
    }
    let ratio = a.max(b) / a.min(b);
    (1..=max_order).any(|m| {
        (1..=m.min(4)).any(|q| {
            (include_unison || m != q) && (ratio / (m as f64 / q as f64) - 1.0).abs() < tolerance
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const C: f64 = 343.0;

    #[test]
    fn closed_form_satisfies_light_time_equation() {
        let (v, d, t0e) = (120.0, 2500.0, 100.0);
        for t_r in [-200.0, 0.0, 100.0, 107.3, 300.0] {
            let tau = retarded_tau(t_r, t0e, v, d, C);
            let r = (d * d + v * v * tau * tau).sqrt();
            assert!((t0e + tau + r / C - t_r).abs() < 1e-9, "t_r={t_r}");
        }
    }

    #[test]
    fn asymptotes_and_cpa() {
        let (f0, v, d) = (110.0, 130.0, 4000.0);
        let early = cv_curve(-1e5, f0, v, d, 0.0, C);
        let late = cv_curve(1e5, f0, v, d, 0.0, C);
        let ro = readouts(f0, v, d, C);
        assert!((early - ro.f_pre).abs() < 0.05 && (late - ro.f_post).abs() < 0.05);
        // at the delayed CPA the received frequency equals f0
        assert!((cv_curve(d / C, f0, v, d, 0.0, C) - f0).abs() < 1e-9);
        assert!((cv_ratio_rel_t0r(0.0, v, d, C) - 1.0).abs() < 1e-12);
    }
}
