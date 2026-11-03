//! WGS84 geodesy: geodetic <-> ECEF, local ENU frames, great-circle helpers.
//!
//! Heights fed in are MSL (orthometric) for both observer and aircraft: the geoid offset is
//! common to both and cancels in the relative geometry to well under a metre.

pub const WGS84_A: f64 = 6_378_137.0;
pub const WGS84_F: f64 = 1.0 / 298.257_223_563;
pub const WGS84_E2: f64 = WGS84_F * (2.0 - WGS84_F);
pub const EARTH_R: f64 = 6_371_008.8;

pub type V3 = [f64; 3];

#[inline]
pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
#[inline]
pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
#[inline]
pub fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
#[inline]
pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
#[inline]
pub fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}

pub fn geodetic_to_ecef(lat_deg: f64, lon_deg: f64, h: f64) -> V3 {
    let (lat, lon) = (lat_deg.to_radians(), lon_deg.to_radians());
    let (s, c) = lat.sin_cos();
    let n = WGS84_A / (1.0 - WGS84_E2 * s * s).sqrt();
    [
        (n + h) * c * lon.cos(),
        (n + h) * c * lon.sin(),
        (n * (1.0 - WGS84_E2) + h) * s,
    ]
}

pub fn ecef_to_geodetic(p: V3) -> (f64, f64, f64) {
    let [x, y, z] = p;
    let lon = y.atan2(x);
    let pp = x.hypot(y);
    let mut lat = z.atan2(pp * (1.0 - WGS84_E2));
    let mut h = 0.0;
    for _ in 0..8 {
        let s = lat.sin();
        let n = WGS84_A / (1.0 - WGS84_E2 * s * s).sqrt();
        h = pp / lat.cos() - n;
        lat = z.atan2(pp * (1.0 - WGS84_E2 * n / (n + h)));
    }
    (lat.to_degrees(), lon.to_degrees(), h)
}

/// Local east-north-up frame tangent at (lat0, lon0, h0).
#[derive(Clone, Copy, Debug)]
pub struct Enu {
    pub lat0: f64,
    pub lon0: f64,
    pub h0: f64,
    o: V3,
    r: [[f64; 3]; 3],
}

impl Enu {
    pub fn new(lat0: f64, lon0: f64, h0: f64) -> Self {
        let (la, lo) = (lat0.to_radians(), lon0.to_radians());
        let r = [
            [-lo.sin(), lo.cos(), 0.0],
            [-la.sin() * lo.cos(), -la.sin() * lo.sin(), la.cos()],
            [la.cos() * lo.cos(), la.cos() * lo.sin(), la.sin()],
        ];
        Self {
            lat0,
            lon0,
            h0,
            o: geodetic_to_ecef(lat0, lon0, h0),
            r,
        }
    }
    pub fn from_ecef(&self, p: V3) -> V3 {
        let d = sub(p, self.o);
        [dot(self.r[0], d), dot(self.r[1], d), dot(self.r[2], d)]
    }
    /// Rotate an ECEF vector (velocity) into ENU components.
    pub fn vec_from_ecef(&self, v: V3) -> V3 {
        [dot(self.r[0], v), dot(self.r[1], v), dot(self.r[2], v)]
    }
    pub fn to_ecef(&self, e: V3) -> V3 {
        let r = &self.r;
        [
            self.o[0] + r[0][0] * e[0] + r[1][0] * e[1] + r[2][0] * e[2],
            self.o[1] + r[0][1] * e[0] + r[1][1] * e[1] + r[2][1] * e[2],
            self.o[2] + r[0][2] * e[0] + r[1][2] * e[1] + r[2][2] * e[2],
        ]
    }
    pub fn vec_to_ecef(&self, e: V3) -> V3 {
        let r = &self.r;
        [
            r[0][0] * e[0] + r[1][0] * e[1] + r[2][0] * e[2],
            r[0][1] * e[0] + r[1][1] * e[1] + r[2][1] * e[2],
            r[0][2] * e[0] + r[1][2] * e[1] + r[2][2] * e[2],
        ]
    }
    pub fn from_geodetic(&self, lat: f64, lon: f64, h: f64) -> V3 {
        self.from_ecef(geodetic_to_ecef(lat, lon, h))
    }
    pub fn to_geodetic(&self, e: V3) -> (f64, f64, f64) {
        ecef_to_geodetic(self.to_ecef(e))
    }
}

/// Great-circle distance (m) on the mean sphere.
pub fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_R * a.sqrt().min(1.0).asin()
}

/// Initial bearing (degrees clockwise from true north) from point 1 to point 2.
pub fn bearing_deg(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dl = (lon2 - lon1).to_radians();
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    (y.atan2(x).to_degrees() + 360.0) % 360.0
}

/// Point reached from (lat, lon) after `dist_m` along initial bearing `brg_deg`.
pub fn destination(lat: f64, lon: f64, brg_deg: f64, dist_m: f64) -> (f64, f64) {
    let d = dist_m / EARTH_R;
    let (p1, l1, b) = (lat.to_radians(), lon.to_radians(), brg_deg.to_radians());
    let p2 = (p1.sin() * d.cos() + p1.cos() * d.sin() * b.cos()).asin();
    let l2 = l1 + (b.sin() * d.sin() * p1.cos()).atan2(d.cos() - p1.sin() * p2.sin());
    (p2.to_degrees(), ((l2.to_degrees() + 540.0) % 360.0) - 180.0)
}

/// Compass point name for a bearing ("N", "NNE", ...).
pub fn compass16(brg_deg: f64) -> &'static str {
    const P: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ];
    P[(((brg_deg % 360.0) + 360.0 + 11.25) / 22.5) as usize % 16]
}

/// Metres per degree of latitude / longitude at `lat` (local equirectangular scale).
pub fn m_per_deg(lat: f64) -> (f64, f64) {
    let la = lat.to_radians();
    let mlat = 111_132.92 - 559.82 * (2.0 * la).cos() + 1.175 * (4.0 * la).cos();
    let mlon = 111_412.84 * la.cos() - 93.5 * (3.0 * la).cos();
    (mlat, mlon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecef_roundtrip() {
        let p = geodetic_to_ecef(40.876466, -73.524951, 31.5);
        let (la, lo, h) = ecef_to_geodetic(p);
        assert!(
            (la - 40.876466).abs() < 1e-9
                && (lo + 73.524951).abs() < 1e-9
                && (h - 31.5).abs() < 1e-4
        );
    }

    #[test]
    fn enu_roundtrip_and_axes() {
        let enu = Enu::new(40.0, -74.0, 0.0);
        let e = [1000.0, -2000.0, 300.0];
        let back = enu.from_ecef(enu.to_ecef(e));
        for k in 0..3 {
            assert!((back[k] - e[k]).abs() < 1e-6);
        }
        let north = enu.from_geodetic(40.01, -74.0, 0.0);
        assert!(north[1] > 1100.0 && north[0].abs() < 1e-6);
    }

    #[test]
    fn destination_inverts_haversine() {
        let (la, lo) = destination(40.0, -74.0, 63.0, 12_345.0);
        assert!((haversine_m(40.0, -74.0, la, lo) - 12_345.0).abs() < 0.5);
        assert!((bearing_deg(40.0, -74.0, la, lo) - 63.0).abs() < 0.01);
        assert_eq!(compass16(63.0), "ENE");
    }
}
