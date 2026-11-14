//! ADS-B track model: cubic Hermite interpolation of ECEF position using per-sample velocity
//! vectors (velocity-Hermite). Reported ground speed / track / vertical rate are used when
//! present and consistent with the positions; otherwise velocity comes from second-order
//! finite differences. If one motion component fails, an independently supported horizontal
//! or vertical component can still replace its finite difference. Tracks are split at gaps
//! longer than `max_gap_s` so interpolation never invents geometry across a coverage hole.
//! The longest physically consistent chain anchors each
//! raw piece: archived legs can interleave junk positions (MLAT mis-solutions, another aircraft's
//! position under the same address) that interpolation would otherwise fly through. Supported
//! chains outside the winner's time coverage can survive as separate models when the same
//! physical limits connect their endpoints. Overlapping alternatives stay rejected. Points without
//! GNSS height that GNSS-height points bracket closely are dropped first (`drop_bracketed_non_gnss`).

use fdop_types::engine::EngineClass;
use fdop_types::geo::{Enu, V3, add, dot, ecef_to_geodetic, geodetic_to_ecef, norm, scale, sub};
use fdop_types::store::{FlightLeg, alt_src, pt_flags};

#[derive(Clone, Debug)]
pub struct TrackModel {
    /// Original leg/split key; cleaning adds `#recoveredN`, corrected-height recovery `#correctedN`.
    pub key: String,
    pub icao: String,
    pub reg: String,
    pub typ: String,
    pub engine: EngineClass,
    pub source: String,
    pub t: Vec<f64>,
    pub pos: Vec<V3>,
    pub vel: Vec<V3>,
    /// Share of samples accepted by the full reported gs/track check (with reported vertical
    /// rate when available). Partial component recovery does not add report credit.
    pub reported_vel_frac: f64,
    pub mlat_frac: f64,
}

/// Conservative ECEF box of a complete interpolated track, including excursions between knots.
/// Construct once with [`TrackModel::bounds`], then reuse it to rule out distant observers before
/// solving closest approaches. A box hit only permits the exact geometry check; it proves no pass.
#[derive(Clone, Copy, Debug)]
pub struct TrackBounds {
    lo: V3,
    hi: V3,
}

impl TrackBounds {
    /// Whether any part of the track could be within `distance_m` of ECEF position `x`.
    /// Unknown inputs fail open. Round the lower distance down and the radius up so a grazing
    /// pass cannot be lost to subtraction, squaring, accumulation or the final distance's sqrt.
    pub fn may_reach(&self, x: V3, distance_m: f64) -> bool {
        if !distance_m.is_finite() || distance_m < 0.0 || x.iter().any(|v| !v.is_finite()) {
            return true;
        }
        let mut distance2 = 0.0;
        for (k, &v) in x.iter().enumerate() {
            let gap = (self.lo[k] - v).max(v - self.hi[k]).max(0.0);
            let lower = gap.next_down().max(0.0);
            distance2 = (distance2 + (lower * lower).next_down().max(0.0))
                .next_down()
                .max(0.0);
        }
        let radius = distance_m.next_up();
        distance2 <= (radius * radius).next_up()
    }
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
    /// Widest stretch between two GNSS-height points across which a point without GNSS height
    /// is dropped (s); see `drop_bracketed_non_gnss`. Zero keeps every point.
    pub gnss_bridge_s: f64,
}

impl Default for BuildParams {
    fn default() -> Self {
        Self {
            max_gap_s: 180.0,
            vel_consistency_mps: 25.0,
            max_vz_mps: 30.0,
            pos_slack_m: (250.0, 120.0),
            gnss_bridge_s: 60.0,
        }
    }
}

/// Drops each point without GNSS height that two GNSS-height points bracket within
/// `max_span_s`.
///
/// Archived legs interleave points of another quality with the aircraft's own reports. On the
/// legs checked against the raw `prod` archive, the points absent from it were the ones without
/// GNSS height, likely from the merged `mlatonly` feed (inferred from that absence: the archive
/// sets no MLAT flag on them). On one day's legs they lie a median 89 m and a 90th percentile
/// 417 m from the line through the GNSS points. That is inside `pos_slack_m`, so the chain keeps
/// most of them, and a Hermite knot on one turns the offset into tens of m/s of false velocity:
/// several Hz of false Doppler, against a trace good to a few tenths of a Hz.
///
/// Dropping a point is not free, because the spline must then cross the whole stretch. On a
/// sustained 3 deg/s turn, the steepest routine manoeuvre, that costs less than keeping the noisy
/// points up to 60 s and more by 90 s (`the_bridge_stops_where_a_turn_costs_more_than_the_noise`).
/// A wider stretch keeps its points, and so does a leg with fewer than two GNSS-height points,
/// since they are all it has.
fn drop_bracketed_non_gnss(pts: &mut Vec<fdop_types::store::TrackPoint>, max_span_s: f64) {
    let t_gnss: Vec<f64> = pts
        .iter()
        .filter(|p| p.alt_src == alt_src::GEOM)
        .map(|p| p.t)
        .collect();
    pts.retain(|p| {
        if p.alt_src == alt_src::GEOM {
            return true;
        }
        let i = t_gnss.partition_point(|&t| t < p.t);
        !(i > 0 && i < t_gnss.len() && t_gnss[i] - t_gnss[i - 1] <= max_span_s)
    });
}

/// Missing groundspeed is not evidence for the old 200 m/s limit. A piece with missing
/// coverage can infer its scale from three positions with agreeing horizontal secants. Use
/// the reported-speed envelope, but cap this recovery at the nominal sound speed: repeated
/// position jumps must not infer an arbitrarily large aircraft speed. This is deliberately
/// limited to locally steady, ground-subsonic motion; unsupported turns stay refused.
fn position_speed_limit(
    pts: &[fdop_types::store::TrackPoint],
    xy: &[[f64; 2]],
    bp: &BuildParams,
) -> Option<f64> {
    let mut speeds = Vec::new();
    for (p, x) in pts.windows(3).zip(xy.windows(3)) {
        let dt = [p[1].t - p[0].t, p[2].t - p[1].t];
        if dt
            .iter()
            .any(|&t| !t.is_finite() || t <= 0.0 || t > bp.max_gap_s)
        {
            continue;
        }
        let velocity = [
            [(x[1][0] - x[0][0]) / dt[0], (x[1][1] - x[0][1]) / dt[0]],
            [(x[2][0] - x[1][0]) / dt[1], (x[2][1] - x[1][1]) / dt[1]],
        ];
        let speed = velocity.map(|v| v[0].hypot(v[1]));
        if speed
            .iter()
            .any(|&v| !v.is_finite() || v <= 0.0 || v > fdop_types::C_SOUND)
            || (0..2).any(|k| {
                (p[k + 1].alt_m - p[k].alt_m).abs() as f64
                    > bp.max_vz_mps * dt[k] + bp.pos_slack_m.1
            })
            || (velocity[1][0] - velocity[0][0]).hypot(velocity[1][1] - velocity[0][1])
                > bp.vel_consistency_mps
        {
            continue;
        }
        speeds.push(0.5 * (speed[0] + speed[1]));
    }
    if speeds.is_empty() {
        return None;
    }
    let mid = speeds.len() / 2;
    let (_, median, _) = speeds.select_nth_unstable_by(mid, f64::total_cmp);
    Some((1.5 * *median + 30.0).min(fdop_types::C_SOUND))
}

/// Reported-speed envelope shared by chain selection and separated corrected-height recovery.
fn reported_speed_limit(pts: &[fdop_types::store::TrackPoint]) -> f64 {
    let mut gs: Vec<f64> = pts
        .iter()
        .filter(|p| p.gs_mps.is_finite() && p.gs_mps > 0.0)
        .map(|p| p.gs_mps as f64)
        .collect();
    if gs.is_empty() {
        200.0
    } else {
        gs.sort_by(f64::total_cmp);
        let med = gs[gs.len() / 2];
        (1.5 * med + 30.0).max(80.0)
    }
}

/// Keep the original longest physically consistent chain, then recover well-separated parts
/// before or after it. Added parts must have at least two points, be more than `max_gap_s` from
/// every kept part, and be physically reachable from their kept neighbours. An alternative
/// overlapping the winning chain stays rejected, even if a portion extends beyond it.
fn consistent_chains(pts: &[fdop_types::store::TrackPoint], bp: &BuildParams) -> Vec<Vec<usize>> {
    let n = pts.len();
    if n < 3 {
        return vec![(0..n).collect()];
    }
    let mut v_lim = reported_speed_limit(pts);
    // horizontal position at the leg's mean height (ENU around the first point)
    let enu = Enu::new(pts[0].lat, pts[0].lon, 0.0);
    let xy: Vec<[f64; 2]> = pts
        .iter()
        .map(|p| {
            let e = enu.from_ecef(geodetic_to_ecef(p.lat, p.lon, 0.0));
            [e[0], e[1]]
        })
        .collect();
    let reachable = |i: usize, j: usize, limit: f64| -> bool {
        let dt = pts[j].t - pts[i].t;
        let dh = ((xy[j][0] - xy[i][0]).powi(2) + (xy[j][1] - xy[i][1]).powi(2)).sqrt();
        let dz = (pts[j].alt_m - pts[i].alt_m).abs() as f64;
        dh <= limit * dt + bp.pos_slack_m.0 && dz <= bp.max_vz_mps * dt + bp.pos_slack_m.1
    };
    // A long slow portion can set the leg-wide median below a later genuine cruise speed.
    // Both endpoint reports must independently support the faster position chord. Keep the
    // same velocity residual and vertical bounds; one fast report cannot widen the leg.
    let reported_reachable = |i: usize, j: usize| {
        let dt = pts[j].t - pts[i].t;
        if (pts[j].alt_m - pts[i].alt_m).abs() as f64 > bp.max_vz_mps * dt + bp.pos_slack_m.1 {
            return false;
        }
        let chord = [(xy[j][0] - xy[i][0]) / dt, (xy[j][1] - xy[i][1]) / dt];
        [i, j].into_iter().all(|k| {
            let p = &pts[k];
            let speed = p.gs_mps as f64;
            if !speed.is_finite()
                || speed <= 0.0
                || speed > fdop_types::C_SOUND
                || !p.track_deg.is_finite()
            {
                return false;
            }
            let heading = (p.track_deg as f64).to_radians();
            let local = Enu::new(p.lat, p.lon, 0.0);
            let velocity = enu.vec_from_ecef(local.vec_to_ecef([
                speed * heading.sin(),
                speed * heading.cos(),
                0.0,
            ]));
            (velocity[0] - chord[0]).hypot(velocity[1] - chord[1]) <= bp.vel_consistency_mps
        })
    };
    let select = |limit, allow_reported| {
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
                if best[i] + 1 > best[j]
                    && (reachable(i, j, limit) || (allow_reported && reported_reachable(i, j)))
                {
                    best[j] = best[i] + 1;
                    prev[j] = i;
                }
            }
            prefix_best[j] = prefix_best[j - 1].max(best[j]);
        }
        let winner = (0..n).max_by_key(|&k| (best[k], k)).unwrap_or(0);
        (best, prev, winner)
    };
    let (mut best, mut prev, mut winner) = select(v_lim, false);
    // A few closely spaced reports can pass the default envelope even when the rest of a
    // coherent fast flyover does not. Recover missing coverage only by extending the legacy
    // winner, never by replacing a supported rival. A finite zero must not be reinterpreted,
    // because the later sparse-sample velocity rule can still trust that report.
    let position_limit = (best[winner] < n && pts.iter().all(|p| !p.gs_mps.is_finite()))
        .then(|| position_speed_limit(pts, &xy, bp))
        .flatten()
        .filter(|&limit| limit > v_lim);
    let allow_reported = pts.iter().any(|p| {
        p.gs_mps as f64 > v_lim && p.gs_mps as f64 <= fdop_types::C_SOUND && p.track_deg.is_finite()
    });
    if best[winner] < n && (allow_reported || position_limit.is_some()) {
        let limit = position_limit.unwrap_or(v_lim);
        let mut first = winner;
        while prev[first] != usize::MAX {
            first = prev[first];
        }
        // A longer primary must not swallow an independently supported part that the
        // original cleaner can keep below. Every such part has a supported endpoint
        // more than max_gap_s outside the primary's interval, on either side.
        let disjoint_support = best.iter().enumerate().any(|(k, &len)| {
            len >= 2
                && (pts[first].t - pts[k].t > bp.max_gap_s
                    || pts[k].t - pts[winner].t > bp.max_gap_s)
        });
        if !disjoint_support {
            let (expanded_best, expanded_prev, expanded_winner) = select(limit, allow_reported);
            let mut old = winner;
            let mut expanded = expanded_winner;
            while expanded != usize::MAX && old != usize::MAX {
                if expanded == old {
                    old = prev[old];
                }
                expanded = expanded_prev[expanded];
            }
            if expanded_best[expanded_winner] > best[winner]
                && (best[winner] < 2 || old == usize::MAX)
            {
                v_lim = limit;
                (best, prev, winner) = (expanded_best, expanded_prev, expanded_winner);
            }
        }
    }
    let ok = |i, j| reachable(i, j, v_lim);
    let trace = |mut k: usize| {
        let mut chain = vec![k];
        while prev[k] != usize::MAX {
            k = prev[k];
            chain.push(k);
        }
        chain.reverse();
        chain
    };
    let primary = trace(winner);
    let first = primary[0];
    // Clean tracks, and rejected reports confined to the winner's time support, retain the
    // old result without allocating/sorting other paths. No disjoint part can fit here.
    if primary.len() < 2
        || (pts[first].t - pts[0].t <= bp.max_gap_s
            && pts[n - 1].t - pts[winner].t <= bp.max_gap_s)
    {
        return vec![primary];
    }

    let mut roots: Vec<usize> = (0..n).collect();
    for k in 0..n {
        if prev[k] != usize::MAX {
            roots[k] = roots[prev[k]];
        }
    }
    let mut ends: Vec<usize> = (0..n).filter(|&k| best[k] >= 2).collect();
    ends.sort_unstable_by(|&a, &b| best[b].cmp(&best[a]).then(b.cmp(&a)));
    let mut kept = std::collections::BTreeMap::from([(first, winner)]);
    for end in ends {
        let start = roots[end];
        let before = kept.range(..start).next_back().map(|(_, &k)| k);
        let after = kept.range(start..).next().map(|(&k, _)| k);
        if before.is_some_and(|k| {
            k >= start || pts[start].t - pts[k].t <= bp.max_gap_s || !ok(k, start)
        }) || after.is_some_and(|k| {
            end >= k || pts[k].t - pts[end].t <= bp.max_gap_s || !ok(end, k)
        }) {
            continue;
        }
        // A long interval is used only to check physical reachability; no model interpolates
        // across it. The predecessor paths themselves still obey the original time-gap limit.
        kept.insert(start, end);
    }
    // Keep the legacy winner first so its model and key remain unchanged. Recovered parts
    // follow in chronological order. Their disjoint spans bound total traceback work by n.
    let mut chains = vec![primary];
    for (_, end) in kept {
        if end != winner {
            chains.push(trace(end));
        }
    }
    chains
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
/// With `vu = None`, test horizontal motion alone; no vertical report contributes to the residual.
fn turn_consistent(
    pts: &[fdop_types::store::TrackPoint],
    pos: &[V3],
    k: usize,
    enu: &Enu,
    vu: Option<f64>,
    tolerance: f64,
) -> bool {
    let p = &pts[k];
    if p.gs_mps <= 0.0 {
        return false;
    }
    let (gs, heading) = (p.gs_mps as f64, (p.track_deg as f64).to_radians());
    let reported = [gs * heading.sin(), gs * heading.cos(), vu.unwrap_or(0.0)];
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
                vu.map_or(0.0, |_| chord[2]),
            ];
            norm(sub(reported, expected)) <= tolerance
        })
}

/// Recover one supported component after the original full-vector check has failed, or when
/// horizontal reports are missing. Every new component needs position support; the original
/// sparse-sample exemption is not enough.
/// If both components fit separately, keep the old fallback: accepting both here could bypass
/// a failed joint residual and enlarge its tolerance by sqrt(2).
fn recover_velocity_component(
    pts: &[fdop_types::store::TrackPoint],
    pos: &[V3],
    k: usize,
    fd: V3,
    tolerance: f64,
) -> V3 {
    let p = &pts[k];
    let enu = Enu::new(p.lat, p.lon, 0.0);
    let fallback = enu.vec_from_ecef(fd);
    let vertical = p.vrate_mps as f64;
    let vertical_ok = vertical.is_finite() && (vertical - fallback[2]).abs() <= tolerance;
    let horizontal = if p.gs_mps.is_finite() && p.gs_mps >= 0.0 && p.track_deg.is_finite() {
        let (gs, heading) = (p.gs_mps as f64, (p.track_deg as f64).to_radians());
        let reported = [gs * heading.sin(), gs * heading.cos()];
        ((reported[0] - fallback[0]).hypot(reported[1] - fallback[1]) <= tolerance
            || turn_consistent(pts, pos, k, &enu, None, tolerance))
        .then_some(reported)
    } else {
        None
    };
    match (horizontal, vertical_ok) {
        (Some([ve, vn]), false) => enu.vec_to_ecef([ve, vn, fallback[2]]),
        (None, true) => add(fd, enu.vec_to_ecef([0.0, 0.0, vertical - fallback[2]])),
        _ => fd,
    }
}

/// A missing speed report must not reinstate the 200 m/s default between models whose
/// retained positions already support faster motion. Only literal NaNs permit this retry;
/// finite or invalid non-NaN reports retain the reported envelope. Rejected GNSS positions
/// cannot set the limit: use the same checked secants on the preferred models' own samples.
fn corrected_recovery_speed_limit(
    pts: &[fdop_types::store::TrackPoint],
    preferred: &[TrackModel],
    bp: &BuildParams,
) -> f64 {
    let mut limit = reported_speed_limit(pts);
    if pts.iter().any(|p| !p.gs_mps.is_nan()) {
        return limit;
    }
    for model in preferred.iter().filter(|model| model.t.len() >= 3) {
        // The builder supplies its sorted, bridge-filtered points. Each model time
        // names exactly one of them; no chord crosses into a rejected report.
        let support: Vec<_> = model
            .t
            .iter()
            .filter_map(|t| {
                pts.binary_search_by(|p| p.t.total_cmp(t))
                    .ok()
                    .map(|i| pts[i])
            })
            .collect();
        if support.len() != model.t.len() {
            continue;
        }
        let enu = Enu::new(support[0].lat, support[0].lon, 0.0);
        let xy: Vec<_> = support
            .iter()
            .map(|p| {
                let e = enu.from_ecef(geodetic_to_ecef(p.lat, p.lon, 0.0));
                [e[0], e[1]]
            })
            .collect();
        if let Some(inferred) = position_speed_limit(&support, &xy, bp) {
            limit = limit.max(inferred);
        }
    }
    limit
}

/// Separate models may describe the same aircraft only if their endpoints remain reachable
/// under the chain cleaner's horizontal/vertical limits. No interpolation crosses this gap.
fn separated_models_reachable(
    left: &TrackModel,
    right: &TrackModel,
    speed_limit: f64,
    bp: &BuildParams,
) -> bool {
    let dt = right.t0() - left.t1();
    if !dt.is_finite() || dt <= bp.max_gap_s {
        return false;
    }
    let (lat0, lon0, h0) = ecef_to_geodetic(*left.pos.last().unwrap());
    let (lat1, lon1, h1) = ecef_to_geodetic(right.pos[0]);
    let horizontal = Enu::new(lat0, lon0, 0.0).from_ecef(geodetic_to_ecef(lat1, lon1, 0.0));
    horizontal[0].hypot(horizontal[1]) <= speed_limit * dt + bp.pos_slack_m.0
        && (h1 - h0).abs() <= bp.max_vz_mps * dt + bp.pos_slack_m.1
}

/// Build track models from one stored leg (possibly several, split at gaps). Corrected-height
/// recovery keeps preferred models intact and restores only separately supported, reachable parts.
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
    let original_len = pts.len();
    drop_bracketed_non_gnss(&mut pts, bp.gnss_bridge_s);
    let bridge_dropped = pts.len() < original_len;
    let mut out = Vec::new();
    let mut start = 0;
    let mut part = 0;
    for i in 1..=pts.len() {
        if i == pts.len() || pts[i].t - pts[i - 1].t > bp.max_gap_s {
            let chains = consistent_chains(&pts[start..i], bp);
            let primary_kept = chains.first().is_some_and(|chain| chain.len() >= 2);
            let base_key = if part == 0 && i == pts.len() && start == 0 {
                leg.key.clone()
            } else {
                format!("{}#{}", leg.key, part)
            };
            for (recovered, keep) in chains.into_iter().enumerate() {
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
                                || turn_consistent(
                                    seg,
                                    &pos,
                                    k,
                                    &enu,
                                    Some(vu),
                                    bp.vel_consistency_mps,
                                )
                            {
                                vel[k] = rep;
                                n_rep += 1;
                                continue;
                            }
                        }
                        vel[k] =
                            recover_velocity_component(seg, &pos, k, fd[k], bp.vel_consistency_mps);
                    }
                    let key = if recovered == 0 {
                        base_key.clone()
                    } else {
                        format!("{base_key}#recovered{}", recovered - 1)
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
                }
            }
            if primary_kept {
                part += 1;
            }
            start = i;
        }
    }
    if bridge_dropped {
        // Preferred models retain their exact samples, motion and keys. Corrected reports
        // close to their coverage cannot extend or compete with that preferred geometry.
        let mut kept: Vec<usize> = (0..out.len()).collect();
        kept.sort_unstable_by(|&a, &b| out[a].t0().total_cmp(&out[b].t0()));
        let eligible = |p: &fdop_types::store::TrackPoint| {
            if p.alt_src != alt_src::BARO_CORRECTED || !p.t.is_finite() {
                return false;
            }
            let i = kept.partition_point(|&k| out[k].t0() <= p.t);
            (i == 0 || p.t - out[kept[i - 1]].t1() > bp.max_gap_s)
                && (i == kept.len() || out[kept[i]].t0() - p.t > bp.max_gap_s)
        };
        // Avoid cloning the leg when ordinary preferred coverage already contains it.
        if leg.points.iter().filter(|p| eligible(p)).take(3).count() == 3 {
            let mut corrected = leg.clone();
            corrected.points.retain(eligible);
            let unbridged = BuildParams {
                gnss_bridge_s: 0.0,
                ..*bp
            };
            let mut recovered = build(&corrected, &unbridged);
            // Two points bypass consistency cleaning. Recovery requires two checked chords.
            recovered.retain(|model| model.t.len() >= 3);
            if out.is_empty() {
                // Preserve the established all-preferred-failed fallback, including its keys.
                out = recovered;
            } else {
                let speed_limit = if corrected.points.iter().all(|p| p.gs_mps.is_nan()) {
                    corrected_recovery_speed_limit(&pts, &out, bp)
                } else {
                    reported_speed_limit(&pts)
                };
                let original_len = out.len();
                for mut model in recovered {
                    let i = kept.partition_point(|&k| out[k].t0() < model.t0());
                    if (i > 0
                        && !separated_models_reachable(&out[kept[i - 1]], &model, speed_limit, bp))
                        || (i < kept.len()
                            && !separated_models_reachable(&model, &out[kept[i]], speed_limit, bp))
                    {
                        continue;
                    }
                    model.key = format!("{}#corrected{}", leg.key, out.len() - original_len);
                    kept.insert(i, out.len());
                    out.push(model);
                }
            }
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

    /// Bound every cubic Hermite segment by its Bezier control hull. Bounding only the reported
    /// positions would miss a turn whose curve reaches beyond both knots. Invalid/overflowing
    /// inputs yield no bound and must retain the track for the normal geometry checks.
    pub fn bounds(&self) -> Option<TrackBounds> {
        self.segment_bounds(0..self.t.len().saturating_sub(1))
    }

    /// Bound the knot segments touching a time interval. Complete segments deliberately
    /// include any out-of-interval control points, so a partial segment cannot lose a turn.
    /// As with interpolation, callers supply finite, strictly increasing model times.
    /// Unknown intervals or invalid selected geometry have no bound and must retain the
    /// exact check; callers can validate the whole model once with `bounds` first.
    pub(crate) fn bounds_between(&self, lo: f64, hi: f64) -> Option<TrackBounds> {
        let n = self.t.len();
        if n < 2
            || !lo.is_finite()
            || !hi.is_finite()
            || hi < lo
            || hi < self.t0()
            || lo > self.t1()
        {
            return None;
        }
        let first = self
            .t
            .partition_point(|&t| t <= lo)
            .saturating_sub(1)
            .min(n - 2);
        let end = self
            .t
            .partition_point(|&t| t < hi)
            .min(n - 1)
            .max(first + 1);
        self.segment_bounds(first..end)
    }

    fn segment_bounds(&self, segments: std::ops::Range<usize>) -> Option<TrackBounds> {
        let n = self.t.len();
        if n < 2 || self.pos.len() != n || self.vel.len() != n {
            return None;
        }
        let mut bounds = TrackBounds {
            lo: [f64::INFINITY; 3],
            hi: [f64::NEG_INFINITY; 3],
        };
        for i in segments {
            let (ta, tb) = (self.t[i], self.t[i + 1]);
            let h = tb - ta;
            if !ta.is_finite() || !tb.is_finite() || h <= 0.0 || !h.is_finite() {
                return None;
            }
            for k in 0..3 {
                let (pa, pb) = (self.pos[i][k], self.pos[i + 1][k]);
                let (va, vb) = (h * self.vel[i][k], h * self.vel[i + 1][k]);
                let control = [pa, pa + va / 3.0, pb - vb / 3.0, pb];
                if [pa, pb, va, vb].iter().any(|v| !v.is_finite())
                    || control.iter().any(|v| !v.is_finite())
                {
                    return None;
                }
                // `pv_in` evaluates the Hermite basis in ECEF, where large signed positions
                // can cancel. This allowance bounds its basis/products/sums, plus construction
                // of the control points, in units of the largest input term on this axis.
                let scale = pa.abs().max(pb.abs()).max(va.abs()).max(vb.abs()).max(1.0);
                let pad = 128.0 * f64::EPSILON * scale;
                let lo =
                    (control.iter().copied().fold(f64::INFINITY, f64::min) - pad).next_down();
                let hi =
                    (control.iter().copied().fold(f64::NEG_INFINITY, f64::max) + pad).next_up();
                if !lo.is_finite() || !hi.is_finite() {
                    return None;
                }
                bounds.lo[k] = bounds.lo[k].min(lo);
                bounds.hi[k] = bounds.hi[k].max(hi);
            }
        }
        Some(bounds)
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
        if g.abs() < 1e-3 && te > t0 && te < t1 {
            return Some((te, r, vr));
        }
        if !c.is_finite() || c <= 0.0 {
            return None;
        }
        bracketed_emission(t0, t1, 1e-7, |t| {
            let (p, v) = self.pv(t);
            let rv = sub(p, x);
            let r = norm(rv);
            let vr = dot(rv, v) / r;
            let rate = 1.0 + vr / c;
            (rate.is_finite() && rate > 0.0).then_some((t + r / c - t_r, (r, vr)))
        })
        .map(|(t, (r, vr))| (t, r, vr))
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
        if g.abs() < 1e-3 && te > t0 && te < t1 && ratio.is_finite() && ratio > 0.0 {
            return Some((te, r, ratio));
        }
        bracketed_emission(t0, t1, tol, |t| {
            *hint = self.seg_near(t, *hint);
            let (p, v) = self.pv_in(*hint, t);
            let rv = sub(p, x);
            let r = norm(rv);
            let (travel, rate) = m.travel_and_rate(rv, r, v);
            let received_rate = 1.0 + rate;
            let ratio = 1.0 / received_rate;
            (received_rate.is_finite() && received_rate > 0.0 && ratio.is_finite())
                .then_some((t + travel - t_r, (r, ratio)))
        })
        .map(|(t, (r, ratio))| (t, r, ratio))
    }

    /// Smallest range to `x` near `t_guess` (emission time). A returning aircraft can have
    /// several local CPAs in this interval; this chooses the closest in distance, not time.
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
        let (a, b) = (
            lo + step * kb.saturating_sub(1) as f64,
            (lo + step * (kb + 1) as f64).min(hi),
        );
        Some(self.refine_cpa(x, a, b, lo, hi))
    }

    /// Local range minima in an emission-time interval, in time order. As in `cpa_near`, a
    /// minimum at a clipped boundary is returned with `interior = false`; it cannot pass the
    /// search's CPA gate. The 2 s survey uses constant scratch space and refines only minima.
    pub fn local_cpas(&self, x: V3, ta: f64, tb: f64) -> impl Iterator<Item = Cpa> + '_ {
        let (lo, hi) = (ta.max(self.t0()), tb.min(self.t1()));
        let n = if hi > lo {
            (((hi - lo) / 2.0).ceil() as usize).max(4)
        } else {
            0
        };
        let step = if n > 0 { (hi - lo) / n as f64 } else { 0.0 };
        let dist2 = move |t: f64| {
            let rv = sub(self.pos_at(t), x);
            dot(rv, rv)
        };
        let (mut k, mut left, mut here) = (0usize, f64::INFINITY, dist2(lo));
        std::iter::from_fn(move || {
            while n > 0 && k <= n {
                let right = if k < n {
                    dist2(lo + (hi - lo) * (k + 1) as f64 / n as f64)
                } else {
                    f64::INFINITY
                };
                // A tie belongs to its first sample, matching `cpa_near` and avoiding two
                // refinements when a CPA sits halfway between survey samples.
                let minimum = here.is_finite() && here < left && here <= right;
                let a = lo + k.saturating_sub(1) as f64 * step;
                let b = (lo + (k + 1) as f64 * step).min(hi);
                k += 1;
                left = here;
                here = right;
                if minimum {
                    return Some(self.refine_cpa(x, a, b, lo, hi));
                }
            }
            None
        })
    }

    /// Recover the pass identified by an emission-time hypothesis. A closer return elsewhere
    /// in the window must not replace it. Boundary minima are a fallback only, still marked as
    /// non-interior so callers retain their existing gates.
    pub fn cpa_nearest_time(&self, x: V3, t_guess: f64, half: f64) -> Option<Cpa> {
        let nearer = |a: &Cpa, b: &Cpa| {
            b.interior
                .cmp(&a.interior)
                .then_with(|| (a.t_e - t_guess).abs().total_cmp(&(b.t_e - t_guess).abs()))
        };
        if !t_guess.is_finite() || !half.is_finite() {
            return self
                .local_cpas(x, t_guess - half, t_guess + half)
                .min_by(nearer);
        }
        let (lo, hi) = (
            (t_guess - half).max(self.t0()),
            (t_guess + half).min(self.t1()),
        );
        if hi <= lo {
            return None;
        }
        let n = (((hi - lo) / 2.0).ceil() as usize).max(4);
        let step = (hi - lo) / n as f64;
        let sample = |k| {
            // Keep local_cpas' arithmetic, including its initial sample at lo.
            let t = if k == 0 {
                lo
            } else {
                lo + (hi - lo) * k as f64 / n as f64
            };
            let rv = sub(self.pos_at(t), x);
            dot(rv, rv)
        };
        let centre = (((t_guess - lo) / (hi - lo) * n as f64).round() as usize).min(n);
        let values = [
            if centre > 0 {
                sample(centre - 1)
            } else {
                f64::INFINITY
            },
            sample(centre),
            if centre < n {
                sample(centre + 1)
            } else {
                f64::INFINITY
            },
        ];
        // Two three-sample cursors visit the same minima outward from the hypothesis,
        // with constant scratch space and no repeated distance evaluation on either side.
        let mut scans = [
            Some((centre, values)),
            (centre < n).then(|| {
                (
                    centre + 1,
                    [
                        values[1],
                        values[2],
                        if centre + 1 < n {
                            sample(centre + 2)
                        } else {
                            f64::INFINITY
                        },
                    ],
                )
            }),
        ];
        let mut best: Option<(usize, Cpa)> = None;
        loop {
            let mut active = false;
            for (side, scan) in scans.iter_mut().enumerate() {
                let Some((k, [left, here, right])) = *scan else {
                    continue;
                };
                let a = lo + k.saturating_sub(1) as f64 * step;
                let b = (lo + (k + 1) as f64 * step).min(hi);
                if let Some((_, cpa)) = &best
                    && cpa.interior
                    && (if side == 0 { t_guess - b } else { a - t_guess })
                        > (cpa.t_e - t_guess).abs()
                {
                    // Every later bracket on this side is at least this far away. Strict
                    // inequality leaves ties available in the original time/sample order.
                    *scan = None;
                    continue;
                }
                active = true;
                if here.is_finite() && here < left && here <= right {
                    let cpa = self.refine_cpa(x, a, b, lo, hi);
                    if best
                        .as_ref()
                        .is_none_or(|(old_k, old)| nearer(&cpa, old).then(k.cmp(old_k)).is_lt())
                    {
                        best = Some((k, cpa));
                    }
                }
                *scan = if side == 0 && k > 0 {
                    Some((
                        k - 1,
                        [
                            if k > 1 { sample(k - 2) } else { f64::INFINITY },
                            left,
                            here,
                        ],
                    ))
                } else if side == 1 && k < n {
                    Some((
                        k + 1,
                        [
                            here,
                            right,
                            if k + 1 < n {
                                sample(k + 2)
                            } else {
                                f64::INFINITY
                            },
                        ],
                    ))
                } else {
                    None
                };
            }
            if !active {
                return best.map(|(_, cpa)| cpa);
            }
        }
    }

    fn refine_cpa(&self, x: V3, mut a: f64, mut b: f64, lo: f64, hi: f64) -> Cpa {
        let dist2 = |t: f64| {
            let rv = sub(self.pos_at(t), x);
            dot(rv, rv)
        };
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
        Cpa {
            t_e: te,
            d,
            speed: norm(v),
            interior,
            rv: sub(p, x),
            vel: v,
        }
    }
}

/// Newton can alternate between distant turns even when reception time increases everywhere.
/// Preserve its converged answers; only a failed solve pays for this bounded signed bracket.
/// Callers reject invalid propagation or nonpositive reception derivatives at every probe.
/// Strict endpoint signs retain the rule that emission outside or at a track end is unavailable.
fn bracketed_emission<T>(
    mut lo: f64,
    mut hi: f64,
    tol: f64,
    mut residual: impl FnMut(f64) -> Option<(f64, T)>,
) -> Option<(f64, T)> {
    let (start, end) = (lo, hi);
    let (left, _) = residual(lo)?;
    let (right, _) = residual(hi)?;
    if !left.is_finite() || !right.is_finite() || left >= 0.0 || right <= 0.0 {
        return None;
    }
    for _ in 0..64 {
        let t = lo + 0.5 * (hi - lo);
        let (g, value) = residual(t)?;
        if !g.is_finite() {
            return None;
        }
        // At UNIX-epoch magnitudes the representable time spacing can exceed `tol`.
        // A stalled bracket still has to meet the same reception residual as Newton.
        if g.abs() < 1e-3 && (hi - lo <= tol || t == lo || t == hi || g == 0.0) {
            return (t > start && t < end).then_some((t, value));
        }
        if g > 0.0 {
            hi = t;
        } else {
            lo = t;
        }
    }
    None
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
    /// Centre measured by a still-air constant-velocity fit to this pass's received curve.
    /// In oblique wind this differs from the geometric CPA's heard time above. A turning
    /// track uses its local CPA velocity, as do the speed, distance and source-tone gates.
    pub fn t_r_fit_p(&self, m: &Prop) -> f64 {
        if m.w == [0.0; 3] {
            // Preserve the established still-air gate arithmetic exactly.
            self.t_r(m.c)
        } else {
            self.t_e + m.fit_center_offset(self.rv, self.vel)
        }
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
    /// Received centre of the equivalent still-air curve, relative to this emission time.
    /// Completing the wavefront quadratic gives reception as
    /// `alpha * t + beta + sqrt(A * t² + 2 B * t + C) / a`, where
    /// `a = c² - |w|²`. Its fitted centre is `beta - alpha * B / A + sqrt(C - B² / A) / a`.
    /// This also handles a coarse sample with `rv.v != 0`; it need not be the exact CPA.
    /// The equivalent curve requires a moving source subsonic relative to the medium.
    #[inline]
    pub fn fit_center_offset(&self, rv: V3, v: V3) -> f64 {
        let a = self.c * self.c - dot(self.w, self.w);
        let relative = sub(v, self.w);
        if !(self.c > 0.0 && a > 0.0 && a.is_finite())
            || rv.iter().chain(&v).chain(&self.w).any(|x| !x.is_finite())
            || dot(relative, relative) >= self.c * self.c
        {
            return f64::NAN;
        }
        let (b, q) = (dot(self.w, rv), dot(self.w, v));
        let aa = q * q + a * dot(v, v);
        let bb = b * q + a * dot(rv, v);
        let cc = b * b + a * dot(rv, rv);
        let alpha = 1.0 + q / a;
        let closest = cc - bb * bb / aa;
        let centre = b / a - alpha * bb / aa + closest.sqrt() / a;
        if aa > 0.0 && alpha > 0.0 && closest > 0.0 && centre.is_finite() {
            centre
        } else {
            f64::NAN
        }
    }

    /// Speed (m/s) and closest distance (m) of the equivalent still-air fit at `c_fit`.
    /// A constant-velocity source in homogeneous wind still describes one retarded curve,
    /// but its speed and range have different scales. With `a = c² - |w|²`, `q = w.v`
    /// and `A = q² + a |v|²`, its fitted speed is `c_fit sqrt(A) / (a + q)`.
    /// The range follows the same quadratic after shifting to its minimum. Projecting
    /// `rv` perpendicular to `v` first avoids subtracting two large squared along-track
    /// offsets; this remains valid when the supplied reference is not the physical CPA.
    /// A turning track uses its instantaneous tangent, as do the other coarse fit gates.
    pub fn fit_geometry(&self, rv: V3, v: V3, c_fit: f64) -> Option<(f64, f64)> {
        let a = self.c * self.c - dot(self.w, self.w);
        let v2 = dot(v, v);
        let relative = sub(v, self.w);
        if !(self.c > 0.0
            && c_fit > 0.0
            && c_fit.is_finite()
            && a > 0.0
            && a.is_finite()
            && v2 > 0.0
            && v2.is_finite())
            || rv.iter().chain(&v).chain(&self.w).any(|x| !x.is_finite())
            || dot(relative, relative) >= self.c * self.c
        {
            return None;
        }
        let cross = |a: V3, b: V3| {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let perpendicular = scale(cross(v, cross(rv, v)), 1.0 / v2);
        let d2 = dot(perpendicular, perpendicular);
        let q = dot(self.w, v);
        let transverse_c2 = a + q * q / v2;
        let (speed, distance) = if self.w == [0.0; 3] {
            (v2.sqrt() * c_fit / self.c, d2.sqrt() * c_fit / self.c)
        } else {
            let b = dot(self.w, perpendicular);
            (
                c_fit * v2.sqrt() * transverse_c2.sqrt() / (a + q),
                c_fit * (d2 + b * b / transverse_c2).sqrt() / a.sqrt(),
            )
        };
        (speed > 0.0
            && speed < c_fit
            && speed.is_finite()
            && distance > 0.0
            && distance.is_finite())
        .then_some((speed, distance))
    }

    /// Physical/fitted speed scale and bounds on the physical/fitted CPA-distance scale.
    /// Before an observer is known, the perpendicular range can point anywhere around
    /// the velocity axis. Its wind projection spans zero to the transverse wind speed;
    /// these bounds retain every orientation for the exact `fit_geometry` check later.
    pub fn fit_gate_scales(&self, v: V3, c_fit: f64) -> Option<(f64, [f64; 2])> {
        let a = self.c * self.c - dot(self.w, self.w);
        let v2 = dot(v, v);
        let relative = sub(v, self.w);
        if !(self.c > 0.0
            && c_fit > 0.0
            && c_fit.is_finite()
            && a > 0.0
            && a.is_finite()
            && v2 > 0.0
            && v2.is_finite())
            || v.iter().chain(&self.w).any(|x| !x.is_finite())
            || dot(relative, relative) >= self.c * self.c
        {
            return None;
        }
        if self.w == [0.0; 3] {
            let scale = self.c / c_fit;
            return (scale > 0.0 && scale.is_finite()).then_some((scale, [scale; 2]));
        }
        let q = dot(self.w, v);
        let transverse_c = (a + q * q / v2).sqrt();
        let speed = (a + q) / (c_fit * transverse_c);
        let d_hi = a.sqrt() / c_fit;
        let d_lo = (d_hi * transverse_c / self.c).min(d_hi);
        // Include extrema that round onto either side of the exact bound.
        let (d_lo, d_hi) = (d_lo.next_down(), d_hi.next_up());
        (speed > 0.0 && speed.is_finite() && d_lo > 0.0 && d_lo.is_finite() && d_hi.is_finite())
            .then_some((speed, [d_lo, d_hi]))
    }

    /// Legacy first-order scale for speeds/distances fitted in still air at
    /// `c_fit`: (c, v, d) -> lambda (c, v, d) leaves a still-air curve invariant, and the
    /// leading wind correction adds v.w/c to the effective c. This is an approximation;
    /// new hypothesis gates use `fit_gate_scales` and exact checks use `fit_geometry`.
    #[inline]
    pub fn fit_scale(&self, v: V3, c_fit: f64) -> f64 {
        (self.c + dot(v, self.w) / self.c) / c_fit
    }

    /// Convert a still-air fit's tone to the emitted tone in this moving medium.
    /// For constant velocity, reception time is `alpha * t_e + beta + sqrt(quadratic)`.
    /// With `a = c² - |w|²`, `alpha = 1 + v.w/a`; rescaling emission time by alpha gives
    /// the still-air curve, whose fitted tone is therefore `f_source / alpha`.
    /// A turning track uses its local CPA velocity, as do the existing geometric gates.
    /// Invalid media or nonpositive time scales cannot supply a source-tone constraint.
    #[inline]
    pub fn source_tone_scale(&self, v: V3) -> f64 {
        let a = self.c * self.c - dot(self.w, self.w);
        if !(self.c > 0.0 && a > 0.0 && a.is_finite()) || v.iter().any(|x| !x.is_finite()) {
            return f64::NAN;
        }
        let alpha = 1.0 + dot(v, self.w) / a;
        if alpha.is_finite() && alpha > 0.0 {
            alpha
        } else {
            f64::NAN
        }
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
    fn fitted_wind_centre_matches_independent_wavefront_observations() {
        let emitted = 350.0;
        let rotate = |v: V3| [v[2], v[0], v[1]];
        for (rv, v, w) in [
            ([0.0, 11_000.0, 1500.0], [70.0, 0.0, 0.0], [0.0; 3]),
            ([0.0, 11_000.0, 1500.0], [70.0, 0.0, 0.0], [65.0, 0.0, 0.0]),
            ([0.0, 11_000.0, 1500.0], [70.0, 0.0, 0.0], [0.0, 65.0, 0.0]),
            ([0.0, 11_000.0, 1500.0], [70.0, 0.0, 0.0], [65.0, 65.0, 0.0]),
            (
                [0.0, 8000.0, 1200.0],
                [25.0, 0.0, 0.0],
                [-85.0, -130.0, 0.0],
            ),
        ] {
            for (rv, v, w) in [(rv, v, w), (rotate(rv), rotate(v), rotate(w))] {
                let prop = Prop { c: 343.0, w };
                let a = prop.c.powi(2) - dot(w, w);
                let (b, q) = (dot(w, rv), dot(w, v));
                let aa = q * q + a * dot(v, v);
                let bb = b * q + a * dot(rv, v);
                let cc = b * b + a * dot(rv, rv);
                let alpha = 1.0 + q / a;
                let centre = prop.fit_center_offset(rv, v);
                // A different reference emission time must leave the received centre fixed;
                // the sampled position is deliberately no longer perpendicular to velocity.
                assert!(
                    (50.0 + prop.fit_center_offset(add(rv, scale(v, 50.0)), v) - centre).abs()
                        < 1e-10
                );
                for c_fit in [300.0, 343.0, 360.0] {
                    let speed = c_fit * aa.sqrt() / (a + q);
                    let distance = c_fit / a * (cc - bb * bb / aa).sqrt();
                    for te in [-240.0, -90.0, 0.0, 90.0, 240.0] {
                        let position = add(rv, scale(v, te));
                        let heard = te + wavefront_travel(position, w, prop.c);
                        let dt = 0.01;
                        let before = wavefront_travel(sub(position, scale(v, dt)), w, prop.c);
                        let after = wavefront_travel(add(position, scale(v, dt)), w, prop.c);
                        let observed = emitted / (1.0 + (after - before) / (2.0 * dt));
                        let fitted = cv_curve(
                            heard,
                            emitted / alpha,
                            speed,
                            distance,
                            centre - distance / c_fit,
                            c_fit,
                        );
                        assert!(
                            (fitted - observed).abs() < 2e-6,
                            "rv={rv:?}, wind={w:?}, te={te}: {fitted} vs {observed}"
                        );
                    }
                }
                let cpa = Cpa {
                    t_e: 1_788_895_000.0,
                    d: norm(rv),
                    speed: norm(v),
                    interior: true,
                    rv,
                    vel: v,
                };
                assert!((cpa.t_r_fit_p(&prop) - cpa.t_e - centre).abs() < 2e-7);
                if w == [0.0; 3] {
                    assert_eq!(cpa.t_r_fit_p(&prop).to_bits(), cpa.t_r(prop.c).to_bits());
                }
            }
        }
    }

    #[test]
    fn fitted_wind_centre_requires_a_valid_subsonic_curve() {
        let rv = [0.0, 8000.0, 1200.0];
        let v = [25.0, 0.0, 0.0];
        let prop = Prop {
            c: 343.0,
            w: [-85.0, -130.0, 0.0],
        };
        assert!(prop.fit_center_offset(rv, v) < -16.0);
        assert!(prop.travel(rv, norm(rv)) > 17.0);
        for invalid in [
            Prop::still(0.0),
            Prop::still(f64::NAN),
            Prop {
                c: 343.0,
                w: [343.0, 0.0, 0.0],
            },
            Prop {
                c: 343.0,
                w: [f64::NAN, 0.0, 0.0],
            },
        ] {
            assert!(invalid.fit_center_offset(rv, v).is_nan());
        }
        assert!(prop.fit_center_offset(rv, [0.0; 3]).is_nan());
        assert!(prop.fit_center_offset(rv, [500.0, 0.0, 0.0]).is_nan());
        assert!(prop.fit_center_offset([f64::NAN, 0.0, 0.0], v).is_nan());
        assert!(
            prop.fit_center_offset(rv, [f64::INFINITY, 0.0, 0.0])
                .is_nan()
        );
    }

    #[test]
    fn fitted_wind_geometry_reproduces_independent_wavefront_observations() {
        let emitted = 180.0;
        let rotate = |v: V3| [v[2], v[0], v[1]];
        for (rv, v, w) in [
            ([0.0, 3000.0, 1200.0], [180.0, 0.0, 0.0], [-80.0, 0.0, 0.0]),
            ([0.0, 3000.0, 1200.0], [180.0, 0.0, 0.0], [80.0, 0.0, 0.0]),
            (
                [1200.0, 0.0, 4000.0],
                [0.0, 110.0, 0.0],
                [40.0, -25.0, 10.0],
            ),
            (
                [1000.0, -2000.0, 1700.0],
                [150.0, 75.0, -20.0],
                [35.0, -60.0, 5.0],
            ),
            ([0.0, 1000.0, 500.0], [200.0, 80.0, 20.0], [0.0; 3]),
        ] {
            for (rv, v, w) in [(rv, v, w), (rotate(rv), rotate(v), rotate(w))] {
                for c in [300.0, 343.0, 360.0] {
                    let prop = Prop { c, w };
                    for c_fit in [300.0, 343.0, 360.0] {
                        let (speed, distance) = prop.fit_geometry(rv, v, c_fit).unwrap();
                        let centre = prop.fit_center_offset(rv, v);
                        let tone = emitted / prop.source_tone_scale(v);
                        for shift in [-500.0, 50.0, 500.0] {
                            let shifted = prop
                                .fit_geometry(add(rv, scale(v, shift)), v, c_fit)
                                .unwrap();
                            assert!((shifted.0 - speed).abs() < 1e-12);
                            assert!((shifted.1 - distance).abs() < 1e-9);
                        }
                        for te in [-180.0, -30.0, 0.0, 30.0, 180.0] {
                            let position = add(rv, scale(v, te));
                            let heard = te + wavefront_travel(position, w, c);
                            let dt = 0.001;
                            let before = wavefront_travel(sub(position, scale(v, dt)), w, c);
                            let after = wavefront_travel(add(position, scale(v, dt)), w, c);
                            let observed = emitted / (1.0 + (after - before) / (2.0 * dt));
                            let fitted = cv_curve(
                                heard,
                                tone,
                                speed,
                                distance,
                                centre - distance / c_fit,
                                c_fit,
                            );
                            assert!(
                                (fitted - observed).abs() < 2e-6,
                                "c={c}, c_fit={c_fit}, rv={rv:?}, v={v:?}, w={w:?}, te={te}: {fitted} vs {observed}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn wind_geometry_gate_bounds_cover_every_cpa_direction() {
        let v = [180.0, 0.0, 0.0];
        for w in [
            [0.0; 3],
            [-80.0, 0.0, 0.0],
            [0.0, 80.0, 0.0],
            [40.0, -60.0, 30.0],
        ] {
            let prop = Prop { c: 320.0, w };
            for c_fit in [300.0, 343.0, 360.0] {
                let (speed_scale, [d_lo, d_hi]) = prop.fit_gate_scales(v, c_fit).unwrap();
                if w == [0.0; 3] {
                    assert_eq!(
                        (speed_scale, d_lo, d_hi),
                        (320.0 / c_fit, 320.0 / c_fit, 320.0 / c_fit)
                    );
                }
                let mut minimum = f64::INFINITY;
                let mut maximum = f64::NEG_INFINITY;
                // The two axes attain both limits for pure crosswind; the full circle
                // also exercises oblique wind without deriving the fitted range twice.
                for degrees in 0..=360 {
                    let angle = (degrees as f64).to_radians();
                    let rv = [0.0, 3000.0 * angle.cos(), 3000.0 * angle.sin()];
                    let (speed, distance) = prop.fit_geometry(rv, v, c_fit).unwrap();
                    let actual = norm(rv) / distance;
                    assert!((speed * speed_scale - norm(v)).abs() < 1e-10);
                    assert!(actual >= d_lo - 1e-12 && actual <= d_hi + 1e-12);
                    minimum = minimum.min(actual);
                    maximum = maximum.max(actual);
                }
                if w[2] == 0.0 {
                    assert!((minimum - d_lo).abs() < 1e-12);
                    assert!((maximum - d_hi).abs() < 1e-12);
                }
            }
        }
        // At ordinary fit floors, one scale for both dimensions rejects a valid
        // headwind pass even though its equivalent fitted speed is below 0.8 c_fit.
        let prop = Prop {
            c: 320.0,
            w: [-80.0, 0.0, 0.0],
        };
        let (speed, distance) = prop.fit_geometry([0.0, 3000.0, 0.0], v, 343.0).unwrap();
        assert!(speed < 0.8 * 343.0);
        let legacy = prop.fit_scale(v, 343.0);
        assert!((speed * 0.95 - 3.0) * legacy > norm(v));
        let (speed_scale, [_, distance_scale]) = prop.fit_gate_scales(v, 343.0).unwrap();
        assert!((speed * speed_scale - 180.0).abs() < 1e-10);
        assert!((distance * distance_scale - 3000.0).abs() < 1e-9);
        assert!((speed_scale - distance_scale).abs() > 0.1);
    }

    #[test]
    fn fitted_geometry_refuses_invalid_or_degenerate_curves() {
        let rv = [0.0, 3000.0, 1200.0];
        let v = [180.0, 0.0, 0.0];
        for prop in [
            Prop::still(0.0),
            Prop::still(-343.0),
            Prop::still(f64::NAN),
            Prop::still(f64::INFINITY),
            Prop {
                c: 343.0,
                w: [343.0, 0.0, 0.0],
            },
            Prop {
                c: 343.0,
                w: [f64::NAN, 0.0, 0.0],
            },
            Prop {
                c: 343.0,
                w: [-200.0, 0.0, 0.0],
            },
        ] {
            assert!(prop.fit_geometry(rv, v, 343.0).is_none());
            assert!(prop.fit_gate_scales(v, 343.0).is_none());
        }
        let prop = Prop {
            c: 343.0,
            w: [-80.0, 0.0, 0.0],
        };
        for c_fit in [0.0, -343.0, f64::NAN, f64::INFINITY] {
            assert!(prop.fit_geometry(rv, v, c_fit).is_none());
            assert!(prop.fit_gate_scales(v, c_fit).is_none());
        }
        for v in [
            [0.0; 3],
            [500.0, 0.0, 0.0],
            [f64::NAN, 0.0, 0.0],
            [f64::INFINITY, 0.0, 0.0],
        ] {
            assert!(prop.fit_geometry(rv, v, 343.0).is_none());
            assert!(prop.fit_gate_scales(v, 343.0).is_none());
        }
        for rv in [
            [0.0; 3],
            [3000.0, 0.0, 0.0],
            [f64::NAN, 0.0, 0.0],
            [0.0, f64::INFINITY, 0.0],
        ] {
            assert!(prop.fit_geometry(rv, v, 343.0).is_none());
        }
    }

    #[test]
    fn source_tone_scale_matches_independent_wind_asymptotes() {
        let emitted = 350.0;
        let rotate = |v: V3| [v[2], v[0], v[1]];
        for c in [300.0, 343.0, 360.0] {
            for speed in [70.0, 175.0, 245.0] {
                for wind in [-35.0, 0.0, 35.0] {
                    // Along the flight, sound travels at c +/- wind in the ground frame.
                    // A still-air fit's f0 is the harmonic mean of its two asymptotes.
                    let approach = emitted / (1.0 - speed / (c + wind));
                    let recede = emitted / (1.0 + speed / (c - wind));
                    let measured = 2.0 * approach * recede / (approach + recede);
                    let v = [speed, 0.0, 0.0];
                    let w = [wind, 0.0, 0.0];
                    let factor = Prop { c, w }.source_tone_scale(v);
                    assert!((measured * factor - emitted).abs() < 1e-10);
                    assert_eq!(
                        Prop { c, w: rotate(w) }.source_tone_scale(rotate(v)),
                        factor
                    );
                    if wind == 0.0 {
                        assert_eq!(factor, 1.0);
                    }
                }
            }
        }
    }

    #[test]
    fn invalid_medium_or_velocity_cannot_supply_source_tone_bounds() {
        for prop in [
            Prop::still(0.0),
            Prop::still(-343.0),
            Prop::still(f64::NAN),
            Prop::still(f64::INFINITY),
            Prop {
                c: 343.0,
                w: [343.0, 0.0, 0.0],
            },
            Prop {
                c: 343.0,
                w: [f64::NAN, 0.0, 0.0],
            },
        ] {
            assert!(prop.source_tone_scale([175.0, 0.0, 0.0]).is_nan());
        }
        assert!(
            Prop::still(343.0)
                .source_tone_scale([f64::NAN, 0.0, 0.0])
                .is_nan()
        );
        assert!(
            Prop {
                c: 343.0,
                w: [300.0, 0.0, 0.0],
            }
            .source_tone_scale([-500.0, 0.0, 0.0])
            .is_nan()
        );
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
    fn turning_emission_recovers_a_newton_two_cycle() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let x = enu.to_ecef([6000.0, 0.0, 0.0]);
        for base in [0.0, 1_788_895_000.0] {
            for direction in [-1.0, 1.0] {
                let state = |t: f64| {
                    let angle = direction * 0.06 * t;
                    (
                        [3000.0 * angle.cos(), 3000.0 * angle.sin(), 900.0],
                        [
                            -direction * 180.0 * angle.sin(),
                            direction * 180.0 * angle.cos(),
                            0.0,
                        ],
                    )
                };
                let mut tr = straight_track(
                    &enu,
                    [0.0; 3],
                    [180.0, 0.0, 0.0],
                    base,
                    base - 120.0,
                    base + 120.0,
                    1.0,
                );
                for (i, &time) in tr.t.iter().enumerate() {
                    let (p, v) = state(time - base);
                    tr.pos[i] = enu.to_ecef(p);
                    tr.vel[i] = enu.vec_to_ecef(v);
                }
                let emitted = base - 20.0;
                let (p, v) = state(-20.0);
                let rv = sub(p, [6000.0, 0.0, 0.0]);
                for w in [[0.0; 3], [20.0, -15.0 * direction, 0.0]] {
                    let prop = Prop {
                        c: 343.0,
                        w: enu.vec_to_ecef(w),
                    };
                    let received = emitted + wavefront_travel(rv, w, prop.c);
                    // Pin the defect without copying the old solver: these two Newton steps
                    // repeat forever, despite strictly subsonic 180 m/s motion everywhere.
                    if w == [0.0; 3] {
                        let step = |t| {
                            let (p, v) = tr.pv(t);
                            let rv = sub(p, x);
                            let (travel, rate) = prop.travel_and_rate(rv, norm(rv), v);
                            (t - (t + travel - received) / (1.0 + rate))
                                .clamp(tr.t0(), tr.t1())
                        };
                        let jump = step(tr.t0());
                        assert!(jump > base + 80.0);
                        assert_eq!(step(jump), tr.t0());
                    }
                    let dt = 1e-3;
                    let expected_ratio = 1.0
                        / (1.0
                            + (wavefront_travel(add(rv, scale(v, dt)), w, prop.c)
                                - wavefront_travel(sub(rv, scale(v, dt)), w, prop.c))
                                / (2.0 * dt));
                    for warm in [None, Some(tr.t0()), Some(tr.t1()), Some(emitted)] {
                        let mut hint = tr.t.len() - 2;
                        let (solved, range, ratio) = tr
                            .solve_emission_sweep(received, x, &prop, warm, &mut hint, 1e-7)
                            .expect("a unique physical reception must survive a Newton cycle");
                        assert!((solved - emitted).abs() < 1e-6);
                        assert!((range - norm(rv)).abs() < 1e-3);
                        assert!((ratio - expected_ratio).abs() < 1e-7);
                        assert_eq!(hint, tr.seg(solved));
                        assert_eq!(
                            tr.solve_emission_p(received, x, &prop, warm),
                            Some((solved, range, ratio))
                        );
                        if w == [0.0; 3] {
                            let (still_t, still_r, radial) =
                                tr.solve_emission(received, x, prop.c, warm).unwrap();
                            assert!((still_t - emitted).abs() < 1e-6);
                            assert!((still_r - norm(rv)).abs() < 1e-3);
                            assert!((radial - dot(rv, v) / norm(rv)).abs() < 1e-3);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn turning_emission_recovers_without_a_warm_start() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let x = enu.to_ecef([40_000.0, 0.0, 0.0]);
        // A wider, slower turn needs no bad warm hint to lose Newton convergence. The
        // source stays subsonic at 270 m/s, turning 1.55 degrees/s on a 10 km radius.
        for base in [0.0, 1_788_895_000.0] {
            let state = |t: f64| {
                let angle = 0.027 * t;
                (
                    [10_000.0 * angle.cos(), 10_000.0 * angle.sin(), 900.0],
                    [-270.0 * angle.sin(), 270.0 * angle.cos(), 0.0],
                )
            };
            let mut tr = straight_track(
                &enu,
                [0.0; 3],
                [270.0, 0.0, 0.0],
                base,
                base - 120.0,
                base + 120.0,
                1.0,
            );
            for (i, &time) in tr.t.iter().enumerate() {
                let (p, v) = state(time - base);
                tr.pos[i] = enu.to_ecef(p);
                tr.vel[i] = enu.vec_to_ecef(v);
            }
            let emitted = base + 20.0;
            let (p, v) = state(20.0);
            let rv = sub(p, [40_000.0, 0.0, 0.0]);
            let received = emitted + wavefront_travel(rv, [0.0; 3], 343.0);
            let expected_radial = dot(rv, v) / norm(rv);
            let (solved, range, ratio) = tr
                .solve_emission_p(received, x, &Prop::still(343.0), None)
                .expect("cold emission solve must retain the physical curved pass");
            assert!((solved - emitted).abs() < 1e-6);
            assert!((range - norm(rv)).abs() < 1e-3);
            assert!((ratio - 1.0 / (1.0 + expected_radial / 343.0)).abs() < 1e-7);
            let (still_t, still_r, radial) = tr.solve_emission(received, x, 343.0, None).unwrap();
            assert!((still_t - emitted).abs() < 1e-6);
            assert!((still_r - norm(rv)).abs() < 1e-3);
            assert!((radial - expected_radial).abs() < 1e-3);
        }
    }

    #[test]
    fn emission_recovery_keeps_track_coverage_boundaries() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let x = enu.to_ecef([0.0; 3]);
        let tr = straight_track(
            &enu,
            [0.0, 2000.0, 1200.0],
            [180.0, 0.0, 0.0],
            1000.0,
            800.0,
            1200.0,
            20.0,
        );
        for prop in [
            Prop::still(343.0),
            Prop {
                c: 343.0,
                w: enu.vec_to_ecef([20.0, -15.0, 0.0]),
            },
        ] {
            for emitted in [tr.t0() - 1.0, tr.t0(), tr.t1(), tr.t1() + 1.0] {
                let rv = sub(tr.pos_at(emitted), x);
                let received = emitted + prop.travel(rv, norm(rv));
                for warm in [None, Some(tr.t0()), Some(tr.t1())] {
                    assert!(tr.solve_emission_p(received, x, &prop, warm).is_none());
                    if prop.w == [0.0; 3] {
                        assert!(tr.solve_emission(received, x, prop.c, warm).is_none());
                    }
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
            let p = &corrupt.points[2];
            let local = Enu::new(p.lat, p.lon, 0.0);
            let got = local.vec_from_ecef(model.vel[2]);
            let fallback = local.vec_from_ecef(fd[2]);
            if field < 2 {
                assert!(
                    (got[0] - fallback[0]).hypot(got[1] - fallback[1]) < 1e-9,
                    "corrupt horizontal field {field} was accepted"
                );
                assert!((got[2] - p.vrate_mps as f64).abs() < 1e-9);
            } else {
                let heading = (p.track_deg as f64).to_radians();
                assert!((got[0] - p.gs_mps as f64 * heading.sin()).abs() < 1e-9);
                assert!((got[1] - p.gs_mps as f64 * heading.cos()).abs() < 1e-9);
                assert!(
                    (got[2] - fallback[2]).abs() < 1e-9,
                    "corrupt vertical rate was accepted"
                );
            }
            assert_eq!(model.reported_vel_frac, 0.8);
        }
        let mut missing_headings = leg.clone();
        missing_headings.points[1].track_deg = f32::NAN;
        missing_headings.points[3].track_deg = f32::NAN;
        let models = build(&missing_headings, &BuildParams::default());
        let model = &models[0];
        let fd = fd_velocity(&model.t, &model.pos);
        let p = &missing_headings.points[2];
        let local = Enu::new(p.lat, p.lon, 0.0);
        let got = local.vec_from_ecef(model.vel[2]);
        let fallback = local.vec_from_ecef(fd[2]);
        assert!(
            (got[0] - fallback[0]).hypot(got[1] - fallback[1]) < 1e-9,
            "an unobserved turn was assumed"
        );
        assert!((got[2] - p.vrate_mps as f64).abs() < 1e-9);
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
                let p = &corrupt.points[2];
                let local = Enu::new(p.lat, p.lon, 0.0);
                let got = local.vec_from_ecef(model.vel[2]);
                let fallback = local.vec_from_ecef(fd[2]);
                assert!(
                    (got[0] - fallback[0]).hypot(got[1] - fallback[1]) < 1e-9,
                    "{error} degree heading error was accepted"
                );
                assert!((got[2] - p.vrate_mps as f64).abs() < 1e-9);
            }
        }
    }

    // Independent position and received-frequency oracle from the analytic source states.
    // Reception times use the exact source's range; only the tested model solves emission time.
    fn analytic_flight_errors(
        model: &TrackModel,
        observer: V3,
        state: impl Fn(f64) -> (V3, V3),
    ) -> (f64, f64) {
        let (mut position, mut frequency) = (0.0_f64, 0.0_f64);
        for interval in model.t.windows(2) {
            for j in 1..10 {
                let te = interval[0] + (interval[1] - interval[0]) * j as f64 / 10.0;
                let (p, v) = state(te - 1000.0);
                position = position.max(norm(sub(model.pos_at(te), p)));
                let rv = sub(p, observer);
                let range = norm(rv);
                let received = te + range / 343.0;
                let truth_hz = 200.0 / (1.0 + dot(rv, v) / (range * 343.0));
                let (_, _, ratio) = model
                    .solve_emission_p(received, observer, &Prop::still(343.0), None)
                    .expect("analytic reception stays inside the model");
                frequency = frequency.max((200.0 * ratio - truth_hz).abs());
            }
        }
        (position, frequency)
    }

    #[test]
    fn sparse_turns_recover_horizontal_motion_despite_bad_climb_rates() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let observer = enu.to_ecef([0.0, 0.0, 0.0]);
        for degrees_per_second in [-3.0_f64, 3.0] {
            let omega = degrees_per_second.to_radians();
            for times in [
                vec![0.0, 25.0, 50.0, 75.0, 100.0],
                vec![0.0, 25.0, 53.0, 77.0, 106.0],
            ] {
                for error in [-60.0, 60.0] {
                    let mut leg = turning_leg(&enu, &times, omega);
                    for p in &mut leg.points {
                        p.vrate_mps += error;
                    }
                    let models = build(&leg, &BuildParams::default());
                    assert_eq!(models.len(), 1);
                    let model = &models[0];
                    assert_eq!(model.t.len(), times.len());
                    assert_eq!(model.reported_vel_frac, 0.0);
                    let mut old_fallback = model.clone();
                    old_fallback.vel = fd_velocity(&model.t, &model.pos);
                    let state = |t| turn_state(&enu, t, omega);
                    let recovered = analytic_flight_errors(model, observer, state);
                    let old = analytic_flight_errors(&old_fallback, observer, state);
                    assert!(recovered.0 < 30.0, "recovered position: {recovered:?}");
                    assert!(old.0 > 100.0, "old position: {old:?}");
                    assert!(
                        recovered.1 < old.1 * 0.25,
                        "Doppler: {recovered:?} vs {old:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn climbing_tracks_keep_vertical_reports_without_horizontal_metadata() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let observer = enu.to_ecef([0.0, 0.0, 0.0]);
        for sign in [-1.0, 1.0] {
            // A cubic height curve is exactly reconstructible from its true endpoint rates.
            // Finite differences alone lose those rates at every knot, especially the ends.
            let state = |t: f64| {
                let q = t - 50.0;
                (
                    enu.to_ecef([100.0 * q, 800.0, 1200.0 + sign * 0.003 * q.powi(3)]),
                    enu.vec_to_ecef([100.0, 0.0, sign * 0.009 * q.powi(2)]),
                )
            };
            for times in [
                vec![0.0, 25.0, 50.0, 75.0, 100.0],
                vec![0.0, 19.0, 43.0, 72.0, 100.0],
            ] {
                for missing in 1..=3 {
                    let mut leg = leg_from_states(&times, state);
                    for p in &mut leg.points {
                        if missing & 1 != 0 {
                            p.gs_mps = f32::NAN;
                        }
                        if missing & 2 != 0 {
                            p.track_deg = f32::NAN;
                        }
                    }
                    let models = build(&leg, &BuildParams::default());
                    assert_eq!(models.len(), 1);
                    let model = &models[0];
                    assert_eq!(model.t.len(), times.len());
                    assert_eq!(model.reported_vel_frac, 0.0);
                    let mut old_fallback = model.clone();
                    old_fallback.vel = fd_velocity(&model.t, &model.pos);
                    for (k, p) in leg.points.iter().enumerate() {
                        let local = Enu::new(p.lat, p.lon, 0.0);
                        let got = local.vec_from_ecef(model.vel[k]);
                        let fallback = local.vec_from_ecef(old_fallback.vel[k]);
                        assert!((got[2] - p.vrate_mps as f64).abs() < 1e-9);
                        assert!((got[0] - fallback[0]).hypot(got[1] - fallback[1]) < 1e-9);
                    }
                    let recovered = analytic_flight_errors(model, observer, state);
                    let old = analytic_flight_errors(&old_fallback, observer, state);
                    assert!(recovered.0 < 0.1, "recovered position: {recovered:?}");
                    assert!(old.0 > 20.0, "old position: {old:?}");
                    assert!(recovered.1 < 0.01, "recovered Doppler: {recovered:?}");
                    assert!(old.1 > 0.2, "old Doppler: {old:?}");
                }
            }
        }
    }

    #[test]
    fn partial_velocity_recovery_requires_position_support_and_keeps_joint_limit() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        for times in [[0.0, 25.0, 50.0], [0.0, 60.0, 120.0]] {
            for error in [-60.0, 60.0] {
                let mut leg = straight_leg(&enu, &times);
                for p in &mut leg.points {
                    p.gs_mps = f32::NAN;
                    p.vrate_mps += error;
                }
                let models = build(&leg, &BuildParams::default());
                assert_eq!(models.len(), 1);
                let model = &models[0];
                assert_eq!(model.vel, fd_velocity(&model.t, &model.pos));
                assert_eq!(model.reported_vel_frac, 0.0);
            }
        }
        let mut leg = straight_leg(&enu, &[0.0, 10.0, 20.0]);
        leg.points[1].gs_mps += 20.0;
        leg.points[1].vrate_mps += 20.0;
        let models = build(&leg, &BuildParams::default());
        let model = &models[0];
        let fd = fd_velocity(&model.t, &model.pos);
        // Each component fits the 25 m/s limit, but their joint residual does not.
        // Partial recovery must not reinstate the complete previously rejected vector.
        assert_eq!(model.vel[1], fd[1]);
        assert_eq!(model.reported_vel_frac, 2.0 / 3.0);
    }

    #[test]
    fn accepted_velocity_reports_remain_bitwise_unchanged() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let mut sparse = straight_leg(&enu, &[0.0, 60.0, 120.0]);
        for p in &mut sparse.points {
            p.vrate_mps += 60.0;
        }
        let mut missing_vertical = straight_leg(&enu, &[0.0, 10.0, 20.0]);
        for p in &mut missing_vertical.points {
            p.vrate_mps = f32::NAN;
        }
        for leg in [
            straight_leg(&enu, &[0.0, 10.0, 20.0]),
            turning_leg(&enu, &[0.0, 25.0, 53.0, 77.0, 106.0], 3.0_f64.to_radians()),
            missing_vertical,
            sparse, // Preserve the original !near full-report path as well.
        ] {
            let models = build(&leg, &BuildParams::default());
            assert_eq!(models.len(), 1);
            let model = &models[0];
            assert_eq!(model.reported_vel_frac, 1.0);
            let fd = fd_velocity(&model.t, &model.pos);
            for (k, p) in leg.points.iter().enumerate() {
                let local = Enu::new(p.lat, p.lon, 0.0);
                let vertical = if p.vrate_mps.is_finite() {
                    p.vrate_mps as f64
                } else {
                    local.vec_from_ecef(fd[k])[2]
                };
                let heading = (p.track_deg as f64).to_radians();
                let expected = local.vec_to_ecef([
                    p.gs_mps as f64 * heading.sin(),
                    p.gs_mps as f64 * heading.cos(),
                    vertical,
                ]);
                assert_eq!(model.vel[k].map(f64::to_bits), expected.map(f64::to_bits));
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

    #[test]
    fn local_cpas_agree_with_the_global_minimum_on_a_single_pass() {
        let enu = Enu::new(40.8765, -73.525, 0.0);
        let tr = straight_track(
            &enu,
            [0.0, 800.0, 600.0],
            [80.0, 0.0, 0.0],
            1000.0,
            600.0,
            1400.0,
            20.0,
        );
        let x = enu.to_ecef([0.0; 3]);
        for guess in [965.0, 995.0, 1050.0] {
            let global = tr.cpa_near(x, guess, 150.0).unwrap();
            let local: Vec<_> = tr.local_cpas(x, guess - 150.0, guess + 150.0).collect();
            assert_eq!(local.len(), 1, "{local:?}");
            let nearest = tr.cpa_nearest_time(x, guess, 150.0).unwrap();
            assert!(nearest.interior);
            assert!((nearest.t_e - 1000.0).abs() < 0.001, "{nearest:?}");
            assert_eq!(nearest.t_e, global.t_e);
            assert_eq!(nearest.d, global.d);
            assert_eq!(nearest.speed, global.speed);
        }
    }

    fn exhaustive_nearest_cpa(tr: &TrackModel, x: V3, guess: f64, half: f64) -> Option<Cpa> {
        tr.local_cpas(x, guess - half, guess + half).min_by(|a, b| {
            b.interior
                .cmp(&a.interior)
                .then_with(|| (a.t_e - guess).abs().total_cmp(&(b.t_e - guess).abs()))
        })
    }

    fn cpa_bits(cpa: Option<Cpa>) -> Option<(bool, [u64; 9])> {
        cpa.map(|c| {
            (
                c.interior,
                [
                    c.t_e, c.d, c.speed, c.rv[0], c.rv[1], c.rv[2], c.vel[0], c.vel[1], c.vel[2],
                ]
                .map(f64::to_bits),
            )
        })
    }

    #[test]
    fn nearest_cpa_preserves_exhaustive_results_across_tracks_and_windows() {
        let enu = Enu::new(40.8765, -73.525, 0.0);
        let mut checked = 0;
        for epoch in [0.0, 1_788_895_000.0] {
            let straight = straight_track(
                &enu,
                [0.0, 800.0, 600.0],
                [80.0, 0.0, 0.0],
                epoch + 200.0,
                epoch,
                epoch + 400.0,
                2.5,
            );
            // Repeated turning visits, with simultaneous climbs and descents, admit
            // several interior minima. The nearest in time need not be closest in range.
            let mut turning = straight.clone();
            let omega = std::f64::consts::PI / 90.0;
            for (i, &time) in turning.t.iter().enumerate() {
                let q = omega * (time - epoch - 200.0);
                turning.pos[i] = enu.to_ecef([
                    2400.0 * q.sin(),
                    1000.0 * q.cos(),
                    600.0 + 60.0 * (2.0 * q).sin(),
                ]);
                turning.vel[i] = enu.vec_to_ecef([
                    2400.0 * omega * q.cos(),
                    -1000.0 * omega * q.sin(),
                    120.0 * omega * (2.0 * q).cos(),
                ]);
            }
            let mut sparse = turning.clone();
            let keep: Vec<_> = (0..turning.t.len())
                .filter(|&i| i % 13 == 0 || i + 1 == turning.t.len())
                .collect();
            sparse.t = keep.iter().map(|&i| turning.t[i]).collect();
            sparse.pos = keep.iter().map(|&i| turning.pos[i]).collect();
            sparse.vel = keep.iter().map(|&i| turning.vel[i]).collect();
            let mut gap = turning.clone();
            gap.pos[80][1] = f64::NAN;
            for (label, tr) in [
                ("straight", straight),
                ("turning", turning),
                ("sparse", sparse),
                ("invalid segment", gap),
            ] {
                for offset in [[0.0; 3], [500.0, -1000.0, 100.0], [4500.0, 2000.0, 0.0]] {
                    let x = enu.to_ecef(offset);
                    for time in [
                        -20.0, 0.0, 0.25, 15.75, 69.5, 99.75, 100.0, 120.125, 199.125, 200.0,
                        201.0, 240.3, 399.75, 400.0, 420.0,
                    ] {
                        for half in [-5.0, 0.0, 0.25, 1.5, 15.125, 60.0, 150.0, 500.0] {
                            let guess = epoch + time;
                            assert_eq!(
                                cpa_bits(tr.cpa_nearest_time(x, guess, half)),
                                cpa_bits(exhaustive_nearest_cpa(&tr, x, guess, half)),
                                "{label}: epoch {epoch}, offset {offset:?}, guess {time}, half {half}"
                            );
                            checked += 1;
                        }
                    }
                    for (guess, half) in [
                        (f64::NAN, 150.0),
                        (f64::INFINITY, 150.0),
                        (f64::NEG_INFINITY, 150.0),
                        (epoch + 200.0, f64::NAN),
                        (epoch + 200.0, f64::INFINITY),
                        (epoch + 200.0, f64::NEG_INFINITY),
                    ] {
                        assert_eq!(
                            cpa_bits(tr.cpa_nearest_time(x, guess, half)),
                            cpa_bits(exhaustive_nearest_cpa(&tr, x, guess, half))
                        );
                        checked += 1;
                    }
                }
            }
        }
        eprintln!("nearest CPA: {checked} exhaustive comparisons retain every result bit");
    }

    #[test]
    fn nearest_cpa_preserves_time_ties_and_boundary_preference() {
        let enu = Enu::new(40.8765, -73.525, 0.0);
        let mut tr = straight_track(&enu, [0.0; 3], [1.0, 0.0, 0.0], 0.0, 0.0, 200.0, 5.0);
        for (i, &t) in tr.t.iter().enumerate() {
            // A quadratic path is represented exactly by its Hermite segments and
            // visits the same closest point at 50 and 150 s, on either side of 100 s.
            tr.pos[i] = [(t - 50.0) * (t - 150.0), 1000.0, 0.0];
            tr.vel[i] = [2.0 * t - 200.0, 0.0, 0.0];
        }
        for guess in [
            0.0, 49.75, 50.0, 99.999, 100.0, 100.001, 150.0, 150.25, 200.0,
        ] {
            for half in [0.1, 0.5, 2.0, 49.5, 50.0, 50.5, 150.0] {
                assert_eq!(
                    cpa_bits(tr.cpa_nearest_time([0.0; 3], guess, half)),
                    cpa_bits(exhaustive_nearest_cpa(&tr, [0.0; 3], guess, half)),
                    "guess {guess}, half {half}"
                );
            }
        }
    }

    #[test]
    #[ignore = "timing benchmark"]
    fn nearest_cpa_benchmark() {
        let enu = Enu::new(40.8765, -73.525, 0.0);
        let tr = straight_track(
            &enu,
            [0.0, 800.0, 600.0],
            [80.0, 0.0, 0.0],
            200.0,
            0.0,
            400.0,
            5.0,
        );
        let x = enu.to_ecef([0.0; 3]);
        let measure = |accelerated| {
            let started = std::time::Instant::now();
            let mut checksum = 0u64;
            for i in 0..20_000 {
                let guess = std::hint::black_box(197.0 + (i % 64) as f64 * 0.1);
                let cpa = if accelerated {
                    tr.cpa_nearest_time(x, guess, 150.0)
                } else {
                    exhaustive_nearest_cpa(&tr, x, guess, 150.0)
                }
                .unwrap();
                checksum = checksum
                    .wrapping_mul(31)
                    .wrapping_add(std::hint::black_box(cpa.t_e.to_bits()));
            }
            (started.elapsed(), checksum)
        };
        for trial in 0..6 {
            let (old, new) = if trial % 2 == 0 {
                (measure(false), measure(true))
            } else {
                let new = measure(true);
                (measure(false), new)
            };
            assert_eq!(old.1, new.1);
            eprintln!(
                "nearest CPA trial {trial}, 20,000 hypotheses, 150 s half-window: exhaustive {:?}, outward {:?}, ratio {:.3}",
                old.0,
                new.0,
                old.0.as_secs_f64() / new.0.as_secs_f64()
            );
        }
    }

    #[test]
    fn bounds_enclose_hermite_excursions_in_every_ecef_octant() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let base = straight_track(
            &enu,
            [0.0; 3],
            [50.0, 0.0, 0.0],
            0.0,
            0.0,
            20.0,
            20.0,
        );
        for magnitude in [0.0, 6_000_000.0, 6_000_000_000_000.0] {
            for octant in 0..8 {
                let offset: V3 = std::array::from_fn(|k| {
                    if octant & (1 << k) == 0 {
                        magnitude
                    } else {
                        -magnitude
                    }
                });
                let mut tr = base.clone();
                tr.t = vec![10.0, 30.0, 55.0];
                tr.pos = [
                    [-500.0, 0.0, -300.0],
                    [500.0, 0.0, 100.0],
                    [900.0, 700.0, 900.0],
                ]
                .map(|p| add(offset, p))
                .to_vec();
                tr.vel = vec![
                    [50.0, 300.0, 100.0],
                    [50.0, -300.0, -100.0],
                    [30.0, 40.0, 50.0],
                ];
                let bounds = tr.bounds().unwrap();
                // Both knots are south of the bowed first segment: a point-only box loses it.
                let middle = tr.pos_at(20.0);
                assert!(
                    middle[1]
                        > tr.pos
                            .iter()
                            .map(|p| p[1])
                            .fold(f64::NEG_INFINITY, f64::max)
                );
                assert!(bounds.may_reach(middle, 0.0));
                for i in 0..=400 {
                    let p = tr.pos_at(10.0 + 45.0 * i as f64 / 400.0);
                    for d in [0.0, 0.0001, 1.0, 7999.5] {
                        let x = add(p, [0.6 * d, -0.8 * d, 0.0]);
                        let actual = norm(sub(p, x));
                        assert!(
                            bounds.may_reach(x, actual),
                            "{octant}, {magnitude}, {i}, {d}"
                        );
                    }
                }
                let far = [bounds.hi[0] + 8001.0, bounds.hi[1], bounds.hi[2]];
                assert!(!bounds.may_reach(far, 8000.0));
            }
        }
    }

    #[test]
    fn interval_bounds_preserve_selected_hermite_paths_and_prune_distant_parts() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let mut tr = straight_track(
            &enu,
            [0.0, 0.0, 1000.0],
            [100.0, 0.0, 0.0],
            1000.0,
            1000.0,
            1040.0,
            10.0,
        );
        tr.vel[1] = enu.vec_to_ecef([100.0, 200.0, 0.0]);
        tr.vel[2] = enu.vec_to_ecef([100.0, -200.0, 0.0]);
        // The middle segment leaves its endpoint chord. Partial intervals must keep
        // that excursion, without admitting a later, distant part of the flight.
        for (lo, hi) in [(1010.0, 1020.0), (1012.25, 1018.75), (1015.0, 1015.0)] {
            let bounds = tr.bounds_between(lo, hi).unwrap();
            for k in 0..=100 {
                let t = lo + (hi - lo) * k as f64 / 100.0;
                assert!(bounds.may_reach(tr.pos_at(t), 0.0), "lost path at {t}");
            }
            assert!(!bounds.may_reach(tr.pos_at(1040.0), 100.0));
        }
        let full = tr.bounds().unwrap();
        for (lo, hi) in [(1000.0, 1040.0), (999.0, 1041.0)] {
            let interval = tr.bounds_between(lo, hi).unwrap();
            assert_eq!(interval.lo.map(f64::to_bits), full.lo.map(f64::to_bits));
            assert_eq!(interval.hi.map(f64::to_bits), full.hi.map(f64::to_bits));
        }
    }

    #[test]
    fn interval_bounds_refuse_unknown_ranges_and_invalid_selected_geometry() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let tr = straight_track(
            &enu,
            [0.0, 0.0, 1000.0],
            [100.0, 0.0, 0.0],
            1000.0,
            1000.0,
            1040.0,
            10.0,
        );
        for (lo, hi) in [
            (f64::NAN, 1020.0),
            (1010.0, f64::INFINITY),
            (1020.0, 1010.0),
            (900.0, 999.0),
            (1041.0, 1100.0),
        ] {
            assert!(tr.bounds_between(lo, hi).is_none());
        }
        let mut invalid = tr.clone();
        invalid.pos[1][0] = f64::NAN;
        assert!(invalid.bounds_between(1010.0, 1020.0).is_none());
        let mut invalid = tr;
        invalid.vel.truncate(1);
        assert!(invalid.bounds_between(1010.0, 1020.0).is_none());
    }

    #[test]
    fn bounds_distance_keeps_grazing_faces_edges_and_corners() {
        for offset in [-6_000_000.0, 0.0, 6_000_000.0] {
            let bounds = TrackBounds {
                lo: [offset; 3],
                hi: [offset + 100.0; 3],
            };
            for x in [bounds.lo, bounds.hi, [offset + 50.0; 3]] {
                assert!(bounds.may_reach(x, 0.0));
            }
            for direction in [-1.0, 1.0] {
                let corner = if direction < 0.0 {
                    bounds.lo
                } else {
                    bounds.hi
                };
                for delta in [[5.0, 0.0, 0.0], [3.0, 4.0, 0.0], [1.0, 2.0, 2.0]] {
                    let x = add(corner, scale(delta, direction));
                    let radius = norm(sub(x, corner));
                    assert!(
                        bounds.may_reach(x, radius),
                        "{offset}, {direction}, {delta:?}"
                    );
                    assert!(!bounds.may_reach(x, radius - 0.001));
                }
            }
            for radius in [-1.0, f64::NAN, f64::INFINITY, f64::MAX] {
                assert!(bounds.may_reach([offset + 1e100; 3], radius));
            }
            for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                assert!(bounds.may_reach([v, 0.0, 0.0], 1.0));
            }
        }
    }

    #[test]
    fn invalid_track_geometry_has_no_pruning_bound() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let base = straight_track(
            &enu,
            [0.0; 3],
            [50.0, 0.0, 0.0],
            0.0,
            0.0,
            20.0,
            20.0,
        );
        assert!(base.bounds().is_some());
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut tr = base.clone();
            tr.t[0] = bad;
            assert!(tr.bounds().is_none());
            for k in 0..3 {
                let mut tr = base.clone();
                tr.pos[1][k] = bad;
                assert!(tr.bounds().is_none());
                let mut tr = base.clone();
                tr.vel[0][k] = bad;
                assert!(tr.bounds().is_none());
            }
        }
        for end in [-1.0, 0.0] {
            let mut tr = base.clone();
            tr.t[1] = end;
            assert!(tr.bounds().is_none());
        }
        let mut tr = base.clone();
        tr.vel[0][0] = f64::MAX; // finite input whose control-point construction overflows
        assert!(tr.bounds().is_none());
        for n in 0..2 {
            let mut tr = base.clone();
            tr.pos.truncate(n);
            assert!(tr.bounds().is_none());
            let mut tr = base.clone();
            tr.vel.truncate(n);
            assert!(tr.bounds().is_none());
            let mut tr = base.clone();
            tr.t.truncate(n);
            assert!(tr.bounds().is_none());
        }
    }

    #[test]
    fn local_cpas_keep_the_interior_gate_at_clipped_boundaries() {
        let enu = Enu::new(40.8765, -73.525, 0.0);
        let tr = straight_track(
            &enu,
            [0.0, 800.0, 600.0],
            [80.0, 0.0, 0.0],
            100.0,
            0.0,
            200.0,
            5.0,
        );
        let x = enu.to_ecef([0.0; 3]);
        for (lo, hi, expected_time, interior) in [
            (20.0, 80.0, 80.0, false),
            (120.0, 180.0, 120.0, false),
            (99.75, 120.0, 100.0, false),
            (80.0, 100.25, 100.0, false),
            (99.0, 102.0, 100.0, true),
            (-50.0, 250.0, 100.0, true),
        ] {
            let cpas: Vec<_> = tr.local_cpas(x, lo, hi).collect();
            assert_eq!(cpas.len(), 1, "[{lo}, {hi}]: {cpas:?}");
            assert_eq!(cpas[0].interior, interior, "[{lo}, {hi}]: {cpas:?}");
            assert!((cpas[0].t_e - expected_time).abs() < 0.001, "{cpas:?}");
        }
        assert_eq!(tr.local_cpas(x, 220.0, 250.0).count(), 0);
        assert_eq!(tr.local_cpas(x, -50.0, -10.0).count(), 0);
        assert_eq!(tr.local_cpas(x, 120.0, 100.0).count(), 0);
        assert_eq!(tr.local_cpas(x, 100.0, 100.0).count(), 0);
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

    fn position_only_leg(
        speed: f64,
        heading: f64,
        epoch: f64,
        step: f64,
    ) -> (FlightLeg, TrackModel, Enu) {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let truth = straight_track(
            &enu,
            [0.0, 0.0, 900.0],
            [speed * heading.cos(), speed * heading.sin(), 0.0],
            epoch + 90.0,
            epoch,
            epoch + 180.0,
            step,
        );
        let points = truth
            .t
            .iter()
            .zip(&truth.pos)
            .map(|(&t, &pos)| {
                let (lat, lon, alt) = ecef_to_geodetic(pos);
                fdop_types::store::TrackPoint {
                    t,
                    lat,
                    lon,
                    alt_m: alt as f32,
                    gs_mps: f32::NAN,
                    track_deg: f32::NAN,
                    vrate_mps: f32::NAN,
                    alt_src: alt_src::GEOM,
                    flags: 0,
                }
            })
            .collect();
        (gap_fixture_leg(points), truth, enu)
    }

    #[test]
    fn changing_speed_reports_preserve_a_supported_faster_pass() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let distance = |t: f64| {
            45.0 * t + 0.5 * (t - 420.0).clamp(0.0, 100.0).powi(2) + 100.0 * (t - 520.0).max(0.0)
        };
        let times: Vec<_> = (0..=37).map(|k| k as f64 * 20.0).collect();
        for reverse in [false, true] {
            for [east, north] in [[1.0, 0.0], [0.6, 0.8]] {
                let state = |t: f64| {
                    let q = if reverse { 740.0 - t } else { t };
                    let s = if reverse {
                        distance(740.0) - distance(q)
                    } else {
                        distance(q)
                    };
                    let speed = 45.0 + (q - 420.0).clamp(0.0, 100.0);
                    (
                        enu.to_ecef([east * s, north * s, 1200.0]),
                        enu.vec_to_ecef([east * speed, north * speed, 0.0]),
                    )
                };
                let leg = leg_from_states(&times, state);
                let models = build(&leg, &BuildParams::default());
                assert_eq!(models.len(), 1);
                let model = &models[0];
                eprintln!(
                    "changing-speed reverse={reverse} heading=({east},{north}): retained {} of {} reports, {:.1}..{:.1} s",
                    model.t.len(),
                    times.len(),
                    model.t0() - 1000.0,
                    model.t1() - 1000.0
                );
                assert_eq!(
                    model.t.len(),
                    times.len(),
                    "the faster physical pass was lost"
                );
                assert_eq!(model.reported_vel_frac, 1.0);
                let pass = if reverse { 90.0 } else { 650.0 };
                let observer = add(
                    state(pass).0,
                    enu.vec_to_ecef([-800.0 * north, 800.0 * east, -1200.0]),
                );
                let errors = analytic_flight_errors(model, observer, state);
                eprintln!("changing-speed physical errors: {errors:?}");
                assert!(errors.0 < 0.01, "position error: {errors:?}");
                assert!(errors.1 < 0.001, "Doppler error: {errors:?}");
            }
        }
    }

    #[test]
    fn changing_speed_recovery_requires_independent_local_motion_support() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let times: Vec<_> = (0..=37).map(|k| k as f64 * 20.0).collect();
        let state = |t: f64| {
            let ramp = (t - 420.0).clamp(0.0, 100.0);
            (
                enu.to_ecef([
                    45.0 * t + 0.5 * ramp * ramp + 100.0 * (t - 520.0).max(0.0),
                    0.0,
                    1200.0,
                ]),
                enu.vec_to_ecef([45.0 + ramp, 0.0, 0.0]),
            )
        };
        let physical = leg_from_states(&times, state);
        for invalid in [
            "single speed",
            "heading absent",
            "wrong heading",
            "supersonic",
            "height jump",
        ] {
            let mut leg = physical.clone();
            for p in &mut leg.points {
                if p.t < 1500.0 {
                    continue;
                }
                match invalid {
                    "single speed" => {
                        if p.t != 1640.0 {
                            p.gs_mps = f32::NAN;
                        }
                    }
                    "heading absent" => p.track_deg = f32::NAN,
                    "wrong heading" => p.track_deg += 90.0,
                    "supersonic" => p.gs_mps = 400.0,
                    "height jump" => p.alt_m += 10_000.0,
                    _ => unreachable!(),
                }
            }
            let models = build(&leg, &BuildParams::default());
            assert!(!models.is_empty(), "the earlier supported portion was lost");
            assert!(
                models.iter().all(|m| !(m.t0() < 1650.0 && m.t1() > 1650.0)),
                "{invalid} justified unsupported cruise coverage"
            );
        }
    }

    #[test]
    fn reported_speed_extension_preserves_separated_legacy_support() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        for reverse in [false, true] {
            let time = |t: f64| if reverse { 1500.0 - t } else { t };
            let point = |t: f64, east: f64, speed: f32| {
                let (lat, lon, alt) = ecef_to_geodetic(enu.to_ecef([east, 0.0, 900.0]));
                fdop_types::store::TrackPoint {
                    t: time(t),
                    lat,
                    lon,
                    alt_m: alt as f32,
                    gs_mps: speed,
                    track_deg: if reverse { 270.0 } else { 90.0 },
                    vrate_mps: 0.0,
                    alt_src: alt_src::GEOM,
                    flags: 0,
                }
            };
            let mut primary: Vec<_> = (0..100).map(|i| f64::from(i) * 0.1).collect();
            let secondary: Vec<_> = (0..99).map(|i| 500.05 + f64::from(i) * 0.1).collect();
            let mut points: Vec<_> = primary
                .iter()
                .chain(&secondary)
                .map(|&t| point(t, 45.0 * t, 45.0))
                .collect();
            // An early fast report belongs to the old winner. Extending it through the
            // later coherent fast reports would displace an independently kept slow part.
            points.extend((2..=150).map(|i| {
                let t = f64::from(i) * 10.0;
                point(t, 1000.0 + 145.0 * (t - 20.0), 145.0)
            }));
            primary.push(if reverse { 20.0 } else { 30.0 });
            points.sort_by(|a, b| a.t.total_cmp(&b.t));
            let actual: Vec<Vec<_>> = consistent_chains(&points, &BuildParams::default())
                .iter()
                .map(|chain| chain.iter().map(|&i| points[i].t).collect())
                .collect();
            let expected: Vec<Vec<_>> = [primary, secondary]
                .into_iter()
                .map(|times| {
                    let mut times: Vec<_> = times.into_iter().map(time).collect();
                    times.sort_by(f64::total_cmp);
                    times
                })
                .collect();
            assert_eq!(
                actual, expected,
                "reported extension lost a supported part, reverse={reverse}"
            );
        }
    }

    #[test]
    fn missing_speed_reports_do_not_erase_a_supported_fast_flyover() {
        let bp = BuildParams::default();
        for epoch in [0.0, 1_788_895_000.0] {
            for (speed, step) in [(215.0, 30.0), (245.0, 10.0), (270.0, 45.0)] {
                for heading in [0.0, 1.3] {
                    let (leg, truth, enu) = position_only_leg(speed, heading, epoch, step);
                    let models = build(&leg, &bp);
                    assert_eq!(models.len(), 1, "{speed} m/s, {step} s, epoch {epoch}");
                    let model = &models[0];
                    assert_eq!(model.t, truth.t, "supported positions were discarded");
                    assert_eq!(model.reported_vel_frac, 0.0);
                    let observer = enu.to_ecef([0.0; 3]);
                    let prop = Prop::still(343.0);
                    for t in [30.25, 60.5, 90.0, 121.75, 149.5] {
                        let te = epoch + t;
                        assert!(norm(sub(model.pos_at(te), truth.pos_at(te))) < 0.001);
                        assert!(norm(sub(model.pv(te).1, truth.pv(te).1)) < 0.001);
                        let range = norm(sub(truth.pos_at(te), observer));
                        let received = te + range / prop.c;
                        let (_, _, actual) = model
                            .solve_emission_p(received, observer, &prop, None)
                            .unwrap();
                        let expected =
                            prop.ratio(sub(truth.pos_at(te), observer), range, truth.pv(te).1);
                        assert!((100.0 * (actual - expected)).abs() < 0.001);
                    }
                }
            }
        }
    }

    #[test]
    fn dense_reports_do_not_hide_a_position_only_fast_chain() {
        let bp = BuildParams::default();
        for epoch in [0.0, 1_788_895_000.0] {
            for heading in [0.0_f64, 1.3] {
                let enu = Enu::new(40.876466, -73.524951, 0.0);
                let point = |dt: f64, north: f64| {
                    let along = 245.0 * (dt - 90.0);
                    let (lat, lon, alt) = ecef_to_geodetic(enu.to_ecef([
                        along * heading.cos(),
                        along * heading.sin() + north,
                        900.0,
                    ]));
                    fdop_types::store::TrackPoint {
                        t: epoch + dt,
                        lat,
                        lon,
                        alt_m: alt as f32,
                        gs_mps: f32::NAN,
                        track_deg: f32::NAN,
                        vrate_mps: f32::NAN,
                        alt_src: alt_src::GEOM,
                        flags: 0,
                    }
                };
                let sparse: Vec<_> = (0..=18).map(|k| point(f64::from(k) * 10.0, 0.0)).collect();
                assert_eq!(
                    consistent_chains(&sparse, &bp),
                    vec![(0..sparse.len()).collect::<Vec<_>>()]
                );
                for dense_time in [1.0, 81.0, 85.0, 89.0, 179.0] {
                    let mut points = sparse.clone();
                    points.push(point(dense_time, 0.0));
                    points.sort_by(|a, b| a.t.total_cmp(&b.t));
                    assert_eq!(
                        consistent_chains(&points, &bp),
                        vec![(0..points.len()).collect::<Vec<_>>()],
                        "a genuine extra report at {dense_time} s cropped the flyover, epoch {epoch}, heading {heading}"
                    );
                }
                // A separate supported rival is not an extension of the fast track.
                // More fast reports must not displace either of its accepted anchors.
                let mut ambiguous = sparse;
                ambiguous.extend([point(85.0, 100_000.0), point(86.0, 100_000.0)]);
                ambiguous.sort_by(|a, b| a.t.total_cmp(&b.t));
                let rival: Vec<_> = ambiguous
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| [epoch + 85.0, epoch + 86.0].contains(&p.t))
                    .map(|(i, _)| i)
                    .collect();
                assert_eq!(consistent_chains(&ambiguous, &bp), vec![rival]);
            }
        }
    }

    #[test]
    fn dense_position_reports_preserve_the_whole_fast_flyover_geometry() {
        let bp = BuildParams::default();
        for epoch in [0.0, 1_788_895_000.0] {
            for (speed, step, extra_time) in [
                (215.0, 30.0, 88.0),
                (245.0, 10.0, 85.0),
                (270.0, 45.0, 88.0),
            ] {
                for heading in [0.0, 1.3] {
                    let (mut leg, truth, enu) = position_only_leg(speed, heading, epoch, step);
                    let extra_time = epoch + extra_time;
                    let (lat, lon, alt) = ecef_to_geodetic(truth.pos_at(extra_time));
                    let mut extra = leg.points[0];
                    extra.t = extra_time;
                    extra.lat = lat;
                    extra.lon = lon;
                    extra.alt_m = alt as f32;
                    leg.points.push(extra);
                    let mut expected_times = truth.t.clone();
                    expected_times.push(extra_time);
                    expected_times.sort_by(f64::total_cmp);
                    let models = build(&leg, &bp);
                    assert_eq!(models.len(), 1, "{speed} m/s, {step} s, epoch {epoch}");
                    let model = &models[0];
                    assert_eq!(
                        model.t, expected_times,
                        "a denser valid report lost track coverage"
                    );
                    assert_eq!(model.reported_vel_frac, 0.0);
                    let observer = enu.to_ecef([0.0; 3]);
                    let prop = Prop::still(343.0);
                    for offset in [30.25, 60.5, 90.0, 121.75, 149.5] {
                        let te = epoch + offset;
                        assert!(norm(sub(model.pos_at(te), truth.pos_at(te))) < 0.001);
                        assert!(norm(sub(model.pv(te).1, truth.pv(te).1)) < 0.001);
                        let range = norm(sub(truth.pos_at(te), observer));
                        let received = te + range / prop.c;
                        let (_, _, actual) = model
                            .solve_emission_p(received, observer, &prop, None)
                            .unwrap();
                        let expected =
                            prop.ratio(sub(truth.pos_at(te), observer), range, truth.pv(te).1);
                        assert!((100.0 * (actual - expected)).abs() < 0.001);
                    }
                }
            }
        }
    }

    #[test]
    fn position_only_fast_extension_preserves_separated_legacy_support() {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let bp = BuildParams::default();
        for reverse in [false, true] {
            let time = |t: f64| if reverse { 1500.0 - t } else { t };
            let point = |t: f64, east: f64| {
                let (lat, lon, alt) = ecef_to_geodetic(enu.to_ecef([east, 0.0, 900.0]));
                fdop_types::store::TrackPoint {
                    t: time(t),
                    lat,
                    lon,
                    alt_m: alt as f32,
                    gs_mps: f32::NAN,
                    track_deg: f32::NAN,
                    vrate_mps: f32::NAN,
                    alt_src: alt_src::GEOM,
                    flags: 0,
                }
            };
            let primary: Vec<_> = (0..100).map(|i| f64::from(i) * 0.1).collect();
            let secondary: Vec<_> = (0..99).map(|i| 500.05 + f64::from(i) * 0.1).collect();
            let mut points: Vec<_> = primary
                .iter()
                .chain(&secondary)
                .map(|&t| point(t, 0.0))
                .collect();
            // This coherent faster alternative can extend the primary but conflicts
            // with the separated part that already has its own supported reports.
            points.extend((2..=150).map(|i| {
                let t = f64::from(i) * 10.0;
                point(t, 245.0 * (t - 10.0))
            }));
            points.sort_by(|a, b| a.t.total_cmp(&b.t));
            let actual: Vec<Vec<_>> = consistent_chains(&points, &bp)
                .iter()
                .map(|chain| chain.iter().map(|&i| points[i].t).collect())
                .collect();
            let expected: Vec<Vec<_>> = [primary, secondary]
                .into_iter()
                .map(|times| {
                    let mut times: Vec<_> = times.into_iter().map(time).collect();
                    times.sort_by(f64::total_cmp);
                    times
                })
                .collect();
            assert_eq!(
                actual, expected,
                "fast extension lost supported disjoint coverage, reverse={reverse}"
            );
        }
    }

    #[test]
    fn position_only_fast_recovery_requires_coherent_subsonic_support() {
        let bp = BuildParams::default();
        let (mut leg, truth, enu) = position_only_leg(245.0, 0.0, 0.0, 10.0);
        let mut corrupt = leg.points[8];
        corrupt.t += 5.0;
        let (lat, lon, alt) = ecef_to_geodetic(enu.to_ecef([0.0, 100_000.0, 900.0]));
        (corrupt.lat, corrupt.lon, corrupt.alt_m) = (lat, lon, alt as f32);
        leg.points.push(corrupt);
        let models = build(&leg, &bp);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].t, truth.t, "isolated position jump was retained");

        let (mut teleport, _, _) = position_only_leg(5000.0, 0.0, 0.0, 10.0);
        for p in &mut teleport.points {
            p.alt_m = 900.0;
        }
        assert!(build(&teleport, &bp).is_empty());
        let (mut corner, _, enu) = position_only_leg(270.0, 0.0, 0.0, 30.0);
        corner.points.truncate(3);
        for (p, xy) in corner.points.iter_mut().zip([
            [0.0, 0.0],
            [8100.0, 0.0],
            [12_150.0, 8100.0 * 3.0_f64.sqrt() / 2.0],
        ]) {
            let (lat, lon, alt) = ecef_to_geodetic(enu.to_ecef([xy[0], xy[1], 900.0]));
            (p.lat, p.lon, p.alt_m) = (lat, lon, alt as f32);
        }
        assert!(
            build(&corner, &bp).is_empty(),
            "inconsistent directions justified a recovery"
        );
        // These three reports imply opposing 270 m/s chords. Their scalar speeds
        // agree, but they do not corroborate a velocity or justify a wider filter.
        let (mut alternating, _, enu) = position_only_leg(270.0, 0.0, 0.0, 30.0);
        alternating.points.truncate(3);
        for (i, p) in alternating.points.iter_mut().enumerate() {
            let (lat, lon, alt) =
                ecef_to_geodetic(enu.to_ecef([if i == 1 { 8100.0 } else { 0.0 }, 0.0, 900.0]));
            (p.lat, p.lon, p.alt_m) = (lat, lon, alt as f32);
        }
        // The old cleaner keeps the two matching endpoints; recovery must leave
        // this already-supported result alone, not insert the intervening jump.
        let models = build(&alternating, &bp);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].t, vec![0.0, 60.0]);

        for report in [0.0, -1.0, 80.0] {
            let (mut leg, _, _) = position_only_leg(270.0, 0.0, 0.0, 45.0);
            for p in &mut leg.points {
                p.gs_mps = report;
            }
            assert!(
                build(&leg, &bp).is_empty(),
                "finite report {report} was overridden"
            );
        }

        // A supported rival already wins this ambiguous leg. Position-based recovery
        // must not replace it just because faster unsupported reports also exist.
        let (mut mixed, _, enu) = position_only_leg(245.0, 0.0, 0.0, 10.0);
        let mut rival = mixed.clone();
        rival.points.truncate(2);
        for (i, p) in rival.points.iter_mut().enumerate() {
            let (lat, lon, alt) = ecef_to_geodetic(enu.to_ecef([0.0, 100_000.0, 900.0]));
            p.t = 85.0 + i as f64;
            (p.lat, p.lon, p.alt_m) = (lat, lon, alt as f32);
        }
        let expected = build(&rival, &bp);
        mixed.points.extend(rival.points);
        let actual = build(&mixed, &bp);
        assert_eq!(actual.len(), 1);
        assert_same_gap_model(&actual[0], &expected[0]);
    }

    fn gap_fixture_point(t: f64, north: f64) -> fdop_types::store::TrackPoint {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let bad = north != 0.0;
        let p = enu.to_ecef([
            100.0 * (t - 1000.0),
            north,
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
            // Position corruption need not imply a missing geometric height. Keep these
            // reports through the separate GNSS bridge filter to exercise chain selection.
            alt_src: alt_src::GEOM,
            flags: if bad { pt_flags::MLAT } else { 0 },
        }
    }

    fn gap_fixture_leg(points: Vec<fdop_types::store::TrackPoint>) -> FlightLeg {
        FlightLeg {
            key: "synthetic-coverage-gap".into(),
            icao: "000000".into(),
            reg: String::new(),
            typ: "PC12".into(),
            source: "synthetic".into(),
            engine: EngineClass::Turboprop,
            points,
        }
    }

    fn gap_fixture_bridge(n: usize) -> Vec<fdop_types::store::TrackPoint> {
        (0..n)
            .map(|i| gap_fixture_point(1081.0 + 318.0 * i as f64 / (n - 1) as f64, 100_000.0))
            .collect()
    }

    fn assert_same_gap_model(actual: &TrackModel, expected: &TrackModel) {
        assert_eq!(
            (&actual.key, &actual.icao, &actual.reg, &actual.typ, &actual.source, actual.engine),
            (&expected.key, &expected.icao, &expected.reg, &expected.typ, &expected.source, expected.engine)
        );
        assert_eq!(
            (actual.t.len(), actual.pos.len(), actual.vel.len()),
            (expected.t.len(), expected.pos.len(), expected.vel.len())
        );
        let bits = |m: &TrackModel| {
            m.t.iter()
                .chain(m.pos.iter().flatten())
                .chain(m.vel.iter().flatten())
                .map(|v| v.to_bits())
                .chain([m.reported_vel_frac.to_bits(), m.mlat_frac.to_bits()])
                .collect::<Vec<_>>()
        };
        assert_eq!(bits(actual), bits(expected));
    }

    #[test]
    fn rejected_reports_cannot_erase_either_side_of_a_real_coverage_gap() {
        let p = BuildParams::default();
        for (left, right) in [(80usize, 81usize), (81, 80)] {
            let points: Vec<_> = (0..left)
                .chain(400..400 + right)
                .map(|i| gap_fixture_point(1000.0 + i as f64, 0.0))
                .collect();
            let leg = gap_fixture_leg(points);
            let clean = build(&leg, &p);
            assert_eq!(clean.len(), 2);
            assert!(clean[1].t0() - clean[0].t1() > p.max_gap_s);
            let winner = usize::from(right > left);
            for bad_count in [3, 49] {
                let mut dirty_leg = leg.clone();
                dirty_leg.points.extend(gap_fixture_bridge(bad_count));
                let dirty = build(&dirty_leg, &p);
                let spans: Vec<_> = dirty.iter().map(|m| (m.t0(), m.t1(), m.t.len())).collect();
                assert_eq!(dirty.len(), 2, "{left}/{right}, {bad_count} false reports: {spans:?}");
                // The old winner remains first, with the old unsplit key and identical
                // positions, velocities and quality fractions. The restored part is separate.
                let mut primary = clean[winner].clone();
                primary.key = leg.key.clone();
                assert_same_gap_model(&dirty[0], &primary);
                let mut recovered = clean[1 - winner].clone();
                recovered.key = format!("{}#recovered0", leg.key);
                assert_same_gap_model(&dirty[1], &recovered);
                assert_eq!(dirty.iter().map(|m| m.t.len()).sum::<usize>(), left + right);
                assert!(
                    dirty.iter().all(|m| m.t.windows(2).all(|w| w[1] - w[0] <= p.max_gap_s)),
                    "the cleaned coverage hole must never be interpolated"
                );
            }
        }
    }

    #[test]
    fn a_denser_ambiguous_component_keeps_the_original_winner() {
        let bad = gap_fixture_bridge(97);
        let expected = build(&gap_fixture_leg(bad.clone()), &BuildParams::default());
        let mut points: Vec<_> = (0..80)
            .chain(400..480)
            .map(|i| gap_fixture_point(1000.0 + i as f64, 0.0))
            .collect();
        points.extend(bad);
        let actual = build(&gap_fixture_leg(points), &BuildParams::default());
        // Neither 80-point component defeats the 97-point winner. Its identity is ambiguous;
        // recovery must not replace it by joining the two shorter components over a long gap.
        assert_eq!(actual.len(), 1);
        assert_same_gap_model(&actual[0], &expected[0]);
    }

    #[test]
    fn overlapping_coherent_reports_do_not_replace_a_complete_track() {
        let mut leg = gap_fixture_leg(
            (0..=1000).map(|i| gap_fixture_point(1000.0 + i as f64, 0.0)).collect(),
        );
        let expected = build(&leg, &BuildParams::default());
        // Unlimited predecessor lookback could jump from the early genuine track to these
        // dense late reports and outvote the genuine middle. The existing winning path is
        // complete; an overlapping alternative must remain rejected in its entirety.
        leg.points.extend((0..800).map(|i| {
            gap_fixture_point(1900.0 + (i / 8) as f64 + 0.1 * (i % 8 + 1) as f64, 100_000.0)
        }));
        let actual = build(&leg, &BuildParams::default());
        assert_eq!(actual.len(), 1);
        assert_same_gap_model(&actual[0], &expected[0]);
    }

    #[test]
    fn unreachable_disjoint_tracks_are_not_recovered() {
        let leg = gap_fixture_leg(
            (0..=80).map(|i| gap_fixture_point(1000.0 + i as f64, 0.0)).collect(),
        );
        let expected = build(&leg, &BuildParams::default());
        for direction in [-1.0, 1.0] {
            let mut dirty = leg.clone();
            let time = |t: f64| 1040.0 + direction * (t - 1040.0);
            // A coherent 40-point tail is separated by more than max_gap_s, but cannot
            // physically reach the winner. Isolated outliers fill the raw timestamp gap.
            dirty.points.extend(
                (0..40).map(|i| gap_fixture_point(time(1500.0 + i as f64), 100_000.0)),
            );
            for (t, north) in [
                (1180.0, 300_000.0),
                (1280.0, -300_000.0),
                (1380.0, 500_000.0),
                (1480.0, -500_000.0),
            ] {
                dirty.points.push(gap_fixture_point(time(t), north));
            }
            let actual = build(&dirty, &BuildParams::default());
            assert_eq!(actual.len(), 1, "direction {direction}: {actual:?}");
            assert_same_gap_model(&actual[0], &expected[0]);
        }
    }

    #[test]
    fn recovered_parts_do_not_rename_later_original_models() {
        let later = (2000..2080).map(|i| gap_fixture_point(1000.0 + i as f64, 0.0));
        let original: Vec<_> = (400..481)
            .map(|i| gap_fixture_point(1000.0 + i as f64, 0.0))
            .chain(later)
            .collect();
        let mut leg = gap_fixture_leg(original);
        let expected = build(&leg, &BuildParams::default());
        assert_eq!(expected.len(), 2);
        leg.points.extend((0..80).map(|i| gap_fixture_point(1000.0 + i as f64, 0.0)));
        leg.points.extend(gap_fixture_bridge(3));
        let actual = build(&leg, &BuildParams::default());
        assert_eq!(actual.len(), 3);
        assert_same_gap_model(&actual[0], &expected[0]);
        assert_same_gap_model(&actual[2], &expected[1]);
        assert_eq!(actual[1].key, format!("{}#recovered0", expected[0].key));
        assert_eq!(actual.iter().map(|m| m.t.len()).sum::<usize>(), 241);
    }

    #[test]
    fn a_recovered_part_still_needs_two_consistent_reports() {
        for count in [1, 2] {
            let mut leg = gap_fixture_leg(
                (400..481).map(|i| gap_fixture_point(1000.0 + i as f64, 0.0)).collect(),
            );
            leg.points.extend((0..count).map(|i| gap_fixture_point(1000.0 + i as f64, 0.0)));
            // All raw intervals stay under max_gap_s, including the sparse genuine prefix.
            for t in [1100.0, 1250.0, 1399.0] {
                leg.points.push(gap_fixture_point(t, 100_000.0));
            }
            let actual = build(&leg, &BuildParams::default());
            assert_eq!(actual.len(), count, "{count} earlier report(s): {actual:?}");
            assert_eq!(actual[0].t.len(), 81);
            if count == 2 {
                assert_eq!(actual[1].t, vec![1000.0, 1001.0]);
            }
        }
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

    const NO_BRIDGE: BuildParams = BuildParams {
        max_gap_s: 180.0,
        vel_consistency_mps: 25.0,
        max_vz_mps: 30.0,
        pos_slack_m: (250.0, 120.0),
        gnss_bridge_s: 0.0,
    };
    // Tone and sound speed for quoting Doppler in Hz.
    const F0_HZ: f64 = 100.0;
    const C_MPS: f64 = 343.0;
    // The best a trace is ever trusted to: sigma_hz at the 100 Hz tone is 0.002 f0 = 0.2 Hz, with
    // no fit residual. A knot rule must leave the model error well below that.
    const TRACE_FLOOR_HZ: f64 = 0.2;

    // Points without GNSS height every 7.5 s, between GNSS-height points every `gnss_every` s.
    // Offsets (east, north; m) repeat a fixed pattern with a median near the measured 89 m, all
    // inside the chain's slack; reports are coarse, as on the real points. `noisy` false leaves
    // them exact, so only their dropping is under test.
    fn mixed_leg(
        span: (f64, f64),
        gnss_every: f64,
        noisy: bool,
        state: impl Fn(f64) -> (V3, V3),
    ) -> FlightLeg {
        const OFFSETS: [(f64, f64); 8] = [
            (90.0, -40.0),
            (-120.0, 60.0),
            (70.0, 110.0),
            (-60.0, -100.0),
            (140.0, 30.0),
            (-30.0, 80.0),
            (110.0, -70.0),
            (-150.0, -20.0),
        ];
        const REPORTS: [(f32, f32); 4] = [(6.0, 4.0), (-5.0, -3.0), (3.0, 5.0), (-8.0, -4.0)];
        let count = ((span.1 - span.0) / gnss_every).round() as usize;
        let gnss: Vec<f64> = (0..=count)
            .map(|k| span.0 + k as f64 * gnss_every)
            .collect();
        let mut times = gnss.clone();
        let mut t = span.0 + 3.75;
        while t < span.1 {
            if gnss.iter().all(|g| (g - t).abs() > 1.0) {
                times.push(t);
            }
            t += 7.5;
        }
        times.sort_by(f64::total_cmp);
        let mut leg = leg_from_states(&times, state);
        let mut k = 0;
        for (point, t) in leg.points.iter_mut().zip(&times) {
            if gnss.contains(t) {
                continue;
            }
            point.alt_src = alt_src::BARO_CORRECTED;
            if noisy {
                let (east, north) = OFFSETS[k % OFFSETS.len()];
                let (gs, track) = REPORTS[k % REPORTS.len()];
                point.lat += north / 110_540.0;
                point.lon += east / (111_320.0 * point.lat.to_radians().cos());
                point.gs_mps += gs;
                point.track_deg = (point.track_deg + track).rem_euclid(360.0);
            }
            k += 1;
        }
        leg
    }

    fn doppler_rms_hz(
        model: &TrackModel,
        observer: V3,
        span: (f64, f64),
        state: impl Fn(f64) -> (V3, V3),
    ) -> f64 {
        let medium = Prop::still(C_MPS);
        let ratio = |p: V3, v: V3| {
            let rv = sub(p, observer);
            medium.ratio(rv, norm(rv), v)
        };
        let (mut sum, mut n) = (0.0, 0.0);
        let mut t = span.0;
        while t < span.1 {
            let (p, v) = model.pv(1000.0 + t);
            let (p_true, v_true) = state(t);
            sum += (F0_HZ * (ratio(p, v) - ratio(p_true, v_true))).powi(2);
            n += 1.0;
            t += 0.25;
        }
        (sum / n).sqrt()
    }

    #[test]
    fn noisy_points_without_gnss_height_do_not_bend_a_straight_pass() {
        let enu = Enu::new(40.0, -75.0, 0.0);
        let state = |t: f64| {
            (
                enu.to_ecef([60.0 * t, 0.0, 600.0]),
                enu.vec_to_ecef([60.0, 0.0, 0.0]),
            )
        };
        let span = (-200.0, 200.0);
        let leg = mixed_leg(span, 20.0, true, state);
        let observer = enu.to_ecef([0.0, 1500.0, 0.0]);
        let inner = (-120.0, 120.0);
        let without = doppler_rms_hz(&build(&leg, &NO_BRIDGE)[0], observer, inner, state);
        let with = doppler_rms_hz(
            &build(&leg, &BuildParams::default())[0],
            observer,
            inner,
            state,
        );
        assert!(with < TRACE_FLOOR_HZ, "with the rule: {with:.3} Hz");
        assert!(
            without > TRACE_FLOOR_HZ,
            "without the rule: {without:.3} Hz"
        );
    }

    #[test]
    fn a_real_turn_stays_intact_when_its_points_are_dropped() {
        let enu = Enu::new(40.0, -75.0, 0.0);
        let omega = 3.0_f64.to_radians();
        let state = |t: f64| turn_state(&enu, t, omega);
        let span = (0.0, 120.0);
        // Exact points: the rule's whole cost is the wider spline span, not the noise it removes.
        let leg = mixed_leg(span, 20.0, false, state);
        let model = &build(&leg, &BuildParams::default())[0];
        assert_eq!(model.t.len(), 7, "only the GNSS-height points remain");
        let observer = enu.to_ecef([0.0, 1500.0, 0.0]);
        let doppler = doppler_rms_hz(model, observer, span, state);
        assert!(doppler < TRACE_FLOOR_HZ, "turn Doppler {doppler:.3} Hz");
        let mut t = 0.0;
        while t < 120.0 {
            let error = norm(sub(model.pos_at(1000.0 + t), state(t).0));
            assert!(error < 15.0, "{error:.1} m off the arc at {t} s");
            t += 0.5;
        }
    }

    #[test]
    fn the_bridge_stops_where_a_turn_costs_more_than_the_noise() {
        // Sustained standard-rate turn, the worst routine case for crossing a stretch without a
        // knot. Up to the bridge, dropping the points beats keeping them; past it, it loses.
        // Several observers, since one sees only one heading. A 120 s stretch would be exactly
        // one circle at this rate, with both ends agreeing, so it is not a fair comparison.
        let enu = Enu::new(40.0, -75.0, 0.0);
        let omega = 3.0_f64.to_radians();
        let state = |t: f64| turn_state(&enu, t, omega);
        let span = (0.0, 360.0);
        let inner = (90.0, 270.0);
        let observers = [
            [0.0, 1500.0, 0.0],
            [0.0, -1500.0, 0.0],
            [1500.0, 0.0, 0.0],
            [-2500.0, 2500.0, 0.0],
            [3000.0, 3000.0, 0.0],
        ]
        .map(|p| enu.to_ecef(p));
        let errors = |gnss_every: f64| {
            let leg = mixed_leg(span, gnss_every, true, state);
            let dropped = BuildParams {
                gnss_bridge_s: gnss_every,
                ..NO_BRIDGE
            };
            let rms = |bp: &BuildParams| {
                let model = &build(&leg, bp)[0];
                let sum: f64 = observers
                    .iter()
                    .map(|&o| doppler_rms_hz(model, o, inner, state).powi(2))
                    .sum();
                (sum / observers.len() as f64).sqrt()
            };
            let (drop, keep) = (rms(&dropped), rms(&NO_BRIDGE));
            println!("{gnss_every} s: dropping {drop:.3} Hz, keeping {keep:.3} Hz");
            (drop, keep)
        };
        for s in [20.0, 30.0, 45.0, 60.0] {
            let (drop, keep) = errors(s);
            assert!(
                drop < keep,
                "{s} s: dropping {drop:.3} Hz, keeping {keep:.3} Hz"
            );
        }
        let (drop, keep) = errors(90.0);
        assert!(
            drop > keep,
            "90 s: dropping {drop:.3} Hz, keeping {keep:.3} Hz"
        );
    }

    #[test]
    fn points_the_bridge_does_not_cover_are_kept() {
        let enu = Enu::new(40.0, -75.0, 0.0);
        let times: Vec<f64> = (0..=20).map(|k| k as f64 * 5.0).collect();
        let source = |gnss: &[usize]| {
            let mut leg = straight_leg(&enu, &times);
            for (k, point) in leg.points.iter_mut().enumerate() {
                if !gnss.contains(&k) {
                    point.alt_src = alt_src::BARO_CORRECTED;
                }
            }
            let bp = BuildParams::default();
            build(&leg, &bp)[0].t.clone()
        };
        // No or one GNSS-height point: nothing brackets anything.
        assert_eq!(source(&[]).len(), times.len());
        assert_eq!(source(&[10]).len(), times.len());
        // A 100 s stretch is wider than the bridge.
        assert_eq!(source(&[0, 20]).len(), times.len());
        // Exactly the bridge is crossed; points outside the outermost GNSS-height points stay.
        assert_eq!(source(&[4, 16]).len(), times.len() - 11);
        // Only one side covered: the stretch from 25 s to 100 s is too wide.
        assert_eq!(source(&[0, 5, 20]).len(), times.len() - 4);
    }

    fn failed_gnss_brackets(
        end_s: f64,
        retain_corrected: impl Fn(f64) -> bool,
    ) -> (FlightLeg, Enu) {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let mut leg = mixed_leg((0.0, end_s), 20.0, false, |t| {
            (
                enu.to_ecef([100.0 * t, 0.0, 1200.0]),
                enu.vec_to_ecef([100.0, 0.0, 0.0]),
            )
        });
        // Missing corrected reports leave holes in their support, while the GNSS
        // reports retain the original 20 s cadence. Relabelling missing reports as
        // GNSS would create 1.25 s pairs inside the position-noise allowance.
        leg.points
            .retain(|p| p.alt_src == alt_src::GEOM || retain_corrected(p.t - 1000.0));
        for p in &mut leg.points {
            let t = p.t - 1000.0;
            if p.alt_src == alt_src::GEOM {
                // Finite, wrong positions whose 300 m/s chords contradict their
                // unchanged 100 m/s reports. Height itself remains correctly referenced.
                let (lat, lon, _) =
                    ecef_to_geodetic(enu.to_ecef([60_000.0 + 300.0 * t, 60_000.0, 1200.0]));
                (p.lat, p.lon) = (lat, lon);
            }
        }
        (leg, enu)
    }

    #[test]
    fn rejected_gnss_brackets_cannot_erase_a_corrected_height_flyover() {
        let (leg, enu) = failed_gnss_brackets(120.0, |_| true);
        let mut preferred = leg.clone();
        preferred.points.retain(|p| p.alt_src == alt_src::GEOM);
        assert!(build(&preferred, &BuildParams::default()).is_empty());
        let mut corrected = leg.clone();
        corrected
            .points
            .retain(|p| p.alt_src == alt_src::BARO_CORRECTED);
        let expected = build(&corrected, &NO_BRIDGE);
        assert_eq!(expected.len(), 1);

        let recovered = build(&leg, &BuildParams::default());
        assert_eq!(recovered.len(), 1);
        assert_same_gap_model(&recovered[0], &expected[0]);
        // The same physical path was available before the GNSS bridge removed it.
        let unbridged = build(&leg, &NO_BRIDGE);
        assert_eq!(unbridged.len(), 1);
        assert_same_gap_model(&recovered[0], &unbridged[0]);
        let state = |t| {
            (
                enu.to_ecef([100.0 * t, 0.0, 1200.0]),
                enu.vec_to_ecef([100.0, 0.0, 0.0]),
            )
        };
        for t in 10..=110 {
            let error = norm(sub(
                recovered[0].pos_at(1000.0 + t as f64),
                state(t as f64).0,
            ));
            assert!(error < 0.01, "{error} m at {t} s");
        }
        let doppler = doppler_rms_hz(
            &recovered[0],
            enu.to_ecef([6000.0, 1500.0, 0.0]),
            (10.0, 110.0),
            state,
        );
        assert!(doppler < 1e-4, "recovered Doppler RMS {doppler} Hz");
    }

    #[test]
    fn gnss_bridge_recovery_needs_consistent_corrected_height_support() {
        let (leg, _) = failed_gnss_brackets(120.0, |_| true);
        let mut raw = leg.clone();
        for p in &mut raw.points {
            if p.alt_src == alt_src::BARO_CORRECTED {
                p.alt_src = alt_src::BARO_RAW;
                p.alt_m -= 150.0;
            }
        }
        assert!(build(&raw, &BuildParams::default()).is_empty());

        // Two unverified reports, or only two that agree out of three, cannot
        // exploit the ordinary builder's two-point shortcut in this retry.
        for corrected_count in [2, 3] {
            let mut sparse = leg.clone();
            let mut kept = 0;
            sparse.points.retain(|p| {
                if p.alt_src == alt_src::GEOM {
                    true
                } else {
                    kept += 1;
                    kept <= corrected_count
                }
            });
            if corrected_count == 3 {
                sparse
                    .points
                    .iter_mut()
                    .rfind(|p| p.alt_src == alt_src::BARO_CORRECTED)
                    .unwrap()
                    .alt_m += 5000.0;
            }
            assert!(build(&sparse, &BuildParams::default()).is_empty());
        }
    }

    #[test]
    fn gnss_bridge_recovery_preserves_gaps_and_discards_two_point_fragments() {
        let (leg, _) = failed_gnss_brackets(720.0, |t| {
            t < 120.0 || (300.0..312.0).contains(&t) || t > 600.0
        });
        let mut preferred = leg.clone();
        preferred.points.retain(|p| p.alt_src == alt_src::GEOM);
        assert!(build(&preferred, &BuildParams::default()).is_empty());
        let mut corrected = leg.clone();
        corrected
            .points
            .retain(|p| p.alt_src == alt_src::BARO_CORRECTED);
        let expected = build(&corrected, &NO_BRIDGE);
        assert_eq!(expected.len(), 3);
        assert_eq!(expected[1].t.len(), 2);

        let recovered = build(&leg, &BuildParams::default());
        assert_eq!(recovered.len(), 2);
        assert!(recovered[0].t1() < 1120.0);
        assert!(recovered[1].t0() > 1600.0);
        for model in &recovered {
            assert!(model.t.len() >= 3);
            assert!(
                model
                    .t
                    .windows(2)
                    .all(|t| t[1] - t[0] <= BuildParams::default().max_gap_s)
            );
        }
        assert_same_gap_model(&recovered[0], &expected[0]);
        assert_same_gap_model(&recovered[1], &expected[2]);
    }

    fn partial_gnss_leg(retain_corrected: impl Fn(f64) -> bool) -> (FlightLeg, Enu) {
        let (mut leg, enu) = failed_gnss_brackets(1080.0, retain_corrected);
        for p in &mut leg.points {
            let t = p.t - 1000.0;
            if p.alt_src == alt_src::GEOM && (480.0..=520.0).contains(&t) {
                let (lat, lon, _) = ecef_to_geodetic(enu.to_ecef([100.0 * t, 0.0, 1200.0]));
                (p.lat, p.lon) = (lat, lon);
            }
        }
        (leg, enu)
    }

    #[test]
    fn partial_gnss_recovery_restores_distant_corrected_flyovers() {
        for gaps in [true, false] {
            let (leg, enu) = partial_gnss_leg(|t| !gaps || t < 180.0 || t > 900.0);
            let mut gnss = leg.clone();
            gnss.points.retain(|p| p.alt_src == alt_src::GEOM);
            let preferred = build(&gnss, &NO_BRIDGE);
            assert_eq!(preferred.len(), 1);
            assert_eq!(preferred[0].t, vec![1480.0, 1500.0, 1520.0]);

            // The supported portions outside 300..700 s are each separated from
            // the preferred 480..520 s segment by more than the ordinary 180 s gap.
            let mut corrected = leg.clone();
            corrected
                .points
                .retain(|p| p.alt_src == alt_src::BARO_CORRECTED && (p.t < 1300.0 || p.t > 1700.0));
            let expected = build(&corrected, &NO_BRIDGE);
            assert_eq!(expected.len(), 2);
            let recovered = build(&leg, &BuildParams::default());
            assert_eq!(
                recovered.len(),
                3,
                "both distant physical portions must survive"
            );
            assert_same_gap_model(&recovered[0], &preferred[0]);
            for (k, (model, original)) in recovered[1..].iter().zip(&expected).enumerate() {
                let mut original = original.clone();
                original.key = format!("{}#corrected{k}", leg.key);
                assert_same_gap_model(model, &original);
                assert!(model.t.len() >= 3);
                let span = (model.t0() - 1000.0 + 5.0, model.t1() - 1000.0 - 5.0);
                let state = |t| {
                    (
                        enu.to_ecef([100.0 * t, 0.0, 1200.0]),
                        enu.vec_to_ecef([100.0, 0.0, 0.0]),
                    )
                };
                let observer = enu.to_ecef([50.0 * (span.0 + span.1), 1500.0, 0.0]);
                let rms = doppler_rms_hz(model, observer, span, state);
                assert!(rms < 1e-4, "recovered physical Doppler RMS {rms} Hz");
            }
        }
    }

    #[test]
    fn partial_gnss_recovery_requires_separation_reachability_and_support() {
        let (leg, enu) = partial_gnss_leg(|t| t < 180.0 || t > 900.0);
        let mut gnss = leg.clone();
        gnss.points.retain(|p| p.alt_src == alt_src::GEOM);
        let preferred = build(&gnss, &NO_BRIDGE);
        assert_eq!(preferred.len(), 1);

        let (nearby, _) = partial_gnss_leg(|t| (300.0..=700.0).contains(&t));
        let mut raw = leg.clone();
        for p in &mut raw.points {
            if p.alt_src == alt_src::BARO_CORRECTED {
                p.alt_src = alt_src::BARO_RAW;
                p.alt_m -= 150.0;
            }
        }
        let mut sparse = leg.clone();
        sparse
            .points
            .retain(|p| p.alt_src == alt_src::GEOM || p.t < 1015.0 || p.t > 2065.0);
        assert_eq!(
            sparse
                .points
                .iter()
                .filter(|p| p.alt_src == alt_src::BARO_CORRECTED)
                .count(),
            4
        );
        let mut unreachable = leg;
        for p in &mut unreachable.points {
            if p.alt_src == alt_src::BARO_CORRECTED {
                let t = p.t - 1000.0;
                let (lat, lon, _) =
                    ecef_to_geodetic(enu.to_ecef([500_000.0 + 100.0 * t, 0.0, 1200.0]));
                (p.lat, p.lon) = (lat, lon);
            }
        }
        let mut isolated = unreachable.clone();
        isolated
            .points
            .retain(|p| p.alt_src == alt_src::BARO_CORRECTED);
        assert_eq!(build(&isolated, &NO_BRIDGE).len(), 2);
        for input in [nearby, raw, sparse, unreachable] {
            let got = build(&input, &BuildParams::default());
            assert_eq!(got.len(), 1);
            assert_same_gap_model(&got[0], &preferred[0]);
        }
    }

    fn partial_gnss_missing_speed_leg(speed: f64) -> (FlightLeg, Enu) {
        let enu = Enu::new(40.876466, -73.524951, 0.0);
        let mut leg = mixed_leg((0.0, 1080.0), 20.0, false, |t| {
            (
                enu.to_ecef([speed * t, 0.0, 1200.0]),
                enu.vec_to_ecef([speed, 0.0, 0.0]),
            )
        });
        leg.points.retain(|p| {
            let t = p.t - 1000.0;
            p.alt_src == alt_src::GEOM || t < 120.0 || t > 960.0
        });
        for p in &mut leg.points {
            let t = p.t - 1000.0;
            p.gs_mps = f32::NAN;
            if p.alt_src == alt_src::GEOM && !(480.0..=520.0).contains(&t) {
                // These false brackets exceed even the existing subsonic position
                // envelope. Only the three genuine central GNSS points may survive.
                let (lat, lon, _) =
                    ecef_to_geodetic(enu.to_ecef([600_000.0 + 500.0 * t, 600_000.0, 1200.0]));
                (p.lat, p.lon) = (lat, lon);
            }
        }
        (leg, enu)
    }

    #[test]
    fn corrected_fast_segments_use_their_preferred_neighbours_supported_speed() {
        let bp = BuildParams::default();
        for speed in [100.0, 215.0, 245.0, 270.0] {
            let (leg, enu) = partial_gnss_missing_speed_leg(speed);
            let mut gnss = leg.clone();
            gnss.points.retain(|p| p.alt_src == alt_src::GEOM);
            let preferred = build(&gnss, &NO_BRIDGE);
            assert_eq!(preferred.len(), 1, "{speed} m/s preferred support");
            assert_eq!(preferred[0].t, vec![1480.0, 1500.0, 1520.0]);
            let mut corrected = leg.clone();
            corrected
                .points
                .retain(|p| p.alt_src == alt_src::BARO_CORRECTED);
            let expected = build(&corrected, &NO_BRIDGE);
            assert_eq!(expected.len(), 2, "{speed} m/s corrected support");
            assert!(expected.iter().all(|model| model.t.len() >= 3));
            let reported = reported_speed_limit(&gnss.points);
            assert_eq!(reported, 200.0);
            let supported = corrected_recovery_speed_limit(&gnss.points, &preferred, &bp);
            assert!(supported <= fdop_types::C_SOUND);
            if speed > reported {
                assert!(!separated_models_reachable(
                    &expected[0],
                    &preferred[0],
                    reported,
                    &bp
                ));
                assert!(!separated_models_reachable(
                    &preferred[0],
                    &expected[1],
                    reported,
                    &bp
                ));
                assert!(supported > reported);
            } else {
                assert_eq!(supported, reported);
            }
            let recovered = build(&leg, &bp);
            assert_eq!(
                recovered.len(),
                3,
                "{speed} m/s must retain both separated portions"
            );
            assert_same_gap_model(&recovered[0], &preferred[0]);
            for (k, (actual, original)) in recovered[1..].iter().zip(&expected).enumerate() {
                let mut original = original.clone();
                original.key = format!("{}#corrected{k}", leg.key);
                assert_same_gap_model(actual, &original);
                let span = (actual.t0() - 1000.0 + 5.0, actual.t1() - 1000.0 - 5.0);
                let state = |t| {
                    (
                        enu.to_ecef([speed * t, 0.0, 1200.0]),
                        enu.vec_to_ecef([speed, 0.0, 0.0]),
                    )
                };
                let observer = enu.to_ecef([0.5 * speed * (span.0 + span.1), 1500.0, 0.0]);
                let rms = doppler_rms_hz(actual, observer, span, state);
                assert!(rms < 1e-4, "{speed} m/s recovered Doppler RMS {rms} Hz");
            }
        }
    }

    #[test]
    fn corrected_fast_recovery_keeps_report_and_physical_reach_limits() {
        let bp = BuildParams::default();
        let (leg, enu) = partial_gnss_missing_speed_leg(245.0);
        for report in [0.0, 140.0, f32::INFINITY] {
            let mut reported = leg.clone();
            for p in &mut reported.points {
                p.gs_mps = report;
            }
            let mut gnss = reported.clone();
            gnss.points.retain(|p| p.alt_src == alt_src::GEOM);
            let preferred = build(&gnss, &NO_BRIDGE);
            assert_eq!(
                corrected_recovery_speed_limit(&gnss.points, &preferred, &bp),
                reported_speed_limit(&gnss.points)
            );
            let actual = build(&reported, &bp);
            assert_eq!(actual.len(), preferred.len());
            for (actual, original) in actual.iter().zip(&preferred) {
                assert_same_gap_model(actual, original);
            }
        }

        // Either side's real report must independently block the missing-speed retry.
        for report_on_preferred in [false, true] {
            let mut mixed = leg.clone();
            for p in &mut mixed.points {
                if (p.alt_src == alt_src::GEOM) == report_on_preferred {
                    p.gs_mps = 140.0;
                }
            }
            let mut gnss = mixed.clone();
            gnss.points.retain(|p| p.alt_src == alt_src::GEOM);
            let preferred = build(&gnss, &NO_BRIDGE);
            assert_eq!(preferred.len(), 1);
            let mut corrected = mixed.clone();
            corrected
                .points
                .retain(|p| p.alt_src == alt_src::BARO_CORRECTED);
            assert_eq!(build(&corrected, &NO_BRIDGE).len(), 2);
            let actual = build(&mixed, &bp);
            assert_eq!(
                actual.len(),
                1,
                "finite report on preferred={report_on_preferred}"
            );
            assert_same_gap_model(&actual[0], &preferred[0]);
        }

        let mut preferred_only = leg.clone();
        preferred_only.points.retain(|p| p.alt_src == alt_src::GEOM);
        let preferred = build(&preferred_only, &NO_BRIDGE);
        assert_eq!(preferred.len(), 1);
        let mut unreachable = leg;
        for p in &mut unreachable.points {
            if p.alt_src == alt_src::BARO_CORRECTED {
                let t = p.t - 1000.0;
                let (lat, lon, _) =
                    ecef_to_geodetic(enu.to_ecef([500_000.0 + 245.0 * t, 500_000.0, 1200.0]));
                (p.lat, p.lon) = (lat, lon);
            }
        }
        let mut isolated = unreachable.clone();
        isolated
            .points
            .retain(|p| p.alt_src == alt_src::BARO_CORRECTED);
        let supported = build(&isolated, &NO_BRIDGE);
        assert_eq!(supported.len(), 2);
        assert!(supported.iter().all(|model| model.t.len() >= 3));
        let actual = build(&unreachable, &bp);
        assert_eq!(
            actual.len(),
            1,
            "internally coherent but unreachable portions stay rejected"
        );
        assert_same_gap_model(&actual[0], &preferred[0]);

        // A discarded fast rival cannot lend speed to a slower preferred segment.
        let (mut slow, enu) = partial_gnss_missing_speed_leg(100.0);
        slow.points.retain(|p| p.alt_src == alt_src::GEOM);
        for p in &mut slow.points {
            let t = p.t - 1000.0;
            if !(480.0..=520.0).contains(&t) {
                let (lat, lon, _) =
                    ecef_to_geodetic(enu.to_ecef([600_000.0 + 300.0 * t, 600_000.0, 1200.0]));
                (p.lat, p.lon) = (lat, lon);
            }
        }
        let kept = build(&slow, &NO_BRIDGE);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].t, vec![1480.0, 1500.0, 1520.0]);
        assert_eq!(
            corrected_recovery_speed_limit(&slow.points, &kept, &bp),
            200.0
        );

        // Two preferred samples do not establish agreeing secants for a new envelope.
        let mut short = preferred[0].clone();
        short.t.truncate(2);
        short.pos.truncate(2);
        short.vel.truncate(2);
        assert_eq!(
            corrected_recovery_speed_limit(&preferred_only.points, &[short], &bp),
            200.0
        );
    }

    #[test]
    fn usable_gnss_models_keep_exact_priority_over_bridge_recovery() {
        for repair_all in [false, true] {
            let (mut leg, enu) = failed_gnss_brackets(120.0, |_| true);
            for p in &mut leg.points {
                let t = p.t - 1000.0;
                if p.alt_src == alt_src::GEOM && (repair_all || (40.0..=60.0).contains(&t)) {
                    let (lat, lon, _) = ecef_to_geodetic(enu.to_ecef([100.0 * t, 0.0, 1200.0]));
                    (p.lat, p.lon) = (lat, lon);
                }
            }
            let mut preferred = leg.clone();
            preferred.points.retain(|p| p.alt_src == alt_src::GEOM);
            let expected = build(&preferred, &NO_BRIDGE);
            assert_eq!(expected.len(), 1);
            assert_eq!(expected[0].t.len(), if repair_all { 7 } else { 2 });
            let got = build(&leg, &BuildParams::default());
            assert_eq!(got.len(), expected.len());
            assert_same_gap_model(&got[0], &expected[0]);
        }
    }

    #[test]
    fn a_leg_of_gnss_height_points_is_untouched() {
        let enu = Enu::new(40.0, -75.0, 0.0);
        let times: Vec<f64> = (0..20).map(|k| k as f64 * 7.5).collect();
        let leg = turning_leg(&enu, &times, 2.0_f64.to_radians());
        let on = &build(&leg, &BuildParams::default())[0];
        let off = &build(&leg, &NO_BRIDGE)[0];
        assert_eq!(on.t, off.t);
        assert_eq!(on.pos, off.pos);
        assert_eq!(on.vel, off.vel);
    }
}
