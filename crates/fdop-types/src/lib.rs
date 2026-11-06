//! Shared, dependency-light types for franken_doppler_finder.

pub mod atmo;
pub mod engine;
pub mod font;
pub mod geo;
pub mod store;
pub mod time;

/// Speed of sound used by the model (m/s); matches the reference fit tool.
pub const C_SOUND: f64 = 343.0;
/// Microphone height above ground (m).
pub const MIC_HEIGHT_M: f64 = 1.5;
pub const KT_TO_MPS: f64 = 0.514444;
pub const FT_TO_M: f64 = 0.3048;
pub const FPM_TO_MPS: f64 = 0.3048 / 60.0;

/// Cache root shared by every crate: `$FDOP_CACHE`, else `$FDOP_CACHE_DIR`, else
/// `$XDG_CACHE_HOME/fdop`, else `~/.cache/fdop`.
pub fn default_cache_dir() -> std::path::PathBuf {
    use std::path::PathBuf;
    for var in ["FDOP_CACHE", "FDOP_CACHE_DIR"] {
        if let Some(p) = std::env::var_os(var).filter(|p| !p.is_empty()) {
            return PathBuf::from(p);
        }
    }
    if let Some(p) = std::env::var_os("XDG_CACHE_HOME").filter(|p| !p.is_empty()) {
        return PathBuf::from(p).join("fdop");
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".cache")
        .join("fdop")
}

/// Ground elevation provider (metres above MSL). Implementations must be thread-safe.
pub trait Elevation: Sync + Send {
    fn elev_m(&self, lat: f64, lon: f64) -> f64;
    fn describe(&self) -> String;
}

/// Constant elevation (tests, or when terrain data is unavailable).
pub struct FlatElevation(pub f64);

impl Elevation for FlatElevation {
    fn elev_m(&self, _lat: f64, _lon: f64) -> f64 {
        self.0
    }
    fn describe(&self) -> String {
        format!("flat:{}", self.0)
    }
}

/// Geographic bounding box in degrees: south, west, north, east.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BBox {
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

impl BBox {
    pub fn new(south: f64, west: f64, north: f64, east: f64) -> Self {
        Self {
            south,
            west,
            north,
            east,
        }
    }
    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        lat >= self.south && lat <= self.north && lon >= self.west && lon <= self.east
    }
    /// Whether a line between two `(time, latitude, longitude)` samples enters both this
    /// box and the inclusive time window at the same fraction of the segment. This keeps
    /// interpolation anchors when neither reported point is inside a data-selection window;
    /// it does not create positions or replace the acoustic track interpolation.
    /// Longitudes follow the short way across the dateline; boxes themselves do not wrap.
    pub fn intersects_timed_segment(
        &self,
        a: (f64, f64, f64),
        b: (f64, f64, f64),
        time: (f64, f64),
    ) -> bool {
        if ![a.0, a.1, a.2, b.0, b.1, b.2]
            .into_iter()
            .all(f64::is_finite)
            || b.0 <= a.0
            || !(b.0 - a.0).is_finite()
            || !(-90.0..=90.0).contains(&a.1)
            || !(-90.0..=90.0).contains(&b.1)
            || !(-180.0..=180.0).contains(&a.2)
            || !(-180.0..=180.0).contains(&b.2)
            || !(self.south <= self.north && self.west <= self.east && time.0 <= time.1)
        {
            return false;
        }
        let clip = |span: &mut (f64, f64), start: f64, delta: f64, lo: f64, hi: f64| {
            if delta == 0.0 {
                return start >= lo && start <= hi;
            }
            let (u, v) = ((lo - start) / delta, (hi - start) / delta);
            span.0 = span.0.max(u.min(v));
            span.1 = span.1.min(u.max(v));
            span.0 <= span.1
        };
        let mut span = (0.0, 1.0);
        if !clip(&mut span, a.0, b.0 - a.0, time.0, time.1)
            || !clip(&mut span, a.1, b.1 - a.1, self.south, self.north)
        {
            return false;
        }
        let mut dlon = b.2 - a.2;
        if dlon > 180.0 {
            dlon -= 360.0;
        } else if dlon < -180.0 {
            dlon += 360.0;
        }
        [-360.0, 0.0, 360.0].into_iter().any(|shift| {
            let mut shifted = span;
            clip(
                &mut shifted,
                a.2,
                dlon,
                self.west + shift,
                self.east + shift,
            )
        })
    }
    pub fn center(&self) -> (f64, f64) {
        (
            0.5 * (self.south + self.north),
            0.5 * (self.west + self.east),
        )
    }
    /// Grow by `km` on every side (longitude scaled at the box's most poleward latitude).
    pub fn padded_km(&self, km: f64) -> Self {
        let dlat = km / 111.32;
        let lat_ext = self.south.abs().max(self.north.abs()).min(89.0);
        let dlon = km / (111.32 * lat_ext.to_radians().cos().max(0.01));
        Self {
            south: (self.south - dlat).max(-90.0),
            west: (self.west - dlon).max(-180.0),
            north: (self.north + dlat).min(90.0),
            east: (self.east + dlon).min(180.0),
        }
    }
    pub fn area_km2(&self) -> f64 {
        let (lat_c, _) = self.center();
        (self.north - self.south)
            * 111.32
            * (self.east - self.west)
            * 111.32
            * lat_c.to_radians().cos()
    }
    pub fn as_array(&self) -> [f64; 4] {
        [self.south, self.west, self.north, self.east]
    }
    pub fn union(&self, o: &BBox) -> BBox {
        BBox::new(
            self.south.min(o.south),
            self.west.min(o.west),
            self.north.max(o.north),
            self.east.max(o.east),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timed_segment_intersects_space_and_time_together() {
        let bbox = BBox::new(-1.0, -1.0, 1.0, 1.0);
        let (a, b) = ((0.0, 0.0, -2.0), (4.0, 0.0, 2.0));
        assert!(bbox.intersects_timed_segment(a, b, (1.0, 3.0)));
        assert!(bbox.intersects_timed_segment(a, b, (1.0, 1.0)));
        assert!(bbox.intersects_timed_segment(a, b, (f64::NEG_INFINITY, f64::INFINITY)));
        assert!(!bbox.intersects_timed_segment(a, b, (0.0, 0.9)));
        assert!(!bbox.intersects_timed_segment(a, b, (3.1, 4.0)));
        assert!(!bbox.intersects_timed_segment((0.0, -3.0, 0.0), (4.0, 0.0, 3.0), (0.0, 4.0)));
        assert!(!bbox.intersects_timed_segment((0.0, 2.0, -2.0), (4.0, 2.0, 2.0), (0.0, 4.0)));
        assert!(!bbox.intersects_timed_segment(b, a, (0.0, 4.0)));
        assert!(!bbox.intersects_timed_segment(a, (0.0, 0.0, 2.0), (0.0, 4.0)));
        assert!(!bbox.intersects_timed_segment(a, b, (3.0, 1.0)));
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for point in [(bad, b.1, b.2), (b.0, bad, b.2), (b.0, b.1, bad)] {
                assert!(!bbox.intersects_timed_segment(a, point, (0.0, 4.0)));
            }
        }
    }

    #[test]
    fn timed_segment_crosses_the_dateline_without_crossing_the_country() {
        let (a, b) = ((0.0, 0.0, 179.0), (4.0, 0.0, -179.0));
        assert!(!BBox::new(-1.0, -125.0, 1.0, -66.0).intersects_timed_segment(a, b, (0.0, 4.0)));
        let east = BBox::new(-1.0, 179.5, 1.0, 180.0);
        let west = BBox::new(-1.0, -180.0, 1.0, -179.5);
        assert!(east.intersects_timed_segment(a, b, (1.0, 2.0)));
        assert!(west.intersects_timed_segment(a, b, (2.0, 3.0)));
        assert!(!east.intersects_timed_segment(a, b, (3.0, 4.0)));
        assert!(!west.intersects_timed_segment(a, b, (0.0, 1.0)));
    }
}
