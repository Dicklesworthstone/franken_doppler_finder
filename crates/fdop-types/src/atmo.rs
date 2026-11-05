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
        Self { s, we, wn }
    }

    /// Cumulative integral at height `z`. Beyond the grid the column is constant (as
    /// `from_levels` defines it), so the integral keeps growing at the edge cell's rate. A
    /// clamp here would drop that part of a path while `mean` still divides by its full
    /// height: a terrain value of -600 m over water raised c from 338 to 375 m/s.
    #[inline]
    fn cum(v: &[f64], z: f64) -> f64 {
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

    /// Path average between two heights (MSL).
    pub fn mean(&self, za: f64, zb: f64) -> Medium {
        let (lo, hi) = if za <= zb { (za, zb) } else { (zb, za) };
        let (lo, hi) = if hi - lo < 1.0 {
            (lo - 0.5, lo + 0.5)
        } else {
            (lo, hi)
        };
        let h = hi - lo;
        let ds = Self::cum(&self.s, hi) - Self::cum(&self.s, lo);
        Medium {
            c: h / ds,
            wind_e: (Self::cum(&self.we, hi) - Self::cum(&self.we, lo)) / h,
            wind_n: (Self::cum(&self.wn, hi) - Self::cum(&self.wn, lo)) / h,
        }
    }
}

/// Atmosphere provider. `medium` returns the path average between heights `z_a` and `z_b`
/// (MSL) at a place and UTC time.
pub trait Atmosphere: Send + Sync {
    fn medium(&self, lat: f64, lon: f64, t: f64, z_a: f64, z_b: f64) -> Medium;
    fn describe(&self) -> String;
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
        let integral = u - u * u / (2.0 * self.depth);
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
