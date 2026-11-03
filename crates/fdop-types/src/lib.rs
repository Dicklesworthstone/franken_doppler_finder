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
