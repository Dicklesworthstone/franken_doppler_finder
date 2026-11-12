//! Atmosphere for acoustic propagation: speed of sound and wind as functions of height,
//! interpolated in space and time.
//!
//! A sound path crosses air at different temperatures and winds. This module reduces the
//! height interval between source and observer to a harmonic-mean sound speed c and mean
//! horizontal wind w. `fdop-core` treats those averages as one homogeneous moving medium
//! during each emission-time solve; it does not trace refracted rays through the layers.
//!
//! For a stationary observer and source with ground velocity v_s, let rv = source - observer
//! and R = |rv|. In this homogeneous approximation, the emitted spherical wavefront moves
//! with the wind, so its travel time T satisfies |rv + w T| = c T. For c > |w|:
//!
//! ```text
//!   a = c^2 - w.w,  b = w.rv,  q = sqrt(b^2 + a R^2)
//!   T = (b + q) / a                 (or R^2 / (q - b) when b < 0)
//!   dT/dt_e = (rv.v_s + T w.v_s) / q
//!   t_r = t_e + T,  f / f0 = 1 / (1 + dT/dt_e)
//! ```
//!
//! The rationalized root avoids cancellation for following wind. Travel time, Doppler ratio
//! and the Newton derivative in `fdop-core::track::Prop` use this same wavefront equation,
//! including crosswind displacement. Exactness is limited to the homogeneous medium: path
//! averaging does not account for ray bending or spatial gradients of the medium.

use serde::{Deserialize, Serialize};

/// Speed of sound (m/s) in moist air at `t_c` (deg C) and relative humidity `rh` (0..1):
/// dry-air value times (1 + 0.16 h), h = water-vapour mole fraction (Magnus saturation).
pub fn sound_speed(t_c: f64, rh: f64) -> f64 {
    let dry = 331.3 * (1.0 + t_c / 273.15).max(0.1).sqrt();
    let es = 610.94 * ((17.625 * t_c) / (t_c + 243.04)).exp();
    let h = (rh.clamp(0.0, 1.0) * es / 101_325.0).clamp(0.0, 0.06);
    dry * (1.0 + 0.16 * h)
}

/// Path-averaged propagation medium: harmonic-mean sound speed and mean horizontal wind
/// (east, north; the direction the air moves *towards*), m/s.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Medium {
    pub c: f64,
    pub wind_e: f64,
    pub wind_n: f64,
}

impl Medium {
    pub fn still(c: f64) -> Self {
        Self {
            c,
            wind_e: 0.0,
            wind_n: 0.0,
        }
    }
}

/// Certified bounds on every finite, physically valid path average a provider returns.
/// Wind is a magnitude bound, independent of the observer's local east/north frame.
/// Bounds only support conservative search pruning; each candidate still needs its actual
/// medium. An unknown provider must return `None`, not an envelope from sampled locations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediumBounds {
    pub c: [f64; 2],
    pub wind_max: f64,
}

impl MediumBounds {
    fn checked(self) -> Option<Self> {
        (self.c[0] > 0.0
            && self.c[1] >= self.c[0]
            && self.c.iter().all(|c| c.is_finite())
            && self.wind_max >= 0.0
            && self.wind_max.is_finite())
        .then_some(self)
    }

    // A forward-error allowance gamma_n = n*eps/(1-n*eps), rounded outwards. The callers
    // count interpolation/normalisation operations, including cancellation in wind sums.
    fn outward(self, operations: usize) -> Option<Self> {
        self.checked()?;
        let ne = operations as f64 * f64::EPSILON;
        let gamma = ne / (1.0 - ne);
        if !(0.0..1.0).contains(&gamma) {
            return None;
        }
        Self {
            c: [
                (self.c[0] * (1.0 - gamma)).next_down(),
                (self.c[1] * (1.0 + gamma)).next_up(),
            ],
            wind_max: if self.wind_max == 0.0 {
                0.0
            } else {
                (self.wind_max * (1.0 + gamma) + operations as f64 * f64::from_bits(1)).next_up()
            },
        }
        .checked()
    }

    fn uniform(m: Medium) -> Option<Self> {
        let wind = m.wind_e.hypot(m.wind_n);
        if !(wind.is_finite() && wind < m.c) {
            return None;
        }
        Self {
            c: [m.c; 2],
            // Include rounding of the norm and its later ENU-to-ECEF rotation.
            wind_max: if wind == 0.0 {
                0.0
            } else {
                (wind * (1.0 + 16.0 * f64::EPSILON) + 16.0 * f64::from_bits(1)).next_up()
            },
        }
        .checked()
    }

    fn cover(bounds: impl IntoIterator<Item = Option<Self>>, operations: usize) -> Option<Self> {
        let mut all: Option<Self> = None;
        for bounds in bounds {
            let b = bounds?.checked()?;
            all = Some(match all {
                None => b,
                Some(a) => Self {
                    c: [a.c[0].min(b.c[0]), a.c[1].max(b.c[1])],
                    wind_max: a.wind_max.max(b.wind_max),
                },
            });
        }
        all?.outward(operations)
    }
}

/// One sounding level.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Level {
    /// Height above mean sea level (m).
    pub z_msl: f64,
    pub t_c: f64,
    /// Relative humidity 0..1.
    pub rh: f64,
    pub wind_e: f64,
    pub wind_n: f64,
}

/// Wind (east, north) from meteorological speed and *from* direction (deg).
pub fn wind_en(speed: f64, from_deg: f64) -> (f64, f64) {
    let a = from_deg.to_radians();
    (-speed * a.sin(), -speed * a.cos())
}

const Z0: f64 = -500.0;
const DZ: f64 = 50.0;
const NZ: usize = 311; // up to 15 km

/// A sounding resampled on a 50 m grid with cumulative integrals, so the path average over
/// any height interval costs two lookups.
#[derive(Clone, Debug)]
pub struct Column {
    /// cumulative int dz / c(z)
    s: Vec<f64>,
    /// cumulative int w_e dz, int w_n dz
    we: Vec<f64>,
    wn: Vec<f64>,
    bounds: Option<MediumBounds>,
}

impl Column {
    /// From levels in any order (sorted internally); linear between levels, constant beyond.
    pub fn from_levels(levels: &[Level]) -> Self {
        let mut lv: Vec<Level> = levels
            .iter()
            .copied()
            .filter(|l| l.z_msl.is_finite() && l.t_c.is_finite())
            .collect();
        lv.sort_by(|a, b| a.z_msl.total_cmp(&b.z_msl));
        lv.dedup_by(|a, b| (a.z_msl - b.z_msl).abs() < 1.0);
        if lv.is_empty() {
            lv.push(Level {
                z_msl: 0.0,
                t_c: 15.0,
                rh: 0.5,
                wind_e: 0.0,
                wind_n: 0.0,
            });
        }
        let at = |z: f64| -> (f64, f64, f64) {
            let k = lv.partition_point(|l| l.z_msl <= z);
            let (c, we, wn) = if k == 0 {
                let l = lv[0];
                (sound_speed(l.t_c, l.rh), l.wind_e, l.wind_n)
            } else if k >= lv.len() {
                let l = lv[lv.len() - 1];
                (sound_speed(l.t_c, l.rh), l.wind_e, l.wind_n)
            } else {
                let (a, b) = (lv[k - 1], lv[k]);
                let w = (z - a.z_msl) / (b.z_msl - a.z_msl);
                let t = a.t_c + w * (b.t_c - a.t_c);
                let rh = a.rh + w * (b.rh - a.rh);
                (
                    sound_speed(t, rh),
                    a.wind_e + w * (b.wind_e - a.wind_e),
                    a.wind_n + w * (b.wind_n - a.wind_n),
                )
            };
            (c, we, wn)
        };
        let mut s = vec![0.0; NZ];
        let mut we = vec![0.0; NZ];
        let mut wn = vec![0.0; NZ];
        let mut prev = at(Z0);
        for k in 1..NZ {
            let z = Z0 + k as f64 * DZ;
            let cur = at(z);
            // trapezoid on 1/c and w
            s[k] = s[k - 1] + 0.5 * DZ * (1.0 / prev.0 + 1.0 / cur.0);
            we[k] = we[k - 1] + 0.5 * DZ * (prev.1 + cur.1);
            wn[k] = wn[k - 1] + 0.5 * DZ * (prev.2 + cur.2);
            prev = cur;
        }
        let bounds = Self::cell_bounds(&s, &we, &wn);
        Self { s, we, wn, bounds }
    }

    /// Each effective cell has the slope of the stored cumulative integral, including
    /// both extrapolated tails. Bound those actual slopes, not just the input levels.
    fn cell_bounds(s: &[f64], we: &[f64], wn: &[f64]) -> Option<MediumBounds> {
        let mut b = MediumBounds {
            c: [f64::INFINITY, 0.0],
            wind_max: 0.0,
        };
        for k in 0..NZ - 1 {
            let c = 1.0 / Self::slope(s, k);
            let wind = Self::slope(we, k).hypot(Self::slope(wn, k));
            if !(c > 0.0 && c.is_finite() && wind.is_finite() && wind < c) {
                return None;
            }
            b.c[0] = b.c[0].min(c);
            b.c[1] = b.c[1].max(c);
            b.wind_max = b.wind_max.max(wind);
        }
        // Endpoint weights, the bounded middle difference, normalisation, reciprocal and
        // vector norm take fewer than 32 operations. Leave another factor of two for the
        // independently rounded cumulative-cell slopes used as the reference above.
        b.outward(64)
    }

    #[inline]
    fn slope(v: &[f64], k: usize) -> f64 {
        (v[k + 1] - v[k]) / DZ
    }

    /// Average integral slope in O(1), without subtracting large extrapolated integrals
    /// for a short distant path. At most two partial cells surround one in-grid difference.
    fn mean_slope(v: &[f64], lo: f64, hi: f64) -> f64 {
        let cell = |z: f64| {
            let mut k = (((z - Z0) / DZ).floor().max(0.0) as usize).min(NZ - 2);
            // Division can round a neighbouring float onto a grid boundary. Compare
            // against the exact grid heights so neither partial-cell weight is negative.
            if k > 0 && z < Z0 + k as f64 * DZ {
                k -= 1;
            } else if k < NZ - 2 && z >= Z0 + (k + 1) as f64 * DZ {
                k += 1;
            }
            k
        };
        let (i, j) = (cell(lo), cell(hi));
        if i == j {
            return Self::slope(v, i);
        }
        let a = Z0 + (i + 1) as f64 * DZ;
        let b = Z0 + j as f64 * DZ;
        let h = hi - lo;
        let (wa, wb, wm) = ((a - lo) / h, (hi - b) / h, (b - a) / h);
        let middle = if j > i + 1 {
            (v[j] - v[i + 1]) / (b - a)
        } else {
            0.0
        };
        (wa * Self::slope(v, i) + wb * Self::slope(v, j) + wm * middle) / (wa + wb + wm)
    }

    /// Path average between two heights (MSL).
    pub fn mean(&self, za: f64, zb: f64) -> Medium {
        let (lo, hi) = if za <= zb { (za, zb) } else { (zb, za) };
        let (lo, hi) = if hi - lo < 1.0 {
            (lo - 0.5, lo + 0.5)
        } else {
            (lo, hi)
        };
        let h = hi - lo;
        if !(lo.is_finite() && hi.is_finite() && h > 0.0 && h.is_finite()) {
            return Medium {
                c: f64::NAN,
                wind_e: f64::NAN,
                wind_n: f64::NAN,
            };
        }
        Medium {
            c: 1.0 / Self::mean_slope(&self.s, lo, hi),
            wind_e: Self::mean_slope(&self.we, lo, hi),
            wind_n: Self::mean_slope(&self.wn, lo, hi),
        }
    }
}

/// Atmosphere provider. `medium` returns the path average between heights `z_a` and `z_b`
/// (MSL) at a place and UTC time.
pub trait Atmosphere: Send + Sync {
    fn medium(&self, lat: f64, lon: f64, t: f64, z_a: f64, z_b: f64) -> Medium;
    fn describe(&self) -> String;
    /// The exact medium returned for every path, if the provider is spatially,
    /// temporally and vertically constant. Unknown or varying providers return `None`.
    fn uniform_medium(&self) -> Option<Medium> {
        None
    }
    /// A certified envelope over every place, time and representable height interval.
    /// Default providers can certify only their advertised uniform medium. Returning
    /// `None` disables bound-based pruning; sampled weather is never a certificate.
    fn medium_bounds(&self) -> Option<MediumBounds> {
        self.uniform_medium().and_then(MediumBounds::uniform)
    }
}

/// ICAO-style standard atmosphere from a surface temperature: 6.5 K/km lapse to 11 km, no
/// wind. Used offline or when no sounding is available.
pub struct StandardAtmosphere {
    pub surface_c: f64,
    pub surface_z: f64,
    pub rh: f64,
    pub wind: (f64, f64),
    col: Column,
}

impl StandardAtmosphere {
    pub fn new(surface_c: f64, surface_z: f64, rh: f64) -> Self {
        Self::with_wind(surface_c, surface_z, rh, 0.0, 0.0)
    }
    /// Same, with a height-independent wind (east, north; m/s, direction of motion).
    pub fn with_wind(surface_c: f64, surface_z: f64, rh: f64, wind_e: f64, wind_n: f64) -> Self {
        let levels = standard_levels(surface_c, surface_z, rh, wind_e, wind_n);
        Self {
            surface_c,
            surface_z,
            rh,
            wind: (wind_e, wind_n),
            col: Column::from_levels(&levels),
        }
    }
}

/// The same medium everywhere (tests, or a user-supplied sound speed).
pub struct UniformAtmosphere(pub Medium);

impl Atmosphere for UniformAtmosphere {
    fn medium(&self, _lat: f64, _lon: f64, _t: f64, _z_a: f64, _z_b: f64) -> Medium {
        self.0
    }
    fn describe(&self) -> String {
        format!(
            "uniform c={:.1} m/s, wind ({:.1}, {:.1}) m/s",
            self.0.c, self.0.wind_e, self.0.wind_n
        )
    }
    fn uniform_medium(&self) -> Option<Medium> {
        Some(self.0)
    }
}

/// Levels of a standard-lapse profile from a surface observation.
pub fn standard_levels(
    surface_c: f64,
    surface_z: f64,
    rh: f64,
    wind_e: f64,
    wind_n: f64,
) -> Vec<Level> {
    let top = 11_000.0f64.max(surface_z + 1000.0);
    vec![
        Level {
            z_msl: surface_z,
            t_c: surface_c,
            rh,
            wind_e,
            wind_n,
        },
        Level {
            z_msl: top,
            t_c: surface_c - 0.0065 * (top - surface_z),
            rh: 0.1,
            wind_e,
            wind_n,
        },
    ]
}

impl Atmosphere for StandardAtmosphere {
    fn medium(&self, _lat: f64, _lon: f64, _t: f64, z_a: f64, z_b: f64) -> Medium {
        self.col.mean(z_a, z_b)
    }
    fn describe(&self) -> String {
        format!(
            "standard atmosphere, surface {:.1} C at {:.0} m, lapse 6.5 K/km, wind ({:.1}, {:.1}) m/s",
            self.surface_c, self.surface_z, self.wind.0, self.wind.1
        )
    }
    fn medium_bounds(&self) -> Option<MediumBounds> {
        self.col.bounds
    }
}

/// Soundings on a regular lat/lon/hour grid, interpolated bilinearly in space and linearly
/// in time.
pub struct GriddedAtmosphere {
    pub lats: Vec<f64>,
    pub lons: Vec<f64>,
    pub t0: f64,
    pub dt: f64,
    pub nt: usize,
    pub source: String,
    cols: Vec<Column>,
}

impl GriddedAtmosphere {
    /// `profiles[(it * lats.len() + ia) * lons.len() + io]` = levels at (lats[ia], lons[io],
    /// t0 + it * dt). `lats` and `lons` ascending.
    pub fn new(
        lats: Vec<f64>,
        lons: Vec<f64>,
        t0: f64,
        dt: f64,
        profiles: &[Vec<Level>],
        source: String,
    ) -> Result<Self, String> {
        let nt = profiles.len() / (lats.len() * lons.len()).max(1);
        if lats.is_empty()
            || lons.is_empty()
            || nt == 0
            || nt * lats.len() * lons.len() != profiles.len()
        {
            return Err(format!(
                "gridded atmosphere: {} profiles for {}x{} grid",
                profiles.len(),
                lats.len(),
                lons.len()
            ));
        }
        let cols = profiles.iter().map(|p| Column::from_levels(p)).collect();
        Ok(Self {
            lats,
            lons,
            t0,
            dt,
            nt,
            source,
            cols,
        })
    }

    fn axis(v: &[f64], x: f64) -> (usize, usize, f64) {
        if v.len() == 1 || x <= v[0] {
            return (0, 0, 0.0);
        }
        if x >= v[v.len() - 1] {
            return (v.len() - 1, v.len() - 1, 0.0);
        }
        let k = v.partition_point(|&a| a <= x).max(1);
        (k - 1, k, (x - v[k - 1]) / (v[k] - v[k - 1]))
    }
}

impl Atmosphere for GriddedAtmosphere {
    fn medium(&self, lat: f64, lon: f64, t: f64, z_a: f64, z_b: f64) -> Medium {
        let (a0, a1, wa) = Self::axis(&self.lats, lat);
        let (o0, o1, wo) = Self::axis(&self.lons, lon);
        let u = ((t - self.t0) / self.dt).clamp(0.0, (self.nt - 1) as f64);
        let t0 = (u.floor() as usize).min(self.nt - 1);
        let t1 = (t0 + 1).min(self.nt - 1);
        let wt = u - t0 as f64;
        let (nla, nlo) = (self.lats.len(), self.lons.len());
        let mut acc = (0.0, 0.0, 0.0);
        for (it, w_t) in [(t0, 1.0 - wt), (t1, wt)] {
            for (ia, w_a) in [(a0, 1.0 - wa), (a1, wa)] {
                for (io, w_o) in [(o0, 1.0 - wo), (o1, wo)] {
                    let w = w_t * w_a * w_o;
                    if w <= 0.0 {
                        continue;
                    }
                    let m = self.cols[(it * nla + ia) * nlo + io].mean(z_a, z_b);
                    // interpolate slowness (harmonic mean stays a harmonic mean)
                    acc.0 += w / m.c;
                    acc.1 += w * m.wind_e;
                    acc.2 += w * m.wind_n;
                }
            }
        }
        Medium {
            c: 1.0 / acc.0,
            wind_e: acc.1,
            wind_n: acc.2,
        }
    }
    fn describe(&self) -> String {
        format!(
            "{} ({}x{} grid, {} hours)",
            self.source,
            self.lats.len(),
            self.lons.len(),
            self.nt
        )
    }
    fn medium_bounds(&self) -> Option<MediumBounds> {
        let ordered = |axis: &[f64]| {
            !axis.is_empty()
                && axis.iter().all(|x| x.is_finite())
                && axis.windows(2).all(|w| w[0] < w[1])
        };
        if !(self.dt > 0.0 && self.dt.is_finite() && self.t0.is_finite())
            || !ordered(&self.lats)
            || !ordered(&self.lons)
            || self.nt == 0
            || self
                .nt
                .checked_mul(self.lats.len())?
                .checked_mul(self.lons.len())?
                != self.cols.len()
        {
            return None;
        }
        MediumBounds::cover(self.cols.iter().map(|c| c.bounds), 64)
    }
}

/// Soundings at scattered stations (radiosondes): per station a time series of columns.
/// Soundings at one station: (lat, lon, [(t, levels)]).
pub type StationSoundings = (f64, f64, Vec<(f64, Vec<Level>)>);
/// One station's resampled columns: (lat, lon, [(t, column)] sorted by t).
type StationColumns = (f64, f64, Vec<(f64, Column)>);

/// Inverse-distance-squared weighting across stations (in slowness), linear in time.
pub struct ScatteredAtmosphere {
    stations: Vec<StationColumns>,
    pub source: String,
}

impl ScatteredAtmosphere {
    pub fn new(stations: Vec<StationSoundings>, source: String) -> Result<Self, String> {
        let st: Vec<StationColumns> = stations
            .into_iter()
            .filter(|s| !s.2.is_empty())
            .map(|(la, lo, mut v)| {
                v.sort_by(|a, b| a.0.total_cmp(&b.0));
                (
                    la,
                    lo,
                    v.into_iter()
                        .map(|(t, l)| (t, Column::from_levels(&l)))
                        .collect(),
                )
            })
            .collect();
        if st.is_empty() {
            return Err("no soundings".into());
        }
        Ok(Self {
            stations: st,
            source,
        })
    }
    fn at_station(series: &[(f64, Column)], t: f64, za: f64, zb: f64) -> Medium {
        let k = series.partition_point(|s| s.0 <= t);
        if k == 0 {
            return series[0].1.mean(za, zb);
        }
        if k >= series.len() {
            return series[series.len() - 1].1.mean(za, zb);
        }
        let (a, b) = (&series[k - 1], &series[k]);
        let w = (t - a.0) / (b.0 - a.0);
        let (ma, mb) = (a.1.mean(za, zb), b.1.mean(za, zb));
        Medium {
            c: 1.0 / ((1.0 - w) / ma.c + w / mb.c),
            wind_e: (1.0 - w) * ma.wind_e + w * mb.wind_e,
            wind_n: (1.0 - w) * ma.wind_n + w * mb.wind_n,
        }
    }
}

impl Atmosphere for ScatteredAtmosphere {
    fn medium(&self, lat: f64, lon: f64, t: f64, z_a: f64, z_b: f64) -> Medium {
        let mut acc = (0.0, 0.0, 0.0, 0.0);
        for (la, lo, series) in &self.stations {
            let d = crate::geo::haversine_m(lat, lon, *la, *lo).max(5000.0);
            let w = 1.0 / (d * d);
            let m = Self::at_station(series, t, z_a, z_b);
            acc.0 += w / m.c;
            acc.1 += w * m.wind_e;
            acc.2 += w * m.wind_n;
            acc.3 += w;
        }
        Medium {
            c: acc.3 / acc.0,
            wind_e: acc.1 / acc.3,
            wind_n: acc.2 / acc.3,
        }
    }
    fn describe(&self) -> String {
        format!("{} ({} stations)", self.source, self.stations.len())
    }
    fn medium_bounds(&self) -> Option<MediumBounds> {
        if self.stations.iter().any(|(lat, lon, series)| {
            !lat.is_finite()
                || !lon.is_finite()
                || series.iter().any(|(t, _)| !t.is_finite())
                || series.windows(2).any(|w| w[0].0 >= w[1].0)
        }) {
            return None;
        }
        MediumBounds::cover(
            self.stations
                .iter()
                .flat_map(|(_, _, series)| series.iter().map(|(_, column)| column.bounds)),
            self.stations.len().saturating_mul(16).saturating_add(64),
        )
    }
}

/// Air temperature (C, dry) that has sound speed `c`.
pub fn temp_of_c(c: f64) -> f64 {
    273.15 * ((c / 331.3).powi(2) - 1.0)
}

/// A base atmosphere whose boundary layer is pulled towards hourly surface observations: the
/// surface temperature difference dT(t) = T_obs - T_base is applied in full at the ground and
/// tapers linearly to zero `depth` metres above it (soundings are 12 h apart, while the
/// afternoon surface layer can be 5+ C warmer than the evening sounding).
pub struct AdjustedAtmosphere {
    pub base: Box<dyn Atmosphere>,
    pub surface: Box<dyn Atmosphere>,
    pub depth: f64,
}

impl Atmosphere for AdjustedAtmosphere {
    fn medium(&self, lat: f64, lon: f64, t: f64, z_a: f64, z_b: f64) -> Medium {
        let m = self.base.medium(lat, lon, t, z_a, z_b);
        let zs = z_a.min(z_b);
        let t_base = temp_of_c(self.base.medium(lat, lon, t, zs, zs).c);
        let t_obs = temp_of_c(self.surface.medium(lat, lon, t, zs, zs).c);
        let dt = (t_obs - t_base).clamp(-15.0, 15.0);
        if dt.abs() < 0.05 {
            return m;
        }
        // slowness change at height z: -(dc/dT) dT taper(z) / c^2, dc/dT = c / (2 (T + 273.15))
        let ds0 = -(m.c / (2.0 * (t_base + 273.15))) * dt / (m.c * m.c);
        let (lo, hi) = (zs, z_a.max(z_b));
        let h = (hi - lo).max(1.0);
        let top = (lo + self.depth).min(hi.max(lo + 1.0));
        // integral of (1 - (z - zs)/depth) from lo to top
        let u = top - lo;
        let integral = u * (1.0 - 0.5 * (u / self.depth));
        let mean_ds = ds0 * integral / h.max(u);
        Medium {
            c: 1.0 / (1.0 / m.c + mean_ds),
            ..m
        }
    }
    fn describe(&self) -> String {
        format!(
            "{}, boundary layer adjusted to {}",
            self.base.describe(),
            self.surface.describe()
        )
    }
    fn medium_bounds(&self) -> Option<MediumBounds> {
        if !(self.depth > 0.0 && self.depth.is_finite()) {
            return None;
        }
        let base = self.base.medium_bounds()?.checked()?;
        // Use the same monotone Kelvin calculation as `medium`, including cancellation
        // in temp_of_c + 273.15. Pad the correction before subtracting it from one:
        // padding only the final c cannot bound a nearly cancelled slowness denominator.
        let kelvin_min = temp_of_c(base.c[0]) + 273.15;
        let change = (15.0 / (2.0 * kelvin_min) * (1.0 + 64.0 * f64::EPSILON)).next_up();
        if !(kelvin_min.is_finite() && change >= 0.0 && change < 1.0) {
            return None;
        }
        // c' = c / (1 - dT*q/(2*K_surface)), with |dT| <= 15 K and 0 <= q <= 1.
        // The surface provider need not have bounds: its correction is already clamped.
        MediumBounds {
            c: [base.c[0] / (1.0 + change), base.c[1] / (1.0 - change)],
            wind_max: base.wind_max,
        }
        .outward(64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_enclosed(bounds: MediumBounds, medium: Medium) {
        assert!(
            (bounds.c[0]..=bounds.c[1]).contains(&medium.c),
            "{medium:?} outside {bounds:?}"
        );
        assert!(
            medium.wind_e.hypot(medium.wind_n) <= bounds.wind_max,
            "{medium:?} outside {bounds:?}"
        );
    }

    fn layered_column() -> Column {
        Column::from_levels(&[
            Level {
                z_msl: -500.0,
                t_c: 25.0,
                rh: 0.6,
                wind_e: 17.0,
                wind_n: -31.0,
            },
            Level {
                z_msl: 900.0,
                t_c: 5.0,
                rh: 0.3,
                wind_e: -45.0,
                wind_n: 19.0,
            },
            Level {
                z_msl: 11_000.0,
                t_c: -50.0,
                rh: 0.1,
                wind_e: 60.0,
                wind_n: -25.0,
            },
        ])
    }

    // Previous arithmetic is an independent equivalence check at ordinary heights and
    // reproduces the loss of precision on a short path far outside the tabulated column.
    fn former_cumulative(v: &[f64], z: f64) -> f64 {
        let top = Z0 + (NZ - 1) as f64 * DZ;
        if z < Z0 {
            return v[0] - (Z0 - z) * (v[1] - v[0]) / DZ;
        }
        if z > top {
            return v[NZ - 1] + (z - top) * (v[NZ - 1] - v[NZ - 2]) / DZ;
        }
        let u = (z - Z0) / DZ;
        let i = (u.floor() as usize).min(NZ - 2);
        let w = u - i as f64;
        v[i] * (1.0 - w) + v[i + 1] * w
    }

    #[test]
    fn stable_column_mean_preserves_ordinary_paths_and_its_bounds() {
        let col = layered_column();
        let bounds = col.bounds.unwrap();
        let heights = [
            -700.0, -500.0, -499.7, -449.5, 0.0, 137.2, 899.9, 900.0, 4500.25, 15_000.0, 25_000.0,
        ];
        for za in heights {
            for zb in heights {
                let m = col.mean(za, zb);
                assert_eq!(m, col.mean(zb, za));
                assert_enclosed(bounds, m);
                let lo = za.min(zb);
                let hi = za.max(zb);
                let (lo, hi) = if hi - lo < 1.0 {
                    (lo - 0.5, lo + 0.5)
                } else {
                    (lo, hi)
                };
                let h = hi - lo;
                let old = Medium {
                    c: h / (former_cumulative(&col.s, hi) - former_cumulative(&col.s, lo)),
                    wind_e: (former_cumulative(&col.we, hi) - former_cumulative(&col.we, lo)) / h,
                    wind_n: (former_cumulative(&col.wn, hi) - former_cumulative(&col.wn, lo)) / h,
                };
                assert!((m.c - old.c).abs() < 1e-8, "{za}..{zb}: {m:?} vs {old:?}");
                assert!((m.wind_e - old.wind_e).abs() < 1e-8);
                assert!((m.wind_n - old.wind_n).abs() < 1e-8);
            }
        }
    }

    #[test]
    fn path_ending_next_to_a_grid_boundary_stays_convex() {
        let levels: Vec<_> = [(8100.0, 100.0), (8150.0, 100.0), (8200.0, -100.0)]
            .into_iter()
            .map(|(z_msl, wind_e)| Level {
                z_msl,
                t_c: 20.0,
                rh: 0.0,
                wind_e,
                wind_n: 0.0,
            })
            .collect();
        let col = Column::from_levels(&levels);
        let bounds = col.bounds.unwrap();
        // A rounded cell index formerly gave the first interval a negative endpoint
        // weight, returning 100.00000000009095 m/s despite a 100 m/s maximum.
        for hi in [8150.0_f64.next_down(), 8150.0, 8150.0_f64.next_up()] {
            let m = col.mean(hi - 1.0, hi);
            assert!((0.0..=100.0).contains(&m.wind_e), "{hi}: {m:?}");
            assert_enclosed(bounds, m);
        }
    }

    #[test]
    fn extrapolated_short_paths_keep_the_actual_cell_medium() {
        let col = layered_column();
        let bounds = col.bounds.unwrap();
        for lo in [-1e100_f64, -1e16, 1e16, 1e100] {
            let hi = lo.next_up().next_up().next_up().next_up();
            let k = if lo < Z0 { 0 } else { NZ - 2 };
            let expected = Medium {
                c: 1.0 / Column::slope(&col.s, k),
                wind_e: Column::slope(&col.we, k),
                wind_n: Column::slope(&col.wn, k),
            };
            let m = col.mean(lo, hi);
            assert_eq!(m, expected);
            assert_enclosed(bounds, m);
        }
        assert_enclosed(bounds, col.mean(-1e100, 1e100));
        let lo = 1e16;
        let hi = lo + 8.0;
        let old_c = (hi - lo) / (former_cumulative(&col.s, hi) - former_cumulative(&col.s, lo));
        assert!(
            !(bounds.c[0]..=bounds.c[1]).contains(&old_c),
            "the previous cumulative subtraction must expose the cancellation: {old_c}"
        );
        for (lo, hi) in [(f64::NAN, 1.0), (0.0, f64::INFINITY), (-f64::MAX, f64::MAX)] {
            assert!(col.mean(lo, hi).c.is_nan());
        }
    }

    #[test]
    fn interpolated_providers_keep_certified_envelopes() {
        let profiles: Vec<_> = (0..8)
            .map(|i| {
                standard_levels(
                    -30.0 + 8.0 * f64::from(i),
                    0.0,
                    0.4,
                    20.0 * f64::from(i).sin(),
                    25.0 * f64::from(i).cos(),
                )
            })
            .collect();
        let grid = GriddedAtmosphere::new(
            vec![0.0, 1.0],
            vec![2.0, 4.0],
            0.0,
            3600.0,
            &profiles,
            "test".into(),
        )
        .unwrap();
        let stations = ScatteredAtmosphere::new(
            vec![
                (
                    0.0,
                    2.0,
                    vec![(0.0, profiles[0].clone()), (3600.0, profiles[4].clone())],
                ),
                (
                    1.0,
                    4.0,
                    vec![(0.0, profiles[3].clone()), (3600.0, profiles[7].clone())],
                ),
            ],
            "test".into(),
        )
        .unwrap();
        for provider in [&grid as &dyn Atmosphere, &stations] {
            let bounds = provider.medium_bounds().unwrap();
            for lat in [-5.0, 0.0, 0.25, 1.0, 10.0] {
                for lon in [-5.0, 2.0, 2.75, 4.0, 7.0] {
                    for time in [-1000.0, 0.0, 1234.5, 3600.0, 9000.0] {
                        for (za, zb) in [
                            (-700.0, 500.0),
                            (900.0, 900.0),
                            (0.0, 25_000.0),
                            (1e16, 1e16 + 8.0),
                        ] {
                            assert_enclosed(bounds, provider.medium(lat, lon, time, za, zb));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn adjusted_bounds_cover_both_clamped_temperature_corrections() {
        for temperature in [-100.0, 100.0] {
            for depth in [0.0001, 1.0, 1500.0, 1e100] {
                let atmo = AdjustedAtmosphere {
                    base: Box::new(StandardAtmosphere::with_wind(20.0, 0.0, 0.3, 3.0, -4.0)),
                    surface: Box::new(UniformAtmosphere(Medium::still(sound_speed(
                        temperature,
                        0.0,
                    )))),
                    depth,
                };
                let bounds = atmo.medium_bounds().unwrap();
                for (za, zb) in [
                    (0.0, 0.0),
                    (0.0, 1.0),
                    (0.0, 800.0),
                    (0.0, 6000.0),
                    (-1000.0, 25_000.0),
                ] {
                    assert_enclosed(bounds, atmo.medium(0.0, 0.0, 0.0, za, zb));
                }
            }
        }
    }

    #[test]
    fn adjusted_bounds_fail_open_at_a_nearly_cancelled_slowness() {
        for c in [54.897_360_671_316_21, 55.0, 60.0, 343.0] {
            let atmo = AdjustedAtmosphere {
                base: Box::new(UniformAtmosphere(Medium::still(c))),
                surface: Box::new(UniformAtmosphere(Medium::still(343.0))),
                depth: 1e100,
            };
            let m = atmo.medium(0.0, 0.0, 0.0, 0.0, 1.0);
            if c < 55.0 {
                assert!(
                    m.c > 1e15,
                    "the positive denominator is nearly cancelled: {m:?}"
                );
                assert!(atmo.medium_bounds().is_none());
            } else {
                assert_enclosed(atmo.medium_bounds().unwrap(), m);
            }
        }
    }

    #[test]
    fn unsupported_or_invalid_provider_bounds_fail_open() {
        struct Unknown;
        impl Atmosphere for Unknown {
            fn medium(&self, _: f64, _: f64, _: f64, _: f64, _: f64) -> Medium {
                Medium::still(343.0)
            }
            fn describe(&self) -> String {
                "unknown".into()
            }
        }
        assert!(Unknown.medium_bounds().is_none());
        assert_eq!(
            UniformAtmosphere(Medium::still(343.0)).medium_bounds(),
            Some(MediumBounds {
                c: [343.0; 2],
                wind_max: 0.0
            })
        );
        for m in [
            Medium::still(0.0),
            Medium::still(f64::NAN),
            Medium {
                c: 343.0,
                wind_e: 343.0,
                wind_n: 0.0,
            },
        ] {
            assert!(UniformAtmosphere(m).medium_bounds().is_none());
        }
        assert!(
            StandardAtmosphere::with_wind(15.0, 0.0, 0.5, f64::INFINITY, 0.0)
                .medium_bounds()
                .is_none()
        );
        for depth in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let adjusted = AdjustedAtmosphere {
                base: Box::new(UniformAtmosphere(Medium::still(343.0))),
                surface: Box::new(Unknown),
                depth,
            };
            assert!(adjusted.medium_bounds().is_none());
        }
        let adjusted = AdjustedAtmosphere {
            base: Box::new(Unknown),
            surface: Box::new(Unknown),
            depth: 1000.0,
        };
        assert!(adjusted.medium_bounds().is_none());
    }

    #[test]
    fn only_constant_providers_advertise_a_uniform_medium() {
        for medium in [
            Medium::still(343.0),
            Medium::still(310.0),
            Medium {
                c: 343.0,
                wind_e: 48.0,
                wind_n: -64.0,
            },
        ] {
            let atmo = UniformAtmosphere(medium);
            assert_eq!(atmo.uniform_medium(), Some(medium));
            for (lat, lon, t, za, zb) in [
                (0.0, 0.0, 0.0, 0.0, 1000.0),
                (45.0, -100.0, 1_788_895_000.0, -1000.0, 25_000.0),
            ] {
                assert_eq!(atmo.medium(lat, lon, t, za, zb), medium);
            }
        }
        assert!(
            StandardAtmosphere::new(15.0, 0.0, 0.5)
                .uniform_medium()
                .is_none()
        );
    }

    #[test]
    fn sound_speed_values() {
        assert!((sound_speed(20.0, 0.0) - 343.2).abs() < 0.3);
        assert!((sound_speed(0.0, 0.0) - 331.3).abs() < 1e-9);
        let wet = sound_speed(30.0, 1.0) - sound_speed(30.0, 0.0);
        assert!(wet > 0.5 && wet < 2.5, "{wet}");
    }

    #[test]
    fn harmonic_mean_matches_closed_form() {
        // linear T profile: c = a sqrt(T); int dz / c has a closed form
        let (t0, z0, h) = (26.0, 30.0, 5557.0);
        let atm = StandardAtmosphere::new(t0, z0, 0.0);
        let m = atm.medium(0.0, 0.0, 0.0, z0, h);
        let a = 331.3 / 273.15f64.sqrt();
        let (tk0, l) = (t0 + 273.15, 0.0065);
        let tk1 = tk0 - l * (h - z0);
        let exact = a * l * (h - z0) / (2.0 * (tk0.sqrt() - tk1.sqrt()));
        assert!((m.c - exact).abs() < 0.05, "{} vs {exact}", m.c);
        assert!(m.c < sound_speed(t0, 0.0) - 8.0);
        // order of arguments does not matter; a zero-length path gives the local value
        assert_eq!(atm.medium(0.0, 0.0, 0.0, h, z0), m);
        let p = atm.medium(0.0, 0.0, 0.0, z0, z0);
        assert!((p.c - sound_speed(t0, 0.0)).abs() < 0.1);
    }

    #[test]
    fn a_path_reaching_past_the_grid_keeps_the_column_mean() {
        // A uniform column must average to its own c and wind over any path, including heights
        // below the grid's -500 m floor (terrain values over water reach -722 m) and above
        // its 15 km top.
        let col = Column::from_levels(&[Level {
            z_msl: 0.0,
            t_c: 15.0,
            rh: 0.0,
            wind_e: 3.0,
            wind_n: -4.0,
        }]);
        let c = sound_speed(15.0, 0.0);
        for (za, zb) in [
            (-100.0, 900.0),
            (-599.0, 419.0),
            (-722.0, 306.0),
            (-2000.0, -800.0),
            (100.0, 16_000.0),
            (-700.0, 20_000.0),
        ] {
            let m = col.mean(za, zb);
            assert!(
                (m.c / c - 1.0).abs() < 1e-9,
                "{za}..{zb} m: c {} vs {c}",
                m.c
            );
            assert!(
                (m.wind_e - 3.0).abs() < 1e-9 && (m.wind_n + 4.0).abs() < 1e-9,
                "{za}..{zb} m: wind {} {}",
                m.wind_e,
                m.wind_n
            );
        }
    }

    #[test]
    fn gridded_interpolates() {
        let lv = |t: f64, we: f64| {
            vec![Level {
                z_msl: 0.0,
                t_c: t,
                rh: 0.0,
                wind_e: we,
                wind_n: 0.0,
            }]
        };
        // 1 lat x 2 lons x 2 hours
        let prof = vec![lv(10.0, 0.0), lv(20.0, 4.0), lv(10.0, 0.0), lv(20.0, 4.0)];
        let g = GriddedAtmosphere::new(
            vec![40.0],
            vec![-74.0, -73.0],
            0.0,
            3600.0,
            &prof,
            "test".into(),
        )
        .unwrap();
        let m = g.medium(40.0, -73.5, 1800.0, 0.0, 100.0);
        assert!((m.wind_e - 2.0).abs() < 1e-9);
        let c_lo = sound_speed(10.0, 0.0);
        let c_hi = sound_speed(20.0, 0.0);
        assert!(m.c > c_lo && m.c < c_hi);
        assert!(
            (wind_en(10.0, 270.0).0 - 10.0).abs() < 1e-9,
            "west wind blows east"
        );
    }

    #[test]
    fn boundary_layer_adjustment() {
        let base = StandardAtmosphere::new(20.0, 0.0, 0.0);
        let surf = StandardAtmosphere::new(26.0, 0.0, 0.0);
        let adj = AdjustedAtmosphere {
            base: Box::new(StandardAtmosphere::new(20.0, 0.0, 0.0)),
            surface: Box::new(surf),
            depth: 1500.0,
        };
        // at the ground the adjusted value equals the observation
        let g = adj.medium(0.0, 0.0, 0.0, 0.0, 0.0);
        assert!((temp_of_c(g.c) - 26.0).abs() < 0.3, "{}", temp_of_c(g.c));
        // high paths change less than low ones, and both get faster
        let lo = adj.medium(0.0, 0.0, 0.0, 0.0, 800.0).c - base.medium(0.0, 0.0, 0.0, 0.0, 800.0).c;
        let hi =
            adj.medium(0.0, 0.0, 0.0, 0.0, 6000.0).c - base.medium(0.0, 0.0, 0.0, 0.0, 6000.0).c;
        assert!(lo > hi && hi > 0.0, "{lo} {hi}");
    }

    #[test]
    fn scattered_weights_nearest() {
        let lv = |t: f64| {
            vec![Level {
                z_msl: 0.0,
                t_c: t,
                rh: 0.0,
                wind_e: 0.0,
                wind_n: 0.0,
            }]
        };
        let a = ScatteredAtmosphere::new(
            vec![
                (40.0, -74.0, vec![(0.0, lv(10.0)), (43200.0, lv(20.0))]),
                (44.0, -74.0, vec![(0.0, lv(0.0))]),
            ],
            "t".into(),
        )
        .unwrap();
        let m = a.medium(40.0, -74.0, 21600.0, 0.0, 10.0);
        assert!((temp_of_c(m.c) - 15.0).abs() < 0.5, "{}", temp_of_c(m.c));
    }
}
